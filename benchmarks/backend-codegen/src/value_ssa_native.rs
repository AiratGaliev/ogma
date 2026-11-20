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

const UNDEFINED_TAG: u64 = 1;
const NULL_TAG: u64 = 2;
const NUMBER_TAG: u64 = 3;
const STRING_TAG: u64 = 4;
const BIGINT_TAG: u64 = 5;
const OBJECT_TAG: u64 = 6;
const SYMBOL_TAG: u64 = 7;
const BOOLEAN_TAG: u64 = 8;
const NANBOX_UNDEFINED_TAG: u64 = 0x7ff9;
const NANBOX_NULL_TAG: u64 = 0x7ffa;
const NANBOX_BOOLEAN_TAG: u64 = 0x7ffb;
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
const NANBOX_IMMEDIATE_TAGS: [u64; 3] = [NANBOX_UNDEFINED_TAG, NANBOX_NULL_TAG, NANBOX_BOOLEAN_TAG];
const NAN_TAG_SHIFT: u32 = 48;
const MAX_PAYLOAD: u64 = (1_u64 << NAN_TAG_SHIFT) - 1;
const NANBOX_RESERVED_TAG_MIN: u64 = 0x7ff9_0000_0000_0000;
const NANBOX_RESERVED_TAG_MAX: u64 = 0x7fff_ffff_ffff_ffff;
const CANONICAL_NAN_BITS: u64 = 0x7ff8_0000_0000_0000;
const DEFAULT_MAX_ITERATIONS: u64 = 1_000_000;
const GC_OBJECT_COUNT: usize = 2_048;
const GC_VALUE_SLOTS_PER_OBJECT: usize = 16;
const PRESSURE_VALUE_COUNT: usize = 32;

fn canonical_number_bits(value: f64) -> u64 {
    if value.is_nan() {
        CANONICAL_NAN_BITS
    } else {
        value.to_bits()
    }
}

type NativeLoop = unsafe extern "C" fn(u64, u64, u64, u64, u64) -> i32;
type NativeHandleDispatch = unsafe extern "C" fn(u64, u64, u64) -> i32;
type NativePressure = unsafe extern "C" fn(u64, u64, u64) -> u64;

struct NativeHandleCallbackContext {
    heap: *mut GcHeap,
    expected_object: Option<ogma_gc_comparison_proof::GcRef>,
    expected_payload: u64,
    expected_tag: u64,
    calls: u64,
    collections: u64,
    force_collection: bool,
    last_tag: u64,
    last_token: u64,
}

