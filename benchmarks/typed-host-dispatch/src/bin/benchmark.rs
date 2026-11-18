use ogma_host_execution_scope_proof::{HostExecutionContext, LibrarySyncMode};
use ogma_rust_abi_proof::{
    ogma_context_create_number, ogma_context_get_number, ogma_primitive_add_f64, OgmaCompletion,
    OgmaContextHandle, OgmaHandle, OgmaRuntime, OgmaStatus,
    COMPLETION_NORMAL,
};
use ogma_typed_host_dispatch_proof::{
    DispatchError, DispatchMetrics, DispatchValue, HostFunctionDescriptor, HostFunctionId,
    HostFunctionRegistry, JsCallable, JsCompletion, OrdinaryHostObject, PropertyReceiver,
};
use std::alloc::{GlobalAlloc, Layout, System};
use std::env;
use std::ffi::c_void;
use std::fmt::Write as _;
use std::fs;
use std::hint::black_box;
use std::path::PathBuf;
use std::ptr;
use std::slice;
use std::time::Instant;

struct CountingAllocator;
static ALLOC_CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static REALLOC_CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static ALLOCATED_BYTES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            ALLOC_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            ALLOCATED_BYTES.fetch_add(layout.size() as u64, std::sync::atomic::Ordering::Relaxed);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let replacement = unsafe { System.realloc(pointer, layout, new_size) };
        if !replacement.is_null() {
            REALLOC_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            ALLOCATED_BYTES.fetch_add(new_size as u64, std::sync::atomic::Ordering::Relaxed);
        }
        replacement
    }
}

#[derive(Clone, Copy, Default)]
struct AllocationSnapshot {
    alloc_calls: u64,
    realloc_calls: u64,
    allocated_bytes: u64,
}
impl AllocationSnapshot {
    fn now() -> Self {
        Self {
            alloc_calls: ALLOC_CALLS.load(std::sync::atomic::Ordering::Relaxed),
            realloc_calls: REALLOC_CALLS.load(std::sync::atomic::Ordering::Relaxed),
            allocated_bytes: ALLOCATED_BYTES.load(std::sync::atomic::Ordering::Relaxed),
        }
    }
    fn delta(self, before: Self) -> Self {
        Self {
            alloc_calls: self.alloc_calls.saturating_sub(before.alloc_calls),
            realloc_calls: self.realloc_calls.saturating_sub(before.realloc_calls),
            allocated_bytes: self.allocated_bytes.saturating_sub(before.allocated_bytes),
        }
    }
}

macro_rules! define_f64_binary_thunk {
    ($name:ident, $operation:expr) => {
        unsafe extern "C-unwind" fn $name(
            _runtime: *const OgmaRuntime,
            _context: OgmaContextHandle,
            left: f64,
            right: f64,
            out_result: *mut f64,
            _user_data: *mut c_void,
        ) -> OgmaStatus {
            if out_result.is_null() {
                return OgmaStatus::INVALID_ARGUMENT;
            }
            let result = ($operation)(left, right);
            unsafe { out_result.write(result) };
            OgmaStatus::OK
        }
    };
}

define_f64_binary_thunk!(generated_primitive_add, |left: f64, right: f64| left + right);

unsafe extern "C-unwind" fn generated_handle_add(
    runtime: *const OgmaRuntime,
    context: OgmaContextHandle,
    left: OgmaHandle,
    right: OgmaHandle,
    out_result: *mut f64,
    _user_data: *mut c_void,
) -> OgmaStatus {
    if out_result.is_null() {
        return OgmaStatus::INVALID_ARGUMENT;
    }
    let mut left_value = 0.0;
    let mut right_value = 0.0;
    let status = unsafe { ogma_context_get_number(runtime, context, left, &mut left_value) };
    if status != OgmaStatus::OK {
        return status;
    }
    let status = unsafe { ogma_context_get_number(runtime, context, right, &mut right_value) };
    if status != OgmaStatus::OK {
        return status;
    }
    unsafe { out_result.write(left_value + right_value) };
    OgmaStatus::OK
}

unsafe extern "C-unwind" fn generic_add(
    runtime: *const OgmaRuntime,
    context: OgmaContextHandle,
    args: *const OgmaHandle,
    args_len: usize,
    completion: *mut OgmaCompletion,
    _user_data: *mut c_void,
) -> OgmaStatus {
    if args_len != 2 || args.is_null() || completion.is_null() {
        return OgmaStatus::INVALID_ARGUMENT;
    }
    let args = unsafe { slice::from_raw_parts(args, args_len) };
    let mut left = 0.0;
    let mut right = 0.0;
    let status = unsafe { ogma_context_get_number(runtime, context, args[0], &mut left) };
    if status != OgmaStatus::OK {
        return status;
    }
    let status = unsafe { ogma_context_get_number(runtime, context, args[1], &mut right) };
    if status != OgmaStatus::OK {
        return status;
    }
    let mut handle = OgmaHandle::default();
    let status = unsafe { ogma_context_create_number(runtime, context, left + right, &mut handle) };
    if status != OgmaStatus::OK {
        return status;
    }
    unsafe {
        completion.write(OgmaCompletion {
            kind: COMPLETION_NORMAL,
            reserved: 0,
            value: handle,
        });
    }
    OgmaStatus::OK
}

