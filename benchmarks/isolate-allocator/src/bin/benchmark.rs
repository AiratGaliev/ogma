use ogma_isolate_allocator_proof::{
    Allocation, AllocatorStrategy, CompileUnitArena, IsolateAllocator,
};
use std::alloc::{Layout, System};
use std::env;
use std::fs;
use std::hint::black_box;
use std::path::PathBuf;
use std::process::Command;
use std::ptr::NonNull;
use std::time::Instant;

const ALLOCATOR_QUOTA_BYTES: usize = 1024 * 1024;
const COMPILE_UNIT_QUOTA_BYTES: usize = 2 * 1024 * 1024;
const ALLOCATIONS_PER_ITERATION: usize = 128;
const NODES_PER_COMPILE_UNIT: usize = 256;
const ALLOCATION_SIZES: [usize; 12] = [16, 24, 32, 48, 64, 96, 128, 256, 512, 1024, 2048, 4096];
const NODE_SIZES: [usize; 5] = [24, 40, 64, 128, 256];

#[derive(Clone)]
struct Config {
    iterations: usize,
    warmups: usize,
    samples: usize,
    revision: String,
    tested_revision: String,
    cpu: String,
    output: PathBuf,
}

fn argument(args: &[String], name: &str, default: &str) -> String {
    args.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
        .unwrap_or_else(|| default.to_owned())
}

fn parse_config(args: &[String]) -> Result<Config, String> {
    let iterations = argument(args, "--iterations", "100")
        .parse::<usize>()
        .map_err(|_| "invalid --iterations".to_owned())?;
    let warmups = argument(args, "--warmups", "3")
        .parse::<usize>()
        .map_err(|_| "invalid --warmups".to_owned())?;
    let samples = argument(args, "--samples", "101")
        .parse::<usize>()
        .map_err(|_| "invalid --samples".to_owned())?;
    if iterations == 0 || warmups == 0 || samples < 101 {
        return Err("iterations and warmups must be > 0; samples must be >= 101".to_owned());
    }
    Ok(Config {
        iterations,
        warmups,
        samples,
        revision: argument(args, "--revision", "unknown"),
        tested_revision: argument(args, "--tested-revision", "unknown"),
        cpu: argument(args, "--cpu", "not supplied"),
        output: PathBuf::from(argument(
            args,
            "--output",
            "ogm-24-allocator-arena-report.json",
        )),
    })
}

fn percentile(sorted: &[f64], quantile: f64) -> f64 {
    let rank = ((sorted.len() as f64 * quantile).ceil() as usize).max(1);
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

fn distribution(values: &mut Vec<f64>) -> (f64, f64, f64) {
    values.sort_by(f64::total_cmp);
    (
        percentile(values, 0.50),
        percentile(values, 0.95),
        percentile(values, 0.99),
    )
}

fn rss_high_water_bytes() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with("VmHWM:"))?;
    let kibibytes = line.split_whitespace().nth(1)?.parse::<u64>().ok()?;
    kibibytes.checked_mul(1024)
}

fn touch_allocation(allocation: &Allocation, marker: u8) -> Result<(), String> {
    let size = allocation.requested_size();
    if size == 0 {
        return Ok(());
    }
    let pointer = allocation.as_non_null().as_ptr();
    // SAFETY: the allocation token owns at least the requested writable bytes
    // and is not shared while the benchmark touches it.
    unsafe {
        pointer.write(marker);
        if size > 1 {
            pointer.add(size - 1).write(marker.wrapping_add(1));
            if pointer.read() != marker || pointer.add(size - 1).read() != marker.wrapping_add(1) {
                return Err("allocation touch/checksum mismatch".to_owned());
            }
        }
    }
    black_box(pointer);
    Ok(())
}

#[derive(Default)]
struct AllocatorSample {
    elapsed_ns: f64,
    allocation_latencies: Vec<f64>,
    deallocation_latencies: Vec<f64>,
    peak_reserved_bytes: usize,
    peak_live_requested_bytes: usize,
    peak_free_cached_bytes: usize,
    peak_internal_fragmentation_bytes: usize,
    reused_allocations: u64,
    cache_evictions: u64,
    allocation_failures: u64,
    allocations: u64,
    deallocations: u64,
}