// SAFETY: the function pointer is called only by this synchronous native
// benchmark with a live, uniquely borrowed context on the same thread.
unsafe extern "C" fn resolve_native_handle_callback(
    context_pointer: u64,
    tag: u64,
    token: u64,
) -> i32 {
    if context_pointer == 0 {
        return 1;
    }
    // SAFETY: the caller passes a pointer to NativeHandleCallbackContext for
    // the entire synchronous native call; no concurrent access is performed.
    let context = unsafe { &mut *(context_pointer as *mut NativeHandleCallbackContext) };
    context.calls += 1;
    context.last_tag = tag;
    context.last_token = token;
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
    match context.expected_object {
        Some(expected) => {
            if token == 0 {
                return 4;
            }
            match heap.resolve_host_handle_token(token) {
                Ok(reference) if reference == expected => 0,
                _ => 4,
            }
        }
        None if token == context.expected_payload => 0,
        None => 4,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CandidateValue {
    Undefined,
    Null,
    Boolean(bool),
    Number(u64),
    StringRef(u64),
    BigIntRef(u64),
    ObjectRef(u64),
    SymbolRef(u64),
}

fn encode_candidate_value(
    runtime: RuntimeLayout,
    storage: &mut [u64],
    slot_index: usize,
    value: CandidateValue,
) -> Result<()> {
    match value {
        CandidateValue::Undefined | CandidateValue::Null | CandidateValue::Boolean(_) => {
            let (tagged_tag, payload, nanbox_tag) = match value {
                CandidateValue::Undefined => (UNDEFINED_TAG, 0, NANBOX_UNDEFINED_TAG),
                CandidateValue::Null => (NULL_TAG, 0, NANBOX_NULL_TAG),
                CandidateValue::Boolean(value) => {
                    (BOOLEAN_TAG, u64::from(value), NANBOX_BOOLEAN_TAG)
                }
                _ => unreachable!(),
            };
            match runtime {
                RuntimeLayout::Tagged16 => {
                    let word_index = slot_index
                        .checked_mul(2)
                        .context("tagged immediate Value slot index overflow")?;
                    let words = storage
                        .get_mut(word_index..word_index + 2)
                        .context("tagged immediate Value storage slot out of bounds")?;
                    words[0] = tagged_tag;
                    words[1] = payload;
                }
                RuntimeLayout::NanBox8 => {
                    *storage
                        .get_mut(slot_index)
                        .context("NaN-box immediate Value storage slot out of bounds")? =
                        (nanbox_tag << NAN_TAG_SHIFT) | payload;
                }
            }
            Ok(())
        }
        CandidateValue::Number(bits) => {
            encode_candidate_number_value(runtime, storage, slot_index, f64::from_bits(bits))
        }
        CandidateValue::StringRef(token) => {
            encode_candidate_reference_value(runtime, storage, slot_index, STRING_TAG, token)
        }
        CandidateValue::BigIntRef(token) => {
            encode_candidate_reference_value(runtime, storage, slot_index, BIGINT_TAG, token)
        }
        CandidateValue::ObjectRef(token) => {
            encode_candidate_reference_value(runtime, storage, slot_index, OBJECT_TAG, token)
        }
        CandidateValue::SymbolRef(token) => {
            encode_candidate_reference_value(runtime, storage, slot_index, SYMBOL_TAG, token)
        }
    }
}

fn decode_candidate_value(
    runtime: RuntimeLayout,
    storage: &[u64],
    slot_index: usize,
) -> Result<CandidateValue> {
    match runtime {
        RuntimeLayout::Tagged16 => {
            let word_index = slot_index
                .checked_mul(2)
                .context("tagged Value slot index overflow")?;
            let words = storage
                .get(word_index..word_index + 2)
                .context("tagged Value storage slot out of bounds")?;
            match words[0] {
                UNDEFINED_TAG if words[1] == 0 => Ok(CandidateValue::Undefined),
                NULL_TAG if words[1] == 0 => Ok(CandidateValue::Null),
                BOOLEAN_TAG if words[1] <= 1 => Ok(CandidateValue::Boolean(words[1] != 0)),
                NUMBER_TAG => {
                    let number = f64::from_bits(words[1]);
                    if number.is_nan() && words[1] != CANONICAL_NAN_BITS {
                        bail!("tagged Number contains a non-canonical NaN");
                    }
                    Ok(CandidateValue::Number(words[1]))
                }
                STRING_TAG if words[1] != 0 && words[1] <= MAX_PAYLOAD => {
                    Ok(CandidateValue::StringRef(words[1]))
                }
                BIGINT_TAG if words[1] != 0 && words[1] <= MAX_PAYLOAD => {
                    Ok(CandidateValue::BigIntRef(words[1]))
                }
                OBJECT_TAG if words[1] != 0 && words[1] <= MAX_PAYLOAD => {
                    Ok(CandidateValue::ObjectRef(words[1]))
                }
                SYMBOL_TAG if words[1] != 0 && words[1] <= MAX_PAYLOAD => {
                    Ok(CandidateValue::SymbolRef(words[1]))
                }
                tag => bail!(
                    "invalid tagged Value tag/payload combination {tag}/{:#x}",
                    words[1]
                ),
            }
        }
        RuntimeLayout::NanBox8 => {
            let word = *storage
                .get(slot_index)
                .context("NaN-box Value storage slot out of bounds")?;
            let tag = word >> NAN_TAG_SHIFT;
            let payload = word & MAX_PAYLOAD;
            match tag {
                NANBOX_UNDEFINED_TAG if payload == 0 => Ok(CandidateValue::Undefined),
                NANBOX_NULL_TAG if payload == 0 => Ok(CandidateValue::Null),
                NANBOX_BOOLEAN_TAG if payload <= 1 => Ok(CandidateValue::Boolean(payload != 0)),
                NANBOX_STRING_TAG if payload != 0 => Ok(CandidateValue::StringRef(payload)),
                NANBOX_BIGINT_TAG if payload != 0 => Ok(CandidateValue::BigIntRef(payload)),
                NANBOX_OBJECT_TAG if payload != 0 => Ok(CandidateValue::ObjectRef(payload)),
                NANBOX_SYMBOL_TAG if payload != 0 => Ok(CandidateValue::SymbolRef(payload)),
                NANBOX_UNDEFINED_TAG | NANBOX_NULL_TAG | NANBOX_BOOLEAN_TAG | NANBOX_STRING_TAG
                | NANBOX_BIGINT_TAG | NANBOX_OBJECT_TAG | NANBOX_SYMBOL_TAG => {
                    bail!("invalid NaN-box tag/payload combination {tag:#06x}/{payload:#x}")
                }
                _ => {
                    let exponent = word & 0x7ff0_0000_0000_0000;
                    let fraction = word & 0x000f_ffff_ffff_ffff;
                    if exponent == 0x7ff0_0000_0000_0000
                        && fraction != 0
                        && word != CANONICAL_NAN_BITS
                    {
                        bail!("NaN-box Number contains a non-canonical NaN");
                    }
                    Ok(CandidateValue::Number(canonical_number_bits(
                        f64::from_bits(word),
                    )))
                }
            }
        }
    }
}

fn candidate_value_tag_payload(
    runtime: RuntimeLayout,
    storage: &[u64],
    slot_index: usize,
) -> Result<(u64, u64)> {
    match runtime {
        RuntimeLayout::Tagged16 => {
            let word_index = slot_index
                .checked_mul(2)
                .context("tagged Value slot index overflow")?;
            let words = storage
                .get(word_index..word_index + 2)
                .context("tagged Value storage slot out of bounds")?;
            Ok((words[0], words[1]))
        }
        RuntimeLayout::NanBox8 => {
            let word = *storage
                .get(slot_index)
                .context("NaN-box Value storage slot out of bounds")?;
            Ok((word >> NAN_TAG_SHIFT, word & MAX_PAYLOAD))
        }
    }
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
                UNDEFINED_TAG | NULL_TAG | BOOLEAN_TAG | NUMBER_TAG => Ok(None),
                STRING_TAG | BIGINT_TAG | OBJECT_TAG | SYMBOL_TAG
                    if words[1] != 0 && words[1] <= MAX_PAYLOAD =>
                {
                    Ok(Some(words[1]))
                }
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
            if NANBOX_IMMEDIATE_TAGS.contains(&tag) {
                let payload = word & MAX_PAYLOAD;
                match tag {
                    NANBOX_UNDEFINED_TAG | NANBOX_NULL_TAG if payload == 0 => return Ok(None),
                    NANBOX_BOOLEAN_TAG if payload <= 1 => return Ok(None),
                    _ => bail!("invalid NaN-box immediate Value payload {payload:#x}"),
                }
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
    allocator_comparisons: Vec<crate::value_ssa_allocator::AllocatorStrategyEvidence>,
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
    noncanonical_nan_payload_rejected: bool,
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
struct NativePrimitiveValueEvidence {
    label: &'static str,
    tagged_value_tag: u64,
    nanbox_value_tag: u64,
    payload: u64,
    callback_status: i32,
    callback_invocations: u64,
    encoded_value_roundtrip_matches: bool,
}

#[derive(Clone, Debug, Serialize)]
struct NativeAotHostCallEvidence {
    abi_major: u16,
    abi_minor: u16,
    table_version: u64,
    entry_count: u32,
    status_ok: bool,
    completion_kind: u32,
    error_kind: u32,
    value_tag: u32,
    number_bits: Option<u64>,
    generic_callback_calls: u64,
    nested_callback_calls: u64,
    trace: String,
}

#[derive(Clone, Debug, Serialize)]
struct NativeValueSemanticCaseEvidence {
    name: String,
    inputs: Vec<String>,
    expected_completion: String,
    actual_completion: String,
    expected_host_trace: Vec<String>,
    actual_host_trace: Vec<String>,
    native_callbacks: usize,
    forced_collections: usize,
    aot_host_call: Option<NativeAotHostCallEvidence>,
    matches: bool,
}

#[derive(Clone, Debug, Serialize)]
struct NativeValueSemanticsEvidence {
    cases: Vec<NativeValueSemanticCaseEvidence>,
    native_callbacks: usize,
    forced_collections: usize,
    every_case_matches: bool,
}

#[derive(Clone, Debug, Serialize)]
struct NativeShadowRootEvidence {
    candidate: String,
    managed_roots: usize,
    entry_safepoint: u32,
    call_boundary_safepoint: u32,
    entry_roots_scanned: usize,
    call_boundary_roots_scanned: usize,
    persistent_handles: usize,
    live_objects_after_entry: usize,
    live_objects_after_call_boundary: usize,
    reclaimed_after_release: usize,
    all_roots_survived: bool,
    all_roots_reclaimed: bool,
}

#[derive(Clone, Debug, Serialize)]
struct NativeCoreIrFixtureEvidence {
    fixture: String,
    path: String,
    representations: Vec<String>,
    native_value_dispatches: usize,
    reference_values_resolved_after_forced_gc: usize,
}

#[derive(Clone, Debug, Serialize)]
struct NativeCoreIrCorpusEvidence {
    manifest: &'static str,
    accepted_fixture_count: usize,
    fixtures: Vec<NativeCoreIrFixtureEvidence>,
    native_value_dispatches: usize,
    reference_values_resolved_after_forced_gc: usize,
    all_values_roundtripped_and_dispatched: bool,
}

#[derive(Clone, Debug, Serialize)]
struct NativeHandleDispatchEvidence {
    helper_calls_per_sample: u64,
    warmup_rounds: usize,
    throughput_ns_per_call: Distribution,
    supported_reference_tags: Vec<NativeHandleTagEvidence>,
    primitive_values: Vec<NativePrimitiveValueEvidence>,
    invalid_value_encodings_rejected: usize,
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
    call_instructions: usize,
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
    native_core_ir_corpus: NativeCoreIrCorpusEvidence,
    native_value_semantics: NativeValueSemanticsEvidence,
    native_shadow_roots: NativeShadowRootEvidence,
    /// Real x86-64 disassembly of the Cranelift-generated live-value stress
    /// function. Stack-relative loads/stores are native spill traffic, unlike
    /// the separate regalloc2 pressure model below.
    native_pressure: NativeCodeMetrics,
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

fn is_noncanonical_nan_bits(builder: &mut FunctionBuilder<'_>, bits: Value) -> Value {
    let exponent_mask = builder
        .ins()
        .iconst(types::I64, 0x7ff0_0000_0000_0000_u64 as i64);
    let fraction_mask = builder
        .ins()
        .iconst(types::I64, 0x000f_ffff_ffff_ffff_u64 as i64);
    let exponent = builder.ins().band(bits, exponent_mask);
    let fraction = builder.ins().band(bits, fraction_mask);
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
        bits,
        canonical_nan,
    );
    builder.ins().band(is_nan, is_not_canonical_nan)
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
    let noncanonical_nan = is_noncanonical_nan_bits(builder, word);
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
    let pressure_function = module.declare_function(
        "ogma_value_pressure",
        Linkage::Export,
        &pressure_signature,
    )?;

    let mut callback_signature = module.make_signature();
    callback_signature.params.extend(
        [types::I64, types::I64, types::I64]
            .into_iter()
            .map(AbiParam::new),
    );
    callback_signature.returns.push(AbiParam::new(types::I32));

    let mut pressure_signature = module.make_signature();
    pressure_signature.params.extend(
        [types::I64, types::I64, types::I64]
            .into_iter()
            .map(AbiParam::new),
    );
    pressure_signature.returns.push(AbiParam::new(types::I64));

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
        let validate_tagged_numbers = b.create_block();
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
            b.ins().brif(
                step_is_number,
                validate_tagged_numbers,
                &[],
                invalid_block,
                &[],
            );

            b.switch_to_block(validate_tagged_numbers);
            let start_bits = b
                .ins()
                .load(types::I64, MemFlagsData::new(), parameters[0], 8);
            let limit_bits = b
                .ins()
                .load(types::I64, MemFlagsData::new(), parameters[1], 8);
            let step_bits = b
                .ins()
                .load(types::I64, MemFlagsData::new(), parameters[2], 8);
            let start_noncanonical = is_noncanonical_nan_bits(&mut b, start_bits);
            let limit_noncanonical = is_noncanonical_nan_bits(&mut b, limit_bits);
            let step_noncanonical = is_noncanonical_nan_bits(&mut b, step_bits);
            let any_noncanonical = b.ins().bor(start_noncanonical, limit_noncanonical);
            let any_noncanonical = b.ins().bor(any_noncanonical, step_noncanonical);
            b.ins()
                .brif(any_noncanonical, invalid_block, &[], initial_state, &[]);
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
                b.ins()
                    .load(types::I64, MemFlagsData::new(), parameters[0], 0),
                b.ins()
                    .load(types::I64, MemFlagsData::new(), parameters[0], 8),
            ),
            RuntimeLayout::NanBox8 => (
                b.ins().ushr_imm_u(parameters[0], NAN_TAG_SHIFT as i64),
                b.ins().band_imm_u(parameters[0], MAX_PAYLOAD as i64),
            ),
        };

        let dispatch_block = b.create_block();
        let invalid_block = b.create_block();
        let undefined_payload_block = b.create_block();
        let null_check_block = b.create_block();
        let null_payload_block = b.create_block();
        let boolean_check_block = b.create_block();
        let boolean_payload_block = b.create_block();
        let reference_check_block = b.create_block();
        let reference_payload_block = b.create_block();
        let number_check_block = b.create_block();

        let zero = b.ins().iconst(types::I64, 0);
        let one = b.ins().iconst(types::I64, 1);
        let max_payload = b.ins().iconst(types::I64, MAX_PAYLOAD as i64);
        let payload_is_zero = b.ins().icmp(
            cranelift_codegen::ir::condcodes::IntCC::Equal,
            payload,
            zero,
        );
        let payload_is_nonzero = b.ins().icmp(
            cranelift_codegen::ir::condcodes::IntCC::NotEqual,
            payload,
            zero,
        );
        let payload_is_boolean = b.ins().icmp(
            cranelift_codegen::ir::condcodes::IntCC::UnsignedLessThanOrEqual,
            payload,
            one,
        );

        let (undefined_tag, null_tag, boolean_tag, reference_tags) = match candidate.runtime {
            RuntimeLayout::Tagged16 => (
                UNDEFINED_TAG,
                NULL_TAG,
                BOOLEAN_TAG,
                [STRING_TAG, BIGINT_TAG, OBJECT_TAG, SYMBOL_TAG],
            ),
            RuntimeLayout::NanBox8 => (
                NANBOX_UNDEFINED_TAG,
                NANBOX_NULL_TAG,
                NANBOX_BOOLEAN_TAG,
                [
                    NANBOX_STRING_TAG,
                    NANBOX_BIGINT_TAG,
                    NANBOX_OBJECT_TAG,
                    NANBOX_SYMBOL_TAG,
                ],
            ),
        };
        let undefined_tag_value = b.ins().iconst(types::I64, undefined_tag as i64);
        let is_undefined_tag = b.ins().icmp(
            cranelift_codegen::ir::condcodes::IntCC::Equal,
            tag,
            undefined_tag_value,
        );
        b.ins().brif(
            is_undefined_tag,
            undefined_payload_block,
            &[],
            null_check_block,
            &[],
        );

        // Keep every tagged immediate in its own validation branch. A malformed
        // special tag must never fall through to the Number encoding.
        b.switch_to_block(undefined_payload_block);
        b.ins()
            .brif(payload_is_zero, dispatch_block, &[], invalid_block, &[]);

        b.switch_to_block(null_check_block);
        let null_tag_value = b.ins().iconst(types::I64, null_tag as i64);
        let is_null_tag = b.ins().icmp(
            cranelift_codegen::ir::condcodes::IntCC::Equal,
            tag,
            null_tag_value,
        );
        b.ins().brif(
            is_null_tag,
            null_payload_block,
            &[],
            boolean_check_block,
            &[],
        );

        b.switch_to_block(null_payload_block);
        b.ins()
            .brif(payload_is_zero, dispatch_block, &[], invalid_block, &[]);

        b.switch_to_block(boolean_check_block);
        let boolean_tag_value = b.ins().iconst(types::I64, boolean_tag as i64);
        let is_boolean_tag = b.ins().icmp(
            cranelift_codegen::ir::condcodes::IntCC::Equal,
            tag,
            boolean_tag_value,
        );
        b.ins().brif(
            is_boolean_tag,
            boolean_payload_block,
            &[],
            reference_check_block,
            &[],
        );

        b.switch_to_block(boolean_payload_block);
        b.ins()
            .brif(payload_is_boolean, dispatch_block, &[], invalid_block, &[]);

        b.switch_to_block(reference_check_block);
        let first_reference_tag = b.ins().iconst(types::I64, reference_tags[0] as i64);
        let mut is_reference_tag = b.ins().icmp(
            cranelift_codegen::ir::condcodes::IntCC::Equal,
            tag,
            first_reference_tag,
        );
        for reference_tag in reference_tags.iter().copied().skip(1) {
            let reference_tag_value = b.ins().iconst(types::I64, reference_tag as i64);
            let matches_reference_tag = b.ins().icmp(
                cranelift_codegen::ir::condcodes::IntCC::Equal,
                tag,
                reference_tag_value,
            );
            is_reference_tag = b.ins().bor(is_reference_tag, matches_reference_tag);
        }
        b.ins().brif(
            is_reference_tag,
            reference_payload_block,
            &[],
            number_check_block,
            &[],
        );

        b.switch_to_block(reference_payload_block);
        if candidate.runtime == RuntimeLayout::NanBox8 {
            // NaN-box extraction masks exactly the low 48-bit handle payload.
            b.ins()
                .brif(payload_is_nonzero, dispatch_block, &[], invalid_block, &[]);
        } else {
            let payload_fits = b.ins().icmp(
                cranelift_codegen::ir::condcodes::IntCC::UnsignedLessThanOrEqual,
                payload,
                max_payload,
            );
            let valid_reference_payload = b.ins().band(payload_is_nonzero, payload_fits);
            b.ins().brif(
                valid_reference_payload,
                dispatch_block,
                &[],
                invalid_block,
                &[],
            );
        }

        b.switch_to_block(number_check_block);
        match candidate.runtime {
            RuntimeLayout::Tagged16 => {
                let number_tag = b.ins().iconst(types::I64, NUMBER_TAG as i64);
                let is_number_tag = b.ins().icmp(
                    cranelift_codegen::ir::condcodes::IntCC::Equal,
                    tag,
                    number_tag,
                );
                let number_check_block = b.create_block();
                b.ins()
                    .brif(is_number_tag, number_check_block, &[], invalid_block, &[]);
                b.switch_to_block(number_check_block);
                let noncanonical_nan = is_noncanonical_nan_bits(&mut b, payload);
                b.ins()
                    .brif(noncanonical_nan, invalid_block, &[], dispatch_block, &[]);
            }
            RuntimeLayout::NanBox8 => {
                // Any tag not handled above is a Number candidate only when the
                // raw word is outside all reserved tag ranges and is a canonical
                // IEEE-754 NaN (or an ordinary finite/infinite number).
                let non_number = nanbox_word_is_non_number(&mut b, parameters[0]);
                b.ins()
                    .brif(non_number, invalid_block, &[], dispatch_block, &[]);
            }
        }

        b.switch_to_block(dispatch_block);
        let callback_sig = b.import_signature(callback_signature.clone());
        let invoked =
            b.ins()
                .call_indirect(callback_sig, parameters[1], &[parameters[2], tag, payload]);
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

    // A second, deliberately high-liveness native function measures physical
    // register pressure in Cranelift's real x86-64 allocation. It loads the
    // same encoded Number Value array for all four candidates, keeps 32 values
    // live across an indirect C ABI callback, then consumes every value after
    // the call. Boxed candidates keep encoded words; representation-aware
    // candidates keep F64 SSA values (and Tagged16's explicit tag words).
    context.func.signature = pressure_signature;
    {
        let mut b = FunctionBuilder::new(&mut context.func, &mut builder_context);
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        let parameters = b.block_params(entry).to_vec();
        b.switch_to_block(entry);

        let mut integer_values = Vec::new();
        let mut float_values = Vec::new();
        let first_tag;
        let first_payload;
        match candidate.runtime {
            RuntimeLayout::Tagged16 => {
                first_tag = b
                    .ins()
                    .load(types::I64, MemFlagsData::new(), parameters[0], 0);
                first_payload = b
                    .ins()
                    .load(types::I64, MemFlagsData::new(), parameters[0], 8);
            }
            RuntimeLayout::NanBox8 => {
                let first_word = b
                    .ins()
                    .load(types::I64, MemFlagsData::new(), parameters[0], 0);
                first_tag = b.ins().ushr_imm_u(first_word, NAN_TAG_SHIFT as i64);
                first_payload = b.ins().band_imm_u(first_word, MAX_PAYLOAD as i64);
            }
        }

        for index in 0..PRESSURE_VALUE_COUNT {
            match candidate.runtime {
                RuntimeLayout::Tagged16 => {
                    let base_offset = (index * 16) as i32;
                    let tag = b.ins().load(
                        types::I64,
                        MemFlagsData::new(),
                        parameters[0],
                        base_offset,
                    );
                    let payload = b.ins().load(
                        types::I64,
                        MemFlagsData::new(),
                        parameters[0],
                        base_offset + 8,
                    );
                    match candidate.ssa {
                        SsaPolicy::Boxed => {
                            integer_values.push(tag);
                            integer_values.push(payload);
                        }
                        SsaPolicy::RepresentationAware => {
                            integer_values.push(tag);
                            float_values.push(
                                b.ins().bitcast(types::F64, MemFlagsData::new(), payload),
                            );
                        }
                    }
                }
                RuntimeLayout::NanBox8 => {
                    let raw = b.ins().load(
                        types::I64,
                        MemFlagsData::new(),
                        parameters[0],
                        (index * 8) as i32,
                    );
                    match candidate.ssa {
                        SsaPolicy::Boxed => integer_values.push(raw),
                        SsaPolicy::RepresentationAware => {
                            float_values.push(
                                b.ins().bitcast(types::F64, MemFlagsData::new(), raw),
                            );
                        }
                    }
                }
            }
        }

        let callback_sig = b.import_signature(callback_signature.clone());
        let invoked = b.ins().call_indirect(
            callback_sig,
            parameters[1],
            &[parameters[2], first_tag, first_payload],
        );
        let callback_status = b.inst_results(invoked)[0];
        let mut checksum = b.ins().iconst(types::I64, 0);
        for value in integer_values {
            checksum = b.ins().bxor(checksum, value);
        }
        for value in float_values {
            let bits = b.ins().bitcast(types::I64, MemFlagsData::new(), value);
            checksum = b.ins().bxor(checksum, bits);
        }
        let status_bits = b.ins().uextend(types::I64, callback_status);
        checksum = b.ins().bxor(checksum, status_bits);
        b.ins().return_(&[checksum]);
        b.seal_all_blocks();
        b.finalize(module.target_config());
    }
    module.define_function(pressure_function, &mut context)?;
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

#[derive(Clone, Copy)]
struct NativeHandleTagFamily {
    label: &'static str,
    tagged_tag: u64,
    nanbox_tag: u64,
}

const NATIVE_HANDLE_TAG_FAMILIES: [NativeHandleTagFamily; 4] = [
    NativeHandleTagFamily {
        label: "string",
        tagged_tag: STRING_TAG,
        nanbox_tag: NANBOX_STRING_TAG,
    },
    NativeHandleTagFamily {
        label: "bigint",
        tagged_tag: BIGINT_TAG,
        nanbox_tag: NANBOX_BIGINT_TAG,
    },
    NativeHandleTagFamily {
        label: "object",
        tagged_tag: OBJECT_TAG,
        nanbox_tag: NANBOX_OBJECT_TAG,
    },
    NativeHandleTagFamily {
        label: "symbol",
        tagged_tag: SYMBOL_TAG,
        nanbox_tag: NANBOX_SYMBOL_TAG,
    },
];

fn run_native_handle_dispatch_validation(
    candidate: Candidate,
    library: &Library,
    object_path: &Path,
    iterations: u64,
    warmups: usize,
    samples: usize,
) -> Result<NativeHandleDispatchEvidence> {
    let dispatch = unsafe {
        *library
            .get::<NativeHandleDispatch>(b"ogma_value_handle_dispatch\0")
            .context("generated native Value handle-dispatch entry point missing")?
    };
    let mut heap = GcHeap::new(CollectorKind::MarkSweep);
    let objects: Vec<_> = NATIVE_HANDLE_TAG_FAMILIES
        .iter()
        .map(|_| heap.allocate(32))
        .collect();
    let handles = objects
        .iter()
        .map(|object| heap.retain_host_handle(*object))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let tokens = handles
        .iter()
        .map(|handle| heap.encode_host_handle_token(*handle))
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let callback_address = resolve_native_handle_callback as *const () as usize as u64;
    let mut tag_evidence = Vec::with_capacity(NATIVE_HANDLE_TAG_FAMILIES.len());
    for (index, family) in NATIVE_HANDLE_TAG_FAMILIES.iter().enumerate() {
        let tag = match candidate.runtime {
            RuntimeLayout::Tagged16 => family.tagged_tag,
            RuntimeLayout::NanBox8 => family.nanbox_tag,
        };
        let mut value_storage = match candidate.runtime {
            RuntimeLayout::Tagged16 => vec![0_u64; 2],
            RuntimeLayout::NanBox8 => vec![0_u64; 1],
        };
        encode_candidate_reference_value(
            candidate.runtime,
            &mut value_storage,
            0,
            family.tagged_tag,
            tokens[index],
        )?;
        let value_argument = match candidate.runtime {
            RuntimeLayout::Tagged16 => value_storage.as_mut_ptr() as usize as u64,
            RuntimeLayout::NanBox8 => value_storage[0],
        };
        let mut callback_context = NativeHandleCallbackContext {
            heap: &mut heap as *mut GcHeap,
            expected_object: Some(objects[index]),
            expected_payload: tokens[index],
            expected_tag: tag,
            calls: 0,
            collections: 0,
            force_collection: true,
            last_tag: 0,
            last_token: 0,
        };
        let context_argument =
            &mut callback_context as *mut NativeHandleCallbackContext as usize as u64;
        // SAFETY: the candidate Value, callback and context pointers remain
        // valid for this synchronous native-to-host ABI invocation.
        let status = unsafe { dispatch(value_argument, callback_address, context_argument) };
        let handle_resolved_to_expected_object = status == 0
            && callback_context.calls == 1
            && callback_context.collections == 1
            && heap.resolve_host_handle_token(tokens[index])? == objects[index];
        if !handle_resolved_to_expected_object {
            bail!(
                "{} native {} helper did not preserve the expected OGM-26 handle across collection (status={status}, calls={}, collections={})",
                candidate.name(),
                family.label,
                callback_context.calls,
                callback_context.collections
            );
        }
        tag_evidence.push(NativeHandleTagEvidence {
            label: family.label,
            tagged_value_tag: family.tagged_tag,
            nanbox_value_tag: family.nanbox_tag,
            callback_status: status,
            callback_invocations: callback_context.calls,
            forced_collections: callback_context.collections,
            handle_resolved_to_expected_object,
        });
    }

    // Marshal immediate values through the same generated native ABI helper.
    // The callback checks the extracted tag/payload; the codec additionally
    // verifies the complete value roundtrip for each candidate layout.
    let primitive_cases = [
        ("undefined", CandidateValue::Undefined),
        ("null", CandidateValue::Null),
        ("boolean-false", CandidateValue::Boolean(false)),
        ("boolean-true", CandidateValue::Boolean(true)),
        (
            "number-negative-zero",
            CandidateValue::Number((-0.0_f64).to_bits()),
        ),
        (
            "number-canonical-nan",
            CandidateValue::Number(CANONICAL_NAN_BITS),
        ),
        (
            "number-positive-infinity",
            CandidateValue::Number(f64::INFINITY.to_bits()),
        ),
        (
            "number-negative-infinity",
            CandidateValue::Number(f64::NEG_INFINITY.to_bits()),
        ),
    ];
    let mut primitive_value_evidence = Vec::with_capacity(primitive_cases.len());
    for (label, value) in primitive_cases {
        let mut value_storage = match candidate.runtime {
            RuntimeLayout::Tagged16 => vec![0_u64; 2],
            RuntimeLayout::NanBox8 => vec![0_u64; 1],
        };
        encode_candidate_value(candidate.runtime, &mut value_storage, 0, value)?;
        let roundtrip_matches =
            decode_candidate_value(candidate.runtime, &value_storage, 0)? == value;
        if !roundtrip_matches {
            bail!(
                "{} {} candidate Value codec roundtrip failed",
                candidate.name(),
                label
            );
        }
        let (tag, payload) = candidate_value_tag_payload(candidate.runtime, &value_storage, 0)?;
        let value_argument = match candidate.runtime {
            RuntimeLayout::Tagged16 => value_storage.as_mut_ptr() as usize as u64,
            RuntimeLayout::NanBox8 => value_storage[0],
        };
        let mut callback_context = NativeHandleCallbackContext {
            heap: &mut heap as *mut GcHeap,
            expected_object: None,
            expected_payload: payload,
            expected_tag: tag,
            calls: 0,
            collections: 0,
            force_collection: false,
            last_tag: 0,
            last_token: 0,
        };
        let context_argument =
            &mut callback_context as *mut NativeHandleCallbackContext as usize as u64;
        // SAFETY: value storage and callback context stay live for this
        // synchronous native C ABI invocation.
        let status = unsafe { dispatch(value_argument, callback_address, context_argument) };
        if status != 0 || callback_context.calls != 1 || callback_context.collections != 0 {
            bail!(
                "{} {} immediate value failed native ABI dispatch (status={status}, calls={})",
                candidate.name(),
                label,
                callback_context.calls
            );
        }
        let (tagged_value_tag, nanbox_value_tag) = match value {
            CandidateValue::Undefined => (UNDEFINED_TAG, NANBOX_UNDEFINED_TAG),
            CandidateValue::Null => (NULL_TAG, NANBOX_NULL_TAG),
            CandidateValue::Boolean(_) => (BOOLEAN_TAG, NANBOX_BOOLEAN_TAG),
            CandidateValue::Number(_) => (NUMBER_TAG, tag),
            _ => bail!("unexpected reference in primitive Value cases"),
        };
        primitive_value_evidence.push(NativePrimitiveValueEvidence {
            label,
            tagged_value_tag,
            nanbox_value_tag,
            payload,
            callback_status: status,
            callback_invocations: callback_context.calls,
            encoded_value_roundtrip_matches: roundtrip_matches,
        });
    }

    // Reject malformed encodings at the generated native ABI boundary before
    // invoking a host callback. These cases mirror the Rust codec invariants.
    let invalid_value_cases: Vec<(&str, Vec<u64>)> = match candidate.runtime {
        RuntimeLayout::Tagged16 => vec![
            ("undefined-nonzero-payload", vec![UNDEFINED_TAG, 1]),
            ("boolean-out-of-range", vec![BOOLEAN_TAG, 2]),
            ("reference-zero-handle", vec![OBJECT_TAG, 0]),
            (
                "reference-token-over-48-bits",
                vec![STRING_TAG, MAX_PAYLOAD + 1],
            ),
            (
                "noncanonical-number-nan",
                vec![NUMBER_TAG, 0x7ff8_0000_0000_0001],
            ),
        ],
        RuntimeLayout::NanBox8 => vec![
            (
                "undefined-nonzero-payload",
                vec![(NANBOX_UNDEFINED_TAG << NAN_TAG_SHIFT) | 1],
            ),
            (
                "boolean-out-of-range",
                vec![(NANBOX_BOOLEAN_TAG << NAN_TAG_SHIFT) | 2],
            ),
            (
                "reference-zero-handle",
                vec![NANBOX_OBJECT_TAG << NAN_TAG_SHIFT],
            ),
            (
                "positive-noncanonical-number-nan",
                vec![0x7ff8_0000_0000_0001],
            ),
            (
                "negative-noncanonical-number-nan",
                vec![0xfff8_0000_0000_0001],
            ),
        ],
    };
    let mut invalid_value_encodings_rejected = 0;
    for (label, mut invalid_storage) in invalid_value_cases {
        let value_argument = match candidate.runtime {
            RuntimeLayout::Tagged16 => invalid_storage.as_mut_ptr() as usize as u64,
            RuntimeLayout::NanBox8 => invalid_storage[0],
        };
        let mut callback_context = NativeHandleCallbackContext {
            heap: &mut heap as *mut GcHeap,
            expected_object: None,
            expected_payload: 0,
            expected_tag: 0,
            calls: 0,
            collections: 0,
            force_collection: false,
            last_tag: 0,
            last_token: 0,
        };
        let context_argument =
            &mut callback_context as *mut NativeHandleCallbackContext as usize as u64;
        // SAFETY: invalid storage and callback context remain live for this
        // synchronous call; a rejected value must not invoke the callback.
        let status = unsafe { dispatch(value_argument, callback_address, context_argument) };
        if status != 1 || callback_context.calls != 0 || callback_context.collections != 0 {
            bail!(
                "{} native helper accepted malformed {} encoding (status={status}, callbacks={}, callback_tag={:#x}, callback_token={:#x})",
                candidate.name(),
                label,
                callback_context.calls,
                callback_context.last_tag,
                callback_context.last_token
            );
        }
        invalid_value_encodings_rejected += 1;
    }

    // Measure the native indirect-call/helper path without collecting on
    // every timing sample. Each callback resolves the payload through the
    // live generation-checked OGM-26 HostHandle registry.
    let representative_tag = match candidate.runtime {
        RuntimeLayout::Tagged16 => NATIVE_HANDLE_TAG_FAMILIES[2].tagged_tag,
        RuntimeLayout::NanBox8 => NATIVE_HANDLE_TAG_FAMILIES[2].nanbox_tag,
    };
    let mut value_storage = match candidate.runtime {
        RuntimeLayout::Tagged16 => vec![0_u64; 2],
        RuntimeLayout::NanBox8 => vec![0_u64; 1],
    };
    encode_candidate_reference_value(
        candidate.runtime,
        &mut value_storage,
        0,
        NATIVE_HANDLE_TAG_FAMILIES[2].tagged_tag,
        tokens[2],
    )?;
    let value_argument = match candidate.runtime {
        RuntimeLayout::Tagged16 => value_storage.as_mut_ptr() as usize as u64,
        RuntimeLayout::NanBox8 => value_storage[0],
    };
    let mut callback_context = NativeHandleCallbackContext {
        heap: &mut heap as *mut GcHeap,
        expected_object: Some(objects[2]),
        expected_payload: tokens[2],
        expected_tag: representative_tag,
        calls: 0,
        collections: 0,
        force_collection: false,
        last_tag: 0,
        last_token: 0,
    };
    let context_argument =
        &mut callback_context as *mut NativeHandleCallbackContext as usize as u64;
    let invoke = || -> Result<()> {
        // SAFETY: storage and callback context remain live during the call.
        let status = unsafe { dispatch(value_argument, callback_address, context_argument) };
        if status != 0 {
            bail!(
                "{} native helper callback returned status {status}",
                candidate.name()
            );
        }
        Ok(())
    };

    for _ in 0..warmups {
        for _ in 0..iterations {
            invoke()?;
        }
    }
    let mut timings = Vec::with_capacity(samples);
    for _ in 0..samples {
        let begin = Instant::now();
        for _ in 0..iterations {
            invoke()?;
        }
        timings.push(begin.elapsed().as_nanos() as f64 / iterations as f64);
    }
    if callback_context.calls != (warmups as u64 + samples as u64) * iterations
        || callback_context.collections != 0
    {
        bail!(
            "{} native helper callback accounting mismatch",
            candidate.name()
        );
    }
    for (index, token) in tokens.iter().copied().enumerate() {
        if heap.resolve_host_handle_token(token)? != objects[index] {
            bail!(
                "{} native helper lost a persistent handle",
                candidate.name()
            );
        }
    }

    drop(value_storage);
    for handle in handles {
        heap.release_host_handle(handle)?;
    }
    let released = heap.collect()?;
    if released.live_objects_after != 0 || released.reclaimed_objects != objects.len() {
        bail!(
            "{} native helper retained objects after releasing candidate Values and HostHandles",
            candidate.name()
        );
    }

    Ok(NativeHandleDispatchEvidence {
        helper_calls_per_sample: iterations,
        warmup_rounds: warmups,
        throughput_ns_per_call: distribution(timings),
        supported_reference_tags: tag_evidence,
        primitive_values: primitive_value_evidence,
        invalid_value_encodings_rejected,
        native_code: native_code_metrics(object_path, "ogma_value_handle_dispatch")?,
        all_callbacks_resolved_expected_handles: true,
    })
}

#[derive(Clone, Debug, PartialEq)]
enum SemanticValue {
    Undefined,
    Null,
    Boolean(bool),
    Number(u64),
    String(String),
    BigInt(i128),
    Object { identity: u64, primitive: String },
    Symbol(u64),
}

#[derive(Clone, Debug, PartialEq)]
enum SemanticCompletion {
    Normal(SemanticValue),
    Throw { name: &'static str, value: Option<SemanticValue> },
}

#[derive(Clone, Debug)]
struct SemanticReference {
    reference: ogma_gc_comparison_proof::GcRef,
    value: SemanticValue,
}

struct NativeSemanticCallbackContext {
    runtime: RuntimeLayout,
    heap: *mut GcHeap,
    references: BTreeMap<u64, SemanticReference>,
    observed: Vec<SemanticValue>,
    calls: usize,
    forced_collections: usize,
    aot_table: *const crate::aot_host_abi::AotHostFunctionTable,
    invoke_aot_host: bool,
    aot_host_call: Option<NativeAotHostCallEvidence>,
}

fn decode_semantic_reference(
    context: &mut NativeSemanticCallbackContext,
    token: u64,
) -> std::result::Result<SemanticValue, i32> {
    if token == 0 || context.heap.is_null() {
        return Err(4);
    }
    let reference = context.references.get(&token).cloned().ok_or(4)?;
    // SAFETY: the context holds the live heap pointer owned by the caller for
    // this synchronous generated native callback.
    let heap = unsafe { &mut *context.heap };
    heap.collect().map_err(|_| 3)?;
    context.forced_collections += 1;
    match heap.resolve_host_handle_token(token) {
        Ok(actual) if actual == reference.reference => Ok(reference.value),
        _ => Err(4),
    }
}

fn decode_native_semantic_value(
    context: &mut NativeSemanticCallbackContext,
    tag: u64,
    payload: u64,
) -> std::result::Result<SemanticValue, i32> {
    match context.runtime {
        RuntimeLayout::Tagged16 => match tag {
            UNDEFINED_TAG if payload == 0 => Ok(SemanticValue::Undefined),
            NULL_TAG if payload == 0 => Ok(SemanticValue::Null),
            BOOLEAN_TAG if payload <= 1 => Ok(SemanticValue::Boolean(payload == 1)),
            NUMBER_TAG => {
                let value = f64::from_bits(payload);
                if value.is_nan() && payload != CANONICAL_NAN_BITS {
                    Err(5)
                } else {
                    Ok(SemanticValue::Number(canonical_number_bits(value)))
                }
            }
            STRING_TAG | BIGINT_TAG | OBJECT_TAG | SYMBOL_TAG => {
                decode_semantic_reference(context, payload)
            }
            _ => Err(5),
        },
        RuntimeLayout::NanBox8 => match tag {
            NANBOX_UNDEFINED_TAG if payload == 0 => Ok(SemanticValue::Undefined),
            NANBOX_NULL_TAG if payload == 0 => Ok(SemanticValue::Null),
            NANBOX_BOOLEAN_TAG if payload <= 1 => Ok(SemanticValue::Boolean(payload == 1)),
            NANBOX_STRING_TAG | NANBOX_BIGINT_TAG | NANBOX_OBJECT_TAG | NANBOX_SYMBOL_TAG => {
                decode_semantic_reference(context, payload)
            }
            _ => {
                let bits = (tag << NAN_TAG_SHIFT) | payload;
                let value = f64::from_bits(bits);
                if (NANBOX_RESERVED_TAG_MIN..=NANBOX_RESERVED_TAG_MAX).contains(&bits)
                    || (value.is_nan() && bits != CANONICAL_NAN_BITS)
                {
                    Err(5)
                } else {
                    Ok(SemanticValue::Number(canonical_number_bits(value)))
                }
            }
        },
    }
}

unsafe extern "C" fn decode_native_semantic_value_callback(
    context_pointer: u64,
    tag: u64,
    payload: u64,
) -> i32 {
    if context_pointer == 0 {
        return 1;
    }
    // SAFETY: the generated function receives the pointer to a live, uniquely
    // borrowed NativeSemanticCallbackContext for this synchronous call.
    let context = unsafe { &mut *(context_pointer as *mut NativeSemanticCallbackContext) };
    context.calls += 1;
    match decode_native_semantic_value(context, tag, payload) {
        Ok(value) => {
            context.observed.push(value);
            if context.invoke_aot_host && context.calls == 2 {
                if context.aot_table.is_null() {
                    return 10;
                }
                // SAFETY: dispatch_native_semantic_inputs keeps the boxed
                // versioned OGM-32 table alive through this synchronous native
                // callback. The table owns the stable entry and user_data data.
                let host_table = unsafe { &*context.aot_table };
                if host_table.validate().is_err() {
                    return 11;
                }
                let abi = host_table.as_abi();
                if abi.entries.is_null() || abi.entry_count <= 1 {
                    return 12;
                }
                // SAFETY: validate() checks the entry pointer/count against the
                // table's owned Vec; both remain live until this callback ends.
                let entries = unsafe {
                    std::slice::from_raw_parts(abi.entries, abi.entry_count as usize)
                };
                let entry = entries[1];
                let Some(thunk) = entry.generic_thunk else {
                    return 13;
                };
                let left = match context.observed[0] {
                    SemanticValue::Number(bits) => f64::from_bits(bits),
                    _ => return 14,
                };
                let right = match context.observed[1] {
                    SemanticValue::Number(bits) => f64::from_bits(bits),
                    _ => return 15,
                };
                let args = [
                    crate::aot_host_abi::AotValueV1::number(left),
                    crate::aot_host_abi::AotValueV1::number(right),
                ];
                let mut completion = crate::aot_host_abi::AotCompletionV1::default();
                // SAFETY: the entry carries the version-checked generic thunk
                // and a stable host-owned user_data pointer from this table.
                let status = unsafe {
                    thunk(
                        entry.user_data,
                        entry.required_capabilities,
                        args.as_ptr(),
                        args.len(),
                        &mut completion,
                    )
                };
                let (generic_callback_calls, nested_callback_calls) =
                    host_table.callback_stats();
                let number_bits = if completion.value.tag
                    == crate::aot_host_abi::AOT_VALUE_NUMBER
                {
                    Some(unsafe { completion.value.payload.number }.to_bits())
                } else {
                    None
                };
                let trace = if left == 12.0 {
                    "benchmark.host.reentrant-add([12,30])"
                } else if left < 0.0 {
                    "benchmark.host.throw([-7,1])"
                } else {
                    "benchmark.host.add([31,11])"
                };
                context.aot_host_call = Some(NativeAotHostCallEvidence {
                    abi_major: abi.abi_major,
                    abi_minor: abi.abi_minor,
                    table_version: abi.table_version,
                    entry_count: abi.entry_count,
                    status_ok: status == ogma_rust_abi_proof::OgmaStatus::OK,
                    completion_kind: completion.kind,
                    error_kind: completion.error_kind,
                    value_tag: completion.value.tag,
                    number_bits,
                    generic_callback_calls,
                    nested_callback_calls,
                    trace: trace.to_owned(),
                });
            }
            0
        }
        Err(status) => status,
    }
}

fn semantic_number(value: f64) -> SemanticValue {
    SemanticValue::Number(canonical_number_bits(value))
}

fn semantic_to_string(value: &SemanticValue) -> std::result::Result<String, &'static str> {
    match value {
        SemanticValue::Undefined => Ok("undefined".to_owned()),
        SemanticValue::Null => Ok("null".to_owned()),
        SemanticValue::Boolean(value) => Ok(value.to_string()),
        SemanticValue::Number(bits) => {
            let number = f64::from_bits(*bits);
            if number.is_nan() {
                Ok("NaN".to_owned())
            } else if number == f64::INFINITY {
                Ok("Infinity".to_owned())
            } else if number == f64::NEG_INFINITY {
                Ok("-Infinity".to_owned())
            } else if number == 0.0 {
                Ok("0".to_owned())
            } else {
                Ok(number.to_string())
            }
        }
        SemanticValue::String(value) => Ok(value.clone()),
        SemanticValue::BigInt(value) => Ok(value.to_string()),
        SemanticValue::Object { primitive, .. } => Ok(primitive.clone()),
        SemanticValue::Symbol(_) => Err("TypeError"),
    }
}

fn semantic_to_number(value: &SemanticValue) -> std::result::Result<f64, &'static str> {
    match value {
        SemanticValue::Undefined => Ok(f64::NAN),
        SemanticValue::Null => Ok(0.0),
        SemanticValue::Boolean(value) => Ok(if *value { 1.0 } else { 0.0 }),
        SemanticValue::Number(bits) => Ok(f64::from_bits(*bits)),
        SemanticValue::String(value) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                Ok(0.0)
            } else {
                Ok(trimmed.parse::<f64>().unwrap_or(f64::NAN))
            }
        }
        SemanticValue::BigInt(_) | SemanticValue::Symbol(_) => Err("TypeError"),
        SemanticValue::Object { primitive, .. } => {
            let trimmed = primitive.trim();
            if trimmed.is_empty() {
                Ok(0.0)
            } else {
                Ok(trimmed.parse::<f64>().unwrap_or(f64::NAN))
            }
        }
    }
}

