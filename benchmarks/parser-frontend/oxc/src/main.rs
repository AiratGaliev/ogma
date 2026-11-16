use std::{
    collections::BTreeMap,
    env, fs,
    hint::black_box,
    path::{Path, PathBuf},
    time::Instant,
};

use oxc_allocator::Allocator;
use oxc_parser::Parser;
use oxc_semantic::SemanticBuilder;
use oxc_span::SourceType;
use serde::Deserialize;
use serde_json::{Value, json};

const WARMUP_ROUNDS: usize = 2;
const MEASURED_ROUNDS: usize = 12;
const REPEATS_PER_FIXTURE: usize = 2_000;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    fixtures: Vec<FixtureMeta>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FixtureMeta {
    path: String,
    group: String,
    #[serde(default)]
    features: Vec<String>,
    expected_parse: bool,
    #[serde(default)]
    compare_parse: bool,
    #[serde(default)]
    expected_semantic_error: Option<bool>,
}

struct Fixture {
    meta: FixtureMeta,
    source: String,
}

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures")
}

fn load_fixtures() -> Result<Vec<Fixture>, Box<dyn std::error::Error>> {
    let root = fixture_root();
    let manifest: Manifest =
        serde_json::from_str(&fs::read_to_string(root.join("manifest.json"))?)?;
    let mut fixtures = Vec::with_capacity(manifest.fixtures.len());

    for meta in manifest.fixtures {
        let source = fs::read_to_string(root.join(&meta.path))?;
        fixtures.push(Fixture { meta, source });
    }

    Ok(fixtures)
}

fn source_type(path: &str) -> Result<SourceType, Box<dyn std::error::Error>> {
    Ok(SourceType::from_path(Path::new(path))?)
}

fn peak_rss_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
        let result = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
        if result == 0 {
            return Some((usage.ru_maxrss.max(0) as u64) * 1024);
        }
    }
    None
}

fn rate_per_second(operations: usize, elapsed_ns: u128) -> f64 {
    if elapsed_ns == 0 {
        0.0
    } else {
        operations as f64 * 1_000_000_000.0 / elapsed_ns as f64
    }
}

fn benchmark(fixtures: &[Fixture], full: bool) -> Result<Value, Box<dyn std::error::Error>> {
    let selected: Vec<&Fixture> = fixtures
        .iter()
        .filter(|fixture| fixture.meta.compare_parse && fixture.meta.expected_parse)
        .collect();
    if selected.is_empty() {
        return Err("no accepted parser benchmark fixtures found".into());
    }

    let source_bytes_per_round: usize = selected.iter().map(|item| item.source.len()).sum();
    let mut parse_elapsed_ns: u128 = 0;
    let mut semantic_elapsed_ns: u128 = 0;
    let mut arena_bytes_sum: u128 = 0;
    let mut arena_samples = 0usize;
    let mut max_arena_used_bytes = 0usize;
    let mut max_arena_capacity_bytes = 0usize;

    for round in 0..(WARMUP_ROUNDS + MEASURED_ROUNDS) {
        let measured = round >= WARMUP_ROUNDS;
        for fixture in &selected {
            for _ in 0..REPEATS_PER_FIXTURE {
                let parse_started = Instant::now();
                let allocator = Allocator::default();
                let kind = source_type(&fixture.meta.path)?;
                let parsed = Parser::new(&allocator, &fixture.source, kind).parse();
                let parse_ns = parse_started.elapsed().as_nanos();
            black_box(parsed.diagnostics.len());
            black_box(parsed.program.body.len());

            let mut semantic_ns = 0;
            if full {
                let semantic_started = Instant::now();
                let result = SemanticBuilder::new()
                    .with_check_syntax_error(true)
                    .build(&parsed.program);
                semantic_ns = semantic_started.elapsed().as_nanos();
                black_box(result.diagnostics.len());
                black_box(result.semantic);
            }

                if measured {
                    parse_elapsed_ns += parse_ns;
                    semantic_elapsed_ns += semantic_ns;
                    arena_bytes_sum += allocator.used_bytes() as u128;
                    arena_samples += 1;
                    max_arena_used_bytes = max_arena_used_bytes.max(allocator.used_bytes());
                    max_arena_capacity_bytes = max_arena_capacity_bytes.max(allocator.capacity());
                }
            }
        }
    }

    let operations = selected.len() * MEASURED_ROUNDS * REPEATS_PER_FIXTURE;
    let total_elapsed_ns = parse_elapsed_ns + semantic_elapsed_ns;
    let measured_source_bytes = source_bytes_per_round * MEASURED_ROUNDS * REPEATS_PER_FIXTURE;
    let pipeline_mib_per_second = if total_elapsed_ns == 0 {
        0.0
    } else {
        measured_source_bytes as f64
            / (1024.0 * 1024.0)
            / (total_elapsed_ns as f64 / 1_000_000_000.0)
    };

    Ok(json!({
        "schemaVersion": 1,
        "backend": "oxc",
        "mode": if full { "parse-plus-semantic" } else { "parse-only" },
        "fixtureCountPerRound": selected.len(),
        "warmupRounds": WARMUP_ROUNDS,
        "measuredRounds": MEASURED_ROUNDS,
        "repeatsPerFixture": REPEATS_PER_FIXTURE,
        "operations": operations,
        "sourceBytesPerRound": source_bytes_per_round,
        "measuredSourceBytes": measured_source_bytes,
        "parseElapsedNs": parse_elapsed_ns,
        "semanticElapsedNs": semantic_elapsed_ns,
        "totalElapsedNs": total_elapsed_ns,
        "parseFilesPerSecond": rate_per_second(operations, parse_elapsed_ns),
        "pipelineFilesPerSecond": rate_per_second(operations, total_elapsed_ns),
        "pipelineMiBPerSecond": pipeline_mib_per_second,
        "peakRssBytes": peak_rss_bytes(),
        "maxArenaUsedBytesPerFile": max_arena_used_bytes,
        "meanArenaUsedBytesPerFile": if arena_samples == 0 { 0.0 } else { arena_bytes_sum as f64 / arena_samples as f64 },
        "maxArenaCapacityBytesPerFile": max_arena_capacity_bytes
    }))
}

