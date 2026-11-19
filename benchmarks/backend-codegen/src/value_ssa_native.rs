//! OGM-38 M0 native Value/SSA evidence.
//!
//! The generated function is a numeric lowering of the canonical
//! positive-loop-carried-values Core IR fixture. It is deliberately not a
//! production ECMAScript Runtime or a general Core IR compiler.
use anyhow::{Context, Result, anyhow, bail};
use cranelift_codegen::{
    ir::{AbiParam, BlockArg, InstBuilder, MemFlagsData, Value, types},
    settings::{self, Configurable},
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{Linkage, Module, default_libcall_names};
use cranelift_object::{ObjectBuilder, ObjectModule};
use libloading::Library;
use ogma_gc_comparison_proof::{CollectorKind, FrameKind, GcHeap};
use regalloc2::{
    Algorithm, Block, Edit, Function as RegallocFunction, Inst, InstRange, MachineEnv, Operand,
    PReg, PRegSet, RegClass, RegallocOptions, VReg, run as run_regalloc,
};
use serde::Serialize;
use serde_json::Value as JsonValue;
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    hint::black_box,
    mem::size_of,
    path::{Path, PathBuf},
    process::Command,
    time::Instant,
};

const NUMBER_TAG: u64 = 3;
const STRING_TAG: u64 = 4;
const BIGINT_TAG: u64 = 5;
const OBJECT_TAG: u64 = 6;
const SYMBOL_TAG: u64 = 7;
const NANBOX_STRING_TAG: u64 = 0x7ffc;
const NANBOX_BIGINT_TAG: u64 = 0x7ffd;
const NANBOX_OBJECT_TAG: u64 = 0x7ffe;
const NANBOX_SYMBOL_TAG: u64 = 0x7fff;
const NATIVE_HANDLE_TAGS: [u64; 4] = [STRING_TAG, BIGINT_TAG, OBJECT_TAG, SYMBOL_TAG];
const NANBOX_HANDLE_TAGS: [u64; 4] = [
    NANBOX_STRING_TAG,
    NANBOX_BIGINT_TAG,
    NANBOX_OBJECT_TAG,
    NANBOX_SYMBOL_TAG,
];
const NAN_TAG_SHIFT: u32 = 48;
const MAX_PAYLOAD: u64 = (1_u64 << NAN_TAG_SHIFT) - 1;
const NANBOX_RESERVED_TAG_MIN: u64 = 0x7ff9_0000_0000_0000;
const NANBOX_RESERVED_TAG_MAX: u64 = 0x7fff_ffff_ffff_ffff;
const CANONICAL_NAN_BITS: u64 = 0x7ff8_0000_0000_0000;
const DEFAULT_MAX_ITERATIONS: u64 = 1_000_000;
const GC_OBJECT_COUNT: usize = 2_048;
const GC_VALUE_SLOTS_PER_OBJECT: usize = 16;
const NANBOX_OBJECT_TAG: u64 = 0x7ffe;

fn canonical_number_bits(value: f64) -> u64 {
    if value.is_nan() {
        CANONICAL_NAN_BITS
    } else {
        value.to_bits()
    }
}

type NativeLoop = unsafe extern "C" fn(u64, u64, u64, u64, u64) -> i32;
type NativeHandleDispatch = unsafe extern "C" fn(u64, u64, u64) -> i32;

struct NativeHandleCallbackContext {
    heap: *mut GcHeap,
    expected_object: ogma_gc_comparison_proof::GcRef,
    expected_tag: u64,
    calls: u64,
    collections: u64,
    force_collection: bool,
}

