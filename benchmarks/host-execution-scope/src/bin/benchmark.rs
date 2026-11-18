use ogma_host_execution_scope_proof::{
    HostExecutionContext, HostSnapshot, LibrarySyncMode, LockSnapshot, ScopeMetrics, SharedLockHost,
};
use std::alloc::{GlobalAlloc, Layout, System};
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::hint::black_box;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::time::Instant;

struct CountingAllocator;

static ALLOC_CALLS: AtomicU64 = AtomicU64::new(0);
static REALLOC_CALLS: AtomicU64 = AtomicU64::new(0);
static DEALLOC_CALLS: AtomicU64 = AtomicU64::new(0);
static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);
static FREED_BYTES: AtomicU64 = AtomicU64::new(0);

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            ALLOC_CALLS.fetch_add(1, Ordering::Relaxed);
            ALLOCATED_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        if !pointer.is_null() {
            DEALLOC_CALLS.fetch_add(1, Ordering::Relaxed);
            FREED_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        }
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let replacement = unsafe { System.realloc(pointer, layout, new_size) };
        if !replacement.is_null() {
            REALLOC_CALLS.fetch_add(1, Ordering::Relaxed);
            ALLOCATED_BYTES.fetch_add(new_size as u64, Ordering::Relaxed);
        }
        replacement
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct AllocationSnapshot {
    alloc_calls: u64,
    realloc_calls: u64,
    dealloc_calls: u64,
    allocated_bytes: u64,
    freed_bytes: u64,
}

impl AllocationSnapshot {
    fn now() -> Self {
        Self {
            alloc_calls: ALLOC_CALLS.load(Ordering::Relaxed),
            realloc_calls: REALLOC_CALLS.load(Ordering::Relaxed),
            dealloc_calls: DEALLOC_CALLS.load(Ordering::Relaxed),
            allocated_bytes: ALLOCATED_BYTES.load(Ordering::Relaxed),
            freed_bytes: FREED_BYTES.load(Ordering::Relaxed),
        }
    }

    fn delta(self, before: Self) -> Self {
        Self {
            alloc_calls: self.alloc_calls.saturating_sub(before.alloc_calls),
            realloc_calls: self.realloc_calls.saturating_sub(before.realloc_calls),
            dealloc_calls: self.dealloc_calls.saturating_sub(before.dealloc_calls),
            allocated_bytes: self.allocated_bytes.saturating_sub(before.allocated_bytes),
            freed_bytes: self.freed_bytes.saturating_sub(before.freed_bytes),
        }
    }
}

struct Config {
    iterations: usize,
    warmups: usize,
    samples: usize,
    revision: String,
    tested_revision: String,
    cpu: String,
    output: Option<PathBuf>,
}

fn parse_config() -> Result<Config, String> {
    let mut config = Config {
        iterations: 10_000,
        warmups: 3,
        samples: 101,
        revision: "unknown".into(),
        tested_revision: "unknown".into(),
        cpu: env::var("OGM20_CPU_MODEL").unwrap_or_else(|_| "not supplied".into()),
        output: None,
    };
    let mut args = env::args().skip(1);
    while let Some(argument) = args.next() {
        let value = args
            .next()
            .ok_or_else(|| format!("missing value for {argument}"))?;
        match argument.as_str() {
            "--iterations" => {
                config.iterations = value
                    .parse()
                    .map_err(|_| "invalid --iterations".to_string())?;
            }
            "--warmups" => {
                config.warmups = value.parse().map_err(|_| "invalid --warmups".to_string())?;
            }
            "--samples" => {
                config.samples = value.parse().map_err(|_| "invalid --samples".to_string())?;
            }
            "--revision" => config.revision = value,
            "--tested-revision" => config.tested_revision = value,
            "--cpu" => config.cpu = value,
            "--output" => config.output = Some(PathBuf::from(value)),
            _ => return Err(format!("unknown argument: {argument}")),
        }
    }
    if config.iterations == 0 || config.warmups == 0 || config.samples < 101 {
        return Err("iterations and warmups must be > 0; samples must be >= 101".into());
    }
    Ok(config)
}