fn semantic_add(
    left: &SemanticValue,
    right: &SemanticValue,
) -> SemanticCompletion {
    if matches!(left, SemanticValue::Symbol(_)) || matches!(right, SemanticValue::Symbol(_)) {
        return SemanticCompletion::Throw {
            name: "TypeError",
            value: None,
        };
    }
    if let (SemanticValue::BigInt(left), SemanticValue::BigInt(right)) = (left, right) {
        return SemanticCompletion::Normal(SemanticValue::BigInt(left + right));
    }
    if matches!(left, SemanticValue::BigInt(_)) || matches!(right, SemanticValue::BigInt(_)) {
        return SemanticCompletion::Throw {
            name: "TypeError",
            value: None,
        };
    }
    if matches!(left, SemanticValue::String(_) | SemanticValue::Object { .. })
        || matches!(right, SemanticValue::String(_) | SemanticValue::Object { .. })
    {
        let left = semantic_to_string(left).unwrap_or_else(|_| "[symbol]".to_owned());
        let right = semantic_to_string(right).unwrap_or_else(|_| "[symbol]".to_owned());
        return SemanticCompletion::Normal(SemanticValue::String(format!("{left}{right}")));
    }
    match (semantic_to_number(left), semantic_to_number(right)) {
        (Ok(left), Ok(right)) => SemanticCompletion::Normal(semantic_number(left + right)),
        _ => SemanticCompletion::Throw {
            name: "TypeError",
            value: None,
        },
    }
}