fn run_allocator_sample(
    strategy: AllocatorStrategy,
    iterations: usize,
) -> Result<AllocatorSample, String> {
    let mut allocator = IsolateAllocator::new(strategy, ALLOCATOR_QUOTA_BYTES);
    let mut transient = Vec::with_capacity(ALLOCATIONS_PER_ITERATION);
    let mut retained = Vec::with_capacity(128);
    let mut allocation_latencies = Vec::with_capacity(iterations * ALLOCATIONS_PER_ITERATION);
    let mut deallocation_latencies = Vec::with_capacity(iterations * ALLOCATIONS_PER_ITERATION);
    let started = Instant::now();

    for iteration in 0..iterations {
        transient.clear();
        for index in 0..ALLOCATIONS_PER_ITERATION {
            let size = ALLOCATION_SIZES[(iteration + index) % ALLOCATION_SIZES.len()];
            let layout = Layout::from_size_align(size, 16)
                .map_err(|_| "invalid allocator workload layout".to_owned())?;
            let allocation_started = Instant::now();
            let allocation = allocator
                .allocate(layout)
                .map_err(|error| format!("{} allocation failed: {error:?}", strategy.name()))?;
            allocation_latencies.push(allocation_started.elapsed().as_nanos() as f64);
            touch_allocation(&allocation, (index as u8).wrapping_add(1))?;

            if index % 8 == 0 {
                retained.push(allocation);
            } else {
                transient.push(allocation);
            }
        }

        while let Some(allocation) = transient.pop() {
            let deallocation_started = Instant::now();
            allocator
                .deallocate(allocation)
                .map_err(|_| "allocator rejected a same-isolate deallocation".to_owned())?;
            deallocation_latencies.push(deallocation_started.elapsed().as_nanos() as f64);
        }

        if retained.len() >= 128 {
            for allocation in retained.drain(..64) {
                let deallocation_started = Instant::now();
                allocator
                    .deallocate(allocation)
                    .map_err(|_| "allocator rejected a retained deallocation".to_owned())?;
                deallocation_latencies.push(deallocation_started.elapsed().as_nanos() as f64);
            }
        }
    }

    for allocation in retained.drain(..) {
        let deallocation_started = Instant::now();
        allocator
            .deallocate(allocation)
            .map_err(|_| "allocator rejected final deallocation".to_owned())?;
        deallocation_latencies.push(deallocation_started.elapsed().as_nanos() as f64);
    }
    let elapsed_ns = started.elapsed().as_nanos().max(1) as f64;
    let stats = allocator.stats();
    if stats.live_requested_bytes != 0 || stats.live_backing_bytes != 0 {
        return Err("allocator workload leaked live allocations".to_owned());
    }
    if stats.reserved_bytes > ALLOCATOR_QUOTA_BYTES {
        return Err("allocator exceeded its committed-byte quota".to_owned());
    }

    Ok(AllocatorSample {
        elapsed_ns,
        allocation_latencies,
        deallocation_latencies,
        peak_reserved_bytes: stats.peak_reserved_bytes,
        peak_live_requested_bytes: stats.peak_live_requested_bytes,
        peak_free_cached_bytes: stats.peak_free_cached_bytes,
        peak_internal_fragmentation_bytes: stats.peak_internal_fragmentation_bytes,
        reused_allocations: stats.reused_allocations,
        cache_evictions: stats.cache_evictions,
        allocation_failures: stats.allocation_failures,
        allocations: stats.successful_allocations,
        deallocations: stats.deallocations,
    })
}

