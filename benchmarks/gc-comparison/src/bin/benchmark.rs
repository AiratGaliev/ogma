use ogma_gc_comparison_proof::{CollectorKind, FrameKind, GcHeap, GraphShape};
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;
use std::time::Instant;

#[derive(Clone)]
struct Config {
    objects: usize,
    warmups: usize,
    samples: usize,
    revision: String,
    tested_revision: String,
    cpu: String,
    output: PathBuf,
}

#[derive(Clone, Copy)]
enum Workload {
    BalancedRoots,
    HostRetained,
    AllocationChurn,
}

impl Workload {
    const ALL: [Self; 3] = [
        Self::BalancedRoots,
        Self::HostRetained,
        Self::AllocationChurn,
    ];

    const fn name(self) -> &'static str {
        match self {
            Self::BalancedRoots => "balanced-roots-and-cycles",
            Self::HostRetained => "persistent-host-retention",
            Self::AllocationChurn => "allocation-churn",
        }
    }
}

#[derive(Clone)]
struct Sample {
    build_ns: u128,
    collection_ns: u128,
    root_scan_ns: u128,
    roots_scanned: usize,
    edges_examined: u64,
    objects_before: u64,
    objects_after: u64,
    reclaimed_total: u64,
    reclaimed_during_collection: usize,
    peak_heap_bytes: u64,
    live_heap_bytes_after: u64,
    refcount_operations: u64,
    graph: GraphShape,
}

struct Summary {
    workload: &'static str,
    collector: &'static str,
    samples: Vec<Sample>,
}

fn argument(args: &[String], name: &str, default: &str) -> String {
    args.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
        .unwrap_or_else(|| default.to_owned())
}

fn parse_config() -> Result<Config, String> {
    let args: Vec<String> = env::args().skip(1).collect();
    let objects = argument(&args, "--objects", "2048")
        .parse::<usize>()
        .map_err(|_| "invalid --objects".to_owned())?;
    let warmups = argument(&args, "--warmups", "3")
        .parse::<usize>()
        .map_err(|_| "invalid --warmups".to_owned())?;
    let samples = argument(&args, "--samples", "101")
        .parse::<usize>()
        .map_err(|_| "invalid --samples".to_owned())?;

    if objects < 64 || objects % 4 != 0 || warmups == 0 || samples < 101 {
        return Err(
            "--objects must be >=64 and divisible by 4; warmups > 0; samples >=101".to_owned(),
        );
    }

    Ok(Config {
        objects,
        warmups,
        samples,
        revision: argument(&args, "--revision", "unknown"),
        tested_revision: argument(&args, "--tested-revision", "unknown"),
        cpu: argument(&args, "--cpu", "not supplied"),
        output: PathBuf::from(argument(
            &args,
            "--output",
            "ogm-26-gc-comparison-report.json",
        )),
    })
}

fn build_graph(
    collector: CollectorKind,
    workload: Workload,
    object_count: usize,
) -> Result<(GcHeap, GraphShape), String> {
    let mut heap = GcHeap::new(collector);
    let objects: Vec<_> = (0..object_count)
        .map(|index| heap.allocate(16 + (index % 4) * 16))
        .collect();

    // Every four objects form the same cyclic component for both collectors.
    for base in (0..object_count).step_by(4) {
        heap.add_edge(objects[base], objects[base + 1])
            .map_err(err)?;
        heap.add_edge(objects[base + 1], objects[base + 2])
            .map_err(err)?;
        heap.add_edge(objects[base + 2], objects[base + 3])
            .map_err(err)?;
        heap.add_edge(objects[base + 3], objects[base])
            .map_err(err)?;
        let component = base / 4;
        if component % 7 == 0 && base + 4 < object_count {
            heap.add_edge(objects[base], objects[base + 5])
                .map_err(err)?;
        }
    }

    // Frames have strict LIFO semantics: populate the interpreter frame before
    // opening the native prototype frame, then leave both alive for collection.
    let interpreter = heap.begin_frame(FrameKind::Interpreter).map_err(err)?;
    let scope = heap.open_scope().map_err(err)?;
    let components = object_count / 4;

    for component in 0..components {
        let base = component * 4;
        match workload {
            Workload::BalancedRoots => match component % 5 {
                0 => heap
                    .add_frame_root(interpreter, objects[base])
                    .map_err(err)?,
                2 => {
                    heap.retain_host_handle(objects[base]).map_err(err)?;
                }
                3 => heap.scope_add(scope, objects[base]).map_err(err)?,
                _ => {}
            },
            Workload::HostRetained => match component % 6 {
                0 | 2 => {
                    heap.retain_host_handle(objects[base]).map_err(err)?;
                }
                1 => heap
                    .add_frame_root(interpreter, objects[base])
                    .map_err(err)?,
                4 => heap.scope_add(scope, objects[base]).map_err(err)?,
                _ => {}
            },
            Workload::AllocationChurn => {
                heap.scope_add(scope, objects[base]).map_err(err)?;
                if component % 16 == 0 {
                    heap.retain_host_handle(objects[base + 1]).map_err(err)?;
                }
                if component == 0 {
                    heap.add_frame_root(interpreter, objects[base + 2])
                        .map_err(err)?;
                }
            }
        }
    }

    let native = heap.begin_frame(FrameKind::NativePrototype).map_err(err)?;
    for component in 0..components {
        let base = component * 4;
        match workload {
            Workload::BalancedRoots if component % 5 == 1 => {
                heap.add_frame_root(native, objects[base]).map_err(err)?;
            }
            Workload::HostRetained if component % 6 == 3 => {
                heap.add_frame_root(native, objects[base]).map_err(err)?;
            }
            Workload::AllocationChurn if component == 1 => {
                heap.add_frame_root(native, objects[base + 3])
                    .map_err(err)?;
            }
            _ => {}
        }
    }

    if matches!(workload, Workload::AllocationChurn) {
        // Releasing temporary roots leaves persistent host/frame roots intact.
        heap.close_scope(scope).map_err(err)?;
    }

    let shape = heap.graph_shape().map_err(err)?;
    Ok((heap, shape))
}