struct Config {
    iterations: usize,
    warmups: usize,
    samples: usize,
    revision: String,
    tested_revision: String,
    output: Option<PathBuf>,
}
fn parse_config() -> Result<Config, String> {
    let mut config = Config {
        iterations: 10_000,
        warmups: 3,
        samples: 101,
        revision: "unknown".into(),
        tested_revision: "unknown".into(),
        output: None,
    };
    let mut args = env::args().skip(1);
    while let Some(argument) = args.next() {
        let value = args.next().ok_or_else(|| format!("missing value for {argument}"))?;
        match argument.as_str() {
            "--iterations" => config.iterations = value.parse().map_err(|_| "invalid --iterations")?,
            "--warmups" => config.warmups = value.parse().map_err(|_| "invalid --warmups")?,
            "--samples" => config.samples = value.parse().map_err(|_| "invalid --samples")?,
            "--revision" => config.revision = value,
            "--tested-revision" => config.tested_revision = value,
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
struct Measurement {
    name: &'static str,
    p50_ns: f64,
    p95_ns: f64,
    p99_ns: f64,
    batch_p50_ns: f64,
    throughput_ops_sec: f64,
    allocation: AllocationSnapshot,
    metrics: DispatchMetrics,
    fallback_rate: f64,
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    let rank = ((sorted.len() as f64 * p).ceil() as usize).max(1);
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

fn checked(result: Result<JsCompletion, DispatchError>) -> Result<(), String> {
    match result {
        Ok(JsCompletion::Normal(DispatchValue::Number(value))) if value == 5.0 => Ok(()),
        Ok(other) => Err(format!("unexpected dispatch result: {other:?}")),
        Err(error) => Err(format!("dispatch failed: {error:?}")),
    }
}

fn measure<F>(
    name: &'static str,
    config: &Config,
    registry: &HostFunctionRegistry,
    mut operation: F,
) -> Result<Measurement, String>
where
    F: FnMut() -> Result<(), String>,
{
    for _ in 0..config.warmups {
        for _ in 0..config.iterations {
            operation()?;
        }
    }

    let mut latency = Vec::with_capacity(config.iterations);
    let mut batch_latency = Vec::with_capacity(config.samples);
    let metrics_before = registry.metrics();
    let allocations_before = AllocationSnapshot::now();

    for _ in 0..config.iterations {
        let start = Instant::now();
        operation()?;
        latency.push(start.elapsed().as_nanos() as f64);
    }
    for _ in 0..config.samples {
        let start = Instant::now();
        for _ in 0..config.iterations {
            operation()?;
        }
        batch_latency.push(start.elapsed().as_nanos() as f64 / config.iterations as f64);
    }

    let allocations = AllocationSnapshot::now().delta(allocations_before);
    let metrics_after = registry.metrics();
    latency.sort_by(f64::total_cmp);
    batch_latency.sort_by(f64::total_cmp);
    let typed = metrics_after.typed_calls.saturating_sub(metrics_before.typed_calls);
    let generic = metrics_after.generic_calls.saturating_sub(metrics_before.generic_calls);
    let total = typed.saturating_add(generic);
    let fallback_rate = if total == 0 { 0.0 } else { generic as f64 / total as f64 };
    let batch_p50_ns = percentile(&batch_latency, 0.50);

    Ok(Measurement {
        name,
        p50_ns: percentile(&latency, 0.50),
        p95_ns: percentile(&latency, 0.95),
        p99_ns: percentile(&latency, 0.99),
        batch_p50_ns,
        throughput_ops_sec: if batch_p50_ns == 0.0 { 0.0 } else { 1_000_000_000.0 / batch_p50_ns },
        allocation: allocations,
        metrics: DispatchMetrics {
            name_lookups: metrics_after.name_lookups.saturating_sub(metrics_before.name_lookups),
            typed_calls: typed,
            generic_calls: generic,
            generic_fallback_calls: metrics_after.generic_fallback_calls.saturating_sub(metrics_before.generic_fallback_calls),
            cache_hits: metrics_after.cache_hits.saturating_sub(metrics_before.cache_hits),
            guard_misses: metrics_after.guard_misses.saturating_sub(metrics_before.guard_misses),
            proxy_get_traps: metrics_after.proxy_get_traps.saturating_sub(metrics_before.proxy_get_traps),
            capability_denials: metrics_after.capability_denials.saturating_sub(metrics_before.capability_denials),
            signature_mismatches: metrics_after.signature_mismatches.saturating_sub(metrics_before.signature_mismatches),
            total_calls: metrics_after.total_calls.saturating_sub(metrics_before.total_calls),
            table_version: metrics_after.table_version,
            active_slots: metrics_after.active_slots,
        },
        fallback_rate,
    })
}

fn measure_lookup<F>(
    name: &'static str,
    config: &Config,
    registry: &HostFunctionRegistry,
    operation: F,
) -> Result<Measurement, String>
where
    F: FnMut() -> Result<(), String>,
{
    measure(name, config, registry, operation)
}

fn json_escape(input: &str) -> String {
    let mut output = String::with_capacity(input.len() + 8);
    for ch in input.chars() {
        match ch {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(output, "\\u{:04x}", c as u32);
            }
            c => output.push(c),
        }
    }
    output
}

fn report(config: &Config, measurements: &[Measurement], abi_major: u16, abi_minor: u16) -> String {
    let rustc = env::var("RUSTC_VERSION").unwrap_or_else(|_| "not supplied".into());
    let cpu = env::var("OGM21_CPU_MODEL").unwrap_or_else(|_| "not supplied".into());
    let mut output = String::new();
    let _ = writeln!(output, "{{");
    let _ = writeln!(output, "  \"schema_version\": 1,");
    let _ = writeln!(output, "  \"benchmark\": \"ogma-typed-host-dispatch-m0\",");
    let _ = writeln!(output, "  \"source_revision\": \"{}\",", json_escape(&config.revision));
    let _ = writeln!(output, "  \"tested_revision\": \"{}\",", json_escape(&config.tested_revision));
    let _ = writeln!(output, "  \"environment\": {{");
    let _ = writeln!(output, "    \"rustc\": \"{}\",", json_escape(&rustc));
    let _ = writeln!(output, "    \"os\": \"{}\",", env::consts::OS);
    let _ = writeln!(output, "    \"arch\": \"{}\",", env::consts::ARCH);
    let _ = writeln!(output, "    \"cpu\": \"{}\",", json_escape(&cpu));
    let _ = writeln!(output, "    \"profile\": \"{}\",", if cfg!(debug_assertions) { "debug" } else { "release" });
    let _ = writeln!(output, "    \"iterations_per_batch\": {},", config.iterations);
    let _ = writeln!(output, "    \"warmup_rounds\": {},", config.warmups);
    let _ = writeln!(output, "    \"measurement_samples\": {},", config.samples);
    let _ = writeln!(output, "    \"single_call_latency_samples\": {}", config.iterations);
    let _ = writeln!(output, "  }},");
    let _ = writeln!(output, "  \"abi\": {{\"major\": {}, \"minor\": {}, \"descriptor_version\": 1}},", abi_major, abi_minor);
    let _ = writeln!(output, "  \"measurements\": [");
    for (index, item) in measurements.iter().enumerate() {
        let comma = if index + 1 == measurements.len() { "" } else { "," };
        let _ = writeln!(output, "    {{");
        let _ = writeln!(output, "      \"name\": \"{}\",", item.name);
        let _ = writeln!(output, "      \"single_call_latency_ns\": {{\"p50\": {:.2}, \"p95\": {:.2}, \"p99\": {:.2}}},", item.p50_ns, item.p95_ns, item.p99_ns);
        let _ = writeln!(output, "      \"batch_median_latency_ns\": {:.2},", item.batch_p50_ns);
        let _ = writeln!(output, "      \"throughput_ops_per_second\": {:.2},", item.throughput_ops_sec);
        let _ = writeln!(output, "      \"allocations\": {{\"alloc_calls\": {}, \"realloc_calls\": {}, \"allocated_bytes\": {}}},", item.allocation.alloc_calls, item.allocation.realloc_calls, item.allocation.allocated_bytes);
        let _ = writeln!(output, "      \"dispatch\": {{\"table_version\": {}, \"active_slots\": {}, \"calls\": {}, \"typed_calls\": {}, \"generic_calls\": {}, \"generic_fallback_calls\": {}, \"generic_fallback_rate\": {:.6}, \"name_lookups\": {}, \"cache_hits\": {}, \"guard_misses\": {}, \"proxy_get_traps\": {}, \"capability_denials\": {}, \"signature_mismatches\": {}}}", item.metrics.table_version, item.metrics.active_slots, item.metrics.total_calls, item.metrics.typed_calls, item.metrics.generic_calls, item.metrics.generic_fallback_calls, item.fallback_rate, item.metrics.name_lookups, item.metrics.cache_hits, item.metrics.guard_misses, item.metrics.proxy_get_traps, item.metrics.capability_denials, item.metrics.signature_mismatches);
        let _ = writeln!(output, "    }}{comma}");
    }
    let _ = writeln!(output, "  ]");
    let _ = writeln!(output, "}}");
    output
}

fn main() -> Result<(), String> {
    let config = parse_config()?;
    let registry = HostFunctionRegistry::new().map_err(|e| format!("registry initialization: {e:?}"))?;

    let primitive = registry.register(
        HostFunctionDescriptor::f64_binary(
            HostFunctionId(0x2101), "ogma:bench/add-f64", 0, 0,
            generated_primitive_add, generic_add, ptr::null_mut(),
        ).map_err(|e| format!("primitive descriptor: {e:?}"))?
    ).map_err(|e| format!("primitive registration: {e:?}"))?;

    let handles = registry.register(
        HostFunctionDescriptor::number_handle_binary(
            HostFunctionId(0x2102), "ogma:bench/add-handles", 0, 0,
            generated_handle_add, generic_add, ptr::null_mut(),
        ).map_err(|e| format!("handle descriptor: {e:?}"))?
    ).map_err(|e| format!("handle registration: {e:?}"))?;

    let generic = registry.register(
        HostFunctionDescriptor::generic(
            HostFunctionId(0x2103), "ogma:bench/generic-add", 0, 0,
            generic_add, ptr::null_mut(),
        ).map_err(|e| format!("generic descriptor: {e:?}"))?
    ).map_err(|e| format!("generic registration: {e:?}"))?;

    let left_handle = registry.create_number(2.0).map_err(|e| format!("left handle: {e:?}"))?;
    let right_handle = registry.create_number(3.0).map_err(|e| format!("right handle: {e:?}"))?;
    let handle_args = [DispatchValue::Handle(left_handle), DispatchValue::Handle(right_handle)];
    let primitive_args = [DispatchValue::Number(2.0), DispatchValue::Number(3.0)];

    let host_context = HostExecutionContext::new(LibrarySyncMode::None);
    let scope = host_context.enter_scope().map_err(|e| format!("execution scope: {e:?}"))?;

    let mut object = OrdinaryHostObject::new();
    object.define("add", JsCallable::host(primitive));
    let call_site = registry.resolve_call_site(&object, "add").ok_or("could not resolve call-site guard")?;

    let mut measurements = Vec::new();

    measurements.push(measure("raw_abi_primitive_baseline", &config, &registry, || {
        let mut result = 0.0;
        let status = unsafe { ogma_primitive_add_f64(2.0, 3.0, &mut result) };
        if status != OgmaStatus::OK || result != 5.0 {
            return Err(format!("raw ABI baseline failed: {status:?}, {result}"));
        }
        black_box(result);
        Ok(())
    })?);

    measurements.push(measure_lookup("dynamic_name_lookup_only", &config, &registry, || {
        let resolved = registry.resolve_name("ogma:bench/add-f64").map_err(|e| format!("{e:?}"))?;
        if resolved != primitive { return Err("name lookup resolved a different slot".into()); }
        black_box(resolved);
        Ok(())
    })?);

    measurements.push(measure("dynamic_name_lookup_typed_primitive", &config, &registry, || {
        checked(registry.invoke_by_name(&scope, "ogma:bench/add-f64", 0, &primitive_args))
    })?);

    measurements.push(measure("pre_resolved_typed_primitive_slot", &config, &registry, || {
        checked(registry.invoke_slot(&scope, primitive, 0, &primitive_args))
    })?);

    measurements.push(measure("pre_resolved_typed_opaque_handle_slot", &config, &registry, || {
        checked(registry.invoke_slot(&scope, handles, 0, &handle_args))
    })?);

    measurements.push(measure("dynamic_name_lookup_generic_callback", &config, &registry, || {
        checked(registry.invoke_by_name(&scope, "ogma:bench/generic-add", 0, &handle_args))
    })?);

    measurements.push(measure("pre_resolved_generic_fallback", &config, &registry, || {
        checked(registry.invoke_generic_slot(&scope, primitive, 0, &primitive_args))
    })?);

    measurements.push(measure("guarded_ordinary_object_callsite_hit", &config, &registry, || {
        checked(registry.call_property(&scope, PropertyReceiver::Ordinary(&object), "add",
            Some(&call_site), 0, &primitive_args))
    })?);

    let report = report(&config, &measurements, primitive.abi_major, primitive.abi_minor);
    if let Some(path) = config.output {
        fs::write(&path, &report).map_err(|e| format!("write {}: {e}", path.display()))?;
    } else {
        print!("{report}");
    }
    Ok(())
}