fn allocator_worker(strategy: AllocatorStrategy, config: &Config) -> Result<String, String> {
    for _ in 0..config.warmups {
        black_box(run_allocator_sample(strategy, config.iterations)?);
    }

    let capacity = config
        .iterations
        .saturating_mul(ALLOCATIONS_PER_ITERATION)
        .saturating_mul(config.samples);
    let mut allocation_latencies = Vec::with_capacity(capacity);
    let mut deallocation_latencies = Vec::with_capacity(capacity);
    let mut throughputs = Vec::with_capacity(config.samples);
    let mut peak_reserved = 0usize;
    let mut peak_live = 0usize;
    let mut peak_free = 0usize;
    let mut peak_fragmentation = 0usize;
    let mut reused = 0u64;
    let mut evictions = 0u64;
    let mut failures = 0u64;
    let mut allocations = 0u64;
    let mut deallocations = 0u64;

    for _ in 0..config.samples {
        let sample = run_allocator_sample(strategy, config.iterations)?;
        let operations = (sample.allocations + sample.deallocations).max(1) as f64;
        throughputs.push(operations * 1_000_000_000.0 / sample.elapsed_ns);
        allocation_latencies.extend(sample.allocation_latencies);
        deallocation_latencies.extend(sample.deallocation_latencies);
        peak_reserved = peak_reserved.max(sample.peak_reserved_bytes);
        peak_live = peak_live.max(sample.peak_live_requested_bytes);
        peak_free = peak_free.max(sample.peak_free_cached_bytes);
        peak_fragmentation = peak_fragmentation.max(sample.peak_internal_fragmentation_bytes);
        reused += sample.reused_allocations;
        evictions += sample.cache_evictions;
        failures += sample.allocation_failures;
        allocations += sample.allocations;
        deallocations += sample.deallocations;
    }

    let (alloc_p50, alloc_p95, alloc_p99) = distribution(&mut allocation_latencies);
    let (free_p50, free_p95, free_p99) = distribution(&mut deallocation_latencies);
    let (throughput_p50, _, _) = distribution(&mut throughputs);

    Ok(format!(
        "{{\"strategy\":\"{}\",\"quota_bytes\":{},\"operations_per_sample\":{},\"samples\":{},\"throughput_ops_per_second\":{:.3},\"allocation_latency_ns\":{{\"p50\":{:.3},\"p95\":{:.3},\"p99\":{:.3}}},\"deallocation_latency_ns\":{{\"p50\":{:.3},\"p95\":{:.3},\"p99\":{:.3}}},\"peak_reserved_bytes\":{},\"peak_live_requested_bytes\":{},\"peak_free_cached_bytes\":{},\"peak_internal_fragmentation_bytes\":{},\"reused_allocations_total\":{},\"cache_evictions_total\":{},\"allocation_failures_total\":{},\"allocations_total\":{},\"deallocations_total\":{},\"peak_rss_bytes\":{}}}",
        strategy.name(),
        ALLOCATOR_QUOTA_BYTES,
        config.iterations * ALLOCATIONS_PER_ITERATION * 2,
        config.samples,
        throughput_p50,
        alloc_p50,
        alloc_p95,
        alloc_p99,
        free_p50,
        free_p95,
        free_p99,
        peak_reserved,
        peak_live,
        peak_free,
        peak_fragmentation,
        reused,
        evictions,
        failures,
        allocations,
        deallocations,
        rss_high_water_bytes()
            .map(|value| value.to_string())
            .unwrap_or_else(|| "null".to_owned()),
    ))
}

#[derive(Default)]
struct ArenaSample {
    elapsed_ns: f64,
    allocation_latencies: Vec<f64>,
    release_latencies: Vec<f64>,
    peak_reserved_bytes: usize,
    peak_used_bytes: usize,
    peak_padding_bytes: usize,
    chunk_count: usize,
}