fn diagnostics(fixtures: &[Fixture]) -> Result<Value, Box<dyn std::error::Error>> {
    let mut parse_mismatches = Vec::new();
    let mut semantic_mismatches = Vec::new();
    let mut groups = BTreeMap::<String, usize>::new();
    let mut features = BTreeMap::<String, usize>::new();
    let mut compared_parse = 0usize;
    let mut parse_accepted = 0usize;
    let mut parser_diagnostic_count = 0usize;
    let mut recovered_after_parser_error = 0usize;
    let mut semantic_checks_performed = 0usize;
    let mut semantic_errors_observed = 0usize;
    let mut semantic_checks_skipped = 0usize;

    for fixture in fixtures {
        *groups.entry(fixture.meta.group.clone()).or_default() += 1;
        for feature in &fixture.meta.features {
            *features.entry(feature.clone()).or_default() += 1;
        }

        let allocator = Allocator::default();
        let kind = source_type(&fixture.meta.path)?;
        let parsed = Parser::new(&allocator, &fixture.source, kind).parse();
        let parser_errors = parsed.diagnostics.len();
        let accepted = parser_errors == 0;
        parser_diagnostic_count += parser_errors;
        parse_accepted += usize::from(accepted);

        if fixture.meta.compare_parse {
            compared_parse += 1;
            if accepted != fixture.meta.expected_parse {
                parse_mismatches.push(fixture.meta.path.clone());
            }
        }
        if !accepted && !parsed.program.body.is_empty() {
            recovered_after_parser_error += 1;
        }

        if let Some(expected_error) = fixture.meta.expected_semantic_error {
            if !parsed.program.body.is_empty() {
                let result = SemanticBuilder::new()
                    .with_check_syntax_error(true)
                    .build(&parsed.program);
                let observed_error = !result.diagnostics.is_empty();
                semantic_checks_performed += 1;
                semantic_errors_observed += usize::from(observed_error);
                if observed_error != expected_error {
                    semantic_mismatches.push(fixture.meta.path.clone());
                }
                black_box(result.semantic);
            } else {
                semantic_checks_skipped += 1;
                if expected_error {
                    semantic_mismatches.push(fixture.meta.path.clone());
                }
            }
        }
    }

    Ok(json!({
        "schemaVersion": 1,
        "backend": "oxc",
        "semanticStageAvailable": true,
        "fixtureCount": fixtures.len(),
        "fixtureGroups": groups,
        "featureFixtureCounts": features,
        "parseComparedFixtureCount": compared_parse,
        "parseAcceptedFixtureCount": parse_accepted,
        "parserDiagnosticCount": parser_diagnostic_count,
        "recoveredAfterParserErrorFixtureCount": recovered_after_parser_error,
        "parseMismatches": parse_mismatches,
        "semanticChecksPerformed": semantic_checks_performed,
        "semanticErrorsObserved": semantic_errors_observed,
        "semanticChecksSkipped": semantic_checks_skipped,
        "semanticMismatches": semantic_mismatches,
        "peakRssBytes": peak_rss_bytes()
    }))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mode = env::args().nth(1).unwrap_or_else(|| "parse".to_string());
    let fixtures = load_fixtures()?;
    let result = match mode.as_str() {
        "parse" => benchmark(&fixtures, false)?,
        "full" => benchmark(&fixtures, true)?,
        "diagnostics" => diagnostics(&fixtures)?,
        other => {
            return Err(
                format!("unknown mode: {other}; expected parse, full or diagnostics").into(),
            );
        }
    };
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_manifest_covers_parser_semantic_and_consumer_cases() {
        let fixtures = load_fixtures().expect("manifest and fixture files must load");
        assert!(
            fixtures
                .iter()
                .any(|item| item.meta.compare_parse && item.meta.expected_parse)
        );
        assert!(
            fixtures
                .iter()
                .any(|item| item.meta.compare_parse && !item.meta.expected_parse)
        );
        assert!(
            fixtures
                .iter()
                .any(|item| item.meta.expected_semantic_error == Some(true))
        );
        assert!(fixtures.iter().any(|item| item.meta.group == "consumer"));
    }
}