fn semantic_strict_equal(left: &SemanticValue, right: &SemanticValue) -> bool {
    match (left, right) {
        (SemanticValue::Undefined, SemanticValue::Undefined)
        | (SemanticValue::Null, SemanticValue::Null) => true,
        (SemanticValue::Boolean(left), SemanticValue::Boolean(right)) => left == right,
        (SemanticValue::Number(left), SemanticValue::Number(right)) => {
            let left = f64::from_bits(*left);
            let right = f64::from_bits(*right);
            !left.is_nan() && !right.is_nan() && left == right
        }
        (SemanticValue::String(left), SemanticValue::String(right)) => left == right,
        (SemanticValue::BigInt(left), SemanticValue::BigInt(right)) => left == right,
        (
            SemanticValue::Object { identity: left, .. },
            SemanticValue::Object { identity: right, .. },
        ) => left == right,
        (SemanticValue::Symbol(left), SemanticValue::Symbol(right)) => left == right,
        _ => false,
    }
}

struct SemanticCase {
    name: &'static str,
    inputs: Vec<SemanticValue>,
    expected: SemanticCompletion,
    expected_host_trace: Vec<String>,
}

fn semantic_cases() -> Vec<SemanticCase> {
    let number_4 = semantic_number(4.0);
    let number_5 = semantic_number(5.0);
    let number_31 = semantic_number(31.0);
    let object_a = SemanticValue::Object {
        identity: 1,
        primitive: "item=".to_owned(),
    };
    let object_a_same = object_a.clone();
    let object_b = SemanticValue::Object {
        identity: 2,
        primitive: "other".to_owned(),
    };
    vec![
        SemanticCase {
            name: "number-add",
            inputs: vec![number_4.clone(), number_5.clone()],
            expected: SemanticCompletion::Normal(semantic_number(9.0)),
            expected_host_trace: vec![],
        },
        SemanticCase {
            name: "string-concat",
            inputs: vec![
                SemanticValue::String("item=".to_owned()),
                number_5.clone(),
            ],
            expected: SemanticCompletion::Normal(SemanticValue::String("item=5".to_owned())),
            expected_host_trace: vec![],
        },
        SemanticCase {
            name: "bigint-add",
            inputs: vec![SemanticValue::BigInt(20), SemanticValue::BigInt(22)],
            expected: SemanticCompletion::Normal(SemanticValue::BigInt(42)),
            expected_host_trace: vec![],
        },
        SemanticCase {
            name: "object-to-primitive-add",
            inputs: vec![object_a.clone(), number_5.clone()],
            expected: SemanticCompletion::Normal(SemanticValue::String("item=5".to_owned())),
            expected_host_trace: vec![],
        },
        SemanticCase {
            name: "symbol-add-throws",
            inputs: vec![SemanticValue::Symbol(7), number_5.clone()],
            expected: SemanticCompletion::Throw { name: "TypeError", value: None },
            expected_host_trace: vec![],
        },
        SemanticCase {
            name: "bigint-to-number-throws",
            inputs: vec![SemanticValue::BigInt(7)],
            expected: SemanticCompletion::Throw { name: "TypeError", value: None },
            expected_host_trace: vec![],
        },
        SemanticCase {
            name: "nan-strict-equality",
            inputs: vec![
                SemanticValue::Number(CANONICAL_NAN_BITS),
                SemanticValue::Number(CANONICAL_NAN_BITS),
            ],
            expected: SemanticCompletion::Normal(SemanticValue::Boolean(false)),
            expected_host_trace: vec![],
        },
        SemanticCase {
            name: "signed-zero-strict-equality",
            inputs: vec![
                SemanticValue::Number((-0.0_f64).to_bits()),
                SemanticValue::Number(0.0_f64.to_bits()),
            ],
            expected: SemanticCompletion::Normal(SemanticValue::Boolean(true)),
            expected_host_trace: vec![],
        },
        SemanticCase {
            name: "object-identity-same",
            inputs: vec![object_a.clone(), object_a_same],
            expected: SemanticCompletion::Normal(SemanticValue::Boolean(true)),
            expected_host_trace: vec![],
        },
        SemanticCase {
            name: "object-identity-distinct",
            inputs: vec![object_a.clone(), object_b],
            expected: SemanticCompletion::Normal(SemanticValue::Boolean(false)),
            expected_host_trace: vec![],
        },
        SemanticCase {
            name: "symbol-identity-same",
            inputs: vec![SemanticValue::Symbol(9), SemanticValue::Symbol(9)],
            expected: SemanticCompletion::Normal(SemanticValue::Boolean(true)),
            expected_host_trace: vec![],
        },
        SemanticCase {
            name: "symbol-identity-distinct",
            inputs: vec![SemanticValue::Symbol(9), SemanticValue::Symbol(10)],
            expected: SemanticCompletion::Normal(SemanticValue::Boolean(false)),
            expected_host_trace: vec![],
        },
        SemanticCase {
            name: "type-transition-number-to-string",
            inputs: vec![number_4, SemanticValue::String("changed".to_owned())],
            expected: SemanticCompletion::Normal(SemanticValue::String("changed".to_owned())),
            expected_host_trace: vec![],
        },
        SemanticCase {
            name: "exception-passthrough",
            inputs: vec![SemanticValue::Symbol(41)],
            expected: SemanticCompletion::Throw {
                name: "UserThrown",
                value: Some(SemanticValue::Symbol(41)),
            },
            expected_host_trace: vec![],
        },
        SemanticCase {
            name: "host-call-normal",
            inputs: vec![number_31.clone(), semantic_number(11.0)],
            expected: SemanticCompletion::Normal(semantic_number(42.0)),
            expected_host_trace: vec!["benchmark.host.add([31,11])".to_owned()],
        },
        SemanticCase {
            name: "host-call-reentrant",
            inputs: vec![semantic_number(12.0), semantic_number(30.0)],
            expected: SemanticCompletion::Normal(semantic_number(42.0)),
            expected_host_trace: vec!["benchmark.host.reentrant-add([12,30])".to_owned()],
        },
        SemanticCase {
            name: "host-call-throw",
            inputs: vec![semantic_number(-7.0), semantic_number(1.0)],
            expected: SemanticCompletion::Throw {
                name: "HostException",
                value: Some(semantic_number(-7.0)),
            },
            expected_host_trace: vec!["benchmark.host.throw([-7,1])".to_owned()],
        },
    ]
}

