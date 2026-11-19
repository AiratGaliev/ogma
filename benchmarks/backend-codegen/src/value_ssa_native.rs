//! OGM-38 M0 native Value/SSA evidence.
//!
//! The generated function is a numeric lowering of the canonical
//! positive-loop-carried-values Core IR fixture. It is deliberately not a
//! production ECMAScript Runtime or a general Core IR compiler.
use anyhow::{Context, Result, anyhow, bail};
use cranelift_codegen::{
    ir::{AbiParam, BlockArg, InstBuilder, MemFlags, types},
    settings::{self, Configurable},
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{Linkage, Module, default_libcall_names};
use cranelift_object::{ObjectBuilder, ObjectModule};
use libloading::Library;
use ogma_gc_comparison_proof::{CollectorKind, FrameKind, GcHeap};
use regalloc2::{
    Algorithm, Block, Function as RegallocFunction, Inst, InstRange, MachineEnv, Operand, PReg,
    PRegSet, RegClass, RegallocOptions, VReg, run as run_regalloc,
};
use serde::Serialize;
use serde_json::Value as JsonValue;
use std::{
    collections::BTreeMap,
    env, fs,
    hint::black_box,
    mem::size_of,
    path::{Path, PathBuf},
    process::Command,
    time::Instant,
};

const NUMBER_TAG: u64 = 3;
const OBJECT_TAG: u64 = 6;
const NAN_TAG_SHIFT: u32 = 48;
const MAX_PAYLOAD: u64 = (1_u64 << NAN_TAG_SHIFT) - 1;
const DEFAULT_MAX_ITERATIONS: u64 = 1_000_000;

type NativeLoop = unsafe extern "C" fn(u64, u64, u64, u64, u64) -> i32;

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
    stack_allocations: usize,
    spill_slots: usize,
    spill_bytes: usize,
}