fn run_system_compile_unit_sample(iterations: usize) -> Result<ArenaSample, String> {
    let mut blocks: Vec<(NonNull<u8>, Layout)> = Vec::with_capacity(NODES_PER_COMPILE_UNIT);
    let mut allocation_latencies = Vec::with_capacity(iterations * NODES_PER_COMPILE_UNIT);
    let mut release_latencies = Vec::with_capacity(iterations * NODES_PER_COMPILE_UNIT);
    let mut reserved = 0usize;
    let mut peak_reserved = 0usize;
    let mut peak_used = 0usize;
    let started = Instant::now();

    for unit in 0..iterations {
        blocks.clear();
        let mut unit_bytes = 0usize;
        for node in 0..NODES_PER_COMPILE_UNIT {
            let size = NODE_SIZES[(unit + node) % NODE_SIZES.len()];
            let layout = Layout::from_size_align(size, 8)
                .map_err(|_| "invalid compiler workload layout".to_owned())?;
            if reserved
                .checked_add(layout.size())
                .is_none_or(|total| total > COMPILE_UNIT_QUOTA_BYTES)
            {
                return Err("system compile-unit allocator exceeded quota".to_owned());
            }
            let allocation_started = Instant::now();
            // SAFETY: layout is valid and non-zero; the returned block is stored
            // until the compile-unit lifetime ends below.
            let raw = unsafe { System.alloc(layout) };
            let pointer = NonNull::new(raw)
                .ok_or_else(|| "system compile-unit allocation failed".to_owned())?;
            allocation_latencies.push(allocation_started.elapsed().as_nanos() as f64);
            // SAFETY: allocation owns the requested writable bytes.
            unsafe {
                pointer.as_ptr().write((node as u8).wrapping_add(1));
                pointer
                    .as_ptr()
                    .add(size - 1)
                    .write((node as u8).wrapping_add(2));
            }
            blocks.push((pointer, layout));
            reserved += size;
            unit_bytes += size;
            peak_reserved = peak_reserved.max(reserved);
            peak_used = peak_used.max(unit_bytes);
        }

        while let Some((pointer, layout)) = blocks.pop() {
            let release_started = Instant::now();
            // SAFETY: the pointer/Layout pair was allocated above and is freed once.
            unsafe { System.dealloc(pointer.as_ptr(), layout) };
            release_latencies.push(release_started.elapsed().as_nanos() as f64);
            reserved -= layout.size();
        }
    }
    let elapsed_ns = started.elapsed().as_nanos().max(1) as f64;
    Ok(ArenaSample {
        elapsed_ns,
        allocation_latencies,
        release_latencies,
        peak_reserved_bytes: peak_reserved,
        peak_used_bytes: peak_used,
        peak_padding_bytes: 0,
        chunk_count: 0,
    })
}

fn run_bump_arena_sample(iterations: usize) -> Result<ArenaSample, String> {
    let mut arena = CompileUnitArena::new(COMPILE_UNIT_QUOTA_BYTES);
    let mut allocation_latencies = Vec::with_capacity(iterations * NODES_PER_COMPILE_UNIT);
    let mut reset_latencies = Vec::with_capacity(iterations);
    let started = Instant::now();

    for unit in 0..iterations {
        for node in 0..NODES_PER_COMPILE_UNIT {
            let size = NODE_SIZES[(unit + node) % NODE_SIZES.len()];
            let layout = Layout::from_size_align(size, 8)
                .map_err(|_| "invalid compiler workload layout".to_owned())?;
            let allocation_started = Instant::now();
            let pointer = arena
                .allocate(layout)
                .map_err(|error| format!("compile-unit arena allocation failed: {error:?}"))?;
            allocation_latencies.push(allocation_started.elapsed().as_nanos() as f64);
            // SAFETY: this pointer is valid until the compile-unit arena is reset.
            unsafe {
                pointer.as_ptr().write((node as u8).wrapping_add(1));
                pointer
                    .as_ptr()
                    .add(size - 1)
                    .write((node as u8).wrapping_add(2));
            }
            black_box(pointer);
        }
        let reset_started = Instant::now();
        arena
            .reset()
            .map_err(|error| format!("compile-unit arena reset failed: {error:?}"))?;
        reset_latencies.push(reset_started.elapsed().as_nanos() as f64);
    }
    let elapsed_ns = started.elapsed().as_nanos().max(1) as f64;
    let stats = arena.stats();
    if stats.reserved_bytes > COMPILE_UNIT_QUOTA_BYTES {
        return Err("compile-unit arena exceeded its quota".to_owned());
    }
    Ok(ArenaSample {
        elapsed_ns,
        allocation_latencies,
        release_latencies: reset_latencies,
        peak_reserved_bytes: stats.reserved_bytes,
        peak_used_bytes: stats.peak_used_bytes,
        peak_padding_bytes: stats.peak_padding_bytes,
        chunk_count: stats.chunks,
    })
}