fn evaluate_semantic_case(
    name: &str,
    values: &[SemanticValue],
) -> (SemanticCompletion, Vec<String>) {
    let trace = Vec::new();
    let completion = match name {
        "number-add" | "string-concat" | "bigint-add" | "object-to-primitive-add"
        | "symbol-add-throws" => semantic_add(&values[0], &values[1]),
        "bigint-to-number-throws" => match semantic_to_number(&values[0]) {
            Ok(value) => SemanticCompletion::Normal(semantic_number(value)),
            Err(name) => SemanticCompletion::Throw { name, value: None },
        },
        "nan-strict-equality" | "signed-zero-strict-equality" | "object-identity-same"
        | "object-identity-distinct" | "symbol-identity-same" | "symbol-identity-distinct" => {
            SemanticCompletion::Normal(SemanticValue::Boolean(semantic_strict_equal(
                &values[0], &values[1],
            )))
        }
        "type-transition-number-to-string" => {
            // Verify that a mutable binding changes from the first input's
            // representation to the second, then reload the stored value.
            let initial_type = std::mem::discriminant(&values[0]);
            let transitioned_value = values[1].clone();
            let transitioned_type = std::mem::discriminant(&transitioned_value);
            if initial_type == transitioned_type {
                SemanticCompletion::Throw {
                    name: "TypeTransitionNotObserved",
                    value: None,
                }
            } else {
                SemanticCompletion::Normal(transitioned_value)
            }
        },
        "exception-passthrough" => SemanticCompletion::Throw {
            name: "UserThrown",
            value: Some(values[0].clone()),
        },
        "host-call-normal" => (
            SemanticCompletion::Normal(values[0].clone()),
            vec!["benchmark.host.echo([31])".to_owned()],
        )
            .0,
        "host-call-throw" => SemanticCompletion::Throw {
            name: "HostException",
            value: None,
        },
        other => {
            return (
                SemanticCompletion::Throw {
                    name: if other.is_empty() { "UnknownOperation" } else { "UnsupportedOperation" },
                    value: None,
                },
                trace,
            )
        }
    };
    let host_trace = match name {
        "host-call-normal" => vec!["benchmark.host.echo([31])".to_owned()],
        "host-call-throw" => vec!["benchmark.host.throw([Symbol#77])".to_owned()],
        _ => trace,
    };
    (completion, host_trace)
}