// SAFETY: the function pointer is called only by this synchronous native
// benchmark with a live, uniquely borrowed context on the same thread.
unsafe extern "C" fn resolve_native_handle_callback(
    context_pointer: u64,
    tag: u64,
    token: u64,
) -> i32 {
    if context_pointer == 0 || token == 0 {
        return 1;
    }
    // SAFETY: the caller passes a pointer to NativeHandleCallbackContext for
    // the entire synchronous native call; no concurrent access is performed.
    let context = unsafe { &mut *(context_pointer as *mut NativeHandleCallbackContext) };
    context.calls += 1;
    if tag != context.expected_tag || context.heap.is_null() {
        return 2;
    }
    // SAFETY: context.heap points at the live GcHeap owned by the benchmark.
    let heap = unsafe { &mut *context.heap };
    if context.force_collection {
        if heap.collect().is_err() {
            return 3;
        }
        context.collections += 1;
    }
    match heap.resolve_host_handle_token(token) {
        Ok(reference) if reference == context.expected_object => 0,
        _ => 4,
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TaggedValue {
    pub tag: u64,
    pub payload: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RuntimeLayout {
    Tagged16,
    NanBox8,
}

impl RuntimeLayout {
    fn name(self) -> &'static str {
        match self {
            Self::Tagged16 => "tagged-16",
            Self::NanBox8 => "nanbox-8",
        }
    }

    fn value_bytes(self) -> usize {
        match self {
            Self::Tagged16 => size_of::<TaggedValue>(),
            Self::NanBox8 => size_of::<u64>(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SsaPolicy {
    Boxed,
    RepresentationAware,
}

impl SsaPolicy {
    fn name(self) -> &'static str {
        match self {
            Self::Boxed => "boxed",
            Self::RepresentationAware => "representation-aware",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Candidate {
    runtime: RuntimeLayout,
    ssa: SsaPolicy,
}

impl Candidate {
    const ALL: [Self; 4] = [
        Self {
            runtime: RuntimeLayout::Tagged16,
            ssa: SsaPolicy::Boxed,
        },
        Self {
            runtime: RuntimeLayout::Tagged16,
            ssa: SsaPolicy::RepresentationAware,
        },
        Self {
            runtime: RuntimeLayout::NanBox8,
            ssa: SsaPolicy::Boxed,
        },
        Self {
            runtime: RuntimeLayout::NanBox8,
            ssa: SsaPolicy::RepresentationAware,
        },
    ];

    fn name(self) -> String {
        format!("{}-{}", self.runtime.name(), self.ssa.name())
    }
}

fn encode_candidate_number_value(
    runtime: RuntimeLayout,
    storage: &mut [u64],
    slot_index: usize,
    value: f64,
) -> Result<()> {
    let bits = canonical_number_bits(value);
    match runtime {
        RuntimeLayout::Tagged16 => {
            let word_index = slot_index
                .checked_mul(2)
                .context("tagged Value slot index overflow")?;
            let words = storage
                .get_mut(word_index..word_index + 2)
                .context("tagged Value storage slot out of bounds")?;
            words[0] = NUMBER_TAG;
            words[1] = bits;
        }
        RuntimeLayout::NanBox8 => {
            *storage
                .get_mut(slot_index)
                .context("NaN-box Value storage slot out of bounds")? = bits;
        }
    }
    Ok(())
}

fn encode_candidate_reference_value(
    runtime: RuntimeLayout,
    storage: &mut [u64],
    slot_index: usize,
    tag: u64,
    token: u64,
) -> Result<()> {
    if token == 0 || token > MAX_PAYLOAD {
        bail!("candidate object Value token is outside the 48-bit payload range");
    }
    match runtime {
        RuntimeLayout::Tagged16 => {
            let word_index = slot_index
                .checked_mul(2)
                .context("tagged Value slot index overflow")?;
            let words = storage
                .get_mut(word_index..word_index + 2)
                .context("tagged Value storage slot out of bounds")?;
            if !NATIVE_HANDLE_TAGS.contains(&tag) {
                bail!("unsupported tagged reference Value tag {tag}");
            }
            words[0] = tag;
            words[1] = token;
        }
        RuntimeLayout::NanBox8 => {
            let nanbox_tag = match tag {
                STRING_TAG => NANBOX_STRING_TAG,
                BIGINT_TAG => NANBOX_BIGINT_TAG,
                OBJECT_TAG => NANBOX_OBJECT_TAG,
                SYMBOL_TAG => NANBOX_SYMBOL_TAG,
                _ => bail!("unsupported NaN-box reference Value tag {tag}"),
            };
            *storage
                .get_mut(slot_index)
                .context("NaN-box Value storage slot out of bounds")? =
                (nanbox_tag << NAN_TAG_SHIFT) | token;
        }
    }
    Ok(())
}


fn encode_candidate_object_value(
    runtime: RuntimeLayout,
    storage: &mut [u64],
    slot_index: usize,
    token: u64,
) -> Result<()> {
    encode_candidate_reference_value(runtime, storage, slot_index, OBJECT_TAG, token)
}

fn decode_candidate_object_value(
    runtime: RuntimeLayout,
    storage: &[u64],
    slot_index: usize,
) -> Result<Option<u64>> {
    match runtime {
        RuntimeLayout::Tagged16 => {
            let word_index = slot_index
                .checked_mul(2)
                .context("tagged Value slot index overflow")?;
            let words = storage
                .get(word_index..word_index + 2)
                .context("tagged Value storage slot out of bounds")?;
            match words[0] {
                NUMBER_TAG => Ok(None),
                STRING_TAG | BIGINT_TAG | OBJECT_TAG | SYMBOL_TAG
                    if words[1] != 0 && words[1] <= MAX_PAYLOAD => Ok(Some(words[1])),
                STRING_TAG | BIGINT_TAG | OBJECT_TAG | SYMBOL_TAG => {
                    bail!("tagged reference Value has an invalid handle token")
                }
                tag => bail!("unsupported tagged Value tag {tag} in GC root scan"),
            }
        }
        RuntimeLayout::NanBox8 => {
            let word = *storage
                .get(slot_index)
                .context("NaN-box Value storage slot out of bounds")?;
            let tag = word >> NAN_TAG_SHIFT;
            if NANBOX_HANDLE_TAGS.contains(&tag) {
                let token = word & MAX_PAYLOAD;
                if token == 0 {
                    bail!("NaN-box reference Value has an invalid handle token");
                }
                return Ok(Some(token));
            }
            let exponent = word & 0x7ff0_0000_0000_0000;
            let fraction = word & 0x000f_ffff_ffff_ffff;
            if exponent == 0x7ff0_0000_0000_0000 && fraction != 0 && word != CANONICAL_NAN_BITS {
                bail!("non-canonical NaN encountered while scanning candidate Values");
            }
            if (NANBOX_RESERVED_TAG_MIN..=NANBOX_RESERVED_TAG_MAX).contains(&word) {
                bail!("unsupported NaN-box tag {tag:#06x} in GC root scan");
            }
            Ok(None)
        }
    }
}

fn resolve_candidate_object_value(
    runtime: RuntimeLayout,
    storage: &[u64],
    slot_index: usize,
    heap: &ogma_gc_comparison_proof::GcHeap,
) -> Result<ogma_gc_comparison_proof::GcRef> {
    let token = decode_candidate_object_value(runtime, storage, slot_index)?
        .context("candidate Value slot does not contain an object reference")?;
    heap.resolve_host_handle_token(token)
        .context("candidate Value payload did not resolve through OGM-26 HostHandle registry")
}

#[derive(Clone, Debug, Serialize)]
struct Distribution {
    p50_ns: f64,
    p95_ns: f64,
    p99_ns: f64,
    raw_samples_ns: Vec<f64>,
}

#[derive(Clone, Debug, Serialize)]
struct RegisterPressure {
    allocator: &'static str,
    workload: &'static str,
    machine_env: &'static str,
    integer_registers_used: usize,
    float_registers_used: usize,
    register_allocations: usize,
    spill_store_edits: usize,
    spill_reload_edits: usize,
    spill_slots: usize,
    spill_bytes: usize,
}

#[derive(Clone, Debug, Serialize)]
struct GcSample {
    build_ns: u128,
    collection_ns: u128,
    root_scan_ns: u128,
    cleanup_collection_ns: u128,
    roots_scanned: usize,
    edges_examined: u64,
    graph_edges: usize,
    interpreter_frame_roots: usize,
    native_frame_roots: usize,
    persistent_host_handles: usize,
    temporary_scope_roots: usize,
    logical_heap_bytes_before_collection: u64,
    peak_logical_heap_bytes: u64,
    actual_value_storage_capacity_bytes: usize,
    encoded_object_values_resolved: usize,
    reclaimed_after_root_release: usize,
    live_objects_after_release: u64,
}

#[derive(Clone, Debug, Serialize)]
struct GcCollectorEvidence {
    collector: &'static str,
    object_count: usize,
    edge_count: usize,
    interpreter_frame_roots: usize,
    native_frame_roots: usize,
    persistent_host_handles: usize,
    temporary_scope_roots: usize,
    build_ns: Distribution,
    collection_ns: Distribution,
    root_scan_ns: Distribution,
    cleanup_collection_ns: Distribution,
    peak_logical_heap_bytes_max: u64,
    actual_value_storage_capacity_bytes: usize,
    encoded_object_values_resolved: usize,
    raw_samples: Vec<GcSample>,
}

#[derive(Clone, Debug, Serialize)]
struct GcEvidence {
    collector_implementation: &'static str,
    object_count: usize,
    graph_edges: usize,
    value_slots_per_object: usize,
    value_storage_bytes: usize,
    value_layout_bytes: usize,
    handle_payload_bits: u32,
    encoded_object_values_resolved: usize,
    collector_comparisons: Vec<GcCollectorEvidence>,
}

#[derive(Clone, Debug, Serialize)]
struct NativeSemanticCase {
    name: &'static str,
    expected_status: i32,
    actual_status: i32,
    expected_result_bits: Option<u64>,
    actual_result_bits: Option<u64>,
    matches: bool,
}

#[derive(Clone, Debug, Serialize)]
struct NativeSemanticValidation {
    numeric_edge_cases: Vec<NativeSemanticCase>,
    invalid_tag_rejected: bool,
    noncanonical_nanbox_words_rejected: Option<bool>,
}

#[derive(Clone, Debug, Serialize)]
struct NativeHandleTagEvidence {
    label: &'static str,
    tagged_value_tag: u64,
    nanbox_value_tag: u64,
    callback_status: i32,
    callback_invocations: u64,
    forced_collections: u64,
    handle_resolved_to_expected_object: bool,
}

#[derive(Clone, Debug, Serialize)]
struct NativeHandleDispatchEvidence {
    helper_calls_per_sample: u64,
    warmup_rounds: usize,
    throughput_ns_per_call: Distribution,
    supported_reference_tags: Vec<NativeHandleTagEvidence>,
    native_code: NativeCodeMetrics,
    all_callbacks_resolved_expected_handles: bool,
}

#[derive(Clone, Debug, Serialize)]
struct NativeCodeMetrics {
    disassembler: &'static str,
    symbol: &'static str,
    instruction_count: usize,
    function_code_bytes: usize,
    integer_registers: Vec<String>,
    vector_registers: Vec<String>,
    stack_relative_memory_instructions: usize,
    stack_relative_loads: usize,
    stack_relative_stores: usize,
    direct_call_instructions: usize,
}

#[derive(Clone, Debug, Serialize)]
struct CandidateEvidence {
    name: String,
    runtime_value_bytes: usize,
    native_ir_value_conversion_ops_per_loop_iteration: u32,
    native_ir_value_conversion_ops_per_invocation: u32,
    native_helper_calls_per_loop_iteration: u32,
    compile_first_ns: u64,
    compile_ns: Distribution,
    object_bytes: u64,
    shared_library_bytes: u64,
    iterations_per_sample: u64,
    warmup_rounds: usize,
    throughput_ns_per_call: Distribution,
    calls_per_second_p50: f64,
    completed_loop_iterations: u64,
    result_matches: bool,
    native_semantic_validation: NativeSemanticValidation,
    native_code: NativeCodeMetrics,
    native_handle_dispatch: NativeHandleDispatchEvidence,
    register_pressure: RegisterPressure,
    gc: GcEvidence,
}

#[derive(Clone, Debug, Serialize)]
struct Report {
    schema_version: u32,
    task: &'static str,
    source_revision: String,
    tested_revision: String,
    rustc: String,
    operating_system: String,
    architecture: String,
    target_triple: String,
    cpu_model: String,
    core_ir_contract: &'static str,
    core_ir_fixture: &'static str,
    core_ir_fixture_parameters: Vec<String>,
    semantic_workload: &'static str,
    iterations_per_sample: u64,
    warmup_rounds: usize,
    measurement_samples: usize,
    max_loop_iterations: u64,
    candidates: Vec<CandidateEvidence>,
    measurement_boundaries: BTreeMap<&'static str, &'static str>,
    unsupported_native_semantics: Vec<&'static str>,
}

fn percentile(samples: &[f64], percentile: f64) -> f64 {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let index = ((sorted.len() - 1) as f64 * percentile).ceil() as usize;
    sorted[index]
}

fn distribution(samples: Vec<f64>) -> Distribution {
    Distribution {
        p50_ns: percentile(&samples, 0.50),
        p95_ns: percentile(&samples, 0.95),
        p99_ns: percentile(&samples, 0.99),
        raw_samples_ns: samples,
    }
}

fn command_output(program: &str, args: &[&str]) -> String {
    Command::new(program)
        .args(args)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_else(|| "unavailable".to_owned())
}

fn cpu_model() -> String {
    fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|contents| {
            contents.lines().find_map(|line| {
                line.strip_prefix("model name").and_then(|value| {
                    value
                        .split_once(':')
                        .map(|(_, name)| name.trim().to_owned())
                })
            })
        })
        .unwrap_or_else(|| "unavailable".to_owned())
}

fn canonical_call_boundary_parameters() -> Result<Vec<String>> {
    let fixture: JsonValue = serde_json::from_str(include_str!(
        "../../../tests/core-ir/fixtures/positive-call-boundary.json"
    ))?;
    if fixture["contract"] != "ogma.core-ir" || fixture["version"] != "1.0.0" {
        bail!("OGM-38 expected ogma.core-ir/1.0.0");
    }
    let params = fixture["function"]["parameters"]
        .as_array()
        .context("canonical call-boundary parameters missing")?;
    let names: Vec<String> = params
        .iter()
        .map(|param| {
            param["id"]
                .as_str()
                .map(str::to_owned)
                .context("Core IR parameter id missing")
        })
        .collect::<Result<_>>()?;
    let expected = [
        "callee",
        "receiver",
        "text",
        "object_left",
        "object_right",
        "numeric",
    ];
    if names.iter().map(String::as_str).collect::<Vec<_>>() != expected {
        bail!("canonical call-boundary fixture parameter profile changed");
    }
    Ok(names)
}

fn verify_canonical_loop_fixture() -> Result<()> {
    let fixture: JsonValue = serde_json::from_str(include_str!(
        "../../../tests/core-ir/fixtures/positive-loop-carried-values.json"
    ))?;
    if fixture["contract"] != "ogma.core-ir"
        || fixture["version"] != "1.0.0"
        || fixture["function"]["id"] != "fn_loop_carried"
        || fixture["function"]["entry_block"] != "entry"
    {
        bail!("OGM-38 native lowering expects canonical Core IR loop fixture fn_loop_carried");
    }

    let parameters = fixture["function"]["parameters"]
        .as_array()
        .context("loop parameters missing")?;
    let parameter_profile: Vec<(&str, &str)> = parameters
        .iter()
        .map(|parameter| {
            Ok((
                parameter["id"]
                    .as_str()
                    .context("loop parameter id missing")?,
                parameter["representation"]
                    .as_str()
                    .context("loop parameter representation missing")?,
            ))
        })
        .collect::<Result<_>>()?;
    if parameter_profile != [("start", "f64"), ("limit", "f64"), ("step", "f64")] {
        bail!("canonical loop input representation profile changed");
    }

    let blocks = fixture["function"]["blocks"]
        .as_array()
        .context("loop blocks missing")?;
    let block_ids: Vec<&str> = blocks
        .iter()
        .map(|block| block["id"].as_str().unwrap_or(""))
        .collect();
    if block_ids != ["entry", "loop", "body", "exit"] {
        bail!("canonical loop block order changed");
    }

    let loop_block = &blocks[1];
    let loop_instructions = loop_block["instructions"]
        .as_array()
        .context("loop instructions missing")?;
    if loop_instructions.len() != 1
        || loop_instructions[0]["op"] != "js.strict_equal"
        || loop_instructions[0]["operands"] != serde_json::json!(["current", "loop_limit"])
        || loop_block["terminator"]["kind"] != "branch"
        || loop_block["terminator"]["condition"] != "done"
        || loop_block["terminator"]["then"]["target"] != "exit"
        || loop_block["terminator"]["else"]["target"] != "body"
    {
        bail!("canonical loop comparison/branch profile changed");
    }

    let body = &blocks[2];
    let body_instructions = body["instructions"]
        .as_array()
        .context("loop body instructions missing")?;
    if body_instructions.len() != 2
        || body_instructions[0]["op"] != "js.add"
        || body_instructions[0]["operands"] != serde_json::json!(["body_current", "step"])
        || body_instructions[0]["result"]["id"] != "incremented"
        || body_instructions[1]["op"] != "js.to_number"
        || body_instructions[1]["operands"] != serde_json::json!(["incremented"])
        || body_instructions[1]["result"]["id"] != "next_current"
        || body["terminator"]["kind"] != "jump"
        || body["terminator"]["target"] != "loop"
        || body["terminator"]["arguments"][0] != "next_current"
    {
        bail!("canonical loop body or loop-carried edge changed");
    }

    if blocks[3]["terminator"]["kind"] != "return"
        || blocks[3]["terminator"]["value"] != "final_value"
    {
        bail!("canonical loop exit/return profile changed");
    }
    Ok(())
}

fn candidate_output_path(root: &Path, candidate: Candidate, extension: &str) -> PathBuf {
    root.join(format!("{}.{}", candidate.name(), extension))
}

fn nanbox_word_is_non_number(builder: &mut FunctionBuilder<'_>, word: Value) -> Value {
    let reserved_min = builder
        .ins()
        .iconst(types::I64, NANBOX_RESERVED_TAG_MIN as i64);
    let reserved_max = builder
        .ins()
        .iconst(types::I64, NANBOX_RESERVED_TAG_MAX as i64);
    let reserved_ge = builder.ins().icmp(
        cranelift_codegen::ir::condcodes::IntCC::UnsignedGreaterThanOrEqual,
        word,
        reserved_min,
    );
    let reserved_le = builder.ins().icmp(
        cranelift_codegen::ir::condcodes::IntCC::UnsignedLessThanOrEqual,
        word,
        reserved_max,
    );
    let reserved_tag = builder.ins().band(reserved_ge, reserved_le);

    let exponent_mask = builder
        .ins()
        .iconst(types::I64, 0x7ff0_0000_0000_0000_u64 as i64);
    let fraction_mask = builder
        .ins()
        .iconst(types::I64, 0x000f_ffff_ffff_ffff_u64 as i64);
    let exponent = builder.ins().band(word, exponent_mask);
    let fraction = builder.ins().band(word, fraction_mask);
    let exponent_all_ones = builder.ins().icmp(
        cranelift_codegen::ir::condcodes::IntCC::Equal,
        exponent,
        exponent_mask,
    );
    let zero = builder.ins().iconst(types::I64, 0);
    let fraction_nonzero = builder.ins().icmp(
        cranelift_codegen::ir::condcodes::IntCC::NotEqual,
        fraction,
        zero,
    );
    let is_nan = builder.ins().band(exponent_all_ones, fraction_nonzero);
    let canonical_nan = builder.ins().iconst(types::I64, CANONICAL_NAN_BITS as i64);
    let is_not_canonical_nan = builder.ins().icmp(
        cranelift_codegen::ir::condcodes::IntCC::NotEqual,
        word,
        canonical_nan,
    );
    let noncanonical_nan = builder.ins().band(is_nan, is_not_canonical_nan);
    builder.ins().bor(reserved_tag, noncanonical_nan)
}

fn make_candidate_function(candidate: Candidate, object_path: &Path) -> Result<u64> {
    let start = Instant::now();
    let mut flags = settings::builder();
    flags.set("opt_level", "speed")?;
    let isa = cranelift_native::builder()
        .map_err(|error| anyhow!(error))?
        .finish(settings::Flags::new(flags))?;
    let builder = ObjectBuilder::new(isa, "ogma-38-value-ssa", default_libcall_names())?;
    let mut module = ObjectModule::new(builder);

    let mut signature = module.make_signature();
    signature.params.extend(
        [types::I64, types::I64, types::I64, types::I64, types::I64]
            .into_iter()
            .map(AbiParam::new),
    );
    signature.returns.push(AbiParam::new(types::I32));
    let function = module.declare_function("ogma_value_loop", Linkage::Export, &signature)?;

    let mut dispatch_signature = module.make_signature();
    dispatch_signature.params.extend(
        [types::I64, types::I64, types::I64]
            .into_iter()
            .map(AbiParam::new),
    );
    dispatch_signature.returns.push(AbiParam::new(types::I32));
    let dispatch_function = module.declare_function(
        "ogma_value_handle_dispatch",
        Linkage::Export,
        &dispatch_signature,
    )?;

    let mut callback_signature = module.make_signature();
    callback_signature.params.extend(
        [types::I64, types::I64, types::I64]
            .into_iter()
            .map(AbiParam::new),
    );
    callback_signature.returns.push(AbiParam::new(types::I32));

    let mut context = module.make_context();
    context.func.signature = signature;
    let mut builder_context = FunctionBuilderContext::new();

    {
        let mut b = FunctionBuilder::new(&mut context.func, &mut builder_context);
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        let parameters = b.block_params(entry).to_vec();

        let loop_block = b.create_block();
        let body_block = b.create_block();
        let add_block = b.create_block();
        let done_block = b.create_block();
        let exhausted_block = b.create_block();
        let invalid_block = b.create_block();
        let check_second_tag = b.create_block();
        let check_third_tag = b.create_block();
        let initial_state = b.create_block();

        let state_types: Vec<cranelift_codegen::ir::Type> = match (candidate.runtime, candidate.ssa)
        {
            (RuntimeLayout::Tagged16, SsaPolicy::Boxed) => vec![
                types::I64,
                types::I64,
                types::I64,
                types::I64,
                types::I64,
                types::I64,
                types::I64,
            ],
            (RuntimeLayout::NanBox8, SsaPolicy::Boxed) => {
                vec![types::I64, types::I64, types::I64, types::I64]
            }
            (_, SsaPolicy::RepresentationAware) => {
                vec![types::F64, types::F64, types::F64, types::I64]
            }
        };

        let append_state_params =
            |builder: &mut FunctionBuilder, block: cranelift_codegen::ir::Block| {
                for ty in &state_types {
                    builder.append_block_param(block, *ty);
                }
            };
        for block in [loop_block, body_block, add_block, done_block] {
            append_state_params(&mut b, block);
        }
        b.switch_to_block(entry);
        b.seal_block(entry);

        if candidate.runtime == RuntimeLayout::Tagged16 {
            let number_tag = b.ins().iconst(types::I64, NUMBER_TAG as i64);
            let start_tag = b
                .ins()
                .load(types::I64, MemFlagsData::new(), parameters[0], 0);
            let limit_tag = b
                .ins()
                .load(types::I64, MemFlagsData::new(), parameters[1], 0);
            let step_tag = b
                .ins()
                .load(types::I64, MemFlagsData::new(), parameters[2], 0);
            let start_is_number = b.ins().icmp(
                cranelift_codegen::ir::condcodes::IntCC::Equal,
                start_tag,
                number_tag,
            );
            b.ins()
                .brif(start_is_number, check_second_tag, &[], invalid_block, &[]);
            b.switch_to_block(check_second_tag);
            let limit_is_number = b.ins().icmp(
                cranelift_codegen::ir::condcodes::IntCC::Equal,
                limit_tag,
                number_tag,
            );
            b.ins()
                .brif(limit_is_number, check_third_tag, &[], invalid_block, &[]);
            b.switch_to_block(check_third_tag);
            let step_is_number = b.ins().icmp(
                cranelift_codegen::ir::condcodes::IntCC::Equal,
                step_tag,
                number_tag,
            );
            b.ins()
                .brif(step_is_number, initial_state, &[], invalid_block, &[]);
        } else {
            // OGM-17's NaN-box format reserves 0x7ff9..=0x7fff for non-number
            // tags and accepts only the canonical positive NaN bit pattern.
            // Reject both tagged handles and non-canonical IEEE NaNs before
            // entering a numeric fast path.
            let start_non_number = nanbox_word_is_non_number(&mut b, parameters[0]);
            b.ins()
                .brif(start_non_number, invalid_block, &[], check_second_tag, &[]);

            b.switch_to_block(check_second_tag);
            let limit_non_number = nanbox_word_is_non_number(&mut b, parameters[1]);
            b.ins()
                .brif(limit_non_number, invalid_block, &[], check_third_tag, &[]);

            b.switch_to_block(check_third_tag);
            let step_non_number = nanbox_word_is_non_number(&mut b, parameters[2]);
            b.ins()
                .brif(step_non_number, invalid_block, &[], initial_state, &[]);
        }

        b.switch_to_block(initial_state);
        let initial_values = match (candidate.runtime, candidate.ssa) {
            (RuntimeLayout::Tagged16, SsaPolicy::Boxed) => {
                let start_tag = b
                    .ins()
                    .load(types::I64, MemFlagsData::new(), parameters[0], 0);
                let start_payload = b
                    .ins()
                    .load(types::I64, MemFlagsData::new(), parameters[0], 8);
                let limit_tag = b
                    .ins()
                    .load(types::I64, MemFlagsData::new(), parameters[1], 0);
                let limit_payload = b
                    .ins()
                    .load(types::I64, MemFlagsData::new(), parameters[1], 8);
                let step_tag = b
                    .ins()
                    .load(types::I64, MemFlagsData::new(), parameters[2], 0);
                let step_payload = b
                    .ins()
                    .load(types::I64, MemFlagsData::new(), parameters[2], 8);
                let zero = b.ins().iconst(types::I64, 0);
                vec![
                    start_tag,
                    start_payload,
                    limit_tag,
                    limit_payload,
                    step_tag,
                    step_payload,
                    zero,
                ]
            }
            (RuntimeLayout::Tagged16, SsaPolicy::RepresentationAware) => {
                let start_bits = b
                    .ins()
                    .load(types::I64, MemFlagsData::new(), parameters[0], 8);
                let limit_bits = b
                    .ins()
                    .load(types::I64, MemFlagsData::new(), parameters[1], 8);
                let step_bits = b
                    .ins()
                    .load(types::I64, MemFlagsData::new(), parameters[2], 8);
                let start = b.ins().bitcast(types::F64, MemFlagsData::new(), start_bits);
                let limit = b.ins().bitcast(types::F64, MemFlagsData::new(), limit_bits);
                let step = b.ins().bitcast(types::F64, MemFlagsData::new(), step_bits);
                let zero = b.ins().iconst(types::I64, 0);
                vec![start, limit, step, zero]
            }
            (RuntimeLayout::NanBox8, SsaPolicy::Boxed) => {
                let zero = b.ins().iconst(types::I64, 0);
                vec![parameters[0], parameters[1], parameters[2], zero]
            }
            (RuntimeLayout::NanBox8, SsaPolicy::RepresentationAware) => {
                let start = b
                    .ins()
                    .bitcast(types::F64, MemFlagsData::new(), parameters[0]);
                let limit = b
                    .ins()
                    .bitcast(types::F64, MemFlagsData::new(), parameters[1]);
                let step = b
                    .ins()
                    .bitcast(types::F64, MemFlagsData::new(), parameters[2]);
                let zero = b.ins().iconst(types::I64, 0);
                vec![start, limit, step, zero]
            }
        };
        let initial_args: Vec<BlockArg> = initial_values
            .iter()
            .copied()
            .map(BlockArg::Value)
            .collect();
        b.ins().jump(loop_block, &initial_args);

        b.switch_to_block(loop_block);
        let loop_state = b.block_params(loop_block).to_vec();
        let (current, limit, _step, _count) = match (candidate.runtime, candidate.ssa) {
            (RuntimeLayout::Tagged16, SsaPolicy::Boxed) => {
                let current_bits = b
                    .ins()
                    .bitcast(types::F64, MemFlagsData::new(), loop_state[1]);
                let limit_bits = b
                    .ins()
                    .bitcast(types::F64, MemFlagsData::new(), loop_state[3]);
                (current_bits, limit_bits, loop_state[5], loop_state[6])
            }
            (RuntimeLayout::NanBox8, SsaPolicy::Boxed) => {
                let current_bits = b
                    .ins()
                    .bitcast(types::F64, MemFlagsData::new(), loop_state[0]);
                let limit_bits = b
                    .ins()
                    .bitcast(types::F64, MemFlagsData::new(), loop_state[1]);
                (current_bits, limit_bits, loop_state[2], loop_state[3])
            }
            (_, SsaPolicy::RepresentationAware) => {
                (loop_state[0], loop_state[1], loop_state[2], loop_state[3])
            }
        };
        let equal = b.ins().fcmp(
            cranelift_codegen::ir::condcodes::FloatCC::Equal,
            current,
            limit,
        );
        let done_args: Vec<BlockArg> = loop_state.iter().copied().map(BlockArg::Value).collect();
        let body_args: Vec<BlockArg> = loop_state.iter().copied().map(BlockArg::Value).collect();
        b.ins()
            .brif(equal, done_block, &done_args, body_block, &body_args);

        b.switch_to_block(body_block);
        let body_state = b.block_params(body_block).to_vec();
        let body_count = match (candidate.runtime, candidate.ssa) {
            (RuntimeLayout::Tagged16, SsaPolicy::Boxed) => body_state[6],
            (RuntimeLayout::NanBox8, SsaPolicy::Boxed) => body_state[3],
            (_, SsaPolicy::RepresentationAware) => body_state[3],
        };
        let max_iterations = parameters[4];
        let within_budget = b.ins().icmp(
            cranelift_codegen::ir::condcodes::IntCC::UnsignedLessThan,
            body_count,
            max_iterations,
        );
        let add_args: Vec<BlockArg> = body_state.iter().copied().map(BlockArg::Value).collect();
        b.ins()
            .brif(within_budget, add_block, &add_args, exhausted_block, &[]);

        b.switch_to_block(add_block);
        let add_state = b.block_params(add_block).to_vec();
        let (add_current, add_step, add_count) = match (candidate.runtime, candidate.ssa) {
            (RuntimeLayout::Tagged16, SsaPolicy::Boxed) => (
                b.ins()
                    .bitcast(types::F64, MemFlagsData::new(), add_state[1]),
                b.ins()
                    .bitcast(types::F64, MemFlagsData::new(), add_state[5]),
                add_state[6],
            ),
            (RuntimeLayout::NanBox8, SsaPolicy::Boxed) => (
                b.ins()
                    .bitcast(types::F64, MemFlagsData::new(), add_state[0]),
                b.ins()
                    .bitcast(types::F64, MemFlagsData::new(), add_state[2]),
                add_state[3],
            ),
            (_, SsaPolicy::RepresentationAware) => (add_state[0], add_state[2], add_state[3]),
        };
        let sum = b.ins().fadd(add_current, add_step);
        let next_bits = b.ins().bitcast(types::I64, MemFlagsData::new(), sum);
        let next_count = b.ins().iadd_imm_s(add_count, 1);
        let mut next_state: Vec<BlockArg> =
            add_state.iter().copied().map(BlockArg::Value).collect();
        match (candidate.runtime, candidate.ssa) {
            (RuntimeLayout::Tagged16, SsaPolicy::Boxed) => {
                next_state[1] = BlockArg::Value(next_bits);
                next_state[6] = BlockArg::Value(next_count);
            }
            (RuntimeLayout::NanBox8, SsaPolicy::Boxed) => {
                next_state[0] = BlockArg::Value(next_bits);
                next_state[3] = BlockArg::Value(next_count);
            }
            (_, SsaPolicy::RepresentationAware) => {
                next_state[0] = BlockArg::Value(sum);
                next_state[3] = BlockArg::Value(next_count);
            }
        }
        b.ins().jump(loop_block, &next_state);

        b.switch_to_block(done_block);
        let done_state = b.block_params(done_block).to_vec();
        match (candidate.runtime, candidate.ssa) {
            (RuntimeLayout::Tagged16, SsaPolicy::Boxed) => {
                b.ins()
                    .store(MemFlagsData::new(), done_state[0], parameters[3], 0);
                b.ins()
                    .store(MemFlagsData::new(), done_state[1], parameters[3], 8);
            }
            (RuntimeLayout::Tagged16, SsaPolicy::RepresentationAware) => {
                let tag = b.ins().iconst(types::I64, NUMBER_TAG as i64);
                let bits = b
                    .ins()
                    .bitcast(types::I64, MemFlagsData::new(), done_state[0]);
                b.ins().store(MemFlagsData::new(), tag, parameters[3], 0);
                b.ins().store(MemFlagsData::new(), bits, parameters[3], 8);
            }
            (RuntimeLayout::NanBox8, SsaPolicy::Boxed) => {
                b.ins()
                    .store(MemFlagsData::new(), done_state[0], parameters[3], 0);
            }
            (RuntimeLayout::NanBox8, SsaPolicy::RepresentationAware) => {
                let bits = b
                    .ins()
                    .bitcast(types::I64, MemFlagsData::new(), done_state[0]);
                b.ins().store(MemFlagsData::new(), bits, parameters[3], 0);
            }
        }
        let ok = b.ins().iconst(types::I32, 0);
        b.ins().return_(&[ok]);

        b.switch_to_block(exhausted_block);
        let exceeded = b.ins().iconst(types::I32, 2);
        b.ins().return_(&[exceeded]);

        b.switch_to_block(invalid_block);
        let invalid = b.ins().iconst(types::I32, 1);
        b.ins().return_(&[invalid]);

        b.seal_all_blocks();
        b.finalize(module.target_config());
    }

    module.define_function(function, &mut context)?;
    module.clear_context(&mut context);

    // The second export is a real native helper boundary for candidate Values.
    // It extracts a tagged pointer or NaN-box word, then makes an indirect C ABI
    // callback with (context, tag, 48-bit handle token).
    context.func.signature = dispatch_signature;
    {
        let mut b = FunctionBuilder::new(&mut context.func, &mut builder_context);
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        let parameters = b.block_params(entry).to_vec();
        b.switch_to_block(entry);

        let (tag, payload) = match candidate.runtime {
            RuntimeLayout::Tagged16 => (
                b.ins().load(types::I64, MemFlagsData::new(), parameters[0], 0),
                b.ins().load(types::I64, MemFlagsData::new(), parameters[0], 8),
            ),
            RuntimeLayout::NanBox8 => (
                b.ins().ushr_imm(parameters[0], NAN_TAG_SHIFT as u8),
                b.ins().band_imm(parameters[0], MAX_PAYLOAD as i64),
            ),
        };

        let allowed_tags = match candidate.runtime {
            RuntimeLayout::Tagged16 => &NATIVE_HANDLE_TAGS,
            RuntimeLayout::NanBox8 => &NANBOX_HANDLE_TAGS,
        };
        let mut is_supported_reference = b.ins().icmp(
            cranelift_codegen::ir::condcodes::IntCC::Equal,
            tag,
            b.ins().iconst(types::I64, allowed_tags[0] as i64),
        );
        for allowed_tag in allowed_tags.iter().copied().skip(1) {
            let expected_tag = b.ins().iconst(types::I64, allowed_tag as i64);
            let matches = b.ins().icmp(
                cranelift_codegen::ir::condcodes::IntCC::Equal,
                tag,
                expected_tag,
            );
            is_supported_reference = b.ins().bor(is_supported_reference, matches);
        }

        let dispatch_block = b.create_block();
        let invalid_block = b.create_block();
        b.ins().brif(
            is_supported_reference,
            dispatch_block,
            &[],
            invalid_block,
            &[],
        );

        b.switch_to_block(dispatch_block);
        let callback_sig = b.import_signature(callback_signature.clone());
        let invoked = b.ins().call_indirect(
            callback_sig,
            parameters[1],
            &[parameters[2], tag, payload],
        );
        let callback_status = b.inst_results(invoked)[0];
        b.ins().return_(&[callback_status]);

        b.switch_to_block(invalid_block);
        let invalid_status = b.ins().iconst(types::I32, 1);
        b.ins().return_(&[invalid_status]);

        b.seal_all_blocks();
        b.finalize(module.target_config());
    }

    module.define_function(dispatch_function, &mut context)?;
    module.clear_context(&mut context);
    let product = module.finish();
    fs::write(object_path, product.emit()?)?;
    Ok(start.elapsed().as_nanos() as u64)
}

fn link_shared_library(object_path: &Path, library_path: &Path) -> Result<u64> {
    let start = Instant::now();
    let output = Command::new("cc")
        .args(["-shared", "-fPIC", "-Wl,-z,relro,-z,now"])
        .arg(object_path)
        .arg("-o")
        .arg(library_path)
        .output()
        .context("launch C linker for native Value/SSA object")?;
    if !output.status.success() {
        bail!(
            "linking {} failed: {}",
            object_path.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(start.elapsed().as_nanos() as u64)
}

fn compile_distribution(
    candidate: Candidate,
    object_path: &Path,
    warmups: usize,
    samples: usize,
) -> Result<(u64, Distribution)> {
    let first = make_candidate_function(candidate, object_path)?;
    for _ in 0..warmups {
        make_candidate_function(candidate, object_path)?;
    }
    let mut timings = Vec::with_capacity(samples);
    for _ in 0..samples {
        timings.push(make_candidate_function(candidate, object_path)? as f64);
    }
    Ok((first, distribution(timings)))
}

fn run_native_candidate(
    candidate: Candidate,
    library: &Library,
    iterations: u64,
    warmups: usize,
    samples: usize,
    expected: f64,
) -> Result<(Distribution, bool, NativeSemanticValidation)> {
    let function = unsafe {
        *library
            .get::<NativeLoop>(b"ogma_value_loop\0")
            .context("generated native Value/SSA entry point missing")?
    };

    let start = 0.0_f64;
    let limit = expected;
    let step = 1.0_f64;
    let max_iterations = DEFAULT_MAX_ITERATIONS;

    let mut tagged_start = TaggedValue {
        tag: NUMBER_TAG,
        payload: canonical_number_bits(start),
    };
    let mut tagged_limit = TaggedValue {
        tag: NUMBER_TAG,
        payload: canonical_number_bits(limit),
    };
    let mut tagged_step = TaggedValue {
        tag: NUMBER_TAG,
        payload: canonical_number_bits(step),
    };
    let mut tagged_output = TaggedValue::default();
    let mut nanbox_output = 0_u64;

    let (a, b, c, output_pointer) = match candidate.runtime {
        RuntimeLayout::Tagged16 => (
            (&mut tagged_start as *mut TaggedValue) as usize as u64,
            (&mut tagged_limit as *mut TaggedValue) as usize as u64,
            (&mut tagged_step as *mut TaggedValue) as usize as u64,
            (&mut tagged_output as *mut TaggedValue) as usize as u64,
        ),
        RuntimeLayout::NanBox8 => (
            canonical_number_bits(start),
            canonical_number_bits(limit),
            canonical_number_bits(step),
            (&mut nanbox_output as *mut u64) as usize as u64,
        ),
    };

    let invoke = || -> Result<f64> {
        let status = unsafe { function(a, b, c, output_pointer, max_iterations) };
        if status != 0 {
            bail!(
                "native {} execution returned status {status}",
                candidate.name()
            );
        }
        let bits = match candidate.runtime {
            RuntimeLayout::Tagged16 => {
                if tagged_output.tag != NUMBER_TAG {
                    bail!("native tagged result has a non-number tag");
                }
                tagged_output.payload
            }
            RuntimeLayout::NanBox8 => nanbox_output,
        };
        Ok(f64::from_bits(bits))
    };

    let first_result = invoke()?;
    if first_result != expected {
        bail!(
            "{} returned {first_result}, expected {expected}",
            candidate.name()
        );
    }

    let mut semantic_cases = Vec::new();
    {
        let mut run_case = |name: &'static str,
                            case_start: f64,
                            case_limit: f64,
                            case_step: f64,
                            max_iterations: u64,
                            expected_status: i32,
                            expected_result: Option<f64>|
         -> Result<()> {
            let mut case_tagged_start = TaggedValue {
                tag: NUMBER_TAG,
                payload: canonical_number_bits(case_start),
            };
            let mut case_tagged_limit = TaggedValue {
                tag: NUMBER_TAG,
                payload: canonical_number_bits(case_limit),
            };
            let mut case_tagged_step = TaggedValue {
                tag: NUMBER_TAG,
                payload: canonical_number_bits(case_step),
            };
            let mut case_tagged_output = TaggedValue::default();
            let mut case_nanbox_output = 0_u64;
            let (case_a, case_b, case_c, case_output) = match candidate.runtime {
                RuntimeLayout::Tagged16 => (
                    (&mut case_tagged_start as *mut TaggedValue) as usize as u64,
                    (&mut case_tagged_limit as *mut TaggedValue) as usize as u64,
                    (&mut case_tagged_step as *mut TaggedValue) as usize as u64,
                    (&mut case_tagged_output as *mut TaggedValue) as usize as u64,
                ),
                RuntimeLayout::NanBox8 => (
                    canonical_number_bits(case_start),
                    canonical_number_bits(case_limit),
                    canonical_number_bits(case_step),
                    (&mut case_nanbox_output as *mut u64) as usize as u64,
                ),
            };
            // SAFETY: the generated function uses the same C ABI and input/output
            // layout that was used for the primary native benchmark invocation.
            let status = unsafe { function(case_a, case_b, case_c, case_output, max_iterations) };
            let actual_bits = if status == 0 {
                Some(match candidate.runtime {
                    RuntimeLayout::Tagged16 => {
                        if case_tagged_output.tag != NUMBER_TAG {
                            bail!("{name}: tagged output did not contain a Number");
                        }
                        case_tagged_output.payload
                    }
                    RuntimeLayout::NanBox8 => case_nanbox_output,
                })
            } else {
                None
            };
            let expected_bits = expected_result.map(canonical_number_bits);
            let matches = status == expected_status
                && match (expected_bits, actual_bits) {
                    (Some(expected), Some(actual)) => expected == actual,
                    (None, None) => true,
                    _ => false,
                };
            semantic_cases.push(NativeSemanticCase {
                name,
                expected_status,
                actual_status: status,
                expected_result_bits: expected_bits,
                actual_result_bits: actual_bits,
                matches,
            });
            if !matches {
                bail!(
                    "{} native semantic case {name} mismatch: status {status}, expected {expected_status}, result {actual_bits:?}, expected bits {expected_bits:?}",
                    candidate.name()
                );
            }
            Ok(())
        };

        // Exercise the loop semantics at values where strict equality, IEEE-754
        // edge cases, and loop-carried conversions can change observable behavior.
        run_case("fractional-loop", 0.25, 1.0, 0.25, 8, 0, Some(1.0))?;
        run_case("negative-step-loop", 3.0, 0.0, -1.0, 8, 0, Some(0.0))?;
        run_case(
            "signed-zero-strict-equality",
            -0.0,
            0.0,
            1.0,
            8,
            0,
            Some(-0.0),
        )?;
        run_case(
            "positive-infinity-equality",
            f64::INFINITY,
            f64::INFINITY,
            1.0,
            8,
            0,
            Some(f64::INFINITY),
        )?;
        run_case(
            "nan-is-not-strictly-equal-to-itself",
            f64::NAN,
            f64::NAN,
            1.0,
            3,
            2,
            None,
        )?;
    }

    let invalid_tag_rejected = match candidate.runtime {
        RuntimeLayout::Tagged16 => {
            let mut invalid_tagged_start = tagged_start;
            invalid_tagged_start.tag = OBJECT_TAG;
            let invalid_tagged_start_ptr =
                (&mut invalid_tagged_start as *mut TaggedValue) as usize as u64;
            // SAFETY: the pointer refers to a live, correctly laid out TaggedValue.
            let status = unsafe {
                function(
                    invalid_tagged_start_ptr,
                    b,
                    c,
                    output_pointer,
                    max_iterations,
                )
            };
            status == 1
        }
        RuntimeLayout::NanBox8 => {
            let invalid_object_word = (0x7ffe_u64 << NAN_TAG_SHIFT) | 1;
            // SAFETY: the NaN-box layout passes the word directly, not a pointer.
            let status =
                unsafe { function(invalid_object_word, b, c, output_pointer, max_iterations) };
            status == 1
        }
    };
    if !invalid_tag_rejected {
        bail!(
            "{} numeric fast path accepted a non-number Value tag",
            candidate.name()
        );
    }
    let noncanonical_nanbox_words_rejected = match candidate.runtime {
        RuntimeLayout::Tagged16 => None,
        RuntimeLayout::NanBox8 => {
            let positive_noncanonical_nan = 0x7ff8_0000_0000_0001_u64;
            let negative_noncanonical_nan = 0xfff8_0000_0000_0001_u64;
            // SAFETY: NaN-box arguments are passed as raw words under the
            // generated candidate's fixed native entry ABI.
            let positive_status = unsafe {
                function(
                    positive_noncanonical_nan,
                    b,
                    c,
                    output_pointer,
                    max_iterations,
                )
            };
            // SAFETY: same native ABI; negative NaNs must also be rejected.
            let negative_status = unsafe {
                function(
                    negative_noncanonical_nan,
                    b,
                    c,
                    output_pointer,
                    max_iterations,
                )
            };
            Some(positive_status == 1 && negative_status == 1)
        }
    };
    if noncanonical_nanbox_words_rejected == Some(false) {
        bail!(
            "{} accepted a non-canonical NaN-box numeric word",
            candidate.name()
        );
    }
    let semantic_validation = NativeSemanticValidation {
        numeric_edge_cases: semantic_cases,
        invalid_tag_rejected,
        noncanonical_nanbox_words_rejected,
    };

    for _ in 0..warmups {
        for _ in 0..iterations {
            black_box(invoke()?);
        }
    }

    let mut timings = Vec::with_capacity(samples);
    for _ in 0..samples {
        let begin = Instant::now();
        for _ in 0..iterations {
            black_box(invoke()?);
        }
        timings.push(begin.elapsed().as_nanos() as f64 / iterations as f64);
    }

    let final_result = invoke()?;
    Ok((
        distribution(timings),
        final_result == expected,
        semantic_validation,
    ))
}

#[derive(Default)]
struct PressureProgram {
    instructions: Vec<Vec<Operand>>,
    next_vreg: usize,
}

impl PressureProgram {
    fn vreg(&mut self, class: RegClass) -> VReg {
        let result = VReg::new(self.next_vreg, class);
        self.next_vreg += 1;
        result
    }

    fn define_value(
        &mut self,
        layout: RuntimeLayout,
        policy: SsaPolicy,
        representation: &str,
    ) -> Vec<VReg> {
        let classes = match policy {
            SsaPolicy::Boxed => vec![
                RegClass::Int;
                if layout == RuntimeLayout::Tagged16 {
                    2
                } else {
                    1
                }
            ],
            SsaPolicy::RepresentationAware => match representation {
                "f64" => vec![RegClass::Float],
                "i1" | "i32" | "u32" => vec![RegClass::Int],
                "boxed" => vec![
                    RegClass::Int;
                    if layout == RuntimeLayout::Tagged16 {
                        2
                    } else {
                        1
                    }
                ],
                "string_ref" | "symbol_ref" | "bigint_ref" | "object_ref" | "array_ref"
                | "function_ref" | "exception_ref" => vec![RegClass::Int],
                other => panic!("unexpected Core IR representation in allocator probe: {other}"),
            },
        };
        let registers: Vec<VReg> = classes.into_iter().map(|class| self.vreg(class)).collect();
        for register in &registers {
            self.instructions.push(vec![Operand::reg_def(*register)]);
        }
        registers
    }

    fn use_values(&mut self, values: &[VReg]) {
        self.instructions
            .push(values.iter().copied().map(Operand::reg_use).collect());
    }
}

impl RegallocFunction for PressureProgram {
    fn num_insts(&self) -> usize {
        self.instructions.len()
    }
    fn num_blocks(&self) -> usize {
        1
    }
    fn entry_block(&self) -> Block {
        Block::new(0)
    }
    fn block_insns(&self, _block: Block) -> InstRange {
        InstRange::new(Inst::new(0), Inst::new(self.instructions.len()))
    }
    fn block_succs(&self, _block: Block) -> &[Block] {
        &[]
    }
    fn block_preds(&self, _block: Block) -> &[Block] {
        &[]
    }
    fn block_params(&self, _block: Block) -> &[VReg] {
        &[]
    }
    fn is_ret(&self, instruction: Inst) -> bool {
        instruction.index() + 1 == self.instructions.len()
    }
    fn is_branch(&self, _instruction: Inst) -> bool {
        false
    }
    fn branch_blockparams(&self, _block: Block, _instruction: Inst, _successor: usize) -> &[VReg] {
        &[]
    }
    fn inst_operands(&self, instruction: Inst) -> &[Operand] {
        &self.instructions[instruction.index()]
    }
    fn inst_clobbers(&self, _instruction: Inst) -> PRegSet {
        PRegSet::empty()
    }
    fn num_vregs(&self) -> usize {
        self.next_vreg
    }
    fn spillslot_size(&self, class: RegClass) -> usize {
        match class {
            RegClass::Int | RegClass::Float => 1,
            RegClass::Vector => 2,
        }
    }
}

fn register_pressure(candidate: Candidate) -> Result<RegisterPressure> {
    let fixture: JsonValue = serde_json::from_str(include_str!(
        "../../../tests/core-ir/fixtures/positive-call-boundary.json"
    ))?;
    let function = &fixture["function"];
    let params = function["parameters"]
        .as_array()
        .context("fixture parameters missing")?;
    let mut program = PressureProgram::default();
    let mut values = BTreeMap::<String, Vec<VReg>>::new();

    for parameter in params {
        let id = parameter["id"].as_str().context("parameter id missing")?;
        let representation = parameter["representation"]
            .as_str()
            .context("parameter representation missing")?;
        values.insert(
            id.to_owned(),
            program.define_value(candidate.runtime, candidate.ssa, representation),
        );
    }

    let blocks = function["blocks"]
        .as_array()
        .context("fixture blocks missing")?;
    let instructions = blocks[0]["instructions"]
        .as_array()
        .context("fixture instructions missing")?;
    for instruction in instructions {
        let operands = instruction["operands"]
            .as_array()
            .context("instruction operands missing")?;
        let mut used = Vec::new();
        for operand in operands {
            let id = operand.as_str().context("operand id missing")?;
            used.extend(
                values
                    .get(id)
                    .with_context(|| format!("undefined pressure-probe value {id}"))?,
            );
        }
        if !used.is_empty() {
            program.use_values(&used);
        }
        if let Some(result) = instruction["result"].as_object()
            && let Some(id) = result.get("id").and_then(JsonValue::as_str)
        {
            let representation = result
                .get("representation")
                .and_then(JsonValue::as_str)
                .context("result representation missing")?;
            values.insert(
                id.to_owned(),
                program.define_value(candidate.runtime, candidate.ssa, representation),
            );
        }
    }
    let terminator = &blocks[0]["terminator"];
    if terminator["kind"] == "return" {
        let id = terminator["value"]
            .as_str()
            .context("return value missing")?;
        let result = values
            .get(id)
            .with_context(|| format!("return value {id} undefined"))?;
        program.use_values(result);
    } else {
        bail!("allocator pressure probe only supports canonical call-boundary return");
    }

    let make_register_set = |class: RegClass| {
        let mut set = PRegSet::empty();
        for index in 0..6 {
            set.add(PReg::new(index, class));
        }
        set
    };
    let empty = PRegSet::empty();
    let machine_env = MachineEnv {
        preferred_regs_by_class: [
            make_register_set(RegClass::Int),
            make_register_set(RegClass::Float),
            make_register_set(RegClass::Vector),
        ],
        non_preferred_regs_by_class: [empty; 3],
        scratch_by_class: [None, None, None],
        fixed_stack_slots: Vec::new(),
    };
    let options = RegallocOptions {
        verbose_log: false,
        validate_ssa: true,
        algorithm: Algorithm::Ion,
    };
    let output = run_regalloc(&program, &machine_env, &options)
        .map_err(|error| anyhow!("regalloc2 failed for {}: {error:?}", candidate.name()))?;

    let mut integer_registers = PRegSet::empty();
    let mut float_registers = PRegSet::empty();
    let mut register_allocations = 0;
    for allocation in &output.allocs {
        if let Some(register) = allocation.as_reg() {
            register_allocations += 1;
            match register.class() {
                RegClass::Int => integer_registers.add(register),
                RegClass::Float => float_registers.add(register),
                RegClass::Vector => {}
            }
        }
    }
    let mut spill_store_edits = 0;
    let mut spill_reload_edits = 0;
    for (_, edit) in &output.edits {
        match edit {
            Edit::Move { from, to } => {
                if to.is_stack() {
                    spill_store_edits += 1;
                }
                if from.is_stack() {
                    spill_reload_edits += 1;
                }
            }
        }
    }
    Ok(RegisterPressure {
        allocator: "regalloc2 0.15.3 (same crate version resolved by Cranelift 0.137.2)",
        workload: "positive-call-boundary.json parameter/operand/result dataflow profile",
        machine_env: "controlled 6 GPR + 6 FPR + 6 vector-register budget; not the host ISA allocation map",
        integer_registers_used: integer_registers.len() as usize,
        float_registers_used: float_registers.len() as usize,
        register_allocations,
        spill_store_edits,
        spill_reload_edits,
        spill_slots: output.num_spillslots,
        spill_bytes: output.num_spillslots * 8,
    })
}

fn run_gc_sample(
    candidate: Candidate,
    collector: CollectorKind,
    object_count: usize,
) -> Result<GcSample> {
    if object_count < 12 || !object_count.is_multiple_of(4) {
        bail!("GC workload object count must be >=12 and divisible by four");
    }

    let build_start = Instant::now();
    let value_words_per_object =
        GC_VALUE_SLOTS_PER_OBJECT * (candidate.runtime.value_bytes() / size_of::<u64>());
    let storage_words = object_count
        .checked_mul(value_words_per_object)
        .context("candidate Value storage size overflow")?;
    let mut value_storage = vec![0_u64; storage_words];

    let mut heap = GcHeap::new(collector);
    let payload_bytes = GC_VALUE_SLOTS_PER_OBJECT * candidate.runtime.value_bytes();
    let objects: Vec<_> = (0..object_count)
        .map(|_| heap.allocate(payload_bytes))
        .collect();

    // Match OGM-26's cyclic graph shape and add deterministic cross-component edges.
    for base in (0..object_count).step_by(4) {
        heap.add_edge(objects[base], objects[base + 1])?;
        heap.add_edge(objects[base + 1], objects[base + 2])?;
        heap.add_edge(objects[base + 2], objects[base + 3])?;
        heap.add_edge(objects[base + 3], objects[base])?;
        let component = base / 4;
        if component % 7 == 0 && base + 4 < object_count {
            heap.add_edge(objects[base], objects[base + 5])?;
        }
    }

    // Candidate Values carry real OGM-26 HostHandle tokens. At this M0
    // safepoint boundary, an encoded Value is decoded through the heap's
    // generation-checked registry and its root is transferred to the matching
    // interpreter/native/scope provider. Persistent host Values retain their
    // token and HostHandle through collection.
    let value_slot_count = object_count * GC_VALUE_SLOTS_PER_OBJECT;
    for slot_index in 0..value_slot_count {
        encode_candidate_number_value(
            candidate.runtime,
            &mut value_storage,
            slot_index,
            (slot_index + 1) as f64,
        )?;
    }

    let interpreter_frame = heap.begin_frame(FrameKind::Interpreter)?;
    let scope = heap.open_scope()?;
    let components = object_count / 4;
    let mut persistent_handles = Vec::new();
    let mut deferred_native_roots = Vec::new();
    let mut deferred_native_handles = Vec::new();
    let mut encoded_object_values_resolved = 0_usize;

    for component in 0..components {
        let base = component * 4;
        let root_object_index = match component % 3 {
            0 => base,
            1 => base + 1,
            _ => base + 2,
        };
        let root_value_slot = root_object_index * GC_VALUE_SLOTS_PER_OBJECT;
        let handle = heap.retain_host_handle(objects[root_object_index])?;
        let token = heap.encode_host_handle_token(handle)?;
        encode_candidate_object_value(
            candidate.runtime,
            &mut value_storage,
            root_value_slot,
            token,
        )?;
        let root = resolve_candidate_object_value(
            candidate.runtime,
            &value_storage,
            root_value_slot,
            &heap,
        )?;
        if root != objects[root_object_index] {
            bail!("candidate Value decoded to a different OGM-26 GcRef");
        }
        encoded_object_values_resolved += 1;

        match component % 3 {
            0 => {
                heap.add_frame_root(interpreter_frame, root)?;
                encode_candidate_number_value(
                    candidate.runtime,
                    &mut value_storage,
                    root_value_slot,
                    0.0,
                )?;
                heap.release_host_handle(handle)?;
            }
            1 => {
                deferred_native_roots.push((root, root_value_slot));
                deferred_native_handles.push(handle);
            }
            _ => persistent_handles.push((handle, root, root_value_slot)),
        }

        if component % 16 == 0 {
            let scope_object_index = base + 3;
            let scope_value_slot = scope_object_index * GC_VALUE_SLOTS_PER_OBJECT + 1;
            let scope_handle = heap.retain_host_handle(objects[scope_object_index])?;
            let scope_token = heap.encode_host_handle_token(scope_handle)?;
            encode_candidate_object_value(
                candidate.runtime,
                &mut value_storage,
                scope_value_slot,
                scope_token,
            )?;
            let scope_root = resolve_candidate_object_value(
                candidate.runtime,
                &value_storage,
                scope_value_slot,
                &heap,
            )?;
            if scope_root != objects[scope_object_index] {
                bail!("temporary-scope Value decoded to a different OGM-26 GcRef");
            }
            heap.scope_add(scope, scope_root)?;
            encode_candidate_number_value(
                candidate.runtime,
                &mut value_storage,
                scope_value_slot,
                0.0,
            )?;
            heap.release_host_handle(scope_handle)?;
            encoded_object_values_resolved += 1;
        }
    }

    // Frame roots must be added in strict LIFO order. A temporary host handle
    // is released only after the object root has moved into the native frame.
    let native_frame = heap.begin_frame(FrameKind::NativePrototype)?;
    for ((root, value_slot), handle) in deferred_native_roots
        .into_iter()
        .zip(deferred_native_handles)
    {
        heap.add_frame_root(native_frame, root)?;
        encode_candidate_number_value(candidate.runtime, &mut value_storage, value_slot, 0.0)?;
        heap.release_host_handle(handle)?;
    }
    black_box(value_storage.as_slice());

    let graph = heap.graph_shape()?;
    let graph_edges = graph.edges;
    let interpreter_frame_roots = graph.roots.interpreter_frame_roots;
    let native_frame_roots = graph.roots.native_frame_roots;
    let persistent_host_handles = graph.roots.persistent_host_handles;
    let temporary_scope_roots = graph.roots.temporary_scope_roots;
    let before = heap.stats();
    let build_ns = build_start.elapsed().as_nanos();
    let retained = heap.collect()?;
    if retained.reclaimed_objects != 0 || retained.live_objects_after != object_count as u64 {
        bail!(
            "{} {} GC proof reclaimed rooted objects: {}/{}",
            candidate.name(),
            collector.name(),
            retained.reclaimed_objects,
            object_count
        );
    }
    if objects.iter().any(|object| !heap.is_alive(*object)) {
        bail!(
            "{} GC proof lost an object retained by one of the root mechanisms",
            candidate.name()
        );
    }

    // Persistent candidate Values keep the exact packed OGM-26 handle token
    // alive across collection; decode the Value again after the safepoint.
    for (handle, expected_object, value_slot) in persistent_handles.iter() {
        let token = heap.encode_host_handle_token(*handle)?;
        if decode_candidate_object_value(candidate.runtime, &value_storage, *value_slot)?
            != Some(token)
            || resolve_candidate_object_value(
                candidate.runtime,
                &value_storage,
                *value_slot,
                &heap,
            )? != *expected_object
        {
            bail!(
                "{} persistent candidate Value did not resolve to its original GcRef",
                candidate.name()
            );
        }
    }
    let actual_value_storage_capacity_bytes = value_storage.capacity() * size_of::<u64>();
    drop(value_storage);

    heap.close_scope(scope)?;
    heap.end_frame(native_frame)?;
    for (handle, _, _) in persistent_handles.iter().copied() {
        heap.release_host_handle(handle)?;
    }
    heap.end_frame(interpreter_frame)?;
    let released = heap.collect()?;
    if released.live_objects_after != 0 || released.reclaimed_objects != object_count {
        bail!(
            "{} {} GC proof did not reclaim the unrooted object graph: {} live / {} reclaimed",
            candidate.name(),
            collector.name(),
            released.live_objects_after,
            released.reclaimed_objects
        );
    }

    let stats = heap.stats();
    Ok(GcSample {
        build_ns,
        collection_ns: retained.collection_ns,
        root_scan_ns: retained.root_scan_ns,
        cleanup_collection_ns: released.collection_ns,
        roots_scanned: retained.roots_scanned,
        edges_examined: retained.edges_examined,
        graph_edges,
        interpreter_frame_roots,
        native_frame_roots,
        persistent_host_handles,
        temporary_scope_roots,
        logical_heap_bytes_before_collection: before.live_bytes,
        peak_logical_heap_bytes: stats.peak_heap_bytes,
        actual_value_storage_capacity_bytes,
        encoded_object_values_resolved,
        reclaimed_after_root_release: released.reclaimed_objects,
        live_objects_after_release: stats.live_objects,
    })
}

fn gc_distribution(samples: &[GcSample], select: impl Fn(&GcSample) -> u128) -> Distribution {
    distribution(samples.iter().map(|sample| select(sample) as f64).collect())
}

fn summarize_gc_collector(
    collector: CollectorKind,
    object_count: usize,
    raw_samples: Vec<GcSample>,
) -> Result<GcCollectorEvidence> {
    let first = raw_samples
        .first()
        .context("GC measurement samples missing")?;
    if raw_samples.iter().any(|sample| {
        sample.graph_edges != first.graph_edges
            || sample.interpreter_frame_roots != first.interpreter_frame_roots
            || sample.native_frame_roots != first.native_frame_roots
            || sample.persistent_host_handles != first.persistent_host_handles
            || sample.temporary_scope_roots != first.temporary_scope_roots
            || sample.actual_value_storage_capacity_bytes
                != first.actual_value_storage_capacity_bytes
            || sample.encoded_object_values_resolved != first.encoded_object_values_resolved
    }) {
        bail!(
            "{} GC benchmark topology/storage changed between samples",
            collector.name()
        );
    }
    let peak_logical_heap_bytes_max = raw_samples
        .iter()
        .map(|sample| sample.peak_logical_heap_bytes)
        .max()
        .unwrap_or_default();
    Ok(GcCollectorEvidence {
        collector: collector.name(),
        object_count,
        edge_count: first.graph_edges,
        interpreter_frame_roots: first.interpreter_frame_roots,
        native_frame_roots: first.native_frame_roots,
        persistent_host_handles: first.persistent_host_handles,
        temporary_scope_roots: first.temporary_scope_roots,
        build_ns: gc_distribution(&raw_samples, |sample| sample.build_ns),
        collection_ns: gc_distribution(&raw_samples, |sample| sample.collection_ns),
        root_scan_ns: gc_distribution(&raw_samples, |sample| sample.root_scan_ns),
        cleanup_collection_ns: gc_distribution(&raw_samples, |sample| sample.cleanup_collection_ns),
        peak_logical_heap_bytes_max,
        actual_value_storage_capacity_bytes: first.actual_value_storage_capacity_bytes,
        encoded_object_values_resolved: first.encoded_object_values_resolved,
        raw_samples,
    })
}

fn gc_evidence(
    candidate: Candidate,
    warmups: usize,
    samples: usize,
    object_count: usize,
) -> Result<GcEvidence> {
    let collectors = CollectorKind::ALL;
    let mut raw_samples: [Vec<GcSample>; 2] =
        [Vec::with_capacity(samples), Vec::with_capacity(samples)];

    // Alternate collector order for paired samples so the first collector does
    // not always receive the colder cache/allocator state.
    for warmup in 0..warmups {
        let order = if warmup % 2 == 0 { [0, 1] } else { [1, 0] };
        for index in order {
            black_box(run_gc_sample(candidate, collectors[index], object_count)?);
        }
    }

    for sample_index in 0..samples {
        let order = if sample_index % 2 == 0 {
            [0, 1]
        } else {
            [1, 0]
        };
        let mut pair: [Option<GcSample>; 2] = [None, None];
        for index in order {
            pair[index] = Some(run_gc_sample(candidate, collectors[index], object_count)?);
        }
        let first = pair[0].take().context("missing RC-cycle-removal sample")?;
        let second = pair[1].take().context("missing MarkSweep sample")?;
        if first.graph_edges != second.graph_edges
            || first.interpreter_frame_roots != second.interpreter_frame_roots
            || first.native_frame_roots != second.native_frame_roots
            || first.persistent_host_handles != second.persistent_host_handles
            || first.temporary_scope_roots != second.temporary_scope_roots
            || first.logical_heap_bytes_before_collection
                != second.logical_heap_bytes_before_collection
            || first.actual_value_storage_capacity_bytes
                != second.actual_value_storage_capacity_bytes
            || first.encoded_object_values_resolved != second.encoded_object_values_resolved
            || first.reclaimed_after_root_release != second.reclaimed_after_root_release
            || first.live_objects_after_release != second.live_objects_after_release
        {
            bail!("OGM-26 collector pair did not execute an equivalent candidate workload");
        }
        raw_samples[0].push(first);
        raw_samples[1].push(second);
    }

    let collector_comparisons = vec![
        summarize_gc_collector(
            collectors[0],
            object_count,
            std::mem::take(&mut raw_samples[0]),
        )?,
        summarize_gc_collector(
            collectors[1],
            object_count,
            std::mem::take(&mut raw_samples[1]),
        )?,
    ];
    let graph_edges = collector_comparisons
        .first()
        .context("GC collector evidence missing")?
        .edge_count;
    let value_storage_bytes =
        object_count * GC_VALUE_SLOTS_PER_OBJECT * candidate.runtime.value_bytes();
    Ok(GcEvidence {
        collector_implementation: "OGM-26 RcCycleRemoval and MarkSweep",
        object_count,
        graph_edges,
        value_slots_per_object: GC_VALUE_SLOTS_PER_OBJECT,
        value_storage_bytes,
        value_layout_bytes: candidate.runtime.value_bytes(),
        handle_payload_bits: NAN_TAG_SHIFT,
        encoded_object_values_resolved: collector_comparisons[0].encoded_object_values_resolved,
        collector_comparisons,
    })
}

fn is_hex_byte(token: &str) -> bool {
    token.len() == 2 && token.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_general_purpose_register(token: &str) -> bool {
    matches!(
        token,
        "rax"
            | "eax"
            | "ax"
            | "al"
            | "ah"
            | "rbx"
            | "ebx"
            | "bx"
            | "bl"
            | "bh"
            | "rcx"
            | "ecx"
            | "cx"
            | "cl"
            | "ch"
            | "rdx"
            | "edx"
            | "dx"
            | "dl"
            | "dh"
            | "rsi"
            | "esi"
            | "si"
            | "sil"
            | "rdi"
            | "edi"
            | "di"
            | "dil"
            | "rbp"
            | "ebp"
            | "bp"
            | "bpl"
            | "rsp"
            | "esp"
            | "sp"
            | "spl"
    ) || (token
        .strip_prefix('r')
        .and_then(|tail| {
            let digits = tail.trim_end_matches(['b', 'w', 'd']);
            if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
                None
            } else {
                digits.parse::<u8>().ok()
            }
        })
        .is_some_and(|register| (8..=15).contains(&register)))
}

fn is_vector_register(token: &str) -> bool {
    ["xmm", "ymm", "zmm", "mm"].iter().any(|prefix| {
        token
            .strip_prefix(prefix)
            .is_some_and(|suffix| !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()))
    })
}

fn parse_native_disassembly(
    disassembly: &str,
    symbol: &'static str,
) -> Result<NativeCodeMetrics> {
    let mut in_function = false;
    let mut found_symbol = false;
    let mut instruction_count = 0_usize;
    let mut function_code_bytes = 0_usize;
    let mut integer_registers = BTreeSet::new();
    let mut vector_registers = BTreeSet::new();
    let mut stack_relative_memory_instructions = 0_usize;
    let mut stack_relative_loads = 0_usize;
    let mut stack_relative_stores = 0_usize;
    let mut direct_call_instructions = 0_usize;

    for line in disassembly.lines() {
        let trimmed = line.trim();
        if trimmed.contains(&format!("<{symbol}>:")) {
            found_symbol = true;
            in_function = true;
            continue;
        }
        if !in_function || trimmed.is_empty() {
            continue;
        }
        if trimmed.contains('<') && trimmed.ends_with(">:") {
            break;
        }
        let Some((_address, bytes_and_instruction)) = trimmed.split_once(':') else {
            continue;
        };
        let tokens: Vec<&str> = bytes_and_instruction.split_whitespace().collect();
        let mut byte_count = 0_usize;
        while byte_count < tokens.len() && is_hex_byte(tokens[byte_count]) {
            byte_count += 1;
        }
        if byte_count == 0 || byte_count == tokens.len() {
            continue;
        }

        let assembly = tokens[byte_count..].join(" ").to_ascii_lowercase();
        let mut words = assembly.split_whitespace();
        let mnemonic = words.next().unwrap_or_default();
        let operands = words.collect::<Vec<_>>().join(" ");
        instruction_count += 1;
        function_code_bytes += byte_count;

        if mnemonic.starts_with("call") {
            direct_call_instructions += 1;
        }
        let stack_access = operands.contains("[rsp")
            || operands.contains("[rbp")
            || mnemonic == "push"
            || mnemonic == "pop";
        if stack_access {
            stack_relative_memory_instructions += 1;
            let first_operand = operands.split(',').next().unwrap_or_default().trim();
            if first_operand.contains('[') || mnemonic == "push" {
                stack_relative_stores += 1;
            } else {
                stack_relative_loads += 1;
            }
        }

        for word in assembly.split(|character: char| !character.is_ascii_alphanumeric()) {
            if is_general_purpose_register(word) {
                integer_registers.insert(word.to_owned());
            } else if is_vector_register(word) {
                vector_registers.insert(word.to_owned());
            }
        }
    }

    if !found_symbol || instruction_count == 0 {
        bail!("objdump did not produce instructions for {symbol}");
    }

    Ok(NativeCodeMetrics {
        disassembler: "GNU objdump -d -M intel",
        symbol,
        instruction_count,
        function_code_bytes,
        integer_registers: integer_registers.into_iter().collect(),
        vector_registers: vector_registers.into_iter().collect(),
        stack_relative_memory_instructions,
        stack_relative_loads,
        stack_relative_stores,
        direct_call_instructions,
    })
}

fn native_code_metrics(
    object_path: &Path,
    symbol: &'static str,
) -> Result<NativeCodeMetrics> {
    if env::consts::ARCH != "x86_64" {
        bail!("OGM-38 native register disassembly currently requires an x86_64 target");
    }
    let symbol_argument = format!("--disassemble={symbol}");
    let output = Command::new("objdump")
        .args(["-d", "-M", "intel"])
        .arg(symbol_argument)
        .arg(object_path)
        .output()
        .context("launch objdump for generated OGM-38 native function")?;
    if !output.status.success() {
        bail!(
            "objdump failed for {}: {}",
            object_path.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let disassembly =
        String::from_utf8(output.stdout).context("objdump returned non-UTF-8 disassembly")?;
    parse_native_disassembly(&disassembly, symbol)
}

fn run(
    output: &Path,
    revision: &str,
    iterations: u64,
    warmups: usize,
    samples: usize,
) -> Result<Report> {
    if iterations == 0 || samples < 101 {
        bail!("iterations must be positive and samples must be >= 101");
    }
    verify_canonical_loop_fixture()?;
    let parameters = canonical_call_boundary_parameters()?;
    let root = env::temp_dir().join(format!("ogm38-native-value-ssa-{}", std::process::id()));
    fs::create_dir_all(&root)?;
    let expected = 128.0;
    let mut candidates = Vec::with_capacity(Candidate::ALL.len());

    for candidate in Candidate::ALL {
        let object_path = candidate_output_path(&root, candidate, "o");
        let library_path = candidate_output_path(&root, candidate, "so");
        let (compile_first_ns, compile_ns) =
            compile_distribution(candidate, &object_path, warmups, samples)?;
        link_shared_library(&object_path, &library_path)?;
        let library = unsafe { Library::new(&library_path) }
            .with_context(|| format!("load generated native object for {}", candidate.name()))?;
        let (throughput_ns_per_call, result_matches, native_semantic_validation) =
            run_native_candidate(candidate, &library, iterations, warmups, samples, expected)?;
        let native_code = native_code_metrics(&object_path)?;
        let register_pressure = register_pressure(candidate)?;
        let gc = gc_evidence(candidate, warmups, samples, GC_OBJECT_COUNT)?;
        candidates.push(CandidateEvidence {
            name: candidate.name(),
            runtime_value_bytes: candidate.runtime.value_bytes(),
            native_ir_value_conversion_ops_per_loop_iteration: if candidate.ssa == SsaPolicy::Boxed
            {
                5
            } else {
                0
            },
            native_ir_value_conversion_ops_per_invocation: if candidate.ssa == SsaPolicy::Boxed {
                0
            } else {
                4
            },
            native_helper_calls_per_loop_iteration: 0,
            compile_first_ns,
            compile_ns,
            object_bytes: fs::metadata(&object_path)?.len(),
            shared_library_bytes: fs::metadata(&library_path)?.len(),
            iterations_per_sample: iterations,
            warmup_rounds: warmups,
            calls_per_second_p50: 1e9 / throughput_ns_per_call.p50_ns,
            throughput_ns_per_call,
            completed_loop_iterations: expected as u64,
            result_matches,
            native_semantic_validation,
            native_code,
            register_pressure,
            gc,
        });
    }

    let report = Report {
        schema_version: 1,
        task: "OGM-38",
        source_revision: revision.to_owned(),
        tested_revision: command_output("git", &["rev-parse", "HEAD"]),
        rustc: command_output("rustc", &["--version"]),
        operating_system: command_output("uname", &["-sr"]),
        architecture: env::consts::ARCH.to_owned(),
        target_triple: command_output("rustc", &["-vV"])
            .lines()
            .find_map(|line| line.strip_prefix("host: ").map(str::to_owned))
            .unwrap_or_else(|| "unavailable".to_owned()),
        cpu_model: cpu_model(),
        core_ir_contract: "ogma.core-ir/1.0.0",
        core_ir_fixture: "tests/core-ir/fixtures/positive-loop-carried-values.json (native loop lowering) + positive-call-boundary.json (register-allocation profile)",
        core_ir_fixture_parameters: parameters,
        semantic_workload: "Number-only fast-path specialization of the canonical strict-equality-controlled loop (js.add + js.to_number) with loop-carried value",
        iterations_per_sample: iterations,
        warmup_rounds: warmups,
        measurement_samples: samples,
        max_loop_iterations: DEFAULT_MAX_ITERATIONS,
        candidates,
        measurement_boundaries: BTreeMap::from([
            (
                "native_code",
                "Cranelift 0.137.2 ObjectModule emits and links host-native code for a numeric loop fixture adapter; not a production Ogma AOT compiler",
            ),
            (
                "throughput",
                "host calls into loaded generated native functions; includes the C ABI call and value encode/decode boundary, but excludes source parsing and production Runtime dispatch",
            ),
            (
                "compile_time",
                "Cranelift ObjectModule candidate function compile samples; excludes whole-program linking, LLVM, and production compiler frontend",
            ),
            (
                "register_pressure",
                "regalloc2 0.15.3 allocates VRegs mapped from canonical call-boundary Core IR value representations under a fixed register budget; spill stores/reloads count Output.edits stack moves; not Cranelift's private allocation dump or host ISA register map",
            ),
            (
                "gc_and_heap",
                "OGM-26 MarkSweep heap with real generation-checked GcRef, native frame roots and persistent HostHandle lifecycle; logical heap accounting is not process RSS",
            ),
        ]),
        unsupported_native_semantics: vec![
            "NaN payload normalization, signed-zero and infinity codec roundtrips are tested by OGM-17's semantic fixture suite but are not part of the native loop workload",
            "BigInt arithmetic, string concatenation, object ToPrimitive, symbols, property access and general host-call semantics are not lowered by this numeric native fixture adapter",
            "the OGM-19 ABI, OGM-27 native shadow-frame model and OGM-26 heap are composed in adjacent M0 proofs; this benchmark does not link a production Runtime or emit real machine stack maps",
            "peak process RSS, allocator-internal fragmentation and production object-graph memory are not measured",
        ],
    };
    let output_path = output.to_path_buf();
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&output_path, serde_json::to_vec_pretty(&report)?)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    println!("wrote {}", output_path.display());
    Ok(report)
}

pub fn parse_args_and_run() -> Result<()> {
    let mut output = PathBuf::from("ogm38-native-value-ssa-report.json");
    let mut revision = env::var("GITHUB_SHA").unwrap_or_else(|_| "working-tree".to_owned());
    let mut iterations = 1_000_u64;
    let mut warmups = 3_usize;
    let mut samples = 101_usize;
    let mut args = env::args().skip(1);
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--output" => output = PathBuf::from(args.next().context("missing output path")?),
            "--revision" => revision = args.next().context("missing revision")?,
            "--iterations" => {
                iterations = args.next().context("missing iteration count")?.parse()?
            }
            "--warmups" => warmups = args.next().context("missing warmup count")?.parse()?,
            "--samples" => samples = args.next().context("missing sample count")?.parse()?,
            other => bail!("unknown argument: {other}"),
        }
    }
    run(&output, &revision, iterations, warmups, samples)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn objdump_parser_counts_actual_instruction_bytes_and_register_classes() {
        let disassembly = r#"
0000000000000000 <ogma_value_loop>:
   0: 48 8b 07                mov rax,QWORD PTR [rdi]
   3: f2 0f 10 44 24 08       movsd xmm0,QWORD PTR [rsp+0x8]
   9: 48 89 44 24 10          mov QWORD PTR [rsp+0x10],rax
   e: c3                      ret
"#;
        let metrics =
            parse_native_disassembly(disassembly, "ogma_value_loop").expect("parse objdump");
        assert_eq!(metrics.instruction_count, 4);
        assert_eq!(metrics.function_code_bytes, 15);
        assert!(metrics.integer_registers.contains(&"rax".to_owned()));
        assert!(metrics.integer_registers.contains(&"rdi".to_owned()));
        assert!(metrics.vector_registers.contains(&"xmm0".to_owned()));
        assert_eq!(metrics.stack_relative_memory_instructions, 2);
        assert_eq!(metrics.stack_relative_loads, 1);
        assert_eq!(metrics.stack_relative_stores, 1);
    }

    #[test]
    fn objdump_parser_accepts_indirect_native_helper_symbol() {
        let disassembly = r#"
0000000000000000 <ogma_value_handle_dispatch>:
   0: 48 89 f8                mov rax,rdi
   3: ff d0                   call rax
   5: c3                      ret
"#;
        let metrics = parse_native_disassembly(disassembly, "ogma_value_handle_dispatch")
            .expect("parse indirect callback helper");
        assert_eq!(metrics.symbol, "ogma_value_handle_dispatch");
        assert_eq!(metrics.instruction_count, 3);
        assert_eq!(metrics.direct_call_instructions, 1);
    }

    #[test]
    fn x86_register_classifier_handles_extended_registers() {
        assert!(is_general_purpose_register("r15"));
        assert!(is_general_purpose_register("r10d"));
        assert!(is_general_purpose_register("r8b"));
        assert!(!is_general_purpose_register("xmm15"));
        assert!(is_vector_register("xmm15"));
        assert!(is_vector_register("ymm3"));
        assert!(is_vector_register("zmm31"));
    }

    #[test]
    fn candidate_matrix_contains_all_four_independent_combinations() {
        let names: Vec<String> = Candidate::ALL.into_iter().map(Candidate::name).collect();
        assert_eq!(
            names,
            [
                "tagged-16-boxed",
                "tagged-16-representation-aware",
                "nanbox-8-boxed",
                "nanbox-8-representation-aware",
            ]
        );
    }

    #[test]
    fn canonical_call_boundary_fixture_is_used_for_pressure_profile() {
        let params = canonical_call_boundary_parameters().expect("canonical Core IR fixture");
        assert_eq!(
            params,
            [
                "callee",
                "receiver",
                "text",
                "object_left",
                "object_right",
                "numeric"
            ]
        );
    }

    #[test]
    fn nanbox_handle_payload_fits_the_documented_48_bit_window() {
        let handle = MAX_PAYLOAD;
        let encoded = (0x7ffe_u64 << NAN_TAG_SHIFT) | handle;
        assert_eq!(encoded >> NAN_TAG_SHIFT, 0x7ffe);
        assert_eq!(encoded & MAX_PAYLOAD, handle);
    }

    #[test]
    fn regalloc_reports_physical_register_and_spill_metrics_for_all_candidates() {
        let mut reports = BTreeMap::new();
        for candidate in Candidate::ALL {
            let evidence = register_pressure(candidate).expect("register allocation");
            assert!(evidence.register_allocations > 0);
            assert!(evidence.integer_registers_used > 0);
            assert_eq!(evidence.spill_bytes, evidence.spill_slots * 8);
            assert_eq!(
                evidence.spill_slots > 0,
                evidence.spill_store_edits > 0 && evidence.spill_reload_edits > 0
            );
            reports.insert(candidate.name(), evidence);
        }
        let tagged_boxed = reports
            .get("tagged-16-boxed")
            .expect("tagged boxed candidate");
        let nanbox_boxed = reports
            .get("nanbox-8-boxed")
            .expect("NaN-box boxed candidate");
        assert!(tagged_boxed.spill_slots > nanbox_boxed.spill_slots);
        assert!(tagged_boxed.spill_store_edits > 0);
        assert!(tagged_boxed.spill_reload_edits > 0);
        assert_eq!(nanbox_boxed.spill_store_edits, 0);
        assert_eq!(nanbox_boxed.spill_reload_edits, 0);
    }

    #[test]
    fn tagged_and_nanbox_object_values_resolve_real_host_handle_tokens() {
        let mut heap = GcHeap::new(CollectorKind::MarkSweep);
        let object = heap.allocate(32);
        let handle = heap
            .retain_host_handle(object)
            .expect("retain object handle");
        let token = heap
            .encode_host_handle_token(handle)
            .expect("encode OGM-26 handle token");

        let mut tagged = vec![0_u64; 2];
        encode_candidate_object_value(RuntimeLayout::Tagged16, &mut tagged, 0, token)
            .expect("encode tagged object Value");
        assert_eq!(
            resolve_candidate_object_value(RuntimeLayout::Tagged16, &tagged, 0, &heap)
                .expect("resolve tagged object Value"),
            object
        );

        let mut nanbox = vec![0_u64; 1];
        encode_candidate_object_value(RuntimeLayout::NanBox8, &mut nanbox, 0, token)
            .expect("encode NaN-box object Value");
        assert_eq!(
            resolve_candidate_object_value(RuntimeLayout::NanBox8, &nanbox, 0, &heap)
                .expect("resolve NaN-box object Value"),
            object
        );

        heap.release_host_handle(handle)
            .expect("release object handle");
        assert!(
            resolve_candidate_object_value(RuntimeLayout::NanBox8, &nanbox, 0, &heap).is_err(),
            "released candidate Value must not resolve through a stale handle token"
        );
    }

    #[test]
    fn candidate_layouts_survive_and_release_equivalent_ogm26_gc_graphs() {
        for candidate in Candidate::ALL {
            let evidence =
                gc_evidence(candidate, 0, 1, 64).expect("OGM-26 allocation/root/GC workload");
            assert_eq!(evidence.object_count, 64);
            assert_eq!(
                evidence.value_storage_bytes,
                64 * GC_VALUE_SLOTS_PER_OBJECT * candidate.runtime.value_bytes()
            );
            assert_eq!(
                evidence.collector_comparisons.len(),
                CollectorKind::ALL.len()
            );
            assert_eq!(evidence.encoded_object_values_resolved, 17);
            for collector in evidence.collector_comparisons {
                assert_eq!(collector.object_count, 64);
                assert_eq!(collector.encoded_object_values_resolved, 17);
                assert!(
                    collector.actual_value_storage_capacity_bytes >= evidence.value_storage_bytes
                );
                assert!(collector.collection_ns.p50_ns > 0.0);
                assert!(collector.root_scan_ns.p50_ns > 0.0);
                assert!(collector.raw_samples[0].roots_scanned > 0);
                assert_eq!(collector.edge_count, 67);
                assert_eq!(collector.interpreter_frame_roots, 6);
                assert_eq!(collector.native_frame_roots, 5);
                assert_eq!(collector.persistent_host_handles, 5);
                assert_eq!(collector.temporary_scope_roots, 1);
                assert_eq!(collector.raw_samples[0].live_objects_after_release, 0);
                assert_eq!(collector.raw_samples[0].reclaimed_after_root_release, 64);
            }
        }
    }
}