fn arena_worker(strategy: &str, config: &Config) -> Result<String, String> {
    let run_sample = |iterations| match strategy {
        "system-per-allocation" => run_system_compile_unit_sample(iterations),
        "compile-unit-bump-arena" => run_bump_arena_sample(iterations),
        _ => Err(format!("unknown arena worker: {strategy}")),
    };

    for _ in 0..config.warmups {
        black_box(run_sample(config.iterations)?);
    }

    let capacity = config
        .iterations
        .saturating_mul(NODES_PER_COMPILE_UNIT)
        .saturating_mul(config.samples);
    let mut allocation_latencies = Vec::with_capacity(capacity);
    let mut release_latencies = Vec::with_capacity(capacity);
    let mut throughputs = Vec::with_capacity(config.samples);
    let mut peak_reserved = 0usize;
    let mut peak_used = 0usize;
    let mut peak_padding = 0usize;
    let mut max_chunks = 0usize;

    for _ in 0..config.samples {
        let sample = run_sample(config.iterations)?;
        throughputs.push(config.iterations as f64 * 1_000_000_000.0 / sample.elapsed_ns);
        allocation_latencies.extend(sample.allocation_latencies);
        release_latencies.extend(sample.release_latencies);
        peak_reserved = peak_reserved.max(sample.peak_reserved_bytes);
        peak_used = peak_used.max(sample.peak_used_bytes);
        peak_padding = peak_padding.max(sample.peak_padding_bytes);
        max_chunks = max_chunks.max(sample.chunk_count);
    }

    let (alloc_p50, alloc_p95, alloc_p99) = distribution(&mut allocation_latencies);
    let (release_p50, release_p95, release_p99) = distribution(&mut release_latencies);
    let (throughput_p50, _, _) = distribution(&mut throughputs);
    let internal_fragmentation = peak_reserved.saturating_sub(peak_used);

    Ok(format!(
        "{{\"strategy\":\"{}\",\"quota_bytes\":{},\"compile_units_per_sample\":{},\"nodes_per_compile_unit\":{},\"samples\":{},\"throughput_compile_units_per_second\":{:.3},\"allocation_latency_ns\":{{\"p50\":{:.3},\"p95\":{:.3},\"p99\":{:.3}}},\"release_latency_ns\":{{\"p50\":{:.3},\"p95\":{:.3},\"p99\":{:.3}}},\"peak_reserved_bytes\":{},\"peak_payload_bytes\":{},\"peak_padding_bytes\":{},\"internal_fragmentation_bytes\":{},\"chunk_count\":{},\"peak_rss_bytes\":{}}}",
        strategy,
        COMPILE_UNIT_QUOTA_BYTES,
        config.iterations,
        NODES_PER_COMPILE_UNIT,
        config.samples,
        throughput_p50,
        alloc_p50,
        alloc_p95,
        alloc_p99,
        release_p50,
        release_p95,
        release_p99,
        peak_reserved,
        peak_used,
        peak_padding,
        internal_fragmentation,
        max_chunks,
        rss_high_water_bytes()
            .map(|value| value.to_string())
            .unwrap_or_else(|| "null".to_owned()),
    ))
}

fn run_worker(worker: &str, config: &Config) -> Result<(), String> {
    let json = if let Some(strategy_name) = worker.strip_prefix("allocator:") {
        let strategy = AllocatorStrategy::parse(strategy_name)
            .ok_or_else(|| format!("unknown allocator strategy: {strategy_name}"))?;
        allocator_worker(strategy, config)?
    } else if let Some(strategy) = worker.strip_prefix("arena:") {
        arena_worker(strategy, config)?
    } else {
        return Err(format!("unknown worker: {worker}"));
    };
    println!("WORKER_JSON\t{json}");
    Ok(())
}

fn json_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