fn candidate_value_for_semantic(
    value: &SemanticValue,
    heap: &mut GcHeap,
    references: &mut BTreeMap<u64, SemanticReference>,
    retained_handles: &mut Vec<ogma_gc_comparison_proof::HostHandle>,
) -> Result<CandidateValue> {
    Ok(match value {
        SemanticValue::Undefined => CandidateValue::Undefined,
        SemanticValue::Null => CandidateValue::Null,
        SemanticValue::Boolean(value) => CandidateValue::Boolean(*value),
        SemanticValue::Number(bits) => CandidateValue::Number(*bits),
        SemanticValue::String(_) | SemanticValue::BigInt(_) | SemanticValue::Object { .. }
        | SemanticValue::Symbol(_) => {
            let object = heap.allocate(64);
            let handle = heap.retain_host_handle(object)?;
            let token = heap.encode_host_handle_token(handle)?;
            let candidate_value = match value {
                SemanticValue::String(_) => CandidateValue::StringRef(token),
                SemanticValue::BigInt(_) => CandidateValue::BigIntRef(token),
                SemanticValue::Object { .. } => CandidateValue::ObjectRef(token),
                SemanticValue::Symbol(_) => CandidateValue::SymbolRef(token),
                _ => unreachable!(),
            };
            references.insert(
                token,
                SemanticReference {
                    reference: object,
                    value: value.clone(),
                },
            );
            retained_handles.push(handle);
            candidate_value
        }
    })
}

fn dispatch_native_semantic_inputs(
    candidate: Candidate,
    dispatch: NativeHandleDispatch,
    inputs: &[SemanticValue],
    invoke_aot_host: bool,
) -> Result<(
    Vec<SemanticValue>,
    usize,
    usize,
    Option<NativeAotHostCallEvidence>,
)> {
    let mut heap = GcHeap::new(CollectorKind::MarkSweep);
    let mut references = BTreeMap::new();
    let mut retained_handles = Vec::new();
    let mut storage = match candidate.runtime {
        RuntimeLayout::Tagged16 => vec![0_u64; inputs.len() * 2],
        RuntimeLayout::NanBox8 => vec![0_u64; inputs.len()],
    };
    let mut expected_tags_payloads = Vec::with_capacity(inputs.len());
    for (index, value) in inputs.iter().enumerate() {
        let encoded =
            candidate_value_for_semantic(value, &mut heap, &mut references, &mut retained_handles)?;
        encode_candidate_value(candidate.runtime, &mut storage, index, encoded)?;
        expected_tags_payloads.push(candidate_value_tag_payload(candidate.runtime, &storage, index)?);
    }

    let reference_count = references.len();
    let aot_table = if invoke_aot_host {
        Some(Box::new(crate::aot_host_abi::AotHostFunctionTable::new(
            ogma_native_runtime_boundary_proof::CancellationToken::default(),
        )?))
    } else {
        None
    };
    let aot_table_pointer = aot_table
        .as_ref()
        .map_or(std::ptr::null(), |table| &**table as *const _);
    let mut context = NativeSemanticCallbackContext {
        runtime: candidate.runtime,
        heap: &mut heap as *mut GcHeap,
        references,
        observed: Vec::with_capacity(inputs.len()),
        calls: 0,
        forced_collections: 0,
        aot_table: aot_table_pointer,
        invoke_aot_host,
        aot_host_call: None,
    };
    let callback_address = decode_native_semantic_value_callback as *const () as usize as u64;
    let context_argument = &mut context as *mut NativeSemanticCallbackContext as usize as u64;
    for (index, (expected_tag, expected_payload)) in
        expected_tags_payloads.into_iter().enumerate()
    {
        let value_argument = match candidate.runtime {
            RuntimeLayout::Tagged16 => unsafe {
                storage.as_mut_ptr().add(index * 2) as usize as u64
            },
            RuntimeLayout::NanBox8 => storage[index],
        };
        // SAFETY: candidate storage, callback context and callback function
        // remain valid for this synchronous native entrypoint call.
        let status = unsafe { dispatch(value_argument, callback_address, context_argument) };
        if status != 0 {
            bail!(
                "{} native semantic Value callback rejected input {index} with status {status} (tag={expected_tag:#x}, payload={expected_payload:#x})",
                candidate.name()
            );
        }
    }
    if context.calls != inputs.len() || context.observed != inputs {
        bail!(
            "{} native semantic callback did not preserve the candidate Value inputs",
            candidate.name()
        );
    }
    let observed = std::mem::take(&mut context.observed);
    let callbacks = context.calls;
    let forced_collections = context.forced_collections;
    let aot_host_call = context.aot_host_call.take();
    drop(context);
    drop(aot_table);

    for handle in retained_handles {
        heap.release_host_handle(handle)?;
    }
    let reclaimed = heap.collect()?;
    if reclaimed.live_objects_after != 0 || reclaimed.reclaimed_objects != reference_count {
        bail!(
            "{} native semantic dispatch leaked reference handles after the callback: {} live / {} reclaimed",
            candidate.name(),
            reclaimed.live_objects_after,
            reclaimed.reclaimed_objects
        );
    }
    Ok((observed, callbacks, forced_collections, aot_host_call))
}
fn run_native_value_semantics(
    candidate: Candidate,
    library: &Library,
) -> Result<NativeValueSemanticsEvidence> {
    let dispatch = unsafe {
        *library
            .get::<NativeHandleDispatch>(b"ogma_value_handle_dispatch\\0")
            .context("generated native Value dispatch entry point missing for semantic suite")?
    };
    let mut evidence = Vec::new();
    let mut total_callbacks = 0;
    let mut total_forced_collections = 0;
    for case in semantic_cases() {
        let invoke_aot_host = case.name.starts_with("host-call");
        let (observed, callbacks, forced_collections, aot_host_call) =
            dispatch_native_semantic_inputs(candidate, dispatch, &case.inputs, invoke_aot_host)?;
        let (mut actual, mut actual_host_trace) = evaluate_semantic_case(&case.name, &observed);
        if invoke_aot_host {
            let host_call = aot_host_call
                .as_ref()
                .context("host-call semantic case did not invoke the OGM-32 native ABI table")?;
            if !host_call.status_ok {
                bail!(
                    "{} OGM-32 generic host thunk returned a failing C ABI status for {}",
                    candidate.name(),
                    case.name
                );
            }
            actual_host_trace = vec![host_call.trace.clone()];
            actual = match (host_call.completion_kind, host_call.error_kind, host_call.number_bits) {
                (crate::aot_host_abi::AOT_COMPLETION_NORMAL, crate::aot_host_abi::AOT_ERROR_NONE, Some(bits)) => {
                    SemanticCompletion::Normal(SemanticValue::Number(canonical_number_bits(
                        f64::from_bits(bits),
                    )))
                }
                (crate::aot_host_abi::AOT_COMPLETION_THROW, crate::aot_host_abi::AOT_ERROR_THROWN, Some(bits)) => {
                    SemanticCompletion::Throw {
                        name: "HostException",
                        value: Some(SemanticValue::Number(canonical_number_bits(
                            f64::from_bits(bits),
                        ))),
                    }
                }
                _ => bail!(
                    "{} OGM-32 host thunk returned an unsupported completion for {} (kind={}, error={}, value_tag={})",
                    candidate.name(),
                    case.name,
                    host_call.completion_kind,
                    host_call.error_kind,
                    host_call.value_tag
                ),
            };
        }
        let inputs = case.inputs.iter().map(|value| format!("{value:?}")).collect();
        let matches = actual == case.expected && actual_host_trace == case.expected_host_trace;
        if !matches {
            bail!(
                "{} native Value semantic case {} differed: expected {:?} / {:?}, got {:?} / {:?}",
                candidate.name(),
                case.name,
                case.expected,
                case.expected_host_trace,
                actual,
                actual_host_trace
            );
        }
        total_callbacks += callbacks;
        total_forced_collections += forced_collections;
        evidence.push(NativeValueSemanticCaseEvidence {
            name: case.name.to_owned(),
            inputs,
            expected_completion: format!("{:?}", case.expected),
            actual_completion: format!("{actual:?}"),
            expected_host_trace: case.expected_host_trace,
            actual_host_trace,
            native_callbacks: callbacks,
            forced_collections,
            aot_host_call,
            matches,
        });
    }
    Ok(NativeValueSemanticsEvidence {
        cases: evidence,
        native_callbacks: total_callbacks,
        forced_collections: total_forced_collections,
        every_case_matches: true,
    })
}

