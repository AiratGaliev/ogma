use std::{collections::BTreeMap, env, fs, hint::black_box, path::PathBuf, time::Instant};

use serde::Deserialize;
use serde_json::{Value, json};
use swc_common::{FileName, SourceMap, sync::Lrc};
use swc_ecma_ast::EsVersion;
use swc_ecma_parser::{EsSyntax, Parser, StringInput, Syntax, lexer::Lexer};

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

fn parse_one(fixture: &Fixture) -> Result<(bool, bool, usize, u128), Box<dyn std::error::Error>> {
    // Include SWC source-map and parser front-end setup in the timed operation.
    let started = Instant::now();
    let source_map: Lrc<SourceMap> = Default::default();
    let source_file = source_map.new_source_file(
        FileName::Custom(fixture.meta.path.clone()).into(),
        fixture.source.clone(),
    );
    let syntax = Syntax::Es(EsSyntax::default());
    let lexer = Lexer::new(
        syntax,
        EsVersion::latest(),
        StringInput::from(&*source_file),
        None,
    );
    let mut parser = Parser::new_from(lexer);
    let parsed = if fixture.meta.path.ends_with(".mjs") {
        parser.parse_module().map(|_| ())
    } else {
        parser.parse_script().map(|_| ())
    };
    let elapsed_ns = started.elapsed().as_nanos();
    let recoverable_errors = parser.take_errors();
    let recovered = parsed.is_ok() && !recoverable_errors.is_empty();
    let error_count = recoverable_errors.len() + usize::from(parsed.is_err());
    Ok((error_count == 0, recovered, error_count, elapsed_ns))
}

fn benchmark(fixtures: &[Fixture]) -> Result<Value, Box<dyn std::error::Error>> {
    let selected: Vec<&Fixture> = fixtures
        .iter()
        .filter(|fixture| fixture.meta.compare_parse && fixture.meta.expected_parse)
        .collect();
    if selected.is_empty() {
        return Err("no accepted parser benchmark fixtures found".into());
    }

    let source_bytes_per_round: usize = selected.iter().map(|item| item.source.len()).sum();
    let mut parse_elapsed_ns = 0u128;
    for round in 0..(WARMUP_ROUNDS + MEASURED_ROUNDS) {
        let measured = round >= WARMUP_ROUNDS;
        for fixture in &selected {
            for _ in 0..REPEATS_PER_FIXTURE {
                let (_, _, _, elapsed_ns) = parse_one(fixture)?;
                black_box(elapsed_ns);
                if measured {
                    parse_elapsed_ns += elapsed_ns;
                }
            }
        }
    }

    let operations = selected.len() * MEASURED_ROUNDS * REPEATS_PER_FIXTURE;
    let measured_source_bytes = source_bytes_per_round * MEASURED_ROUNDS * REPEATS_PER_FIXTURE;
    let mib_per_second = if parse_elapsed_ns == 0 {
        0.0
    } else {
        measured_source_bytes as f64
            / (1024.0 * 1024.0)
            / (parse_elapsed_ns as f64 / 1_000_000_000.0)
    };

    Ok(json!({
        "schemaVersion": 1,
        "backend": "swc",
        "mode": "parse-only",
        "timingScope": "in-memory SWC SourceMap/SourceFile construction (including source String clone) + lexer/parser construction + parse; fixture disk I/O/preloading excluded",
        "fixtureCountPerRound": selected.len(),
        "warmupRounds": WARMUP_ROUNDS,
        "measuredRounds": MEASURED_ROUNDS,
        "repeatsPerFixture": REPEATS_PER_FIXTURE,
        "operations": operations,
        "sourceBytesPerRound": source_bytes_per_round,
        "measuredSourceBytes": measured_source_bytes,
        "parseElapsedNs": parse_elapsed_ns,
        "semanticElapsedNs": null,
        "totalElapsedNs": parse_elapsed_ns,
        "parseFilesPerSecond": rate_per_second(operations, parse_elapsed_ns),
        "pipelineFilesPerSecond": rate_per_second(operations, parse_elapsed_ns),
        "pipelineMiBPerSecond": mib_per_second,
        "peakRssBytes": peak_rss_bytes(),
        "arenaMemoryMetric": null,
        "note": "Parser-only result. No SWC semantic diagnostics phase is represented in this comparison."
    }))
}

fn diagnostics(fixtures: &[Fixture]) -> Result<Value, Box<dyn std::error::Error>> {
    let mut parse_mismatches = Vec::new();
    let mut groups = BTreeMap::<String, usize>::new();
    let mut features = BTreeMap::<String, usize>::new();
    let mut compared_parse = 0usize;
    let mut parse_accepted = 0usize;
    let mut parser_diagnostic_count = 0usize;
    let mut recovered_after_parser_error = 0usize;

    for fixture in fixtures {
        *groups.entry(fixture.meta.group.clone()).or_default() += 1;
        for feature in &fixture.meta.features {
            *features.entry(feature.clone()).or_default() += 1;
        }

        let (accepted, recovered, errors, _) = parse_one(fixture)?;
        parser_diagnostic_count += errors;
        parse_accepted += usize::from(accepted);
        recovered_after_parser_error += usize::from(recovered);

        if fixture.meta.compare_parse {
            compared_parse += 1;
            if accepted != fixture.meta.expected_parse {
                parse_mismatches.push(fixture.meta.path.clone());
            }
        }
    }

    Ok(json!({
        "schemaVersion": 1,
        "backend": "swc",
        "semanticStageAvailable": false,
        "semanticChecksPerformed": 0,
        "semanticCoverage": "not measured: swc_ecma_parser is parser-only; SWC resolver scope/context annotations are not an equivalent semantic-diagnostics phase",
        "fixtureCount": fixtures.len(),
        "fixtureGroups": groups,
        "featureFixtureCounts": features,
        "parseComparedFixtureCount": compared_parse,
        "parseAcceptedFixtureCount": parse_accepted,
        "parserDiagnosticCount": parser_diagnostic_count,
        "recoveredAfterParserErrorFixtureCount": recovered_after_parser_error,
        "parseMismatches": parse_mismatches,
        "peakRssBytes": peak_rss_bytes()
    }))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mode = env::args().nth(1).unwrap_or_else(|| "parse".to_string());
    let fixtures = load_fixtures()?;
    let result = match mode.as_str() {
        "parse" => benchmark(&fixtures)?,
        "diagnostics" => diagnostics(&fixtures)?,
        other => {
            return Err(format!("unknown mode: {other}; expected parse or diagnostics").into());
        }
    };
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_manifest_covers_parser_and_consumer_cases() {
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
        assert!(fixtures.iter().any(|item| item.meta.group == "consumer"));
        assert!(fixtures.iter().all(|item| !item.meta.features.is_empty()));
    }
}