fn run_parent(config: &Config) -> Result<(), String> {
    let executable = env::current_exe().map_err(|error| format!("current_exe failed: {error}"))?;
    let worker_names = [
        "allocator:system-allocator",
        "allocator:per-isolate-exact-reuse",
        "allocator:per-isolate-size-class-pool",
        "arena:system-per-allocation",
        "arena:compile-unit-bump-arena",
    ];
    let mut allocator_reports = Vec::new();
    let mut arena_reports = Vec::new();

    for worker in worker_names {
        let output = Command::new(&executable)
            .arg("--worker")
            .arg(worker)
            .arg("--iterations")
            .arg(config.iterations.to_string())
            .arg("--warmups")
            .arg(config.warmups.to_string())
            .arg("--samples")
            .arg(config.samples.to_string())
            .output()
            .map_err(|error| format!("failed to start {worker}: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "worker {worker} failed ({}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        let stdout = String::from_utf8(output.stdout)
            .map_err(|_| format!("worker {worker} emitted non-UTF-8 output"))?;
        let json = stdout
            .lines()
            .find_map(|line| line.strip_prefix("WORKER_JSON\t"))
            .ok_or_else(|| format!("worker {worker} did not return a JSON record: {stdout}"))?;
        if worker.starts_with("allocator:") {
            allocator_reports.push(json.to_owned());
        } else {
            arena_reports.push(json.to_owned());
        }
    }

    let rustc = env::var("RUSTC_VERSION").unwrap_or_else(|_| "not supplied".to_owned());
    let report = format!(
        concat!(
            "{{\n",
            "  \"task\":\"OGM-24\",\n",
            "  \"schema_version\":1,\n",
            "  \"source_revision\":\"{}\",\n",
            "  \"tested_revision\":\"{}\",\n",
            "  \"rustc_version\":\"{}\",\n",
            "  \"host\":{{\"os\":\"{}\",\"arch\":\"{}\",\"cpu\":\"{}\"}},\n",
            "  \"configuration\":{{\"iterations_per_sample\":{},\"warmups\":{},\"samples\":{},\"allocator_quota_bytes\":{},\"compile_unit_quota_bytes\":{},\"allocator_blocks_per_iteration\":{},\"nodes_per_compile_unit\":{},\"allocation_latency_includes_timer_overhead\":true}},\n",
            "  \"allocator_candidates\":[{}],\n",
            "  \"compile_unit_arenas\":[{}],\n",
            "  \"methodology\":{{",
            "\"allocator_workload\":\"Same mixed-size allocate/retain/churn/free sequence for all candidates; allocation sizes are 16..4096 bytes and alignment is 16.\",",
            "\"compile_unit_workload\":\"Same sequence of 256 aligned AST/Core-IR-like POD allocations per compile unit; system blocks are individually freed and the bump arena is reset.\",",
            "\"quota_semantics\":\"Quota limits allocator-owned committed/reserved bytes, including retained free blocks and arena chunks.\",",
            "\"tail_latency\":\"Nearest-rank p50/p95/p99 over individual allocation/deallocation observations; two Instant reads are included.\",",
            "\"fragmentation\":\"Reports allocator-visible internal slack and retained cache bytes; OS allocator external fragmentation is not directly observable.\"",
            "}},\n",
            "  \"limitations\":{{",
            "\"runtime\":\"No production Ogma Runtime, JavaScript heap, GC, AST or Core IR exists in this proof; all allocations are synthetic raw-byte workloads.\",",
            "\"rss\":\"Peak RSS is isolated per candidate subprocess and includes the benchmark process; null means the platform does not expose /proc/self/status VmHWM.\",",
            "\"system_allocator\":\"System is the platform Rust System allocator with an explicit per-candidate quota wrapper; its internal fragmentation is not introspectable.\",",
            "\"arena_lifetime\":\"The bump arena is suitable only for data whose entire lifetime ends at compile-unit reset; values requiring Drop and JS heap objects are excluded.\",",
            "\"performance\":\"No machine-independent performance threshold is asserted; interpret results only with the recorded revision, toolchain, host and configuration.\"",
            "}}\n",
            "}}\n"
        ),
        json_escape(&config.revision),
        json_escape(&config.tested_revision),
        json_escape(&rustc),
        json_escape(env::consts::OS),
        json_escape(env::consts::ARCH),
        json_escape(&config.cpu),
        config.iterations,
        config.warmups,
        config.samples,
        ALLOCATOR_QUOTA_BYTES,
        COMPILE_UNIT_QUOTA_BYTES,
        ALLOCATIONS_PER_ITERATION,
        NODES_PER_COMPILE_UNIT,
        allocator_reports.join(",\n    "),
        arena_reports.join(",\n    "),
    );

    fs::write(&config.output, report)
        .map_err(|error| format!("failed to write {}: {error}", config.output.display()))?;
    println!("Wrote OGM-24 evidence to {}", config.output.display());
    Ok(())
}

fn main() {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let result = match args.windows(2).find(|pair| pair[0] == "--worker") {
        Some(pair) => parse_config(&args).and_then(|config| run_worker(&pair[1], &config)),
        None => parse_config(&args).and_then(|config| run_parent(&config)),
    };
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