fn err(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn run_sample(
    collector: CollectorKind,
    workload: Workload,
    object_count: usize,
) -> Result<Sample, String> {
    let build_start = Instant::now();
    let (mut heap, graph) = build_graph(collector, workload, object_count)?;
    let build_ns = build_start.elapsed().as_nanos();
    let objects_before = heap.live_objects();
    let report = heap.collect().map_err(err)?;
    let stats = heap.stats();

    Ok(Sample {
        build_ns,
        collection_ns: report.collection_ns,
        root_scan_ns: report.root_scan_ns,
        roots_scanned: report.roots_scanned,
        edges_examined: report.edges_examined,
        objects_before,
        objects_after: report.live_objects_after,
        reclaimed_total: stats.reclaimed_objects,
        reclaimed_during_collection: report.reclaimed_objects,
        peak_heap_bytes: stats.peak_heap_bytes,
        live_heap_bytes_after: report.live_bytes_after,
        refcount_operations: stats.refcount_operations,
        graph,
    })
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    let rank = ((sorted.len() as f64 * p).ceil() as usize).max(1);
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

fn distribution(samples: &[Sample], select: impl Fn(&Sample) -> u128) -> (f64, f64, f64) {
    let mut values: Vec<f64> = samples.iter().map(|sample| select(sample) as f64).collect();
    values.sort_by(f64::total_cmp);
    (
        percentile(&values, 0.50),
        percentile(&values, 0.95),
        percentile(&values, 0.99),
    )
}

fn summarize(workload: Workload, config: &Config) -> Result<Vec<Summary>, String> {
    let collectors = [CollectorKind::RcCycleRemoval, CollectorKind::MarkSweep];
    let mut samples: [Vec<Sample>; 2] = [
        Vec::with_capacity(config.samples),
        Vec::with_capacity(config.samples),
    ];

    // Alternate candidate order for every paired warmup and measurement sample
    // to reduce systematic order, thermal and scheduler bias.
    for warmup in 0..config.warmups {
        let order = if warmup % 2 == 0 {
            collectors
        } else {
            [collectors[1], collectors[0]]
        };
        for collector in order {
            let _ = run_sample(collector, workload, config.objects)?;
        }
    }

    for sample_index in 0..config.samples {
        let order = if sample_index % 2 == 0 {
            collectors
        } else {
            [collectors[1], collectors[0]]
        };
        let mut pair: [Option<Sample>; 2] = [None, None];

        for collector in order {
            let index = match collector {
                CollectorKind::RcCycleRemoval => 0,
                CollectorKind::MarkSweep => 1,
            };
            pair[index] = Some(run_sample(collector, workload, config.objects)?);
        }

        let rc_sample = pair[0].take().ok_or("missing RC benchmark sample")?;
        let tracing_sample = pair[1].take().ok_or("missing tracing benchmark sample")?;
        if rc_sample.graph != tracing_sample.graph {
            return Err(format!(
                "candidate graph mismatch in {} sample {}: {:?} != {:?}",
                workload.name(),
                sample_index,
                rc_sample.graph,
                tracing_sample.graph
            ));
        }

        let rc_liveness = (
            rc_sample.objects_after,
            rc_sample.reclaimed_total,
            rc_sample.live_heap_bytes_after,
        );
        let tracing_liveness = (
            tracing_sample.objects_after,
            tracing_sample.reclaimed_total,
            tracing_sample.live_heap_bytes_after,
        );
        if rc_liveness != tracing_liveness {
            return Err(format!(
                "candidate liveness mismatch in {} sample {}: {:?} != {:?}",
                workload.name(),
                sample_index,
                rc_liveness,
                tracing_liveness
            ));
        }

        samples[0].push(rc_sample);
        samples[1].push(tracing_sample);
    }

    Ok(vec![
        Summary {
            workload: workload.name(),
            collector: collectors[0].name(),
            samples: std::mem::take(&mut samples[0]),
        },
        Summary {
            workload: workload.name(),
            collector: collectors[1].name(),
            samples: std::mem::take(&mut samples[1]),
        },
    ])
}

fn json_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 8);
    for character in input.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            character if character.is_control() => {
                let _ = write!(out, "\\u{:04x}", character as u32);
            }
            character => out.push(character),
        }
    }
    out
}