#[derive(Clone, Debug, Serialize)]
struct GcEvidence {
    collector: &'static str,
    object_count_before_collection: u64,
    logical_heap_bytes_before_collection: u64,
    root_frame_roots: usize,
    persistent_handles: usize,
    rooted_objects_survived_collection: bool,
    reclaimed_after_root_release: usize,
    live_objects_after_release: u64,
    peak_logical_heap_bytes: u64,
    value_layout_bytes: usize,
    handle_payload_bits: u32,
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
            let start_tag = b.ins().load(types::I64, MemFlags::from_u32(0), parameters[0], 0);
            let limit_tag = b.ins().load(types::I64, MemFlags::from_u32(0), parameters[1], 0);
            let step_tag = b.ins().load(types::I64, MemFlags::from_u32(0), parameters[2], 0);
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
            b.ins().jump(initial_state, &[]);
        }

        b.switch_to_block(initial_state);
        let initial_values = match (candidate.runtime, candidate.ssa) {
            (RuntimeLayout::Tagged16, SsaPolicy::Boxed) => {
                let start_tag = b.ins().load(types::I64, MemFlags::from_u32(0), parameters[0], 0);
                let start_payload = b.ins().load(types::I64, MemFlags::from_u32(0), parameters[0], 8);
                let limit_tag = b.ins().load(types::I64, MemFlags::from_u32(0), parameters[1], 0);
                let limit_payload = b.ins().load(types::I64, MemFlags::from_u32(0), parameters[1], 8);
                let step_tag = b.ins().load(types::I64, MemFlags::from_u32(0), parameters[2], 0);
                let step_payload = b.ins().load(types::I64, MemFlags::from_u32(0), parameters[2], 8);
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
                let start_bits = b.ins().load(types::I64, MemFlags::from_u32(0), parameters[0], 8);
                let limit_bits = b.ins().load(types::I64, MemFlags::from_u32(0), parameters[1], 8);
                let step_bits = b.ins().load(types::I64, MemFlags::from_u32(0), parameters[2], 8);
                let start = b.ins().bitcast(types::F64, MemFlags::from_u32(0), start_bits);
                let limit = b.ins().bitcast(types::F64, MemFlags::from_u32(0), limit_bits);
                let step = b.ins().bitcast(types::F64, MemFlags::from_u32(0), step_bits);
                let zero = b.ins().iconst(types::I64, 0);
                vec![start, limit, step, zero]
            }
            (RuntimeLayout::NanBox8, SsaPolicy::Boxed) => {
                let zero = b.ins().iconst(types::I64, 0);
                vec![parameters[0], parameters[1], parameters[2], zero]
            }
            (RuntimeLayout::NanBox8, SsaPolicy::RepresentationAware) => {
                let start = b.ins().bitcast(types::F64, MemFlags::from_u32(0), parameters[0]);
                let limit = b.ins().bitcast(types::F64, MemFlags::from_u32(0), parameters[1]);
                let step = b.ins().bitcast(types::F64, MemFlags::from_u32(0), parameters[2]);
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
        let (current, limit, step, count) = match (candidate.runtime, candidate.ssa) {
            (RuntimeLayout::Tagged16, SsaPolicy::Boxed) => {
                let current_bits = b.ins().bitcast(types::F64, MemFlags::from_u32(0), loop_state[1]);
                let limit_bits = b.ins().bitcast(types::F64, MemFlags::from_u32(0), loop_state[3]);
                (current_bits, limit_bits, loop_state[5], loop_state[6])
            }
            (RuntimeLayout::NanBox8, SsaPolicy::Boxed) => {
                let current_bits = b.ins().bitcast(types::F64, MemFlags::from_u32(0), loop_state[0]);
                let limit_bits = b.ins().bitcast(types::F64, MemFlags::from_u32(0), loop_state[1]);
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
                b.ins().bitcast(types::F64, MemFlags::from_u32(0), add_state[1]),
                b.ins().bitcast(types::F64, MemFlags::from_u32(0), add_state[5]),
                add_state[6],
            ),
            (RuntimeLayout::NanBox8, SsaPolicy::Boxed) => (
                b.ins().bitcast(types::F64, MemFlags::from_u32(0), add_state[0]),
                b.ins().bitcast(types::F64, MemFlags::from_u32(0), add_state[2]),
                add_state[3],
            ),
            (_, SsaPolicy::RepresentationAware) => (add_state[0], add_state[2], add_state[3]),
        };
        let sum = b.ins().fadd(add_current, add_step);
        let next_bits = b.ins().bitcast(types::I64, MemFlags::from_u32(0), sum);
        let next_count = b.ins().iadd_imm(add_count, 1);
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
                    .store(MemFlags::from_u32(0), done_state[0], parameters[3], 0);
                b.ins()
                    .store(MemFlags::from_u32(0), done_state[1], parameters[3], 8);
            }
            (RuntimeLayout::Tagged16, SsaPolicy::RepresentationAware) => {
                let tag = b.ins().iconst(types::I64, NUMBER_TAG as i64);
                let bits = b.ins().bitcast(types::I64, MemFlags::from_u32(0), done_state[0]);
                b.ins().store(MemFlags::from_u32(0), tag, parameters[3], 0);
                b.ins().store(MemFlags::from_u32(0), bits, parameters[3], 8);
            }
            (RuntimeLayout::NanBox8, SsaPolicy::Boxed) => {
                b.ins()
                    .store(MemFlags::from_u32(0), done_state[0], parameters[3], 0);
            }
            (RuntimeLayout::NanBox8, SsaPolicy::RepresentationAware) => {
                let bits = b.ins().bitcast(types::I64, MemFlags::from_u32(0), done_state[0]);
                b.ins().store(MemFlags::from_u32(0), bits, parameters[3], 0);
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
) -> Result<(Distribution, bool)> {
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
        payload: start.to_bits(),
    };
    let mut tagged_limit = TaggedValue {
        tag: NUMBER_TAG,
        payload: limit.to_bits(),
    };
    let mut tagged_step = TaggedValue {
        tag: NUMBER_TAG,
        payload: step.to_bits(),
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
            start.to_bits(),
            limit.to_bits(),
            step.to_bits(),
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
    Ok((distribution(timings), final_result == expected))
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
    fn spillslot_size(&self, _class: RegClass) -> usize {
        1
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
        let op = instruction["op"]
            .as_str()
            .context("instruction op missing")?;
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
        if let Some(result) = instruction["result"].as_object() {
            if let Some(id) = result.get("id").and_then(JsonValue::as_str) {
                let representation = result
                    .get("representation")
                    .and_then(JsonValue::as_str)
                    .context("result representation missing")?;
                if op == "js.strict_equal" {
                    values.insert(
                        id.to_owned(),
                        program.define_value(candidate.runtime, candidate.ssa, representation),
                    );
                } else {
                    values.insert(
                        id.to_owned(),
                        program.define_value(candidate.runtime, candidate.ssa, representation),
                    );
                }
            }
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
    let mut stack_allocations = 0;
    for allocation in &output.allocs {
        if let Some(register) = allocation.as_reg() {
            register_allocations += 1;
            match register.class() {
                RegClass::Int => integer_registers.add(register),
                RegClass::Float => float_registers.add(register),
                RegClass::Vector => {}
            }
        } else if allocation.is_stack() {
            stack_allocations += 1;
        }
    }
    Ok(RegisterPressure {
        allocator: "regalloc2 0.15.3 (same crate version resolved by Cranelift 0.137.2)",
        workload: "positive-call-boundary.json parameter/operand/result dataflow profile",
        machine_env: "controlled 6 GPR + 6 FPR + 6 vector-register budget; not the host ISA allocation map",
        integer_registers_used: integer_registers.len() as usize,
        float_registers_used: float_registers.len() as usize,
        register_allocations,
        stack_allocations,
        spill_slots: output.num_spillslots,
        spill_bytes: output.num_spillslots * 8,
    })
}

fn gc_evidence(candidate: Candidate) -> Result<GcEvidence> {
    let mut heap = GcHeap::new(CollectorKind::MarkSweep);
    let parent = heap.allocate(32);
    let child = heap.allocate(64);
    heap.add_edge(parent, child)?;
    heap.add_edge(child, parent)?;
    let native_frame = heap.begin_frame(FrameKind::NativePrototype)?;
    heap.add_frame_root(native_frame, parent)?;
    let persistent = heap.retain_host_handle(child)?;
    // The NaN-box payload is an opaque token into this runtime-owned registry;
    // the registry stores the real OGM-26 generation-checked HostHandle.
    let opaque_handle_registry = vec![persistent];

    let before = heap.stats();
    let object_token = 1_u64;
    let encoded_handle = match candidate.runtime {
        RuntimeLayout::Tagged16 => TaggedValue {
            tag: OBJECT_TAG,
            payload: object_token,
        },
        RuntimeLayout::NanBox8 => {
            let tagged_word = (0x7ffe_u64 << NAN_TAG_SHIFT) | object_token;
            let decoded_tag = tagged_word >> NAN_TAG_SHIFT;
            if decoded_tag != 0x7ffe {
                bail!("NaN-box object tag was not preserved");
            }
            TaggedValue {
                tag: decoded_tag,
                payload: tagged_word & MAX_PAYLOAD,
            }
        }
    };
    if encoded_handle.payload != object_token {
        bail!("encoded managed handle payload changed");
    }

    let retained = heap.collect()?;
    let resolved_handle = opaque_handle_registry
        .get(usize::try_from(object_token - 1)?)
        .context("NaN-box payload does not resolve to an OGM-26 persistent handle")?;
    let survived = heap.is_alive(parent)
        && heap.is_alive(child)
        && heap.resolve_host_handle(*resolved_handle).is_ok();
    if !survived || retained.reclaimed_objects != 0 {
        bail!("real GC heap reclaimed an object retained by native/persistent roots");
    }

    heap.end_frame(native_frame)?;
    heap.release_host_handle(persistent)?;
    let released = heap.collect()?;
    if heap.is_alive(parent) || heap.is_alive(child) {
        bail!("unrooted cycle survived mark-sweep collection");
    }

    Ok(GcEvidence {
        collector: "OGM-26 GcHeap::MarkSweep",
        object_count_before_collection: before.live_objects,
        logical_heap_bytes_before_collection: before.live_bytes,
        root_frame_roots: 1,
        persistent_handles: 1,
        rooted_objects_survived_collection: survived,
        reclaimed_after_root_release: released.reclaimed_objects,
        live_objects_after_release: heap.stats().live_objects,
        peak_logical_heap_bytes: heap.stats().peak_heap_bytes,
        value_layout_bytes: candidate.runtime.value_bytes(),
        handle_payload_bits: NAN_TAG_SHIFT,
    })
}

pub fn run(
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
        let (throughput_ns_per_call, result_matches) =
            run_native_candidate(candidate, &library, iterations, warmups, samples, expected)?;
        let register_pressure = register_pressure(candidate)?;
        let gc = gc_evidence(candidate)?;
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
                "regalloc2 0.15.3 allocates VRegs mapped from canonical call-boundary Core IR value representations under a fixed register budget; not Cranelift's private allocation dump or host ISA register map",
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
            assert!(evidence.spill_bytes >= evidence.spill_slots * 8);
            reports.insert(candidate.name(), evidence);
        }
        let tagged_boxed = reports
            .get("tagged-16-boxed")
            .expect("tagged boxed candidate");
        let nanbox_boxed = reports
            .get("nanbox-8-boxed")
            .expect("NaN-box boxed candidate");
        assert!(tagged_boxed.spill_slots > nanbox_boxed.spill_slots);
    }

    #[test]
    fn real_gc_heap_preserves_and_then_reclaims_rooted_cycle() {
        for candidate in Candidate::ALL {
            let evidence = gc_evidence(candidate).expect("real OGM-26 heap integration");
            assert!(evidence.rooted_objects_survived_collection);
            assert_eq!(evidence.live_objects_after_release, 0);
            assert!(evidence.reclaimed_after_root_release >= 2);
        }
    }
}