#[derive(Clone, Copy)]
enum Workload {
    Noop,
    Typed,
    Nested,
}

impl Workload {
    const ALL: [Self; 3] = [Self::Noop, Self::Typed, Self::Nested];

    fn name(self) -> &'static str {
        match self {
            Self::Noop => "noop",
            Self::Typed => "typed",
            Self::Nested => "nested",
        }
    }

    fn callbacks_per_iteration(self) -> usize {
        match self {
            Self::Noop | Self::Typed => 1,
            Self::Nested => 3,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct MetricSet {
    application_lock: LockSnapshot,
    runtime_sync: LockSnapshot,
    scope_enters: u64,
    state_borrows: u64,
}

impl MetricSet {
    fn delta(self, before: Self) -> Self {
        Self {
            application_lock: lock_delta(self.application_lock, before.application_lock),
            runtime_sync: lock_delta(self.runtime_sync, before.runtime_sync),
            scope_enters: self.scope_enters.saturating_sub(before.scope_enters),
            state_borrows: self.state_borrows.saturating_sub(before.state_borrows),
        }
    }
}

fn lock_delta(after: LockSnapshot, before: LockSnapshot) -> LockSnapshot {
    LockSnapshot {
        acquisitions: after.acquisitions.saturating_sub(before.acquisitions),
        contentions: after.contentions.saturating_sub(before.contentions),
        wait_ns: after.wait_ns.saturating_sub(before.wait_ns),
    }
}

struct Measurement {
    strategy: &'static str,
    workload: &'static str,
    runtime_sync_mode: &'static str,
    callbacks_per_iteration: usize,
    p50_ns: f64,
    p95_ns: f64,
    p99_ns: f64,
    min_ns: f64,
    max_ns: f64,
    batch_p50_ns: f64,
    batch_p95_ns: f64,
    batch_p99_ns: f64,
    iterations_per_second: f64,
    host_callbacks_per_second: f64,
    allocations: AllocationSnapshot,
    metrics: MetricSet,
    final_state: HostSnapshot,
}

fn percentile(sorted: &[f64], quantile: f64) -> f64 {
    let rank = ((sorted.len() as f64 * quantile).ceil() as usize).max(1);
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

fn measure<F, M, S>(
    strategy: &'static str,
    workload: Workload,
    runtime_sync_mode: &'static str,
    config: &Config,
    mut operation: F,
    metrics: M,
    final_state: S,
) -> Measurement
where
    F: FnMut(),
    M: Fn() -> MetricSet,
    S: Fn() -> HostSnapshot,
{
    for _ in 0..config.warmups {
        for _ in 0..config.iterations {
            operation();
        }
    }

    let mut batch_samples = Vec::with_capacity(config.samples);
    let mut latency_samples = Vec::with_capacity(config.iterations);
    let allocation_before = AllocationSnapshot::now();
    let metrics_before = metrics();

    // Batch measurements estimate steady-state throughput with low timer overhead.
    for _ in 0..config.samples {
        let started = Instant::now();
        for _ in 0..config.iterations {
            operation();
        }
        batch_samples.push(started.elapsed().as_nanos() as f64 / config.iterations as f64);
    }

    // Single-iteration samples expose the latency distribution instead of reporting
    // percentiles of batch means as if they were individual-operation tail latency.
    // The two Instant reads are included in each observation and reported separately.
    for _ in 0..config.iterations {
        let started = Instant::now();
        operation();
        latency_samples.push(started.elapsed().as_nanos() as f64);
    }

    let metrics_after = metrics();
    let allocations = AllocationSnapshot::now().delta(allocation_before);
    batch_samples.sort_by(f64::total_cmp);
    latency_samples.sort_by(f64::total_cmp);

    let p50_ns = percentile(&latency_samples, 0.50);
    let batch_p50_ns = percentile(&batch_samples, 0.50);
    let calls_per_iteration = workload.callbacks_per_iteration();
    let iterations_per_second = 1_000_000_000.0 / batch_p50_ns.max(f64::MIN_POSITIVE);
    Measurement {
        strategy,
        workload: workload.name(),
        runtime_sync_mode,
        callbacks_per_iteration: calls_per_iteration,
        p50_ns,
        p95_ns: percentile(&latency_samples, 0.95),
        p99_ns: percentile(&latency_samples, 0.99),
        min_ns: latency_samples[0],
        max_ns: *latency_samples.last().expect("latency samples are non-empty"),
        batch_p50_ns,
        batch_p95_ns: percentile(&batch_samples, 0.95),
        batch_p99_ns: percentile(&batch_samples, 0.99),
        iterations_per_second,
        host_callbacks_per_second: iterations_per_second * calls_per_iteration as f64,
        allocations,
        metrics: metrics_after.delta(metrics_before),
        final_state: final_state(),
    }
}

fn run_shared(host: &SharedLockHost, workload: Workload) {
    match workload {
        Workload::Noop => {
            black_box(host.noop());
        }
        Workload::Typed => {
            black_box(host.typed_add(0.000_001));
        }
        Workload::Nested => {
            black_box(host.nested(0.000_001));
        }
    }
}

fn run_owner(context: &HostExecutionContext, workload: Workload) {
    let scope = context.enter_scope().expect("owner scope entry");
    match workload {
        Workload::Noop => {
            black_box(scope.noop().expect("no-op host callback"));
        }
        Workload::Typed => {
            black_box(scope.typed_add(0.000_001).expect("typed host callback"));
        }
        Workload::Nested => {
            black_box(scope.nested(0.000_001).expect("nested host callback"));
        }
    }
}

fn shared_metrics(host: &SharedLockHost) -> MetricSet {
    MetricSet {
        application_lock: host.application_lock_metrics(),
        runtime_sync: host.runtime_sync_metrics(),
        ..MetricSet::default()
    }
}

fn owner_metrics(context: &HostExecutionContext) -> MetricSet {
    let scope: ScopeMetrics = context.metrics();
    MetricSet {
        runtime_sync: context.runtime_sync_metrics(),
        scope_enters: scope.scope_enters,
        state_borrows: scope.state_borrows,
        ..MetricSet::default()
    }
}

fn run_contention_probe(iterations_per_worker: usize) -> ContentionProbe {
    let workers = 4usize;
    let host = Arc::new(SharedLockHost::new(LibrarySyncMode::None));
    let barrier = Arc::new(Barrier::new(workers + 1));
    let mut threads = Vec::with_capacity(workers);

    for _ in 0..workers {
        let host = Arc::clone(&host);
        let barrier = Arc::clone(&barrier);
        threads.push(std::thread::spawn(move || {
            let mut latencies = Vec::with_capacity(iterations_per_worker);
            barrier.wait();
            for _ in 0..iterations_per_worker {
                let operation_started = Instant::now();
                black_box(host.typed_add(0.000_001));
                latencies.push(operation_started.elapsed().as_nanos() as f64);
            }
            latencies
        }));
    }

    let started = Instant::now();
    barrier.wait();
    let mut latencies = Vec::with_capacity(workers * iterations_per_worker);
    for worker in threads {
        latencies.extend(worker.join().expect("contention worker"));
    }
    let elapsed_ns = started.elapsed().as_nanos().max(1) as f64;
    latencies.sort_by(f64::total_cmp);
    let lock_metrics = host.application_lock_metrics();
    let total_operations = workers * iterations_per_worker;

    ContentionProbe {
        workers,
        operations: total_operations,
        p50_ns: percentile(&latencies, 0.50),
        p95_ns: percentile(&latencies, 0.95),
        p99_ns: percentile(&latencies, 0.99),
        operations_per_second: total_operations as f64 * 1_000_000_000.0 / elapsed_ns,
        application_lock: lock_metrics,
    }
}

struct ContentionProbe {
    workers: usize,
    operations: usize,
    p50_ns: f64,
    p95_ns: f64,
    p99_ns: f64,
    operations_per_second: f64,
    application_lock: LockSnapshot,
}

fn run() -> Result<(), String> {
    let config = parse_config()?;
    let mut measurements = Vec::new();

    for (mode, mode_name) in [
        (LibrarySyncMode::None, "none"),
        (
            LibrarySyncMode::SyntheticControl,
            "synthetic-runtime-mutex-control",
        ),
    ] {
        for workload in Workload::ALL {
            let shared = SharedLockHost::new(mode);
            let shared_measurement = measure(
                "per-callback-arc-mutex",
                workload,
                mode_name,
                &config,
                || run_shared(&shared, workload),
                || shared_metrics(&shared),
                || shared.snapshot(),
            );

            let owner = HostExecutionContext::new(mode);
            let owner_measurement = measure(
                "owner-thread-host-execution-scope",
                workload,
                mode_name,
                &config,
                || run_owner(&owner, workload),
                || owner_metrics(&owner),
                || owner.snapshot().expect("owner snapshot"),
            );

            if shared_measurement.final_state != owner_measurement.final_state {
                return Err(format!(
                    "workload {} produced different final states: {:?} vs {:?}",
                    workload.name(),
                    shared_measurement.final_state,
                    owner_measurement.final_state
                ));
            }
            if owner_measurement.metrics.application_lock.acquisitions != 0 {
                return Err(
                    "owner-thread strategy unexpectedly acquired an application mutex".into(),
                );
            }

            measurements.push(shared_measurement);
            measurements.push(owner_measurement);
        }
    }

    let contention = run_contention_probe(config.iterations.max(1));
    let report = build_report(&config, &measurements, &contention);

    match config.output {
        Some(path) => fs::write(&path, report)
            .map_err(|error| format!("write {}: {error}", path.display()))?,
        None => print!("{report}"),
    }
    Ok(())
}


struct TimerOverhead {
    p50_ns: f64,
    p95_ns: f64,
    p99_ns: f64,
}

fn measure_timer_overhead(samples_count: usize) -> TimerOverhead {
    let mut samples = Vec::with_capacity(samples_count);
    for _ in 0..samples_count {
        let started = Instant::now();
        black_box(());
        samples.push(started.elapsed().as_nanos() as f64);
    }
    samples.sort_by(f64::total_cmp);
    TimerOverhead {
        p50_ns: percentile(&samples, 0.50),
        p95_ns: percentile(&samples, 0.95),
        p99_ns: percentile(&samples, 0.99),
    }
}

fn json_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 8);
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(escaped, "\\u{:04x}", c as u32);
            }
            c => escaped.push(c),
        }
    }
    escaped
}