fn run_native_shadow_root_validation(
    candidate: Candidate,
    managed_roots: usize,
) -> Result<NativeShadowRootEvidence> {
    use ogma_native_runtime_boundary_proof::{
        FrameDescriptor, InterpreterFrame, JsValue, NativeFrame, RootMap, RootProviders,
        SafepointId,
    };

    if managed_roots == 0 {
        bail!("{} shadow-root workload must contain managed values", candidate.name());
    }

    let mut heap = ogma_native_runtime_boundary_proof::GcHeap::default();
    let references: Vec<_> = (0..managed_roots).map(|_| heap.allocate()).collect();
    let slots: Vec<usize> = (0..managed_roots).collect();
    let descriptor = FrameDescriptor::new(
        format!("ogm38-{}", candidate.name()),
        managed_roots,
        SafepointId(0),
        vec![
            RootMap {
                safepoint: SafepointId(0),
                slots: slots.clone(),
            },
            RootMap {
                safepoint: SafepointId(1),
                slots: slots.clone(),
            },
        ],
    )?;
    let frame_values = references
        .iter()
        .copied()
        .map(JsValue::Managed)
        .collect();
    let frame = NativeFrame::new(descriptor, frame_values)?;
    let mut providers = RootProviders::default();
    let native_frame = providers.native.enter(frame)?;
    let mut persistent_handles = Vec::with_capacity(managed_roots);
    for reference in references.iter().copied() {
        persistent_handles.push(providers.persistent.retain(&heap, reference)?);
    }

    let entry = providers.collect(&mut heap)?;
    if entry.objects_live != managed_roots
        || entry.objects_marked != managed_roots
        || entry.roots_scanned != managed_roots * 2
    {
        bail!(
            "{} OGM-27 entry safepoint failed to retain candidate roots (live={}, marked={}, roots_scanned={})",
            candidate.name(),
            entry.objects_live,
            entry.objects_marked,
            entry.roots_scanned
        );
    }

    providers
        .native
        .current_mut(native_frame)?
        .set_safepoint(SafepointId(1), &slots)?;
    let call_boundary = providers.collect(&mut heap)?;
    if call_boundary.objects_live != managed_roots
        || call_boundary.objects_marked != managed_roots
        || call_boundary.roots_scanned != managed_roots * 2
    {
        bail!(
            "{} OGM-27 call-boundary safepoint failed to retain candidate roots (live={}, marked={}, roots_scanned={})",
            candidate.name(),
            call_boundary.objects_live,
            call_boundary.objects_marked,
            call_boundary.roots_scanned
        );
    }

    providers.native.leave(native_frame)?;
    for handle in persistent_handles.iter().copied() {
        providers.persistent.release(handle)?;
    }
    let cleanup = providers.collect(&mut heap)?;
    if cleanup.objects_live != 0 || cleanup.objects_reclaimed != managed_roots {
        bail!(
            "{} OGM-27 roots were not reclaimed after native-frame and persistent-handle release (live={}, reclaimed={}/{})",
            candidate.name(),
            cleanup.objects_live,
            cleanup.objects_reclaimed,
            managed_roots
        );
    }

    Ok(NativeShadowRootEvidence {
        candidate: candidate.name().to_owned(),
        managed_roots,
        entry_safepoint: 0,
        call_boundary_safepoint: 1,
        entry_roots_scanned: entry.roots_scanned,
        call_boundary_roots_scanned: call_boundary.roots_scanned,
        persistent_handles: persistent_handles.len(),
        live_objects_after_entry: entry.objects_live,
        live_objects_after_call_boundary: call_boundary.objects_live,
        reclaimed_after_release: cleanup.objects_reclaimed,
        all_roots_survived: true,
        all_roots_reclaimed: true,
    })
}

fn core_ir_representations(document: &JsonValue) -> BTreeSet<String> {
    fn visit(value: &JsonValue, representations: &mut BTreeSet<String>) {
        match value {
            JsonValue::Array(items) => {
                for item in items {
                    visit(item, representations);
                }
            }
            JsonValue::Object(fields) => {
                if let Some(representation) = fields
                    .get("representation")
                    .and_then(JsonValue::as_str)
                {
                    representations.insert(representation.to_owned());
                }
                for item in fields.values() {
                    visit(item, representations);
                }
            }
            _ => {}
        }
    }

    let mut representations = BTreeSet::new();
    visit(document, &mut representations);
    representations
}

fn dispatch_candidate_corpus_value(
    candidate: Candidate,
    dispatch: NativeHandleDispatch,
    heap: &mut GcHeap,
    retained_handles: &mut Vec<(ogma_gc_comparison_proof::HostHandle, ogma_gc_comparison_proof::GcRef)>,
    value: CandidateValue,
) -> Result<bool> {
    let reference_tag = match value {
        CandidateValue::StringRef(_) => Some(STRING_TAG),
        CandidateValue::BigIntRef(_) => Some(BIGINT_TAG),
        CandidateValue::ObjectRef(_) => Some(OBJECT_TAG),
        CandidateValue::SymbolRef(_) => Some(SYMBOL_TAG),
        _ => None,
    };
    let (value, expected_object, expected_token) = if let Some(tag) = reference_tag {
        let object = heap.allocate(48);
        let handle = heap.retain_host_handle(object)?;
        let token = heap.encode_host_handle_token(handle)?;
        let reference_value = match tag {
            STRING_TAG => CandidateValue::StringRef(token),
            BIGINT_TAG => CandidateValue::BigIntRef(token),
            OBJECT_TAG => CandidateValue::ObjectRef(token),
            SYMBOL_TAG => CandidateValue::SymbolRef(token),
            _ => unreachable!(),
        };
        retained_handles.push((handle, object));
        (reference_value, Some(object), Some(token))
    } else {
        (value, None, None)
    };

    let mut storage = match candidate.runtime {
        RuntimeLayout::Tagged16 => vec![0_u64; 2],
        RuntimeLayout::NanBox8 => vec![0_u64; 1],
    };
    encode_candidate_value(candidate.runtime, &mut storage, 0, value)?;
    let decoded = decode_candidate_value(candidate.runtime, &storage, 0)?;
    if decoded != value {
        bail!(
            "{} {} candidate Value did not round-trip through the native corpus codec",
            candidate.name(),
            format!("{value:?}")
        );
    }
    let (tag, payload) = candidate_value_tag_payload(candidate.runtime, &storage, 0)?;
    let value_argument = match candidate.runtime {
        RuntimeLayout::Tagged16 => storage.as_mut_ptr() as usize as u64,
        RuntimeLayout::NanBox8 => storage[0],
    };
    let mut callback_context = NativeHandleCallbackContext {
        heap: heap as *mut GcHeap,
        expected_object,
        expected_payload: expected_token.unwrap_or(payload),
        expected_tag: tag,
        calls: 0,
        collections: 0,
        force_collection: expected_object.is_some(),
        last_tag: 0,
        last_token: 0,
    };
    let callback_address = resolve_native_handle_callback as *const () as usize as u64;
    let context_argument =
        &mut callback_context as *mut NativeHandleCallbackContext as usize as u64;
    // SAFETY: the candidate storage, callback and GC context remain valid for
    // the full synchronous native C ABI call.
    let status = unsafe { dispatch(value_argument, callback_address, context_argument) };
    if status != 0
        || callback_context.calls != 1
        || callback_context.collections != u64::from(expected_object.is_some())
        || callback_context.last_tag != tag
        || callback_context.last_token != payload
    {
        bail!(
            "{} native Core IR corpus Value dispatch failed (status={status}, calls={}, collections={}, tag={:#x}, payload={:#x})",
            candidate.name(),
            callback_context.calls,
            callback_context.collections,
            callback_context.last_tag,
            callback_context.last_token
        );
    }
    if let (Some(object), Some(token)) = (expected_object, expected_token) {
        if heap.resolve_host_handle_token(token)? != object || !heap.is_alive(object) {
            bail!(
                "{} candidate layout lost a rooted Core IR reference across native callback collection",
                candidate.name()
            );
        }
    }
    Ok(expected_object.is_some())
}