fn graph_json(graph: &GraphShape) -> String {
    format!(
        "{{\"objects\":{},\"edges\":{},\"root_counts\":{{\"interpreter_frames\":{},\"native_frame_prototype\":{},\"persistent_host_handles\":{},\"temporary_scopes\":{}}},\"fingerprint\":\"{}\"}}",
        graph.objects,
        graph.edges,
        graph.roots.interpreter_frame_roots,
        graph.roots.native_frame_roots,
        graph.roots.persistent_host_handles,
        graph.roots.temporary_scope_roots,
        graph.fingerprint
    )
}

fn sample_json(sample: &Sample) -> String {
    format!(
        "{{\"build_ns\":{},\"collection_ns\":{},\"root_scan_ns\":{},\"roots_scanned\":{},\"edges_examined\":{},\"objects_before\":{},\"objects_after\":{},\"reclaimed_total\":{},\"reclaimed_during_collection\":{},\"peak_heap_bytes\":{},\"live_heap_bytes_after\":{},\"refcount_operations\":{},\"graph\":{}}}",
        sample.build_ns,
        sample.collection_ns,
        sample.root_scan_ns,
        sample.roots_scanned,
        sample.edges_examined,
        sample.objects_before,
        sample.objects_after,
        sample.reclaimed_total,
        sample.reclaimed_during_collection,
        sample.peak_heap_bytes,
        sample.live_heap_bytes_after,
        sample.refcount_operations,
        graph_json(&sample.graph)
    )
}

fn summary_json(summary: &Summary) -> String {
    let collection = distribution(&summary.samples, |s| s.collection_ns);
    let roots = distribution(&summary.samples, |s| s.root_scan_ns);
    let build = distribution(&summary.samples, |s| s.build_ns);
    let mut end_to_end: Vec<f64> = summary
        .samples
        .iter()
        .map(|s| (s.build_ns + s.collection_ns) as f64)
        .collect();
    end_to_end.sort_by(f64::total_cmp);

    let sample_count = summary.samples.len();
    let first = &summary.samples[0];
    let average_edges = summary
        .samples
        .iter()
        .map(|s| s.edges_examined as f64)
        .sum::<f64>()
        / sample_count as f64;
    let average_roots = summary
        .samples
        .iter()
        .map(|s| s.roots_scanned as f64)
        .sum::<f64>()
        / sample_count as f64;
    let average_refcount_ops = summary
        .samples
        .iter()
        .map(|s| s.refcount_operations as f64)
        .sum::<f64>()
        / sample_count as f64;

    let raw_samples = summary
        .samples
        .iter()
        .map(sample_json)
        .collect::<Vec<_>>()
        .join(",");

    format!(
        "{{\"workload\":\"{}\",\"collector\":\"{}\",\"graph\":{},\"metrics\":{{\"collection_ns\":{{\"p50\":{:.3},\"p95\":{:.3},\"p99\":{:.3}}},\"root_scan_ns\":{{\"p50\":{:.3},\"p95\":{:.3},\"p99\":{:.3}}},\"build_ns\":{{\"p50\":{:.3},\"p95\":{:.3},\"p99\":{:.3}}},\"end_to_end_ns\":{{\"p50\":{:.3},\"p95\":{:.3},\"p99\":{:.3}}},\"objects_per_second_at_median_build\":{:.3},\"operations_per_second_at_median_end_to_end\":{:.3},\"average_edges_examined\":{:.3},\"average_roots_scanned\":{:.3},\"average_refcount_operations\":{:.3},\"live_objects_after\":{},\"total_reclaimed_objects\":{},\"peak_heap_bytes\":{},\"live_heap_bytes_after\":{}}},\"raw_samples\":[{}]}}",
        summary.workload,
        summary.collector,
        graph_json(&first.graph),
        collection.0,
        collection.1,
        collection.2,
        roots.0,
        roots.1,
        roots.2,
        build.0,
        build.1,
        build.2,
        percentile(&end_to_end, 0.50),
        percentile(&end_to_end, 0.95),
        percentile(&end_to_end, 0.99),
        first.objects_before as f64 * 1_000_000_000.0 / build.0.max(1.0),
        (first.graph.objects + first.graph.edges) as f64 * 1_000_000_000.0
            / percentile(&end_to_end, 0.50).max(1.0),
        average_edges,
        average_roots,
        average_refcount_ops,
        first.objects_after,
        first.reclaimed_total,
        summary
            .samples
            .iter()
            .map(|s| s.peak_heap_bytes)
            .max()
            .unwrap_or(0),
        first.live_heap_bytes_after,
        raw_samples
    )
}