fn write_lock_metrics(output: &mut String, metrics: LockSnapshot) {
    let _ = write!(
        output,
        "{{\"acquisitions\":{},\"contentions\":{},\"wait_ns\":{}}}",
        metrics.acquisitions, metrics.contentions, metrics.wait_ns
    );
}

fn build_report(
    config: &Config,
    measurements: &[Measurement],
    contention: &ContentionProbe,
) -> String {
    let rustc = env::var("RUSTC_VERSION").unwrap_or_else(|_| "not supplied".into());
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let timer_overhead = measure_timer_overhead(config.iterations);
    let mut output = String::new();
    let _ = writeln!(output, "{{");
    let _ = writeln!(output, "  \"schema_version\": 1,");
    let _ = writeln!(
        output,
        "  \"benchmark\": \"ogm-20-host-execution-scope-m0\","
    );
    let _ = writeln!(
        output,
        "  \"source_revision\": \"{}\",",
        json_escape(&config.revision)
    );
    let _ = writeln!(
        output,
        "  \"tested_revision\": \"{}\",",
        json_escape(&config.tested_revision)
    );
    let _ = writeln!(output, "  \"environment\": {{");
    let _ = writeln!(output, "    \"rustc\": \"{}\",", json_escape(&rustc));
    let _ = writeln!(output, "    \"os\": \"{}\",", env::consts::OS);
    let _ = writeln!(output, "    \"arch\": \"{}\",", env::consts::ARCH);
    let _ = writeln!(
        output,
        "    \"cpu_model\": \"{}\",",
        json_escape(&config.cpu)
    );
    let _ = writeln!(output, "    \"profile\": \"{}\",", profile);
    let _ = writeln!(
        output,
        "    \"iterations_per_sample\": {},",
        config.iterations
    );
    let _ = writeln!(output, "    \"warmup_rounds\": {},", config.warmups);
    let _ = writeln!(output, "    \"batch_measurement_samples\": {}", config.samples);
    let _ = writeln!(output, "    \"single_iteration_latency_samples\": {}", config.iterations);
    let _ = writeln!(output, "  }},");
    let _ = writeln!(
        output,
        "  \"timing_instrumentation\": {{\"method\":\"each single workload iteration is bracketed by two Instant reads; measured values are raw and timer overhead is not subtracted\",\"instant_pair_overhead_ns\":{{\"p50\":{:.3},\"p95\":{:.3},\"p99\":{:.3}}}}},",
        timer_overhead.p50_ns,
        timer_overhead.p95_ns,
        timer_overhead.p99_ns
    );
    let _ = writeln!(
        output,
        "  \"synchronization_accounting\": {{\"application_state_lock\": \"instrumented separately\", \"binding_library_reference\": \"source-audited separately below; its lock/borrow counts are not part of mock timings\", \"synthetic_control\": \"an independent mutex acquired once per host callback to show non-application synchronization cost\"}},"
    );
    let _ = writeln!(
        output,
        "  \"binding_library_reference\": {{\"crate\":\"rquickjs\",\"version\":\"0.14.0\",\"consumer_repository\":\"AiratGaliev/cge-experiments\",\"consumer_revision\":\"c47ad08c3da9a3857f8429cebbed6ce88969715d\",\"consumer_dependency\":\"rquickjs 0.14.0 with default features and std enabled\",\"enabled_features\":[\"default\",\"std\"],\"disabled_features\":[\"parallel\",\"futures\"],\"runtime_storage\":\"Rc<Mut<RawRuntime>>; Ref=Rc and Mut=core::cell::RefCell<T>\",\"runtime_lock_operation\":\"Mut::lock calls RefCell::borrow_mut; this is single-thread borrow exclusion, not an OS mutex\",\"context_entry\":\"Context::with acquires Runtime.inner once and holds the RefMut over the closure and JS execution\",\"host_callback_entry\":\"Rust host callbacks execute inside the active Context::with scope; ordinary callback dispatch does not acquire Runtime.inner again per callback\",\"parallel_feature_behavior\":\"when enabled, Ref=Arc and Mut=std::sync::Mutex<T>; not enabled in the inspected CGE dependency configuration\",\"futures_feature_behavior\":\"AsyncRuntime uses async_lock::Mutex<InnerRuntime>; not enabled in the inspected CGE dependency configuration\",\"source_audit\":\"DelSkayn/rquickjs commit 5a9560dd821d5498ded9e0745250d42fcadf8608: core/src/safe_ref.rs, core/src/runtime/base.rs, core/src/context/base.rs; CGE call site: rust-brick-breaker/src/scripting.rs call_hook\"}},"
    );
    let _ = writeln!(output, "  \"measurements\": [");
    for (index, measurement) in measurements.iter().enumerate() {
        let comma = if index + 1 == measurements.len() {
            ""
        } else {
            ","
        };
        let _ = write!(
            output,
            "    {{\"strategy\":\"{}\",\"workload\":\"{}\",\"runtime_sync_mode\":\"{}\",\"callbacks_per_iteration\":{},\"single_iteration_latency_unit\":\"ns/workload_iteration_including_timing_overhead\",\"p50\":{:.3},\"p95\":{:.3},\"p99\":{:.3},\"min\":{:.3},\"max\":{:.3},\"batch_iteration_mean_unit\":\"ns/workload_iteration\",\"batch_p50\":{:.3},\"batch_p95\":{:.3},\"batch_p99\":{:.3},\"workload_iterations_per_second_at_batch_p50\":{:.3},\"host_callbacks_per_second_at_batch_p50\":{:.3},",
            measurement.strategy,
            measurement.workload,
            measurement.runtime_sync_mode,
            measurement.callbacks_per_iteration,
            measurement.p50_ns,
            measurement.p95_ns,
            measurement.p99_ns,
            measurement.min_ns,
            measurement.max_ns,
            measurement.batch_p50_ns,
            measurement.batch_p95_ns,
            measurement.batch_p99_ns,
            measurement.iterations_per_second,
            measurement.host_callbacks_per_second
        );
        let _ = write!(
            output,
            "\"allocations\":{{\"alloc_calls\":{},\"realloc_calls\":{},\"dealloc_calls\":{},\"allocated_bytes\":{},\"freed_bytes\":{} }},",
            measurement.allocations.alloc_calls,
            measurement.allocations.realloc_calls,
            measurement.allocations.dealloc_calls,
            measurement.allocations.allocated_bytes,
            measurement.allocations.freed_bytes
        );
        let _ = write!(output, "\"application_lock\":");
        write_lock_metrics(&mut output, measurement.metrics.application_lock);
        let _ = write!(output, ",\"synthetic_runtime_sync\":");
        write_lock_metrics(&mut output, measurement.metrics.runtime_sync);
        let _ = write!(
            output,
            ",\"scope_enters\":{},\"state_borrows\":{},\"final_value\":{:.9},\"final_typed_calls\":{}}}{}",
            measurement.metrics.scope_enters,
            measurement.metrics.state_borrows,
            measurement.final_state.value,
            measurement.final_state.typed_calls,
            comma
        );
        let _ = writeln!(output);
    }
    let _ = writeln!(output, "  ],");
    let _ = writeln!(
        output,
        "  \"shared_lock_contention_probe\": {{\"workers\":{},\"operations\":{},\"unit\":\"ns/operation\",\"p50\":{:.3},\"p95\":{:.3},\"p99\":{:.3},\"operations_per_second\":{:.3},\"application_lock\":",
        contention.workers,
        contention.operations,
        contention.p50_ns,
        contention.p95_ns,
        contention.p99_ns,
        contention.operations_per_second
    );
    write_lock_metrics(&mut output, contention.application_lock);
    let _ = writeln!(output, "}},");
    let _ = writeln!(output, "  \"limitations\": [");
    let _ = writeln!(
        output,
        "    \"mock host dispatch only; no ECMAScript parser, interpreter or production Runtime\","
    );
    let _ = writeln!(output, "    \"the binding-library reference is a source audit of the exact rquickjs 0.14.0 CGE profile; no rquickjs execution timings are included in the mock measurements\",");
    let _ = writeln!(output, "    \"the single-owner benchmark is serialized by design; cross-thread contention is reported separately for the shared-lock baseline\",");
    let _ = writeln!(output, "    \"allocation sample buffers are preallocated before measurement; process-wide counters still include any unrelated process activity during the interval\",");
    let _ = writeln!(output, "    \"single-iteration latency percentiles include Instant timing overhead; an independent timer-pair baseline is reported and not subtracted\",");
    let _ = writeln!(
        output,
        "    \"timings have no machine-independent pass/fail threshold\""
    );
    let _ = writeln!(output, "  ]");
    let _ = writeln!(output, "}}");
    output
}

fn main() {
    if let Err(error) = run() {
        eprintln!("ogma-host-scope-bench: {error}");
        std::process::exit(2);
    }
}