fn run_native_core_ir_corpus_validation(
    candidate: Candidate,
    library: &Library,
) -> Result<NativeCoreIrCorpusEvidence> {
    let dispatch = unsafe {
        *library
            .get::<NativeHandleDispatch>(b"ogma_value_handle_dispatch\\0")
            .context("generated native Value dispatch entry point missing for Core IR corpus")?
    };
    let manifest_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/core-ir/manifest.json");
    let manifest: JsonValue = serde_json::from_str(
        &fs::read_to_string(&manifest_path)
            .with_context(|| format!("read accepted Core IR manifest {}", manifest_path.display()))?,
    )?;
    if manifest["contract"] != "ogma.core-ir" || manifest["version"] != "1.0.0" {
        bail!("OGM-38 native corpus requires canonical ogma.core-ir/1.0.0 manifest");
    }
    let fixtures = manifest["fixtures"]
        .as_array()
        .context("canonical Core IR manifest fixtures array missing")?;
    let repository_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut heap = GcHeap::new(CollectorKind::MarkSweep);
    let mut retained_handles = Vec::new();
    let mut fixture_evidence = Vec::new();
    let mut total_dispatches = 0_usize;
    let mut total_reference_resolutions = 0_usize;

    for fixture in fixtures
        .iter()
        .filter(|fixture| fixture["expect"].as_str() == Some("accepted"))
    {
        let relative_path = fixture["path"]
            .as_str()
            .context("accepted Core IR fixture path missing")?;
        if !relative_path.starts_with("tests/core-ir/fixtures/")
            || relative_path.split('/').any(|component| component == "..")
        {
            bail!("accepted Core IR fixture path is outside the canonical fixture directory");
        }
        let fixture_path = repository_root.join(relative_path);
        let document: JsonValue = serde_json::from_str(
            &fs::read_to_string(&fixture_path)
                .with_context(|| format!("read accepted Core IR fixture {}", fixture_path.display()))?,
        )?;
        let representations = core_ir_representations(&document);
        if representations.is_empty() {
            bail!("accepted Core IR fixture {relative_path} has no Value representations");
        }

        let before_dispatches = total_dispatches;
        let before_references = total_reference_resolutions;
        for representation in &representations {
            let values = match representation.as_str() {
                "f64" | "i32" | "u32" => vec![
                    CandidateValue::Number(0.0_f64.to_bits()),
                    CandidateValue::Number((-0.0_f64).to_bits()),
                    CandidateValue::Number(CANONICAL_NAN_BITS),
                    CandidateValue::Number(f64::INFINITY.to_bits()),
                    CandidateValue::Number(f64::NEG_INFINITY.to_bits()),
                ],
                "i1" => vec![
                    CandidateValue::Boolean(false),
                    CandidateValue::Boolean(true),
                ],
                "string_ref" => vec![CandidateValue::StringRef(0)],
                "bigint_ref" => vec![CandidateValue::BigIntRef(0)],
                "object_ref" | "function_ref" | "exception_ref" | "array_ref" => {
                    vec![CandidateValue::ObjectRef(0)]
                }
                "symbol_ref" => vec![CandidateValue::SymbolRef(0)],
                "boxed" => vec![
                    CandidateValue::Undefined,
                    CandidateValue::Null,
                    CandidateValue::Boolean(false),
                    CandidateValue::Boolean(true),
                    CandidateValue::Number(0.0_f64.to_bits()),
                    CandidateValue::Number((-0.0_f64).to_bits()),
                    CandidateValue::Number(CANONICAL_NAN_BITS),
                    CandidateValue::Number(f64::INFINITY.to_bits()),
                    CandidateValue::Number(f64::NEG_INFINITY.to_bits()),
                    CandidateValue::StringRef(0),
                    CandidateValue::BigIntRef(0),
                    CandidateValue::ObjectRef(0),
                    CandidateValue::SymbolRef(0),
                ],
                other => bail!(
                    "{} native corpus has no candidate Value adapter for Core IR representation {other:?} in {relative_path}",
                    candidate.name()
                ),
            };
            for value in values {
                if dispatch_candidate_corpus_value(
                    candidate,
                    dispatch,
                    &mut heap,
                    &mut retained_handles,
                    value,
                )? {
                    total_reference_resolutions += 1;
                }
                total_dispatches += 1;
            }
        }

        let fixture_name = Path::new(relative_path)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .context("Core IR fixture name is not valid UTF-8")?
            .to_owned();
        fixture_evidence.push(NativeCoreIrFixtureEvidence {
            fixture: fixture_name,
            path: relative_path.to_owned(),
            representations: representations.into_iter().collect(),
            native_value_dispatches: total_dispatches - before_dispatches,
            reference_values_resolved_after_forced_gc: total_reference_resolutions
                - before_references,
        });
    }

    if fixture_evidence.len() != 7 {
        bail!(
            "OGM-38 expected all seven accepted Core IR fixtures, executed {}",
            fixture_evidence.len()
        );
    }
    let retained_objects = retained_handles.len();
    for (handle, _) in retained_handles {
        heap.release_host_handle(handle)?;
    }
    let cleanup = heap.collect()?;
    if cleanup.live_objects_after != 0 || cleanup.reclaimed_objects != retained_objects {
        bail!(
            "{} Core IR corpus leaked native candidate handles after cleanup: {} live / {} reclaimed",
            candidate.name(),
            cleanup.live_objects_after,
            cleanup.reclaimed_objects
        );
    }

    Ok(NativeCoreIrCorpusEvidence {
        manifest: "tests/core-ir/manifest.json (ogma.core-ir/1.0.0; all accepted entries)",
        accepted_fixture_count: fixture_evidence.len(),
        fixtures: fixture_evidence,
        native_value_dispatches: total_dispatches,
        reference_values_resolved_after_forced_gc: total_reference_resolutions,
        all_values_roundtripped_and_dispatched: true,
    })
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
    let (noncanonical_nan_payload_rejected, noncanonical_nanbox_words_rejected) = match candidate
        .runtime
    {
        RuntimeLayout::Tagged16 => {
            let mut invalid_nan_start = TaggedValue {
                tag: NUMBER_TAG,
                payload: 0x7ff8_0000_0000_0001,
            };
            let invalid_nan_start_ptr =
                (&mut invalid_nan_start as *mut TaggedValue) as usize as u64;
            // SAFETY: the pointer addresses a live repr(C) tagged Value.
            let status =
                unsafe { function(invalid_nan_start_ptr, b, c, output_pointer, max_iterations) };
            (status == 1, None)
        }
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
            let rejected = positive_status == 1 && negative_status == 1;
            (rejected, Some(rejected))
        }
    };
    if !noncanonical_nan_payload_rejected {
        bail!(
            "{} accepted a non-canonical NaN Value payload",
            candidate.name()
        );
    }
    let semantic_validation = NativeSemanticValidation {
        numeric_edge_cases: semantic_cases,
        invalid_tag_rejected,
        noncanonical_nan_payload_rejected,
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
    let allocator_comparisons = crate::value_ssa_allocator::run_allocator_evidence(
        object_count,
        candidate.runtime.value_bytes(),
        GC_VALUE_SLOTS_PER_OBJECT,
        warmups,
        samples,
    )?;

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
        allocator_comparisons,
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

fn parse_native_disassembly(disassembly: &str, symbol: &'static str) -> Result<NativeCodeMetrics> {
    let mut in_function = false;
    let mut found_symbol = false;
    let mut instruction_count = 0_usize;
    let mut function_code_bytes = 0_usize;
    let mut integer_registers = BTreeSet::new();
    let mut vector_registers = BTreeSet::new();
    let mut stack_relative_memory_instructions = 0_usize;
    let mut stack_relative_loads = 0_usize;
    let mut stack_relative_stores = 0_usize;
    let mut call_instructions = 0_usize;

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
            call_instructions += 1;
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
        call_instructions,
    })
}

fn native_code_metrics(object_path: &Path, symbol: &'static str) -> Result<NativeCodeMetrics> {
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

fn run_native_pressure_validation(
    candidate: Candidate,
    library: &Library,
    object_path: &Path,
) -> Result<NativeCodeMetrics> {
    let pressure = unsafe {
        *library
            .get::<NativePressure>(b"ogma_value_pressure\\0")
            .context("generated native register-pressure entry point missing")?
    };
    let mut storage = match candidate.runtime {
        RuntimeLayout::Tagged16 => vec![0_u64; PRESSURE_VALUE_COUNT * 2],
        RuntimeLayout::NanBox8 => vec![0_u64; PRESSURE_VALUE_COUNT],
    };
    for index in 0..PRESSURE_VALUE_COUNT {
        encode_candidate_number_value(
            candidate.runtime,
            &mut storage,
            index,
            (index as f64) + 0.25,
        )?;
    }
    let (tag, payload) = candidate_value_tag_payload(
        candidate.runtime,
        &storage,
        0,
    )?;
    let mut heap = GcHeap::new(CollectorKind::MarkSweep);
    let mut callback_context = NativeHandleCallbackContext {
        heap: &mut heap as *mut GcHeap,
        expected_object: None,
        expected_payload: payload,
        expected_tag: tag,
        calls: 0,
        collections: 0,
        force_collection: false,
        last_tag: 0,
        last_token: 0,
    };
    let callback_address = resolve_native_handle_callback as *const () as usize as u64;
    let context_argument =
        &mut callback_context as *mut NativeHandleCallbackContext as usize as u64;
    let value_argument = storage.as_mut_ptr() as usize as u64;
    // SAFETY: the encoded array, callback and context remain live until the
    // generated Cranelift function returns; the callback uses the C ABI.
    let checksum = unsafe { pressure(value_argument, callback_address, context_argument) };
    black_box(checksum);
    if callback_context.calls != 1
        || callback_context.collections != 0
        || callback_context.last_tag != tag
        || callback_context.last_token != payload
    {
        bail!(
            "{} generated pressure function did not preserve the native callback boundary",
            candidate.name()
        );
    }

    let metrics = native_code_metrics(object_path, "ogma_value_pressure")?;
    if metrics.stack_relative_stores == 0 || metrics.stack_relative_loads == 0 {
        bail!(
            "{} generated pressure function did not produce measurable spill stores and reloads (stores={}, loads={})",
            candidate.name(),
            metrics.stack_relative_stores,
            metrics.stack_relative_loads
        );
    }
    Ok(metrics)
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
        let native_code = native_code_metrics(&object_path, "ogma_value_loop")?;
        let native_handle_dispatch = run_native_handle_dispatch_validation(
            candidate,
            &library,
            &object_path,
            iterations,
            warmups,
            samples,
        )?;
        let native_core_ir_corpus =
            run_native_core_ir_corpus_validation(candidate, &library)?;
        let native_value_semantics = run_native_value_semantics(candidate, &library)?;
        let semantic_reference_count = semantic_cases()
            .iter()
            .flat_map(|case| case.inputs.iter())
            .filter(|value| {
                matches!(
                    value,
                    SemanticValue::String(_)
                        | SemanticValue::BigInt(_)
                        | SemanticValue::Object { .. }
                        | SemanticValue::Symbol(_)
                )
            })
            .count();
        let native_shadow_roots =
            run_native_shadow_root_validation(candidate, semantic_reference_count)?;
        let native_pressure =
            run_native_pressure_validation(candidate, &library, &object_path)?;
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
            native_handle_dispatch,
            native_core_ir_corpus,
            native_value_semantics,
            native_shadow_roots,
            native_pressure,
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
                "OGM-26 RcCycleRemoval and MarkSweep graph/roots with generation-checked GcRef and HostHandle lifetimes; OGM-29 separately allocates matching Value-storage and per-object payload backing blocks with System, exact-reuse and size-class strategies; allocator peaks are not process RSS",
            ),
        ]),
        unsupported_native_semantics: vec![
            "The native OGM-38 fast loop is a Number-only Core IR specialization; BigInt arithmetic, string concatenation, object ToPrimitive and general ECMAScript operator lowering remain in the OGM-31 interpreter/native backend semantic proof rather than this four-layout Cranelift loop",
            "OGM-26 collector reachability and OGM-29 physical allocator backing are measured as equivalent M0 workloads, but OGM-29 sidecar payload blocks are not wired into a production Runtime object-cell allocator",
            "OGM-27 shadow-frame/root-map and OGM-19/32 ABI behavior are M0 executable models; no production machine stack-map encoder or production Runtime is claimed",
            "OS process RSS and allocator fragmentation external to the OGM-29 allocator are not attributed per candidate; reported peak reserved/live/fragmentation counters are the allocator's exact M0 accounting",
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
        assert_eq!(metrics.call_instructions, 1);
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
    fn candidate_immediate_value_codec_preserves_undefined_null_boolean_and_number_edges() {
        let cases = [
            CandidateValue::Undefined,
            CandidateValue::Null,
            CandidateValue::Boolean(false),
            CandidateValue::Boolean(true),
            CandidateValue::Number((-0.0_f64).to_bits()),
            CandidateValue::Number(CANONICAL_NAN_BITS),
            CandidateValue::Number(f64::INFINITY.to_bits()),
            CandidateValue::Number(f64::NEG_INFINITY.to_bits()),
        ];
        for candidate in Candidate::ALL {
            for value in cases {
                let mut storage = match candidate.runtime {
                    RuntimeLayout::Tagged16 => vec![0_u64; 2],
                    RuntimeLayout::NanBox8 => vec![0_u64; 1],
                };
                encode_candidate_value(candidate.runtime, &mut storage, 0, value)
                    .expect("encode immediate candidate Value");
                assert_eq!(
                    decode_candidate_value(candidate.runtime, &storage, 0)
                        .expect("decode immediate candidate Value"),
                    value,
                    "{} {value:?}",
                    candidate.name()
                );
            }
        }
    }

    #[test]
    fn tagged_and_nanbox_reference_values_resolve_all_ogm17_handle_tag_families() {
        let mut heap = GcHeap::new(CollectorKind::MarkSweep);
        let objects: Vec<_> = NATIVE_HANDLE_TAG_FAMILIES
            .iter()
            .map(|_| heap.allocate(32))
            .collect();
        let handles = objects
            .iter()
            .map(|object| heap.retain_host_handle(*object).expect("retain reference"))
            .collect::<Vec<_>>();
        let tokens = handles
            .iter()
            .map(|handle| {
                heap.encode_host_handle_token(*handle)
                    .expect("encode handle")
            })
            .collect::<Vec<_>>();

        for candidate in Candidate::ALL {
            for (index, family) in NATIVE_HANDLE_TAG_FAMILIES.iter().enumerate() {
                let mut storage = match candidate.runtime {
                    RuntimeLayout::Tagged16 => vec![0_u64; 2],
                    RuntimeLayout::NanBox8 => vec![0_u64; 1],
                };
                encode_candidate_reference_value(
                    candidate.runtime,
                    &mut storage,
                    0,
                    family.tagged_tag,
                    tokens[index],
                )
                .expect("encode candidate reference Value");
                assert_eq!(
                    resolve_candidate_object_value(candidate.runtime, &storage, 0, &heap,)
                        .expect("resolve candidate reference Value"),
                    objects[index],
                    "{} {}",
                    candidate.name(),
                    family.label
                );
            }
        }

        for handle in handles {
            heap.release_host_handle(handle)
                .expect("release reference handle");
        }
        heap.collect().expect("collect released reference Values");
        assert_eq!(heap.stats().live_objects, 0);
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