fn report(config: &Config, summaries: &[Summary]) -> String {
    let rustc = env::var("RUSTC_VERSION").unwrap_or_else(|_| "not supplied".to_owned());
    let mut out = String::new();
    let _ = writeln!(out, "{{");
    let _ = writeln!(out, "  \"schema_version\": 1,");
    let _ = writeln!(out, "  \"task\": \"OGM-26\",");
    let _ = writeln!(
        out,
        "  \"source_revision\": \"{}\",",
        json_escape(&config.revision)
    );
    let _ = writeln!(
        out,
        "  \"tested_revision\": \"{}\",",
        json_escape(&config.tested_revision)
    );
    let _ = writeln!(out, "  \"environment\": {{");
    let _ = writeln!(out, "    \"rustc\": \"{}\",", json_escape(&rustc));
    let _ = writeln!(out, "    \"os\": \"{}\",", env::consts::OS);
    let _ = writeln!(out, "    \"arch\": \"{}\",", env::consts::ARCH);
    let _ = writeln!(out, "    \"cpu\": \"{}\"", json_escape(&config.cpu));
    let _ = writeln!(out, "  }},");
    let _ = writeln!(
        out,
        "  \"configuration\": {{\"objects\":{},\"warmups\":{},\"samples\":{},\"percentile_method\":\"nearest-rank\",\"candidate_order\":\"alternating-per-sample\"}},",
        config.objects, config.warmups, config.samples
    );
    let _ = writeln!(out, "  \"limitations\": [");
    let _ = writeln!(
        out,
        "    \"synthetic non-moving Rust heap model; not a production JavaScript Runtime or allocator\","
    );
    let _ = writeln!(
        out,
        "    \"native frame roots are explicit prototype roots, not compiler-generated AOT stack maps\","
    );
    let _ = writeln!(
        out,
        "    \"logical heap bytes include object headers, payload and edge slots, but exclude Vec capacity, allocator metadata, root registries and process overhead\","
    );
    let _ = writeln!(
        out,
        "    \"timings are host- and toolchain-specific and have no pass/fail performance threshold\""
    );
    let _ = writeln!(out, "  ],");
    let _ = writeln!(out, "  \"results\": [");
    for (index, summary) in summaries.iter().enumerate() {
        let comma = if index + 1 == summaries.len() {
            ""
        } else {
            ","
        };
        let _ = writeln!(out, "    {}{}", summary_json(summary), comma);
    }
    let _ = writeln!(out, "  ]");
    let _ = writeln!(out, "}}");
    out
}

fn run() -> Result<(), String> {
    let config = parse_config()?;
    let mut summaries = Vec::new();

    for workload in Workload::ALL {
        summaries.extend(summarize(workload, &config)?);
    }

    let output = report(&config, &summaries);
    fs::write(&config.output, output)
        .map_err(|error| format!("write {}: {error}", config.output.display()))?;
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("ogma-gc-comparison: {error}");
        std::process::exit(2);
    }
}
