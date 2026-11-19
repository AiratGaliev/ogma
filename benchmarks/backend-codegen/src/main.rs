use anyhow::{Context as _, Result, anyhow, bail};
use cranelift_codegen::{
    ir::{
        AbiParam, InstBuilder, Signature, UserFuncName, Value,
        condcodes::{FloatCC, IntCC},
        instructions::BlockArg,
        types,
    },
    settings::{self, Configurable},
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{FuncId, Linkage, Module, default_libcall_names};
use cranelift_object::{ObjectBuilder, ObjectModule};
use inkwell::{
    FloatPredicate, IntPredicate, OptimizationLevel,
    context::Context,
    targets::{
        CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine, TargetTriple,
    },
    values::{FloatValue, IntValue},
};
use libloading::Library;
use ogma_native_runtime_boundary_proof as boundary;
use ogma_rust_abi_proof as abi;
use ogma_typed_host_dispatch_proof::HostFunctionRegistry;

mod aot_host_abi;
use aot_host_abi::{
    AOT_ABI_MAJOR, AOT_CAPABILITY_MATH, AOT_ERROR_CANCELLED, AOT_ERROR_CAPABILITY,
    AOT_ERROR_THROWN, AOT_ERROR_TYPE, AotHostFunctionTable, AotHostFunctionTableV1,
};
use serde::Serialize;
use serde_json::Value as JsonValue;
use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    ffi::{CString, c_void},
    fs,
    hint::black_box,
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    process::Command,
    ptr,
    time::Instant,
};
use target_lexicon::Triple;

type UnaryI64 = unsafe extern "C" fn(i64) -> i64;
type BinaryI64 = unsafe extern "C" fn(i64, i64) -> i64;
type BinaryF64 = unsafe extern "C" fn(f64, f64) -> f64;
type TernaryF64 = unsafe extern "C" fn(f64, f64, f64) -> f64;
type AddNumber = unsafe extern "C" fn(f64, f64) -> i64;
type ToNumber = unsafe extern "C" fn(i64) -> f64;
type CallBoundary = unsafe extern "C" fn(i64, i64, i64, i64, i64, f64) -> i64;
type CallTarget = unsafe extern "C" fn(i64, i64) -> i64;
type AotDispatchF64 = unsafe extern "C" fn(i64, i32, f64, f64, u64) -> i64;
type AotDispatchHandles = unsafe extern "C" fn(i64, i32, i64, i64, u64) -> i64;
type AotMetric = unsafe extern "C" fn() -> u64;
type AotDetails = unsafe extern "C" fn() -> u64;
type AotNumber = unsafe extern "C" fn() -> f64;

type ProofSafepointCallback = unsafe extern "C" fn(u32, *mut c_void) -> i32;
type BindProofAbi = unsafe extern "C" fn(
    *const OgmaProofAbi,
    *const abi::OgmaRuntime,
    abi::OgmaContextHandle,
    abi::OgmaHandle,
    abi::OgmaHandle,
    ProofSafepointCallback,
    *mut c_void,
);
type ProofCounter = unsafe extern "C" fn(u32) -> u64;
type ProofStatus = unsafe extern "C" fn() -> i32;
type ProofStatusU32 = unsafe extern "C" fn() -> u32;
type ProofCompletionKind = unsafe extern "C" fn() -> u32;
type ProofCompletionPayload = unsafe extern "C" fn() -> i64;

#[repr(C)]
struct OgmaProofAbi {
    version: abi::OgmaAbiVersion,
    context_create_number: unsafe extern "C" fn(
        *const abi::OgmaRuntime,
        abi::OgmaContextHandle,
        f64,
        *mut abi::OgmaHandle,
    ) -> abi::OgmaStatus,
    context_get_number: unsafe extern "C" fn(
        *const abi::OgmaRuntime,
        abi::OgmaContextHandle,
        abi::OgmaHandle,
        *mut f64,
    ) -> abi::OgmaStatus,
    root_scope_open: unsafe extern "C" fn(
        *const abi::OgmaRuntime,
        abi::OgmaContextHandle,
        *mut abi::OgmaRootScopeHandle,
    ) -> abi::OgmaStatus,
    root_scope_add: unsafe extern "C" fn(
        *const abi::OgmaRuntime,
        abi::OgmaContextHandle,
        abi::OgmaRootScopeHandle,
        abi::OgmaHandle,
    ) -> abi::OgmaStatus,
    root_scope_close: unsafe extern "C" fn(
        *const abi::OgmaRuntime,
        abi::OgmaContextHandle,
        abi::OgmaRootScopeHandle,
    ) -> abi::OgmaStatus,
    context_call: unsafe extern "C" fn(
        *const abi::OgmaRuntime,
        abi::OgmaContextHandle,
        abi::OgmaHandle,
        *const abi::OgmaHandle,
        usize,
        *mut abi::OgmaCompletion,
    ) -> abi::OgmaStatus,
}

struct NativeProofState {
    heap: boundary::GcHeap,
    roots: boundary::RootProviders,
    native_frame_token: boundary::NativeFrameToken,
    sentinel: boundary::GcRef,
    budget: boundary::BudgetMeter,
    safepoint_counts: BTreeMap<u32, u64>,
    callback_count: u64,
    callback_trace: Vec<JsonValue>,
    normal_completion_count: u64,
    throw_completion_count: u64,
    argument_roots_survived_collection: bool,
    transition_paths: Vec<Vec<&'static str>>,
    failure: Option<String>,
    aot_registry: *const HostFunctionRegistry,
    aot_roots: Vec<(abi::OgmaHandle, f64)>,
    aot_root_checks_enabled: bool,
    aot_root_checks: u64,
}

impl NativeProofState {
    fn new() -> Result<Box<Self>> {
        let mut heap = boundary::GcHeap::default();
        let sentinel = heap.allocate();
        let maps = (1_u32..=5)
            .map(|site| boundary::RootMap {
                safepoint: boundary::SafepointId(site),
                slots: vec![0],
            })
            .collect();
        let descriptor = boundary::FrameDescriptor::new(
            "ogm31-native-aot-frame",
            1,
            boundary::SafepointId(1),
            maps,
        )
        .map_err(|error| anyhow!("create OGM-27 shadow-frame descriptor: {error:?}"))?;
        let frame =
            boundary::NativeFrame::new(descriptor, vec![boundary::JsValue::Managed(sentinel)])
                .map_err(|error| anyhow!("create OGM-27 native frame: {error:?}"))?;
        let mut roots = boundary::RootProviders::default();
        let native_frame_token = roots
            .native
            .enter(frame)
            .map_err(|error| anyhow!("push OGM-27 native shadow frame: {error:?}"))?;
        let budget = boundary::BudgetMeter::new(10_000_000, 64)
            .map_err(|error| anyhow!("create OGM-27 execution budget: {error:?}"))?;
        Ok(Box::new(Self {
            heap,
            roots,
            native_frame_token,
            sentinel,
            budget,
            safepoint_counts: BTreeMap::new(),
            callback_count: 0,
            callback_trace: Vec::new(),
            normal_completion_count: 0,
            throw_completion_count: 0,
            argument_roots_survived_collection: false,
            transition_paths: Vec::new(),
            failure: None,
            aot_registry: ptr::null(),
            aot_roots: Vec::new(),
            aot_root_checks_enabled: false,
            aot_root_checks: 0,
        }))
    }

    fn bind_aot_roots(
        &mut self,
        registry: &HostFunctionRegistry,
        roots: Vec<(abi::OgmaHandle, f64)>,
    ) {
        self.aot_registry = registry as *const HostFunctionRegistry;
        self.aot_roots = roots;
    }

    fn set_aot_root_checks_enabled(&mut self, enabled: bool) {
        self.aot_root_checks_enabled = enabled;
    }

    fn on_safepoint(&mut self, site: u32) -> Result<()> {
        let point = boundary::SafepointId(site);
        self.roots
            .native
            .current_mut(self.native_frame_token)
            .map_err(|error| anyhow!("OGM-27 native frame is not current: {error:?}"))?
            .set_safepoint(point, &[0])
            .map_err(|error| anyhow!("OGM-27 root map mismatch at site {site}: {error:?}"))?;
        self.budget
            .record_work(1)
            .map_err(|error| anyhow!("OGM-27 work accounting failed: {error:?}"))?;
        let poll_site = match site {
            1 => boundary::PollSite::FunctionEntry,
            2 => boundary::PollSite::LoopBackEdge,
            3 => boundary::PollSite::AllocationBoundary,
            4 | 5 => boundary::PollSite::CallBoundary,
            _ => bail!("unknown OGM-27 native safepoint site {site}"),
        };
        self.budget
            .poll(poll_site)
            .map_err(|error| anyhow!("OGM-27 budget/cancellation poll failed: {error:?}"))?;
        self.roots
            .collect(&mut self.heap)
            .map_err(|error| anyhow!("OGM-27 root collection failed: {error:?}"))?;
        if !self.heap.is_alive(self.sentinel) {
            bail!("OGM-27 live native-frame root was reclaimed at safepoint {site}");
        }
        if self.aot_root_checks_enabled && !self.aot_registry.is_null() {
            let registry = unsafe { &*self.aot_registry };
            registry.collect_unrooted().map_err(|error| {
                anyhow!("OGM-32 host handle collection at safepoint {site}: {error:?}")
            })?;
            for (handle, expected) in &self.aot_roots {
                let actual = registry.read_number(*handle).map_err(|error| {
                    anyhow!("OGM-32 host root was invalid at safepoint {site}: {error:?}")
                })?;
                if actual.to_bits() != expected.to_bits() {
                    bail!("OGM-32 opaque host handle changed across safepoint {site}");
                }
            }
            self.aot_root_checks = self.aot_root_checks.saturating_add(1);
        }
        *self.safepoint_counts.entry(site).or_default() += 1;
        Ok(())
    }

    fn exercise_transition_model(&mut self, is_throw: bool) -> Result<()> {
        let completion_value = self.heap.allocate();
        let managed = boundary::JsValue::Managed(completion_value);
        let expected = if is_throw {
            boundary::Completion::Throw(managed)
        } else {
            boundary::Completion::Normal(managed)
        };
        let mut activations = boundary::ActivationStack::default();
        let native = boundary::Activation::new(
            boundary::ActivationKind::Native,
            "ogma_host_call_aot",
            None,
            boundary::SourceLocation {
                file: "ogm31.native.js".into(),
                line: 1,
                column: 0,
            },
        );
        let interpreter = boundary::Activation::new(
            boundary::ActivationKind::Interpreter,
            "ogma_abi_callback_dispatch",
            None,
            boundary::SourceLocation {
                file: "ogm31.host.js".into(),
                line: 2,
                column: 0,
            },
        );
        let host = boundary::Activation::new(
            boundary::ActivationKind::HostCallback,
            "ogm31_registered_rust_callback",
            None,
            boundary::SourceLocation {
                file: "ogm31.host.js".into(),
                line: 3,
                column: 0,
            },
        );

        let returned = activations
            .invoke(native, |stack| {
                let interpreter_result = stack
                    .invoke(interpreter, |stack| {
                        let callback_result = stack
                            .invoke(host, |stack| {
                                if self.transition_paths.is_empty() {
                                    self.transition_paths.push(
                                        stack
                                            .snapshot()
                                            .iter()
                                            .map(|frame| match frame.kind {
                                                boundary::ActivationKind::Native => "Native",
                                                boundary::ActivationKind::Interpreter => {
                                                    "Interpreter"
                                                }
                                                boundary::ActivationKind::HostCallback => {
                                                    "HostCallback"
                                                }
                                            })
                                            .collect(),
                                    );
                                }
                                expected
                            })
                            .unwrap_or_else(|error| {
                                self.failure =
                                    Some(format!("OGM-27 host activation failed: {error:?}"));
                                expected
                            });
                        if let Err(error) = stack.consume_pending_completion(callback_result) {
                            self.failure =
                                Some(format!("OGM-27 callback completion transfer: {error:?}"));
                        }
                        if let Err(error) = self.roots.collect_with_execution(&mut self.heap, stack)
                        {
                            self.failure =
                                Some(format!("OGM-27 interpreter completion root: {error:?}"));
                        }
                        if let Err(error) = stack.release_consumed_completion(callback_result) {
                            self.failure =
                                Some(format!("OGM-27 callback completion release: {error:?}"));
                        }
                        callback_result
                    })
                    .unwrap_or_else(|error| {
                        self.failure =
                            Some(format!("OGM-27 interpreter activation failed: {error:?}"));
                        expected
                    });
                if let Err(error) = stack.consume_pending_completion(interpreter_result) {
                    self.failure = Some(format!(
                        "OGM-27 native/interpreter completion transfer: {error:?}"
                    ));
                }
                if let Err(error) = self.roots.collect_with_execution(&mut self.heap, stack) {
                    self.failure = Some(format!("OGM-27 native completion root: {error:?}"));
                }
                if let Err(error) = stack.release_consumed_completion(interpreter_result) {
                    self.failure = Some(format!("OGM-27 native completion release: {error:?}"));
                }
                interpreter_result
            })
            .map_err(|error| anyhow!("OGM-27 activation transition failed: {error:?}"))?;
        if let Some(error) = self.failure.take() {
            bail!("{error}");
        }
        if returned != expected {
            bail!("OGM-27 native/interpreter/host transition changed completion semantics");
        }
        let host_completion = activations
            .host_completion()
            .ok_or_else(|| anyhow!("OGM-27 top-level completion was not retained"))?;
        self.roots
            .collect_with_execution(&mut self.heap, &activations)
            .map_err(|error| anyhow!("OGM-27 top-level completion was not rooted: {error:?}"))?;
        if !self.heap.is_alive(completion_value) {
            bail!("OGM-27 top-level completion was reclaimed before persistent promotion");
        }
        let promoted = self
            .roots
            .persistent
            .retain(&self.heap, completion_value)
            .map_err(|error| anyhow!("OGM-27 persistent promotion failed: {error:?}"))?;
        activations
            .release_host_completion_after_promotion(
                host_completion,
                Some(promoted),
                &self.roots.persistent,
            )
            .map_err(|error| anyhow!("OGM-27 completion release-before-promotion: {error:?}"))?;
        self.roots
            .persistent
            .release(promoted)
            .map_err(|error| anyhow!("OGM-27 persistent-root release failed: {error:?}"))?;
        self.roots
            .collect(&mut self.heap)
            .map_err(|error| anyhow!("OGM-27 post-completion collection failed: {error:?}"))?;
        if self.heap.is_alive(completion_value) {
            bail!("OGM-27 completion root remained live after persistent promotion was released");
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<JsonValue> {
        if let Some(error) = self.failure.take() {
            bail!("{error}");
        }
        for site in 1..=5 {
            if self.safepoint_counts.get(&site).copied().unwrap_or(0) == 0 {
                bail!("native AOT code did not publish required safepoint site {site}");
            }
        }
        if !self.argument_roots_survived_collection
            || self.callback_count < 5
            || self.callback_trace.len() != 5
        {
            bail!(
                "OGM-19 callback/root contract and its five semantic trace cases were not exercised by native code"
            );
        }
        if self.transition_paths != vec![vec!["Native", "Interpreter", "HostCallback"]] {
            bail!("OGM-27 native/interpreter/host activation path was not observed exactly");
        }
        if self.normal_completion_count == 0 || self.throw_completion_count == 0 {
            bail!("native AOT did not exercise both explicit OGM-19 completion kinds");
        }
        if self.roots.native.depth() != 1 {
            bail!("OGM-27 native shadow frame was unexpectedly popped before finish");
        }
        if !self.aot_registry.is_null() && self.aot_root_checks == 0 {
            bail!("OGM-32 did not validate persistent opaque handles at native safepoints");
        }
        let root_maps: BTreeMap<String, u64> = self
            .safepoint_counts
            .iter()
            .map(|(site, count)| (site.to_string(), *count))
            .collect();
        let result = serde_json::json!({
            "abi_version": {"major": 1, "minor": 0, "patch": 0},
            "abi_features": ["opaque-handles", "root-scopes", "persistent-handles", "callbacks", "explicit-completions", "mock-proof"],
            "callback_invocations": self.callback_count,
            "callback_trace": self.callback_trace,
            "argument_handles_survived_collection_inside_callback": self.argument_roots_survived_collection,
            "normal_completions": self.normal_completion_count,
            "throw_completions": self.throw_completion_count,
            "completion_transition_paths": self.transition_paths,
            "native_shadow_frame": {
                "active_during_execution": true,
                "root_maps": root_maps,
                "live_managed_root_survived_every_collection": true
            },
            "execution_budget": {
                "work_units": self.budget.spent(),
                "max_unpolled_work": self.budget.max_unpolled_work(),
                "polls": self.budget.poll_trace().len()
            },
            "ogm32_aot_root_checks": self.aot_root_checks
        });
        self.roots
            .native
            .leave(self.native_frame_token)
            .map_err(|error| {
                anyhow!("OGM-27 native frame did not close in LIFO order: {error:?}")
            })?;
        if self.roots.native.depth() != 0 {
            bail!("OGM-27 native frame remained active after execution");
        }
        Ok(result)
    }
}

unsafe extern "C" fn native_safepoint_callback(site: u32, user_data: *mut c_void) -> i32 {
    if user_data.is_null() {
        return -1;
    }
    let result = catch_unwind(AssertUnwindSafe(|| {
        let state = unsafe { &mut *user_data.cast::<NativeProofState>() };
        state.on_safepoint(site)
    }));
    match result {
        Ok(Ok(())) => 0,
        Ok(Err(error)) => {
            let state = unsafe { &mut *user_data.cast::<NativeProofState>() };
            state.failure = Some(error.to_string());
            -1
        }
        Err(_) => {
            let state = unsafe { &mut *user_data.cast::<NativeProofState>() };
            state.failure = Some("panic inside native safepoint callback".into());
            -1
        }
    }
}

unsafe extern "C-unwind" fn dynamic_add_host_callback(
    runtime: *const abi::OgmaRuntime,
    context: abi::OgmaContextHandle,
    args: *const abi::OgmaHandle,
    args_len: usize,
    completion: *mut abi::OgmaCompletion,
    user_data: *mut c_void,
) -> abi::OgmaStatus {
    callback_entry(
        runtime, context, args, args_len, completion, user_data, true,
    )
}

unsafe extern "C-unwind" fn host_echo_callback(
    runtime: *const abi::OgmaRuntime,
    context: abi::OgmaContextHandle,
    args: *const abi::OgmaHandle,
    args_len: usize,
    completion: *mut abi::OgmaCompletion,
    user_data: *mut c_void,
) -> abi::OgmaStatus {
    callback_entry(
        runtime, context, args, args_len, completion, user_data, false,
    )
}

fn callback_entry(
    runtime: *const abi::OgmaRuntime,
    context: abi::OgmaContextHandle,
    args: *const abi::OgmaHandle,
    args_len: usize,
    completion: *mut abi::OgmaCompletion,
    user_data: *mut c_void,
    dynamic_add: bool,
) -> abi::OgmaStatus {
    if runtime.is_null() || args.is_null() || completion.is_null() || user_data.is_null() {
        return abi::OgmaStatus::INVALID_ARGUMENT;
    }
    let result = catch_unwind(AssertUnwindSafe(|| unsafe {
        let state = &mut *user_data.cast::<NativeProofState>();
        let expected_count = if dynamic_add { 2 } else { 1 };
        if args_len != expected_count {
            return abi::OgmaStatus::INVALID_ARGUMENT;
        }
        let arguments = std::slice::from_raw_parts(args, args_len);
        let mut numbers = Vec::with_capacity(args_len);
        for handle in arguments {
            let mut number = 0.0;
            let status = abi::ogma_context_get_number(runtime, context, *handle, &mut number);
            if status != abi::OgmaStatus::OK {
                return status;
            }
            numbers.push(number);
        }
        // Force a mock collection while context_call's argument roots are active.
        let status = abi::ogma_context_collect_unrooted(runtime, context);
        if status != abi::OgmaStatus::OK {
            return status;
        }
        for (handle, expected) in arguments.iter().zip(numbers.iter()) {
            let mut observed = 0.0;
            let status = abi::ogma_context_get_number(runtime, context, *handle, &mut observed);
            if status != abi::OgmaStatus::OK || observed.to_bits() != expected.to_bits() {
                return if status == abi::OgmaStatus::OK {
                    abi::OgmaStatus::INTERNAL_ERROR
                } else {
                    status
                };
            }
        }
        state.argument_roots_survived_collection = true;
        state.callback_count += 1;

        let is_throw = if dynamic_add {
            numbers[0] == i64::MIN as f64
        } else {
            numbers[0] < 0.0
        };
        if let Err(error) = state.exercise_transition_model(is_throw) {
            state.failure = Some(error.to_string());
            return abi::OgmaStatus::INTERNAL_ERROR;
        }

        let (kind, value) = if is_throw {
            state.throw_completion_count += 1;
            (abi::COMPLETION_THROW, arguments[0])
        } else {
            state.normal_completion_count += 1;
            let value = if dynamic_add {
                numbers[0] + numbers[1]
            } else {
                numbers[0] + 11.0
            };
            let mut result_handle = abi::OgmaHandle::default();
            let status =
                abi::ogma_context_create_number(runtime, context, value, &mut result_handle);
            if status != abi::OgmaStatus::OK {
                return status;
            }
            (abi::COMPLETION_NORMAL, result_handle)
        };
        if state.callback_trace.len() < 5 {
            let trace_payload = if is_throw {
                numbers[0]
            } else if dynamic_add {
                numbers[0] + numbers[1]
            } else {
                numbers[0] + 11.0
            };
            state.callback_trace.push(serde_json::json!({
                "symbol": if dynamic_add { "benchmark.core_ir.dynamic_add" } else { "benchmark.host.echo" },
                "args": numbers.clone(),
                "completion": {"kind": kind, "value": trace_payload}
            }));
        }
        completion.write(abi::OgmaCompletion {
            kind,
            reserved: 0,
            value,
        });
        abi::OgmaStatus::OK
    }));
    match result {
        Ok(status) => status,
        Err(_) => abi::OgmaStatus::PANIC,
    }
}

#[derive(Clone)]
struct Options {
    iterations: u64,
    warmups: usize,
    samples: usize,
    revision: String,
    output: PathBuf,
    artifact_dir: Option<PathBuf>,
}
fn options() -> Result<Options> {
    let mut args = env::args().skip(1);
    let mut o = Options {
        iterations: 50_000,
        warmups: 3,
        samples: 101,
        revision: env::var("GITHUB_SHA").unwrap_or_else(|_| "working-tree".into()),
        output: PathBuf::from("ogm30-report.json"),
        artifact_dir: None,
    };
    while let Some(a) = args.next() {
        match a.as_str() {
            "--iterations" => o.iterations = args.next().context("missing iterations")?.parse()?,
            "--warmups" => o.warmups = args.next().context("missing warmups")?.parse()?,
            "--samples" => o.samples = args.next().context("missing samples")?.parse()?,
            "--revision" => o.revision = args.next().context("missing revision")?,
            "--output" => o.output = PathBuf::from(args.next().context("missing output")?),
            "--artifact-dir" => {
                o.artifact_dir = Some(PathBuf::from(
                    args.next().context("missing artifact directory")?,
                ));
            }
            _ => bail!("unknown argument: {a}"),
        }
    }
    if o.iterations == 0 || o.samples < 101 {
        bail!("iterations must be positive and samples >= 101");
    }
    Ok(o)
}

#[derive(Clone)]
struct CoreFixture {
    label: &'static str,
    id: String,
    param_representations: Vec<String>,
    return_representation: String,
    operations: Vec<String>,
    terminators: Vec<String>,
    exception_edge: bool,
    host_symbol: Option<String>,
}
struct CoreCorpus {
    fixtures: Vec<CoreFixture>,
    native_exports: BTreeMap<String, String>,
}
impl CoreCorpus {
    fn get(&self, label: &str) -> Result<&CoreFixture> {
        self.fixtures
            .iter()
            .find(|f| f.label == label)
            .ok_or_else(|| anyhow!("missing Core IR workload {label}"))
    }
    fn symbol(&self, label: &str) -> Result<String> {
        if let Some(fixture) = self.fixtures.iter().find(|f| f.label == label) {
            return Ok(format!("ogma_{}_{}", fixture.label, fixture.id));
        }
        self.native_exports
            .get(label)
            .cloned()
            .ok_or_else(|| anyhow!("missing native export {label}"))
    }
}
fn parse_fixture(label: &'static str, raw: &str) -> Result<CoreFixture> {
    let doc: JsonValue = serde_json::from_str(raw)?;
    if doc["contract"] != "ogma.core-ir" || doc["version"] != "1.0.0" {
        bail!("{label}: not ogma.core-ir/1.0.0");
    }
    let f = &doc["function"];
    let id = f["id"].as_str().context("Core IR id missing")?.to_owned();
    let params = f["parameters"]
        .as_array()
        .context("Core IR parameters missing")?
        .iter()
        .map(|p| p["representation"].as_str().unwrap_or("").to_owned())
        .collect();
    let ret = f["return"]["representation"]
        .as_str()
        .context("Core IR return representation missing")?
        .to_owned();
    let mut operations = Vec::new();
    let mut terminators = Vec::new();
    let mut exception_edge = false;
    let mut host_symbol = None;
    for block in f["blocks"].as_array().context("Core IR blocks missing")? {
        for ins in block["instructions"]
            .as_array()
            .context("instructions missing")?
        {
            let op = ins["op"]
                .as_str()
                .context("instruction op missing")?
                .to_owned();
            if op == "host.call" {
                host_symbol = ins["attributes"]["symbol"].as_str().map(str::to_owned);
            }
            exception_edge |= ins["exception"]["kind"] == "edge";
            operations.push(op);
        }
        terminators.push(
            block["terminator"]["kind"]
                .as_str()
                .context("terminator missing")?
                .to_owned(),
        );
    }
    Ok(CoreFixture {
        label,
        id,
        param_representations: params,
        return_representation: ret,
        operations,
        terminators,
        exception_edge,
        host_symbol,
    })
}
fn function_block<'a>(function: &'a JsonValue, id: &str) -> Result<&'a JsonValue> {
    function["blocks"]
        .as_array()
        .context("Core IR blocks missing")?
        .iter()
        .find(|block| block["id"] == id)
        .ok_or_else(|| anyhow!("Core IR block {id} missing"))
}

fn expect_block_order(function: &JsonValue, expected: &[&str]) -> Result<()> {
    let actual: Vec<&str> = function["blocks"]
        .as_array()
        .context("Core IR blocks missing")?
        .iter()
        .map(|block| block["id"].as_str().unwrap_or(""))
        .collect();
    if actual != expected {
        bail!("Core IR block layout differs from the OGM-30 lowering profile");
    }
    Ok(())
}

fn expect_instruction_count(function: &JsonValue, block_id: &str, expected: usize) -> Result<()> {
    let actual = function_block(function, block_id)?["instructions"]
        .as_array()
        .context("Core IR instructions missing")?
        .len();
    if actual != expected {
        bail!("{block_id}: expected {expected} instructions, got {actual}");
    }
    Ok(())
}

fn expect_instruction(
    function: &JsonValue,
    block_id: &str,
    index: usize,
    op: &str,
    operands: &[&str],
    result: Option<&str>,
) -> Result<()> {
    let instructions = function_block(function, block_id)?["instructions"]
        .as_array()
        .context("Core IR instructions missing")?;
    let instruction = instructions
        .get(index)
        .with_context(|| format!("{block_id}: instruction {index} missing"))?;
    if instruction["op"].as_str() != Some(op) {
        bail!("{block_id}: instruction {index} is not {op}");
    }
    let actual_operands: Vec<&str> = instruction["operands"]
        .as_array()
        .context("Core IR operands missing")?
        .iter()
        .map(|operand| operand.as_str().unwrap_or(""))
        .collect();
    if actual_operands != operands {
        bail!("{block_id}: {op} operands differ from the OGM-30 lowering profile");
    }
    match result {
        Some(expected) if instruction["result"]["id"].as_str() == Some(expected) => {}
        None if instruction["result"].is_null() => {}
        _ => bail!("{block_id}: {op} result differs from the OGM-30 lowering profile"),
    }
    Ok(())
}

fn expect_terminator<'a>(
    function: &'a JsonValue,
    block_id: &str,
    kind: &str,
) -> Result<&'a JsonValue> {
    let terminator = &function_block(function, block_id)?["terminator"];
    if terminator["kind"].as_str() != Some(kind) {
        bail!("{block_id}: expected {kind} terminator");
    }
    Ok(terminator)
}

fn expect_names(actual: &JsonValue, expected: &[&str], context: &str) -> Result<()> {
    let actual: Vec<&str> = actual
        .as_array()
        .context("Core IR argument list missing")?
        .iter()
        .map(|value| value.as_str().unwrap_or(""))
        .collect();
    if actual != expected {
        bail!("{context}: argument order differs from the OGM-30 lowering profile");
    }
    Ok(())
}

fn validate_lowering_profile(label: &str, document: &JsonValue) -> Result<()> {
    let function = &document["function"];
    match label {
        "branch" => {
            expect_block_order(function, &["entry", "left_block", "right_block"])?;
            expect_instruction_count(function, "entry", 1)?;
            expect_instruction(
                function,
                "entry",
                0,
                "js.strict_equal",
                &["arg_left", "arg_right"],
                Some("same"),
            )?;
            let branch = expect_terminator(function, "entry", "branch")?;
            if branch["condition"] != "same"
                || branch["then"]["target"] != "left_block"
                || branch["else"]["target"] != "right_block"
            {
                bail!("branch fixture CFG differs from the OGM-30 lowering profile");
            }
            expect_names(&branch["then"]["arguments"], &["arg_left"], "branch then")?;
            expect_names(&branch["else"]["arguments"], &["arg_right"], "branch else")?;
            if expect_terminator(function, "left_block", "return")?["value"] != "left_value"
                || expect_terminator(function, "right_block", "return")?["value"] != "right_value"
            {
                bail!("branch fixture return values differ from the OGM-30 lowering profile");
            }
        }
        "loop" => {
            expect_block_order(function, &["entry", "loop", "body", "exit"])?;
            expect_instruction_count(function, "entry", 0)?;
            let entry = expect_terminator(function, "entry", "jump")?;
            if entry["target"] != "loop" {
                bail!("loop entry does not jump to the loop header");
            }
            expect_names(&entry["arguments"], &["start", "limit", "e0"], "loop entry")?;
            expect_instruction_count(function, "loop", 1)?;
            expect_instruction(
                function,
                "loop",
                0,
                "js.strict_equal",
                &["current", "loop_limit"],
                Some("done"),
            )?;
            let branch = expect_terminator(function, "loop", "branch")?;
            if branch["condition"] != "done"
                || branch["then"]["target"] != "exit"
                || branch["else"]["target"] != "body"
            {
                bail!("loop header CFG differs from the OGM-30 lowering profile");
            }
            expect_names(
                &branch["then"]["arguments"],
                &["current", "e_loop"],
                "loop exit",
            )?;
            expect_names(
                &branch["else"]["arguments"],
                &["current", "loop_limit", "e_loop"],
                "loop body",
            )?;
            expect_instruction_count(function, "body", 2)?;
            expect_instruction(
                function,
                "body",
                0,
                "js.add",
                &["body_current", "step"],
                Some("incremented"),
            )?;
            expect_instruction(
                function,
                "body",
                1,
                "js.to_number",
                &["incremented"],
                Some("next_current"),
            )?;
            let back_edge = expect_terminator(function, "body", "jump")?;
            if back_edge["target"] != "loop" {
                bail!("loop body does not carry values back to the loop header");
            }
            expect_names(
                &back_edge["arguments"],
                &["next_current", "body_limit", "e_number"],
                "loop back edge",
            )?;
            if expect_terminator(function, "exit", "return")?["value"] != "final_value" {
                bail!("loop exit return differs from the OGM-30 lowering profile");
            }
        }
        "dynamic_exception" => {
            expect_block_order(function, &["entry", "return_block", "handler"])?;
            expect_instruction_count(function, "entry", 1)?;
            expect_instruction(function, "entry", 0, "js.add", &["lhs", "rhs"], Some("sum"))?;
            let add = &function_block(function, "entry")?["instructions"][0];
            if add["exception"]["kind"] != "edge" || add["exception"]["target"] != "handler" {
                bail!("dynamic exception edge differs from the OGM-30 lowering profile");
            }
            expect_names(
                &add["exception"]["arguments"],
                &["e1"],
                "dynamic add exception edge",
            )?;
            let normal = expect_terminator(function, "entry", "jump")?;
            if normal["target"] != "return_block" {
                bail!("normal add completion does not reach return_block");
            }
            expect_names(
                &normal["arguments"],
                &["sum", "e1"],
                "normal add completion",
            )?;
            if expect_terminator(function, "return_block", "return")?["value"] != "return_value" {
                bail!("normal completion return differs from the OGM-30 lowering profile");
            }
            expect_instruction_count(function, "handler", 1)?;
            expect_instruction(function, "handler", 0, "js.throw", &["caught"], None)?;
            if expect_terminator(function, "handler", "unreachable")?["reason"] != "after_throw" {
                bail!("throw handler terminator differs from the OGM-30 lowering profile");
            }
        }
        "call_boundary" => {
            expect_block_order(function, &["entry"])?;
            expect_instruction_count(function, "entry", 3)?;
            expect_instruction(
                function,
                "entry",
                0,
                "js.strict_equal",
                &["object_left", "object_right"],
                Some("same_object"),
            )?;
            expect_instruction(
                function,
                "entry",
                1,
                "js.add",
                &["text", "numeric"],
                Some("mixed_sum"),
            )?;
            expect_instruction(
                function,
                "entry",
                2,
                "js.call",
                &["callee", "receiver", "mixed_sum"],
                Some("call_result"),
            )?;
            if expect_terminator(function, "entry", "return")?["value"] != "call_result" {
                bail!("call boundary return differs from the OGM-30 lowering profile");
            }
        }
        "host_call" => {
            expect_block_order(function, &["entry"])?;
            expect_instruction_count(function, "entry", 1)?;
            let call = &function_block(function, "entry")?["instructions"][0];
            expect_instruction(function, "entry", 0, "host.call", &["arg"], Some("result"))?;
            if call["attributes"]["symbol"] != "benchmark.host.echo" {
                bail!("host capability symbol differs from the OGM-30 lowering profile");
            }
            if expect_terminator(function, "entry", "return")?["value"] != "result" {
                bail!("host capability return differs from the OGM-30 lowering profile");
            }
        }
        _ => bail!("unknown OGM-30 lowering profile: {label}"),
    }
    Ok(())
}

fn load_corpus() -> Result<CoreCorpus> {
    let manifest: JsonValue = serde_json::from_str(include_str!("../fixtures/workloads.json"))?;
    if manifest["contract"] != "ogma.backend-codegen-workloads"
        || manifest["version"] != "1.0.0"
        || manifest["core_ir_contract"] != "ogma.core-ir/1.0.0"
    {
        bail!("unsupported workload manifest");
    }
    let entries = manifest["workloads"]
        .as_array()
        .context("workloads missing")?;
    let native_exports = manifest["native_exports"]
        .as_array()
        .context("native exports missing")?;
    let sources = [
        (
            "branch",
            "tests/core-ir/fixtures/positive-ssa-branch.json",
            include_str!("../../../tests/core-ir/fixtures/positive-ssa-branch.json"),
        ),
        (
            "loop",
            "tests/core-ir/fixtures/positive-loop-carried-values.json",
            include_str!("../../../tests/core-ir/fixtures/positive-loop-carried-values.json"),
        ),
        (
            "dynamic_exception",
            "tests/core-ir/fixtures/positive-dynamic-exception.json",
            include_str!("../../../tests/core-ir/fixtures/positive-dynamic-exception.json"),
        ),
        (
            "call_boundary",
            "tests/core-ir/fixtures/positive-call-boundary.json",
            include_str!("../../../tests/core-ir/fixtures/positive-call-boundary.json"),
        ),
        (
            "host_call",
            "tests/core-ir/fixtures/positive-host-call.json",
            include_str!("../../../tests/core-ir/fixtures/positive-host-call.json"),
        ),
    ];
    let manifest_ids: Vec<&str> = entries
        .iter()
        .map(|entry| entry["id"].as_str().unwrap_or(""))
        .collect();
    let unique_manifest_ids: BTreeSet<&str> = manifest_ids.iter().copied().collect();
    if entries.len() != sources.len() || unique_manifest_ids.len() != entries.len() {
        bail!("workload manifest must contain each canonical workload exactly once");
    }
    for (label, _, _) in sources {
        if !unique_manifest_ids.contains(label) {
            bail!("workload manifest is missing canonical workload {label}");
        }
    }

    let mut fixtures = Vec::new();
    for (label, path, raw) in sources {
        let entry = entries
            .iter()
            .find(|e| e["id"] == label)
            .with_context(|| format!("manifest workload {label} missing"))?;
        if entry["fixture"] != path {
            bail!("{label}: fixture path mismatch");
        }
        let document: JsonValue = serde_json::from_str(raw)?;
        validate_lowering_profile(label, &document)?;
        let f = parse_fixture(label, raw)?;
        let expected_operations: Vec<&str> = entry["operations"]
            .as_array()
            .context("expected operations missing")?
            .iter()
            .map(|operation| operation.as_str().unwrap_or(""))
            .collect();
        let actual_operations: Vec<&str> = f.operations.iter().map(String::as_str).collect();
        if actual_operations != expected_operations {
            bail!("{label}: Core IR operations differ from the benchmark manifest");
        }
        if let Some(terms) = entry["terminators"].as_array() {
            let expected_terminators: Vec<&str> = terms
                .iter()
                .map(|terminator| terminator.as_str().unwrap_or(""))
                .collect();
            let mut actual_terminators = Vec::new();
            for terminator in &f.terminators {
                let terminator = terminator.as_str();
                if !actual_terminators.contains(&terminator) {
                    actual_terminators.push(terminator);
                }
            }
            if actual_terminators != expected_terminators {
                bail!("{label}: Core IR terminator kinds differ from the benchmark manifest");
            }
        }
        let expected_exception_edge = entry["exception_edge"].as_bool().unwrap_or(false);
        if f.exception_edge != expected_exception_edge {
            bail!("{label}: Core IR exception-edge metadata differs from the benchmark manifest");
        }
        if let Some(symbol) = entry["host_symbol"].as_str()
            && f.host_symbol.as_deref() != Some(symbol)
        {
            bail!("{label}: host symbol mismatch");
        }
        fixtures.push(f);
    }
    let mut native_export_symbols = BTreeMap::new();
    for export in native_exports {
        let id = export["id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .context("native export ID missing")?;
        let symbol = export["symbol"]
            .as_str()
            .filter(|symbol| !symbol.is_empty())
            .context("native export symbol missing")?;
        if symbol != format!("ogma_{id}") {
            bail!("native export {id}: symbol must be ogma_{id}");
        }
        if native_export_symbols
            .insert(id.to_owned(), symbol.to_owned())
            .is_some()
        {
            bail!("duplicate native export {id}");
        }
    }
    let expected_native_exports = [
        ("aot_host_call_f64", "ogma_aot_host_call_f64"),
        ("aot_host_call_handles", "ogma_aot_host_call_handles"),
    ];
    if native_export_symbols.len() != expected_native_exports.len()
        || expected_native_exports.iter().any(|(id, symbol)| {
            native_export_symbols.get(*id).map(String::as_str) != Some(*symbol)
        })
    {
        bail!("workload manifest must declare each OGM-32 generated native export exactly once");
    }
    let c = CoreCorpus {
        fixtures,
        native_exports: native_export_symbols,
    };
    expect_signature(&c, "branch", &["f64", "f64"], "f64")?;
    expect_signature(&c, "loop", &["f64", "f64", "f64"], "f64")?;
    expect_signature(&c, "dynamic_exception", &["boxed", "boxed"], "boxed")?;
    expect_signature(
        &c,
        "call_boundary",
        &[
            "function_ref",
            "boxed",
            "string_ref",
            "object_ref",
            "object_ref",
            "f64",
        ],
        "boxed",
    )?;
    expect_signature(&c, "host_call", &["boxed"], "boxed")?;
    Ok(c)
}
fn expect_signature(c: &CoreCorpus, label: &str, params: &[&str], ret: &str) -> Result<()> {
    let f = c.get(label)?;
    if f.param_representations
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        != params
        || f.return_representation != ret
    {
        bail!("{label}: unsupported Core IR signature");
    }
    Ok(())
}
fn sym(name: &str) -> Vec<u8> {
    let mut b = name.as_bytes().to_vec();
    b.push(0);
    b
}

fn define_clif<F>(m: &mut ObjectModule, id: FuncId, sig: Signature, build: F) -> Result<()>
where
    F: FnOnce(&mut ObjectModule, &mut FunctionBuilder<'_>, &[Value]) -> Result<()>,
{
    let mut ctx = m.make_context();
    ctx.func.name = UserFuncName::user(0, id.as_u32());
    ctx.func.signature = sig;
    let mut fbctx = FunctionBuilderContext::new();
    {
        let mut b = FunctionBuilder::new(&mut ctx.func, &mut fbctx);
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        let p = b.block_params(entry).to_vec();
        build(m, &mut b, &p)?;
        b.seal_all_blocks();
        b.finalize(m.target_config());
    }
    m.define_function(id, &mut ctx)?;
    m.clear_context(&mut ctx);
    Ok(())
}
fn unary_i64_sig(m: &mut ObjectModule) -> Signature {
    let mut s = m.make_signature();
    s.params.push(AbiParam::new(types::I64));
    s.returns.push(AbiParam::new(types::I64));
    s
}
fn binary_i64_sig(m: &mut ObjectModule) -> Signature {
    let mut s = m.make_signature();
    s.params
        .extend([AbiParam::new(types::I64), AbiParam::new(types::I64)]);
    s.returns.push(AbiParam::new(types::I64));
    s
}
fn binary_f64_sig(m: &mut ObjectModule) -> Signature {
    let mut s = m.make_signature();
    s.params
        .extend([AbiParam::new(types::F64), AbiParam::new(types::F64)]);
    s.returns.push(AbiParam::new(types::F64));
    s
}
fn ternary_f64_sig(m: &mut ObjectModule) -> Signature {
    let mut s = m.make_signature();
    s.params.extend([
        AbiParam::new(types::F64),
        AbiParam::new(types::F64),
        AbiParam::new(types::F64),
    ]);
    s.returns.push(AbiParam::new(types::F64));
    s
}
fn emit_clif_safepoint(
    module: &mut ObjectModule,
    builder: &mut FunctionBuilder<'_>,
    safepoint: FuncId,
    site: i64,
) {
    let reference = module.declare_func_in_func(safepoint, builder.func);
    let argument = builder.ins().iconst(types::I32, site);
    let _ = builder.ins().call(reference, &[argument]);
}

fn compile_cranelift(path: &Path, c: &CoreCorpus) -> Result<u64> {
    let start = Instant::now();
    let mut flags = settings::builder();
    flags.set("opt_level", "speed")?;
    let isa = cranelift_native::builder()
        .map_err(|e| anyhow!(e))?
        .finish(settings::Flags::new(flags))?;
    let mut m = ObjectModule::new(ObjectBuilder::new(isa, "ogma-30", default_libcall_names())?);
    let branch_sig = binary_f64_sig(&mut m);
    let loop_sig = ternary_f64_sig(&mut m);
    let exception_sig = binary_i64_sig(&mut m);
    let host_sig = unary_i64_sig(&mut m);
    let mut aot_f64_sig = m.make_signature();
    aot_f64_sig.params.extend(
        [types::I64, types::I32, types::F64, types::F64, types::I64]
            .into_iter()
            .map(AbiParam::new),
    );
    aot_f64_sig.returns.push(AbiParam::new(types::I64));
    let mut aot_handles_sig = m.make_signature();
    aot_handles_sig.params.extend(
        [types::I64, types::I32, types::I64, types::I64, types::I64]
            .into_iter()
            .map(AbiParam::new),
    );
    aot_handles_sig.returns.push(AbiParam::new(types::I64));
    let mut call_sig = m.make_signature();
    call_sig.params.extend(
        [
            types::I64,
            types::I64,
            types::I64,
            types::I64,
            types::I64,
            types::F64,
        ]
        .into_iter()
        .map(AbiParam::new),
    );
    call_sig.returns.push(AbiParam::new(types::I64));
    let branch = m.declare_function(&c.symbol("branch")?, Linkage::Export, &branch_sig)?;
    let loop_fn = m.declare_function(&c.symbol("loop")?, Linkage::Export, &loop_sig)?;
    let exception = m.declare_function(
        &c.symbol("dynamic_exception")?,
        Linkage::Export,
        &exception_sig,
    )?;
    let call = m.declare_function(&c.symbol("call_boundary")?, Linkage::Export, &call_sig)?;
    let host = m.declare_function(&c.symbol("host_call")?, Linkage::Export, &host_sig)?;
    let aot_host_f64 = m.declare_function(
        &c.symbol("aot_host_call_f64")?,
        Linkage::Export,
        &aot_f64_sig,
    )?;
    let aot_host_handles = m.declare_function(
        &c.symbol("aot_host_call_handles")?,
        Linkage::Export,
        &aot_handles_sig,
    )?;
    let aot_dispatch_f64 =
        m.declare_function("ogma_aot_dispatch_f64", Linkage::Import, &aot_f64_sig)?;
    let aot_dispatch_handles = m.declare_function(
        "ogma_aot_dispatch_handles",
        Linkage::Import,
        &aot_handles_sig,
    )?;
    let mut add_num_sig = m.make_signature();
    add_num_sig
        .params
        .extend([AbiParam::new(types::F64), AbiParam::new(types::F64)]);
    add_num_sig.returns.push(AbiParam::new(types::I64));
    let mut to_num_sig = m.make_signature();
    to_num_sig.params.push(AbiParam::new(types::I64));
    to_num_sig.returns.push(AbiParam::new(types::F64));
    let add_number = m.declare_function("ogma_js_add_number", Linkage::Import, &add_num_sig)?;
    let to_number = m.declare_function("ogma_js_to_number", Linkage::Import, &to_num_sig)?;
    let add_completion =
        m.declare_function("ogma_js_add_completion", Linkage::Import, &exception_sig)?;
    let js_throw = m.declare_function("ogma_js_throw", Linkage::Import, &host_sig)?;
    let mut add_mixed_sig = m.make_signature();
    add_mixed_sig
        .params
        .extend([AbiParam::new(types::I64), AbiParam::new(types::F64)]);
    add_mixed_sig.returns.push(AbiParam::new(types::I64));
    let add_mixed = m.declare_function("ogma_js_add_mixed", Linkage::Import, &add_mixed_sig)?;
    let mut js_call_sig = m.make_signature();
    js_call_sig.params.extend([
        AbiParam::new(types::I64),
        AbiParam::new(types::I64),
        AbiParam::new(types::I64),
    ]);
    js_call_sig.returns.push(AbiParam::new(types::I64));
    let js_call = m.declare_function("ogma_js_call", Linkage::Import, &js_call_sig)?;
    let host_helper = m.declare_function("ogma_host_echo", Linkage::Import, &host_sig)?;
    let mut safepoint_sig = m.make_signature();
    safepoint_sig.params.push(AbiParam::new(types::I32));
    safepoint_sig.returns.push(AbiParam::new(types::I32));
    let safepoint = m.declare_function("ogma_proof_safepoint", Linkage::Import, &safepoint_sig)?;
    let completion_kind =
        m.declare_function("ogma_proof_last_completion_kind", Linkage::Import, &{
            let mut signature = m.make_signature();
            signature.returns.push(AbiParam::new(types::I32));
            signature
        })?;

    define_clif(&mut m, branch, branch_sig, |m, b, p| {
        emit_clif_safepoint(m, b, safepoint, 1);
        let eq = b.ins().fcmp(FloatCC::Equal, p[0], p[1]);
        let yes = b.create_block();
        let no = b.create_block();
        b.ins().brif(eq, yes, &[], no, &[]);
        b.switch_to_block(yes);
        b.ins().return_(&[p[0]]);
        b.switch_to_block(no);
        b.ins().return_(&[p[1]]);
        Ok(())
    })?;
    define_clif(&mut m, loop_fn, loop_sig, |m, b, p| {
        emit_clif_safepoint(m, b, safepoint, 1);
        let lp = b.create_block();
        let body = b.create_block();
        let exit = b.create_block();
        let current = b.append_block_param(lp, types::F64);
        let limit = b.append_block_param(lp, types::F64);
        let step = b.append_block_param(lp, types::F64);
        let bc = b.append_block_param(body, types::F64);
        let bl = b.append_block_param(body, types::F64);
        let bs = b.append_block_param(body, types::F64);
        let out = b.append_block_param(exit, types::F64);
        let entry_args = [
            BlockArg::Value(p[0]),
            BlockArg::Value(p[1]),
            BlockArg::Value(p[2]),
        ];
        b.ins().jump(lp, &entry_args);
        b.switch_to_block(lp);
        let eq = b.ins().fcmp(FloatCC::Equal, current, limit);
        let exit_args = [BlockArg::Value(current)];
        let body_args = [
            BlockArg::Value(current),
            BlockArg::Value(limit),
            BlockArg::Value(step),
        ];
        b.ins().brif(eq, exit, &exit_args, body, &body_args);
        b.switch_to_block(body);
        let add_ref = m.declare_func_in_func(add_number, b.func);
        let add = b.ins().call(add_ref, &[bc, bs]);
        let boxed = b.inst_results(add)[0];
        let conv_ref = m.declare_func_in_func(to_number, b.func);
        let conv = b.ins().call(conv_ref, &[boxed]);
        let next = b.inst_results(conv)[0];
        emit_clif_safepoint(m, b, safepoint, 2);
        let next_args = [
            BlockArg::Value(next),
            BlockArg::Value(bl),
            BlockArg::Value(bs),
        ];
        b.ins().jump(lp, &next_args);
        b.switch_to_block(exit);
        b.ins().return_(&[out]);
        Ok(())
    })?;
    define_clif(&mut m, exception, exception_sig, |m, b, p| {
        emit_clif_safepoint(m, b, safepoint, 1);
        emit_clif_safepoint(m, b, safepoint, 3);
        let add_ref = m.declare_func_in_func(add_completion, b.func);
        let add = b.ins().call(add_ref, &[p[0], p[1]]);
        let result = b.inst_results(add)[0];
        let completion_ref = m.declare_func_in_func(completion_kind, b.func);
        let kind_call = b.ins().call(completion_ref, &[]);
        let kind = b.inst_results(kind_call)[0];
        let normal_kind = b.ins().iconst(types::I32, abi::COMPLETION_NORMAL as i64);
        let normal = b.ins().icmp(IntCC::Equal, kind, normal_kind);
        let ok = b.create_block();
        let thrown = b.create_block();
        b.ins().brif(normal, ok, &[], thrown, &[]);
        b.switch_to_block(ok);
        b.ins().return_(&[result]);
        b.switch_to_block(thrown);
        let throw_ref = m.declare_func_in_func(js_throw, b.func);
        let rethrow = b.ins().call(throw_ref, &[result]);
        let value = b.inst_results(rethrow)[0];
        b.ins().return_(&[value]);
        Ok(())
    })?;
    define_clif(&mut m, call, call_sig, |m, b, p| {
        emit_clif_safepoint(m, b, safepoint, 1);
        let _same = b.ins().icmp(IntCC::Equal, p[3], p[4]);
        emit_clif_safepoint(m, b, safepoint, 3);
        let add_ref = m.declare_func_in_func(add_mixed, b.func);
        let add = b.ins().call(add_ref, &[p[2], p[5]]);
        let mixed = b.inst_results(add)[0];
        emit_clif_safepoint(m, b, safepoint, 4);
        let call_ref = m.declare_func_in_func(js_call, b.func);
        let invoked = b.ins().call(call_ref, &[p[0], p[1], mixed]);
        let value = b.inst_results(invoked)[0];
        b.ins().return_(&[value]);
        Ok(())
    })?;
    define_clif(&mut m, host, host_sig, |m, b, p| {
        emit_clif_safepoint(m, b, safepoint, 1);
        emit_clif_safepoint(m, b, safepoint, 5);
        let f = m.declare_func_in_func(host_helper, b.func);
        let call = b.ins().call(f, &[p[0]]);
        let value = b.inst_results(call)[0];
        b.ins().return_(&[value]);
        Ok(())
    })?;
    define_clif(&mut m, aot_host_f64, aot_f64_sig, |m, b, p| {
        emit_clif_safepoint(m, b, safepoint, 1);
        emit_clif_safepoint(m, b, safepoint, 5);
        let f = m.declare_func_in_func(aot_dispatch_f64, b.func);
        let call = b.ins().call(f, &[p[0], p[1], p[2], p[3], p[4]]);
        let value = b.inst_results(call)[0];
        b.ins().return_(&[value]);
        Ok(())
    })?;
    define_clif(&mut m, aot_host_handles, aot_handles_sig, |m, b, p| {
        emit_clif_safepoint(m, b, safepoint, 1);
        emit_clif_safepoint(m, b, safepoint, 5);
        let f = m.declare_func_in_func(aot_dispatch_handles, b.func);
        let call = b.ins().call(f, &[p[0], p[1], p[2], p[3], p[4]]);
        let value = b.inst_results(call)[0];
        b.ins().return_(&[value]);
        Ok(())
    })?;
    fs::write(path, m.finish().emit()?)?;
    Ok(start.elapsed().as_nanos() as u64)
}

fn llvm_call_i64<'ctx>(
    b: &inkwell::builder::Builder<'ctx>,
    f: inkwell::values::FunctionValue<'ctx>,
    args: &[inkwell::values::BasicMetadataValueEnum<'ctx>],
    name: &str,
) -> Result<IntValue<'ctx>> {
    Ok(b.build_call(f, args, name)?
        .try_as_basic_value()
        .unwrap_basic()
        .into_int_value())
}
fn llvm_call_f64<'ctx>(
    b: &inkwell::builder::Builder<'ctx>,
    f: inkwell::values::FunctionValue<'ctx>,
    args: &[inkwell::values::BasicMetadataValueEnum<'ctx>],
    name: &str,
) -> Result<FloatValue<'ctx>> {
    Ok(b.build_call(f, args, name)?
        .try_as_basic_value()
        .unwrap_basic()
        .into_float_value())
}
fn llvm_safepoint<'ctx>(
    context: &'ctx Context,
    builder: &inkwell::builder::Builder<'ctx>,
    function: inkwell::values::FunctionValue<'ctx>,
    site: u32,
) -> Result<()> {
    let site_value = context.i32_type().const_int(u64::from(site), false);
    builder.build_call(function, &[site_value.into()], "ogma_safepoint")?;
    Ok(())
}

fn compile_llvm(path: &Path, c: &CoreCorpus) -> Result<u64> {
    let start = Instant::now();
    Target::initialize_all(&InitializationConfig::default());
    let triple = TargetMachine::get_default_triple();
    let target = Target::from_triple(&triple).map_err(|e| anyhow!(e.to_string()))?;
    let host_cpu = TargetMachine::get_host_cpu_name();
    let host_features = TargetMachine::get_host_cpu_features();
    let cpu_name = host_cpu
        .to_str()
        .context("LLVM host CPU name is not valid UTF-8")?;
    let cpu_features = host_features
        .to_str()
        .context("LLVM host CPU features are not valid UTF-8")?;
    let tm = target
        .create_target_machine(
            &triple,
            cpu_name,
            cpu_features,
            OptimizationLevel::Default,
            RelocMode::PIC,
            CodeModel::Default,
        )
        .context("create LLVM target machine")?;
    let cx = Context::create();
    let module = cx.create_module("ogma_ogm30");
    module.set_triple(&triple);
    module.set_data_layout(&tm.get_target_data().get_data_layout());
    let b = cx.create_builder();
    let i64t = cx.i64_type();
    let f64t = cx.f64_type();
    let unary = i64t.fn_type(&[i64t.into()], false);
    let safepoint_type = cx.i32_type().fn_type(&[cx.i32_type().into()], false);
    let safepoint = module.add_function("ogma_proof_safepoint", safepoint_type, None);
    let completion_kind = module.add_function(
        "ogma_proof_last_completion_kind",
        cx.i32_type().fn_type(&[], false),
        None,
    );
    let binary = i64t.fn_type(&[i64t.into(), i64t.into()], false);
    let ternary_f = f64t.fn_type(&[f64t.into(), f64t.into(), f64t.into()], false);
    let branch_type = f64t.fn_type(&[f64t.into(), f64t.into()], false);
    let call_type = i64t.fn_type(
        &[
            i64t.into(),
            i64t.into(),
            i64t.into(),
            i64t.into(),
            i64t.into(),
            f64t.into(),
        ],
        false,
    );
    let aot_f64_type = i64t.fn_type(
        &[
            i64t.into(),
            cx.i32_type().into(),
            f64t.into(),
            f64t.into(),
            i64t.into(),
        ],
        false,
    );
    let aot_handles_type = i64t.fn_type(
        &[
            i64t.into(),
            cx.i32_type().into(),
            i64t.into(),
            i64t.into(),
            i64t.into(),
        ],
        false,
    );

    let branch = module.add_function(&c.symbol("branch")?, branch_type, None);
    let entry = cx.append_basic_block(branch, "entry");
    let yes = cx.append_basic_block(branch, "equal");
    let no = cx.append_basic_block(branch, "not_equal");
    b.position_at_end(entry);
    llvm_safepoint(&cx, &b, safepoint, 1)?;
    let left = branch.get_nth_param(0).unwrap().into_float_value();
    let right = branch.get_nth_param(1).unwrap().into_float_value();
    let eq = b.build_float_compare(FloatPredicate::OEQ, left, right, "strict_equal")?;
    b.build_conditional_branch(eq, yes, no)?;
    b.position_at_end(yes);
    b.build_return(Some(&left))?;
    b.position_at_end(no);
    b.build_return(Some(&right))?;

    let add_number = module.add_function(
        "ogma_js_add_number",
        i64t.fn_type(&[f64t.into(), f64t.into()], false),
        None,
    );
    let to_number = module.add_function(
        "ogma_js_to_number",
        f64t.fn_type(&[i64t.into()], false),
        None,
    );
    let loop_fn = module.add_function(&c.symbol("loop")?, ternary_f, None);
    let loop_entry = cx.append_basic_block(loop_fn, "entry");
    let loop_bb = cx.append_basic_block(loop_fn, "loop");
    let body = cx.append_basic_block(loop_fn, "body");
    let exit = cx.append_basic_block(loop_fn, "exit");
    b.position_at_end(loop_entry);
    llvm_safepoint(&cx, &b, safepoint, 1)?;
    b.build_unconditional_branch(loop_bb)?;
    b.position_at_end(loop_bb);
    let current = b.build_phi(f64t, "current")?;
    let limit = b.build_phi(f64t, "limit")?;
    let step = b.build_phi(f64t, "step")?;
    let current_v = current.as_basic_value().into_float_value();
    let limit_v = limit.as_basic_value().into_float_value();
    let eq = b.build_float_compare(FloatPredicate::OEQ, current_v, limit_v, "at_limit")?;
    b.build_conditional_branch(eq, exit, body)?;
    b.position_at_end(body);
    let step_v = step.as_basic_value().into_float_value();
    let boxed = llvm_call_i64(&b, add_number, &[current_v.into(), step_v.into()], "js_add")?;
    let next = llvm_call_f64(&b, to_number, &[boxed.into()], "to_number")?;
    llvm_safepoint(&cx, &b, safepoint, 2)?;
    b.build_unconditional_branch(loop_bb)?;
    let body_end = b.get_insert_block().context("loop body missing")?;
    let start_value = loop_fn.get_nth_param(0).unwrap().into_float_value();
    let max = loop_fn.get_nth_param(1).unwrap().into_float_value();
    let increment = loop_fn.get_nth_param(2).unwrap().into_float_value();
    current.add_incoming(&[(&start_value, loop_entry), (&next, body_end)]);
    limit.add_incoming(&[(&max, loop_entry), (&limit_v, body_end)]);
    step.add_incoming(&[(&increment, loop_entry), (&step_v, body_end)]);
    b.position_at_end(exit);
    b.build_return(Some(&current_v))?;

    let add_completion = module.add_function("ogma_js_add_completion", binary, None);
    let js_throw = module.add_function("ogma_js_throw", unary, None);
    let exception = module.add_function(&c.symbol("dynamic_exception")?, binary, None);
    let ex_entry = cx.append_basic_block(exception, "entry");
    let ok = cx.append_basic_block(exception, "normal");
    let thrown = cx.append_basic_block(exception, "throw");
    b.position_at_end(ex_entry);
    llvm_safepoint(&cx, &b, safepoint, 1)?;
    llvm_safepoint(&cx, &b, safepoint, 3)?;
    let lhs = exception.get_nth_param(0).unwrap().into_int_value();
    let rhs = exception.get_nth_param(1).unwrap().into_int_value();
    let result = llvm_call_i64(&b, add_completion, &[lhs.into(), rhs.into()], "js_add")?;
    let completion_call = b.build_call(completion_kind, &[], "completion_kind")?;
    let kind = completion_call
        .try_as_basic_value()
        .unwrap_basic()
        .into_int_value();
    let normal = b.build_int_compare(
        IntPredicate::EQ,
        kind,
        cx.i32_type()
            .const_int(u64::from(abi::COMPLETION_NORMAL), false),
        "is_normal",
    )?;
    b.build_conditional_branch(normal, ok, thrown)?;
    b.position_at_end(ok);
    b.build_return(Some(&result))?;
    b.position_at_end(thrown);
    let rethrow = llvm_call_i64(&b, js_throw, &[result.into()], "js_throw")?;
    b.build_return(Some(&rethrow))?;

    let add_mixed = module.add_function(
        "ogma_js_add_mixed",
        i64t.fn_type(&[i64t.into(), f64t.into()], false),
        None,
    );
    let js_call = module.add_function(
        "ogma_js_call",
        i64t.fn_type(&[i64t.into(), i64t.into(), i64t.into()], false),
        None,
    );
    let call = module.add_function(&c.symbol("call_boundary")?, call_type, None);
    b.position_at_end(cx.append_basic_block(call, "entry"));
    llvm_safepoint(&cx, &b, safepoint, 1)?;
    let callee = call.get_nth_param(0).unwrap().into_int_value();
    let receiver = call.get_nth_param(1).unwrap().into_int_value();
    let text = call.get_nth_param(2).unwrap().into_int_value();
    let left = call.get_nth_param(3).unwrap().into_int_value();
    let right = call.get_nth_param(4).unwrap().into_int_value();
    let numeric = call.get_nth_param(5).unwrap().into_float_value();
    let _same = b.build_int_compare(IntPredicate::EQ, left, right, "strict_equal")?;
    llvm_safepoint(&cx, &b, safepoint, 3)?;
    let mixed = llvm_call_i64(&b, add_mixed, &[text.into(), numeric.into()], "js_add")?;
    llvm_safepoint(&cx, &b, safepoint, 4)?;
    let invoked = llvm_call_i64(
        &b,
        js_call,
        &[callee.into(), receiver.into(), mixed.into()],
        "js_call",
    )?;
    b.build_return(Some(&invoked))?;

    let host_helper = module.add_function("ogma_host_echo", unary, None);
    let host = module.add_function(&c.symbol("host_call")?, unary, None);
    b.position_at_end(cx.append_basic_block(host, "entry"));
    llvm_safepoint(&cx, &b, safepoint, 1)?;
    llvm_safepoint(&cx, &b, safepoint, 5)?;
    let arg = host.get_nth_param(0).unwrap().into_int_value();
    let value = llvm_call_i64(&b, host_helper, &[arg.into()], "host_call")?;
    b.build_return(Some(&value))?;

    let aot_dispatch_f64 = module.add_function("ogma_aot_dispatch_f64", aot_f64_type, None);
    let aot_dispatch_handles =
        module.add_function("ogma_aot_dispatch_handles", aot_handles_type, None);

    let aot_host_f64 = module.add_function(&c.symbol("aot_host_call_f64")?, aot_f64_type, None);
    b.position_at_end(cx.append_basic_block(aot_host_f64, "entry"));
    llvm_safepoint(&cx, &b, safepoint, 1)?;
    llvm_safepoint(&cx, &b, safepoint, 5)?;
    let f64_args = [
        aot_host_f64.get_nth_param(0).unwrap().into(),
        aot_host_f64.get_nth_param(1).unwrap().into(),
        aot_host_f64.get_nth_param(2).unwrap().into(),
        aot_host_f64.get_nth_param(3).unwrap().into(),
        aot_host_f64.get_nth_param(4).unwrap().into(),
    ];
    let f64_value = llvm_call_i64(&b, aot_dispatch_f64, &f64_args, "aot_dispatch_f64")?;
    b.build_return(Some(&f64_value))?;

    let aot_host_handles =
        module.add_function(&c.symbol("aot_host_call_handles")?, aot_handles_type, None);
    b.position_at_end(cx.append_basic_block(aot_host_handles, "entry"));
    llvm_safepoint(&cx, &b, safepoint, 1)?;
    llvm_safepoint(&cx, &b, safepoint, 5)?;
    let handle_args = [
        aot_host_handles.get_nth_param(0).unwrap().into(),
        aot_host_handles.get_nth_param(1).unwrap().into(),
        aot_host_handles.get_nth_param(2).unwrap().into(),
        aot_host_handles.get_nth_param(3).unwrap().into(),
        aot_host_handles.get_nth_param(4).unwrap().into(),
    ];
    let handle_value = llvm_call_i64(
        &b,
        aot_dispatch_handles,
        &handle_args,
        "aot_dispatch_handles",
    )?;
    b.build_return(Some(&handle_value))?;

    module
        .verify()
        .map_err(|e| anyhow!("LLVM verification: {e}"))?;
    tm.write_to_file(&module, FileType::Object, path)
        .map_err(|e| anyhow!("LLVM object emission: {e}"))?;
    Ok(start.elapsed().as_nanos() as u64)
}

const HELPERS: &str = r#"
#include "ogma_abi.h"
#include "ogma_aot_host_calls.h"
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <math.h>

typedef struct {
  OgmaAbiVersion version;
  OgmaStatus (*context_create_number)(const OgmaRuntime *, OgmaContextHandle, double, OgmaHandle *);
  OgmaStatus (*context_get_number)(const OgmaRuntime *, OgmaContextHandle, OgmaHandle, double *);
  OgmaStatus (*root_scope_open)(const OgmaRuntime *, OgmaContextHandle, OgmaRootScopeHandle *);
  OgmaStatus (*root_scope_add)(const OgmaRuntime *, OgmaContextHandle, OgmaRootScopeHandle, OgmaHandle);
  OgmaStatus (*root_scope_close)(const OgmaRuntime *, OgmaContextHandle, OgmaRootScopeHandle);
  OgmaStatus (*context_call)(const OgmaRuntime *, OgmaContextHandle, OgmaHandle, const OgmaHandle *, size_t, OgmaCompletion *);
} OgmaProofAbi;
_Static_assert(offsetof(OgmaProofAbi, context_create_number) == sizeof(OgmaAbiVersion),
               "OGM-19 C function-table layout drift");
_Static_assert(sizeof(OgmaProofAbi) == sizeof(OgmaAbiVersion) + 6 * sizeof(void (*)(void)),
               "OGM-19 C function-table size drift");
typedef int32_t (*OgmaProofSafepointCallback)(uint32_t, void *);

static const uint64_t PAYLOAD_MASK = UINT64_C(0x00ffffffffffffff);
static char dynamic_string[128];
static char call_argument[128];
static int64_t call_receiver;
static uint64_t proof_helper_counts[8];
static uint64_t proof_safepoint_counts[6];
static OgmaProofAbi proof_api;
static const OgmaRuntime *proof_runtime;
static OgmaContextHandle proof_context;
static OgmaHandle proof_dynamic_function;
static OgmaHandle proof_host_function;
static OgmaProofSafepointCallback proof_safepoint_callback;
static void *proof_safepoint_user_data;
static uint32_t proof_last_abi_status;
static uint32_t proof_last_safepoint_status;
static uint32_t proof_last_completion_kind;
static int64_t proof_last_completion_payload;
static OgmaAotCompletionV1 aot_last_completion;
static OgmaStatus aot_last_status = OGMA_STATUS_OK;
static uint64_t aot_typed_calls;
static uint64_t aot_generic_calls;
static uint64_t aot_handle_calls;

void ogma_proof_reset(void) {
  memset(proof_helper_counts, 0, sizeof(proof_helper_counts));
  memset(proof_safepoint_counts, 0, sizeof(proof_safepoint_counts));
  call_argument[0] = '\0';
  call_receiver = 0;
  proof_last_abi_status = OGMA_STATUS_OK;
  proof_last_safepoint_status = 0;
  proof_last_completion_kind = OGMA_COMPLETION_NORMAL;
  proof_last_completion_payload = 0;
  memset(&aot_last_completion, 0, sizeof(aot_last_completion));
  aot_last_status = OGMA_STATUS_OK;
  aot_typed_calls = 0;
  aot_generic_calls = 0;
  aot_handle_calls = 0;
}
uint64_t ogma_proof_count(uint32_t index) {
  return index < 8 ? proof_helper_counts[index] : 0;
}
uint64_t ogma_proof_safepoint_count(uint32_t site) {
  return site < 6 ? proof_safepoint_counts[site] : 0;
}
int32_t ogma_proof_last_safepoint_status(void) {
  return (int32_t)proof_last_safepoint_status;
}
uint32_t ogma_proof_last_abi_status(void) {
  return proof_last_abi_status;
}
uint32_t ogma_proof_last_completion_kind(void) {
  return proof_last_completion_kind;
}
int64_t ogma_proof_last_completion_payload(void) {
  return proof_last_completion_payload;
}
int64_t ogma_proof_last_call_receiver(void) {
  return call_receiver;
}
const char *ogma_proof_last_call_argument(void) {
  return call_argument;
}

uint32_t ogma_aot_last_status(void) {
  return aot_last_status;
}
uint32_t ogma_aot_last_completion_kind(void) {
  return aot_last_completion.kind;
}
uint32_t ogma_aot_last_error_kind(void) {
  return aot_last_completion.error_kind;
}
uint64_t ogma_aot_last_details(void) {
  return aot_last_completion.details;
}
double ogma_aot_last_number(void) {
  return aot_last_completion.value.tag == OGMA_AOT_VALUE_NUMBER
      ? aot_last_completion.value.payload.number : 0.0;
}
uint64_t ogma_aot_typed_calls(void) {
  return aot_typed_calls;
}
uint64_t ogma_aot_generic_calls(void) {
  return aot_generic_calls;
}
uint64_t ogma_aot_handle_calls(void) {
  return aot_handle_calls;
}
void ogma_proof_bind_abi(
  const OgmaProofAbi *api,
  const OgmaRuntime *runtime,
  OgmaContextHandle context,
  OgmaHandle dynamic_function,
  OgmaHandle host_function,
  OgmaProofSafepointCallback safepoint_callback,
  void *user_data
) {
  if (!api || !runtime || !safepoint_callback || !user_data) {
    proof_last_abi_status = OGMA_STATUS_INVALID_ARGUMENT;
    return;
  }
  proof_api = *api;
  proof_runtime = runtime;
  proof_context = context;
  proof_dynamic_function = dynamic_function;
  proof_host_function = host_function;
  proof_safepoint_callback = safepoint_callback;
  proof_safepoint_user_data = user_data;
  proof_last_abi_status = OGMA_STATUS_OK;
}
int32_t ogma_proof_safepoint(uint32_t site) {
  if (proof_last_safepoint_status != 0) return (int32_t)proof_last_safepoint_status;
  if (site == 0 || site >= 6 || !proof_safepoint_callback) {
    proof_last_safepoint_status = (uint32_t)-1;
    return -1;
  }
  proof_safepoint_counts[site]++;
  int32_t status = proof_safepoint_callback(site, proof_safepoint_user_data);
  if (status != 0 && proof_last_safepoint_status == 0) {
    proof_last_safepoint_status = (uint32_t)status;
  }
  return status;
}

static int64_t proof_context_call(OgmaHandle function, const int64_t *values, size_t count, uint32_t helper_index) {
  proof_helper_counts[helper_index]++;
  if (proof_last_abi_status != OGMA_STATUS_OK) return INT64_MIN;
  if (!proof_runtime || !proof_safepoint_callback || !values || count == 0 || count > 2) {
    proof_last_abi_status = OGMA_STATUS_INVALID_ARGUMENT;
    return INT64_MIN;
  }
  const uint64_t required = OGMA_FEATURE_OPAQUE_HANDLES
    | OGMA_FEATURE_ROOT_SCOPES
    | OGMA_FEATURE_PERSISTENT_HANDLES
    | OGMA_FEATURE_CALLBACKS
    | OGMA_FEATURE_COMPLETIONS
    | OGMA_FEATURE_MOCK_PROOF;
  if (proof_api.version.major != 1
      || proof_api.version.struct_size != sizeof(OgmaAbiVersion)
      || (proof_api.version.features & required) != required) {
    proof_last_abi_status = OGMA_STATUS_INCOMPATIBLE_ABI;
    return INT64_MIN;
  }

  OgmaRootScopeHandle scope = {0};
  OgmaStatus status = proof_api.root_scope_open(proof_runtime, proof_context, &scope);
  if (status != OGMA_STATUS_OK) {
    if (status != OGMA_STATUS_OK && proof_last_abi_status == OGMA_STATUS_OK) proof_last_abi_status = status;
    return INT64_MIN;
  }

  OgmaHandle arguments[2] = {{0}};
  for (size_t index = 0; index < count; index++) {
    status = proof_api.context_create_number(
      proof_runtime, proof_context, (double)values[index], &arguments[index]
    );
    if (status != OGMA_STATUS_OK) {
      if (status != OGMA_STATUS_OK && proof_last_abi_status == OGMA_STATUS_OK) proof_last_abi_status = status;
      (void)proof_api.root_scope_close(proof_runtime, proof_context, scope);
      return INT64_MIN;
    }
    status = proof_api.root_scope_add(proof_runtime, proof_context, scope, arguments[index]);
    if (status != OGMA_STATUS_OK) {
      if (status != OGMA_STATUS_OK && proof_last_abi_status == OGMA_STATUS_OK) proof_last_abi_status = status;
      (void)proof_api.root_scope_close(proof_runtime, proof_context, scope);
      return INT64_MIN;
    }
  }

  OgmaCompletion completion = {0};
  status = proof_api.context_call(
    proof_runtime, proof_context, function, arguments, count, &completion
  );
  if (status != OGMA_STATUS_OK && proof_last_abi_status == OGMA_STATUS_OK) proof_last_abi_status = status;
  if (status != OGMA_STATUS_OK) {
    (void)proof_api.root_scope_close(proof_runtime, proof_context, scope);
    return INT64_MIN;
  }
  if (completion.kind != OGMA_COMPLETION_NORMAL && completion.kind != OGMA_COMPLETION_THROW) {
    proof_last_abi_status = OGMA_STATUS_INVALID_COMPLETION;
    (void)proof_api.root_scope_close(proof_runtime, proof_context, scope);
    return INT64_MIN;
  }
  proof_last_completion_kind = completion.kind;
  if (completion.value.opaque[0] == 0) {
    proof_last_abi_status = OGMA_STATUS_INVALID_COMPLETION;
    (void)proof_api.root_scope_close(proof_runtime, proof_context, scope);
    return INT64_MIN;
  }
  status = proof_api.root_scope_add(proof_runtime, proof_context, scope, completion.value);
  if (status != OGMA_STATUS_OK) {
    if (status != OGMA_STATUS_OK && proof_last_abi_status == OGMA_STATUS_OK) proof_last_abi_status = status;
    (void)proof_api.root_scope_close(proof_runtime, proof_context, scope);
    return INT64_MIN;
  }
  double result = 0.0;
  status = proof_api.context_get_number(proof_runtime, proof_context, completion.value, &result);
  if (status != OGMA_STATUS_OK) {
    if (status != OGMA_STATUS_OK && proof_last_abi_status == OGMA_STATUS_OK) proof_last_abi_status = status;
    (void)proof_api.root_scope_close(proof_runtime, proof_context, scope);
    return INT64_MIN;
  }
  status = proof_api.root_scope_close(proof_runtime, proof_context, scope);
  if (status != OGMA_STATUS_OK) {
    if (status != OGMA_STATUS_OK && proof_last_abi_status == OGMA_STATUS_OK) proof_last_abi_status = status;
    return INT64_MIN;
  }
  proof_last_completion_payload = (int64_t)result;
  return proof_last_completion_payload;
}

int64_t ogma_js_add_number(double a, double b) {
  proof_helper_counts[0]++;
  double value = a + b;
  int64_t boxed;
  memcpy(&boxed, &value, sizeof(boxed));
  return boxed;
}
double ogma_js_to_number(int64_t boxed) {
  proof_helper_counts[1]++;
  double value;
  memcpy(&value, &boxed, sizeof(value));
  return value;
}
int64_t ogma_js_add_completion(int64_t a, int64_t b) {
  const int64_t values[] = {a, b};
  return proof_context_call(proof_dynamic_function, values, 2, 2);
}
int64_t ogma_js_throw(int64_t completion) {
  proof_helper_counts[3]++;
  return completion;
}
int64_t ogma_js_add_mixed(int64_t text, double number) {
  proof_helper_counts[4]++;
  const char *left = (const char *)(uintptr_t)text;
  (void)snprintf(dynamic_string, sizeof(dynamic_string), "%s%.0f", left, number);
  return (int64_t)(uintptr_t)dynamic_string;
}
typedef int64_t (*OgmaCallTarget)(int64_t, int64_t);
int64_t ogma_js_call(int64_t callee, int64_t receiver, int64_t arg) {
  proof_helper_counts[5]++;
  OgmaCallTarget target = (OgmaCallTarget)(uintptr_t)callee;
  return target(receiver, arg);
}
int64_t ogma_js_call_target(int64_t receiver, int64_t arg) {
  proof_helper_counts[6]++;
  const char *value = (const char *)(uintptr_t)arg;
  call_receiver = receiver;
  (void)snprintf(call_argument, sizeof(call_argument), "%s", value);
  if (strcmp(value, "item=5") != 0) return -1;
  return receiver + (int64_t)strlen(value);
}
int64_t ogma_host_echo(int64_t arg) {
  return proof_context_call(proof_host_function, &arg, 1, 7);
}

static void aot_reset_completion(void) {
  memset(&aot_last_completion, 0, sizeof(aot_last_completion));
  aot_last_completion.kind = OGMA_AOT_COMPLETION_NORMAL;
  aot_last_completion.error_kind = OGMA_AOT_ERROR_NONE;
  aot_last_status = OGMA_STATUS_OK;
}

static int64_t aot_finish_result(void) {
  if (aot_last_status != OGMA_STATUS_OK) return INT64_MIN;
  if (aot_last_completion.kind == OGMA_AOT_COMPLETION_THROW) {
    if (aot_last_completion.error_kind == OGMA_AOT_ERROR_NONE
        || aot_last_completion.error_kind > OGMA_AOT_ERROR_REFERENCE
        || aot_last_completion.value.reserved != 0
        || aot_last_completion.value.tag > OGMA_AOT_VALUE_HANDLE
        || (aot_last_completion.value.tag == OGMA_AOT_VALUE_BOOLEAN
            && aot_last_completion.value.payload.boolean > 1)) {
      aot_last_status = OGMA_STATUS_INVALID_COMPLETION;
    }
    return INT64_MIN;
  }
  if (aot_last_completion.kind != OGMA_AOT_COMPLETION_NORMAL
      || aot_last_completion.error_kind != OGMA_AOT_ERROR_NONE
      || aot_last_completion.details != 0) {
    aot_last_status = OGMA_STATUS_INVALID_COMPLETION;
    return INT64_MIN;
  }
  if (aot_last_completion.value.tag != OGMA_AOT_VALUE_NUMBER
      || aot_last_completion.value.reserved != 0) {
    aot_last_status = OGMA_STATUS_TYPE_MISMATCH;
    return INT64_MIN;
  }
  const double number = aot_last_completion.value.payload.number;
  if (!isfinite(number) || number >= 9223372036854775808.0
      || number < -9223372036854775808.0) {
    aot_last_status = OGMA_STATUS_OUT_OF_RANGE;
    return INT64_MIN;
  }
  return (int64_t)number;
}

static OgmaStatus aot_validate_table(
    const OgmaAotHostFunctionTableV1 *table, int32_t slot,
    const OgmaAotHostFunctionEntryV1 **out_entry) {
  if (table == NULL || out_entry == NULL) return OGMA_STATUS_INVALID_ARGUMENT;
  if (table->abi_major != OGMA_AOT_HOST_ABI_MAJOR
      || table->abi_minor > OGMA_AOT_HOST_ABI_MINOR) {
    return OGMA_STATUS_INCOMPATIBLE_ABI;
  }
  if (table->struct_size != sizeof(OgmaAotHostFunctionTableV1)
      || table->entry_size != sizeof(OgmaAotHostFunctionEntryV1)
      || table->entry_count == 0
      || table->entry_count > OGMA_AOT_MAX_ENTRIES
      || table->entries == NULL
      || table->table_version == 0
      || table->reserved != 0
      || slot < 0 || (uint32_t)slot >= table->entry_count) {
    return OGMA_STATUS_INVALID_ARGUMENT;
  }

  const OgmaAotHostFunctionEntryV1 *entry = &table->entries[(uint32_t)slot];
  if (entry->abi_major != OGMA_AOT_HOST_ABI_MAJOR
      || entry->abi_minor > OGMA_AOT_HOST_ABI_MINOR
      || entry->descriptor_version != OGMA_AOT_HOST_DESCRIPTOR_VERSION) {
    return OGMA_STATUS_INCOMPATIBLE_ABI;
  }
  if (entry->struct_size != sizeof(OgmaAotHostFunctionEntryV1)
      || entry->reserved != 0
      || entry->function_id == 0
      || entry->user_data == NULL
      || entry->generic_thunk == NULL) {
    return OGMA_STATUS_INVALID_ARGUMENT;
  }
  const uint32_t known_effects = OGMA_HOST_EFFECT_MAY_ALLOCATE
      | OGMA_HOST_EFFECT_MAY_GC | OGMA_HOST_EFFECT_MAY_THROW
      | OGMA_HOST_EFFECT_MAY_SUSPEND | OGMA_HOST_EFFECT_MAY_REENTER;
  if ((entry->effects & ~known_effects) != 0) {
    return OGMA_STATUS_UNSUPPORTED_FEATURE;
  }
  *out_entry = entry;
  return OGMA_STATUS_OK;
}

int64_t ogma_aot_dispatch_f64(
    int64_t table_address, int32_t slot, double left, double right,
    uint64_t capabilities) {
  aot_reset_completion();
  if (table_address == 0) {
    aot_last_status = OGMA_STATUS_INVALID_ARGUMENT;
    return INT64_MIN;
  }
  const OgmaAotHostFunctionTableV1 *table =
      (const OgmaAotHostFunctionTableV1 *)(uintptr_t)table_address;
  const OgmaAotHostFunctionEntryV1 *entry = NULL;
  aot_last_status = aot_validate_table(table, slot, &entry);
  if (aot_last_status != OGMA_STATUS_OK) return INT64_MIN;
  if (entry->signature != OGMA_HOST_SIGNATURE_F64_BINARY
      || entry->signature_id != UINT64_C(0x1001)
      || entry->primitive_f64_thunk == NULL) {
    aot_last_status = OGMA_STATUS_TYPE_MISMATCH;
    return INT64_MIN;
  }

  capabilities &= table->capabilities;
  if ((entry->effects & OGMA_AOT_COMPLEX_EFFECT_MASK) != 0) {
    const OgmaAotValueV1 args[2] = {
      { .tag = OGMA_AOT_VALUE_NUMBER, .reserved = 0, .payload.number = left },
      { .tag = OGMA_AOT_VALUE_NUMBER, .reserved = 0, .payload.number = right }
    };
    aot_last_status = entry->generic_thunk(
        entry->user_data, capabilities, args, 2, &aot_last_completion);
    if (aot_last_status == OGMA_STATUS_OK) aot_generic_calls++;
  } else {
    aot_last_status = entry->primitive_f64_thunk(
        entry->user_data, capabilities, left, right, &aot_last_completion);
    if (aot_last_status == OGMA_STATUS_OK) aot_typed_calls++;
  }
  return aot_finish_result();
}

int64_t ogma_aot_dispatch_handles(
    int64_t table_address, int32_t slot, int64_t left_handle_address,
    int64_t right_handle_address, uint64_t capabilities) {
  aot_reset_completion();
  if (table_address == 0 || left_handle_address == 0 || right_handle_address == 0) {
    aot_last_status = OGMA_STATUS_INVALID_ARGUMENT;
    return INT64_MIN;
  }
  const OgmaAotHostFunctionTableV1 *table =
      (const OgmaAotHostFunctionTableV1 *)(uintptr_t)table_address;
  const OgmaAotHostFunctionEntryV1 *entry = NULL;
  aot_last_status = aot_validate_table(table, slot, &entry);
  if (aot_last_status != OGMA_STATUS_OK) return INT64_MIN;
  if (entry->signature != OGMA_HOST_SIGNATURE_NUMBER_HANDLE_BINARY
      || entry->signature_id != UINT64_C(0x1002)
      || entry->number_handle_f64_thunk == NULL) {
    aot_last_status = OGMA_STATUS_TYPE_MISMATCH;
    return INT64_MIN;
  }

  OgmaHandle left;
  OgmaHandle right;
  memcpy(&left, (const void *)(uintptr_t)left_handle_address, sizeof(left));
  memcpy(&right, (const void *)(uintptr_t)right_handle_address, sizeof(right));
  capabilities &= table->capabilities;
  if ((entry->effects & OGMA_AOT_COMPLEX_EFFECT_MASK) != 0) {
    const OgmaAotValueV1 args[2] = {
      { .tag = OGMA_AOT_VALUE_HANDLE, .reserved = 0, .payload.handle = left },
      { .tag = OGMA_AOT_VALUE_HANDLE, .reserved = 0, .payload.handle = right }
    };
    aot_last_status = entry->generic_thunk(
        entry->user_data, capabilities, args, 2, &aot_last_completion);
    if (aot_last_status == OGMA_STATUS_OK) aot_generic_calls++;
  } else {
    aot_last_status = entry->number_handle_f64_thunk(
        entry->user_data, capabilities, left, right, &aot_last_completion);
    if (aot_last_status == OGMA_STATUS_OK) aot_typed_calls++;
  }
  aot_handle_calls++;
  return aot_finish_result();
}

"#;
fn link(object: &Path, shared: &Path, helper: &Path) -> Result<u64> {
    fs::write(helper, HELPERS)?;
    let start = Instant::now();
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let abi_include = manifest_dir.join("../rust-abi/include");
    let typed_include = manifest_dir.join("../typed-host-dispatch/include");
    let aot_include = manifest_dir.join("include");
    let out = Command::new("cc")
        .args(["-shared", "-fPIC"])
        .arg("-I")
        .arg(abi_include)
        .arg("-I")
        .arg(typed_include)
        .arg("-I")
        .arg(aot_include)
        .arg(object)
        .arg("-x")
        .arg("c")
        .arg(helper)
        .arg("-o")
        .arg(shared)
        .output()?;
    if !out.status.success() {
        bail!(
            "native link failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(start.elapsed().as_nanos() as u64)
}
#[derive(Serialize)]
struct Distribution {
    p50_ns_per_call: f64,
    p95_ns_per_call: f64,
    p99_ns_per_call: f64,
    throughput_calls_per_second: f64,
    raw_ns_per_call_samples: Vec<f64>,
}
#[derive(Serialize)]
struct LatencyDistribution {
    p50_ns: f64,
    p95_ns: f64,
    p99_ns: f64,
    raw_samples_ns: Vec<f64>,
}
fn distribution(samples: Vec<f64>) -> Distribution {
    let mut s = samples.clone();
    s.sort_by(f64::total_cmp);
    let p = |q: f64| s[((s.len() - 1) as f64 * q).ceil() as usize];
    let median = p(0.5);
    Distribution {
        p50_ns_per_call: median,
        p95_ns_per_call: p(0.95),
        p99_ns_per_call: p(0.99),
        throughput_calls_per_second: 1e9 / median,
        raw_ns_per_call_samples: samples,
    }
}
fn latency_distribution(samples: Vec<f64>) -> LatencyDistribution {
    let mut s = samples.clone();
    s.sort_by(f64::total_cmp);
    let p = |q: f64| s[((s.len() - 1) as f64 * q).ceil() as usize];
    LatencyDistribution {
        p50_ns: p(0.5),
        p95_ns: p(0.95),
        p99_ns: p(0.99),
        raw_samples_ns: samples,
    }
}
fn bench<F: FnMut(u64)>(o: &Options, mut f: F) -> Distribution {
    for _ in 0..o.warmups {
        for n in 0..o.iterations {
            f(black_box(n));
        }
    }
    let mut samples = Vec::with_capacity(o.samples);
    for _ in 0..o.samples {
        let start = Instant::now();
        for n in 0..o.iterations {
            f(black_box(n));
        }
        samples.push(start.elapsed().as_nanos() as f64 / o.iterations as f64);
    }
    distribution(samples)
}
#[derive(Serialize)]
struct BackendReport {
    first_compile_ns: u64,
    compile_time_ns: LatencyDistribution,
    first_link_ns: u64,
    link_time_ns: LatencyDistribution,
    object_bytes: u64,
    shared_library_bytes: u64,
    native_object_path: Option<String>,
    semantic_observations: JsonValue,
    runtime_helper_calls: JsonValue,
    runtime_integration: JsonValue,
    workloads: JsonValue,
}
fn compile_once(backend: &str, path: &Path, c: &CoreCorpus) -> Result<u64> {
    if backend == "cranelift" {
        compile_cranelift(path, c)
    } else {
        compile_llvm(path, c)
    }
}

fn compile_rounds(
    backend: &str,
    root: &Path,
    c: &CoreCorpus,
    o: &Options,
) -> Result<(u64, Vec<f64>, PathBuf)> {
    let first_path = root.join(format!("{backend}-compile-first.o"));
    let first_compile_ns = compile_once(backend, &first_path, c)?;

    for n in 0..o.warmups {
        let path = root.join(format!("{backend}-compile-warmup-{n}.o"));
        compile_once(backend, &path, c)?;
    }

    let mut samples = Vec::with_capacity(o.samples);
    let mut last = PathBuf::new();
    for n in 0..o.samples {
        last = root.join(format!("{backend}-compile-{n}.o"));
        samples.push(compile_once(backend, &last, c)? as f64);
    }
    Ok((first_compile_ns, samples, last))
}

fn link_rounds(
    object: &Path,
    shared: &Path,
    helper: &Path,
    o: &Options,
) -> Result<(u64, Vec<f64>)> {
    let first_link_ns = link(object, shared, helper)?;
    for _ in 0..o.warmups {
        link(object, shared, helper)?;
    }
    let mut samples = Vec::with_capacity(o.samples);
    for _ in 0..o.samples {
        samples.push(link(object, shared, helper)? as f64);
    }
    Ok((first_link_ns, samples))
}
fn run_backend(name: &str, o: &Options, root: &Path, c: &CoreCorpus) -> Result<BackendReport> {
    let (first_compile_ns, compile, object) = compile_rounds(name, root, c, o)?;
    let shared = root.join(format!("{name}.so"));
    let helper = root.join(format!("{name}.c"));
    let (first_link_ns, link_samples) = link_rounds(&object, &shared, &helper, o)?;
    let lib = unsafe { Library::new(&shared)? };

    type ResetCounters = unsafe extern "C" fn();
    type CountHelper = unsafe extern "C" fn(u32) -> u64;
    type LastCallReceiver = unsafe extern "C" fn() -> i64;
    type LastCallArgument = unsafe extern "C" fn() -> *const std::ffi::c_char;

    let reset: ResetCounters = unsafe { *lib.get(b"ogma_proof_reset\0")? };
    let count: CountHelper = unsafe { *lib.get(b"ogma_proof_count\0")? };
    let safepoint_count: ProofCounter = unsafe { *lib.get(b"ogma_proof_safepoint_count\0")? };
    let last_safepoint_status: ProofStatus =
        unsafe { *lib.get(b"ogma_proof_last_safepoint_status\0")? };
    let last_abi_status: ProofStatusU32 = unsafe { *lib.get(b"ogma_proof_last_abi_status\0")? };
    let last_completion_kind: ProofCompletionKind =
        unsafe { *lib.get(b"ogma_proof_last_completion_kind\0")? };
    let last_completion_payload: ProofCompletionPayload =
        unsafe { *lib.get(b"ogma_proof_last_completion_payload\0")? };
    let last_call_receiver: LastCallReceiver =
        unsafe { *lib.get(b"ogma_proof_last_call_receiver\0")? };
    let last_call_argument: LastCallArgument =
        unsafe { *lib.get(b"ogma_proof_last_call_argument\0")? };
    let aot_last_status: ProofStatusU32 = unsafe { *lib.get(b"ogma_aot_last_status\0")? };
    let aot_last_completion_kind: ProofStatusU32 =
        unsafe { *lib.get(b"ogma_aot_last_completion_kind\0")? };
    let aot_last_error_kind: ProofStatusU32 = unsafe { *lib.get(b"ogma_aot_last_error_kind\0")? };
    let aot_last_details: AotDetails = unsafe { *lib.get(b"ogma_aot_last_details\0")? };
    let aot_last_number: AotNumber = unsafe { *lib.get(b"ogma_aot_last_number\0")? };
    let aot_typed_calls: AotMetric = unsafe { *lib.get(b"ogma_aot_typed_calls\0")? };
    let aot_generic_calls: AotMetric = unsafe { *lib.get(b"ogma_aot_generic_calls\0")? };
    let aot_handle_calls: AotMetric = unsafe { *lib.get(b"ogma_aot_handle_calls\0")? };
    // SAFETY: this helper resets only its private counters and trace snapshots.
    unsafe { reset() };

    // Negotiate the existing versioned OGM-19 C ABI and register real callback-backed
    // mock functions. The native object calls through a C function table populated
    // with these exported ABI entry points; it does not call the callbacks directly.
    let required_features = abi::FEATURE_OPAQUE_HANDLES
        | abi::FEATURE_ROOT_SCOPES
        | abi::FEATURE_PERSISTENT_HANDLES
        | abi::FEATURE_CALLBACKS
        | abi::FEATURE_COMPLETIONS
        | abi::FEATURE_MOCK_PROOF;
    let mut abi_version = abi::OgmaAbiVersion::default();
    let status = unsafe { abi::ogma_abi_negotiate(1, 0, required_features, &mut abi_version) };
    if status != abi::OgmaStatus::OK {
        bail!("{name}: OGM-19 ABI negotiation failed: {status:?}");
    }
    if abi_version.major != 1 || abi_version.reserved != 0 || abi_version.reserved2 != 0 {
        bail!("{name}: unexpected negotiated OGM-19 ABI descriptor");
    }

    let mut runtime = ptr::null_mut();
    let status = unsafe { abi::ogma_runtime_create(&mut runtime) };
    if status != abi::OgmaStatus::OK || runtime.is_null() {
        bail!("{name}: OGM-19 runtime creation failed: {status:?}");
    }
    let mut context = abi::OgmaContextHandle::default();
    let status = unsafe { abi::ogma_context_create(runtime, &mut context) };
    if status != abi::OgmaStatus::OK {
        bail!("{name}: OGM-19 context creation failed: {status:?}");
    }

    let mut proof_state = NativeProofState::new()?;
    let aot_table = AotHostFunctionTable::new(proof_state.budget.cancellation_token())
        .map_err(|error| anyhow!("{name}: create OGM-32 host table: {error:?}"))?;
    let aot_registry = aot_table.registry();
    let aot_left = aot_registry
        .create_number(19.0)
        .map_err(|error| anyhow!("{name}: create OGM-32 left handle: {error:?}"))?;
    let aot_right = aot_registry
        .create_number(23.0)
        .map_err(|error| anyhow!("{name}: create OGM-32 right handle: {error:?}"))?;
    let aot_boolean = aot_registry
        .create_boolean(true)
        .map_err(|error| anyhow!("{name}: create OGM-32 mismatch handle: {error:?}"))?;
    let _aot_left_root = aot_registry
        .root_handle(aot_left)
        .map_err(|error| anyhow!("{name}: root OGM-32 left handle: {error:?}"))?;
    let _aot_right_root = aot_registry
        .root_handle(aot_right)
        .map_err(|error| anyhow!("{name}: root OGM-32 right handle: {error:?}"))?;
    let _aot_boolean_root = aot_registry
        .root_handle(aot_boolean)
        .map_err(|error| anyhow!("{name}: root OGM-32 signature-mismatch handle: {error:?}"))?;
    proof_state.bind_aot_roots(aot_registry, vec![(aot_left, 19.0), (aot_right, 23.0)]);
    let state_ptr = (&mut *proof_state as *mut NativeProofState).cast::<c_void>();
    let mut dynamic_callback = abi::OgmaCallbackHandle::default();
    let status = unsafe {
        abi::ogma_callback_register(
            runtime,
            context,
            Some(dynamic_add_host_callback),
            state_ptr,
            &mut dynamic_callback,
        )
    };
    if status != abi::OgmaStatus::OK {
        bail!("{name}: register OGM-19 dynamic-add callback failed: {status:?}");
    }
    let mut host_callback = abi::OgmaCallbackHandle::default();
    let status = unsafe {
        abi::ogma_callback_register(
            runtime,
            context,
            Some(host_echo_callback),
            state_ptr,
            &mut host_callback,
        )
    };
    if status != abi::OgmaStatus::OK {
        bail!("{name}: register OGM-19 host-echo callback failed: {status:?}");
    }
    let mut dynamic_function = abi::OgmaHandle::default();
    let status = unsafe {
        abi::ogma_mock_function_create(runtime, context, dynamic_callback, &mut dynamic_function)
    };
    if status != abi::OgmaStatus::OK {
        bail!("{name}: create OGM-19 dynamic-add function failed: {status:?}");
    }
    let mut host_function = abi::OgmaHandle::default();
    let status = unsafe {
        abi::ogma_mock_function_create(runtime, context, host_callback, &mut host_function)
    };
    if status != abi::OgmaStatus::OK {
        bail!("{name}: create OGM-19 host-echo function failed: {status:?}");
    }
    let mut dynamic_persistent = abi::OgmaPersistentHandle::default();
    let status = unsafe {
        abi::ogma_persistent_create(runtime, context, dynamic_function, &mut dynamic_persistent)
    };
    if status != abi::OgmaStatus::OK {
        bail!("{name}: persist dynamic-add function failed: {status:?}");
    }
    let mut host_persistent = abi::OgmaPersistentHandle::default();
    let status = unsafe {
        abi::ogma_persistent_create(runtime, context, host_function, &mut host_persistent)
    };
    if status != abi::OgmaStatus::OK {
        bail!("{name}: persist host-echo function failed: {status:?}");
    }

    let proof_api = OgmaProofAbi {
        version: abi_version,
        context_create_number: abi::ogma_context_create_number,
        context_get_number: abi::ogma_context_get_number,
        root_scope_open: abi::ogma_root_scope_open,
        root_scope_add: abi::ogma_root_scope_add,
        root_scope_close: abi::ogma_root_scope_close,
        context_call: abi::ogma_context_call,
    };
    let bind: BindProofAbi = unsafe { *lib.get(b"ogma_proof_bind_abi\0")? };
    // SAFETY: the helper copies the table and token values. proof_state remains boxed
    // and both ABI objects remain alive until all generated calls have completed.
    unsafe {
        bind(
            &proof_api,
            runtime,
            context,
            dynamic_function,
            host_function,
            native_safepoint_callback,
            state_ptr,
        );
    }

    let branch: BinaryF64 = unsafe { *lib.get(sym(&c.symbol("branch")?).as_slice())? };
    let loop_fn: TernaryF64 = unsafe { *lib.get(sym(&c.symbol("loop")?).as_slice())? };
    let exception: BinaryI64 =
        unsafe { *lib.get(sym(&c.symbol("dynamic_exception")?).as_slice())? };
    let call: CallBoundary = unsafe { *lib.get(sym(&c.symbol("call_boundary")?).as_slice())? };
    let call_target: CallTarget = unsafe { *lib.get(b"ogma_js_call_target\0")? };
    let callee = call_target as usize as i64;
    let text = CString::new("item=").unwrap();
    let text_handle = text.as_ptr() as i64;
    let host: UnaryI64 = unsafe { *lib.get(sym(&c.symbol("host_call")?).as_slice())? };
    let aot_host_f64: AotDispatchF64 =
        unsafe { *lib.get(sym(&c.symbol("aot_host_call_f64")?).as_slice())? };
    let aot_host_handles: AotDispatchHandles =
        unsafe { *lib.get(sym(&c.symbol("aot_host_call_handles")?).as_slice())? };

    let branch_equal = unsafe { branch(7.0, 7.0) };
    let branch_unequal = unsafe { branch(7.0, 8.0) };
    if branch_equal != 7.0 || branch_unequal != 8.0 {
        bail!("{name}: branch mismatch");
    }
    if unsafe { branch(0.0, -0.0) }.to_bits() != 0.0_f64.to_bits() {
        bail!("{name}: strict equality must treat positive and negative zero as equal");
    }
    if unsafe { branch(f64::NAN, 3.0) } != 3.0 || !unsafe { branch(3.0, f64::NAN) }.is_nan() {
        bail!("{name}: strict equality must treat NaN as unequal");
    }
    if unsafe { last_safepoint_status() } != 0 {
        bail!("{name}: OGM-27 safepoint callback failed during branch execution");
    }

    let loop_integer = unsafe { loop_fn(0.0, 5.0, 1.0) };
    let loop_fractional = unsafe { loop_fn(0.5, 3.5, 0.75) };
    if loop_integer != 5.0 || loop_fractional != 3.5 {
        bail!("{name}: loop-carried value mismatch");
    }

    let dynamic_add_normal = unsafe { exception(7, 8) };
    let dynamic_normal_kind = unsafe { last_completion_kind() };
    let dynamic_normal_payload = unsafe { last_completion_payload() };
    if dynamic_add_normal != 15
        || dynamic_normal_kind != abi::COMPLETION_NORMAL
        || dynamic_normal_payload != 15
    {
        bail!("{name}: normal JS addition did not preserve the OGM-19 Normal completion");
    }
    let dynamic_add_throw = unsafe { exception(i64::MIN, 9) };
    let dynamic_throw_kind = unsafe { last_completion_kind() };
    let dynamic_throw_payload = unsafe { last_completion_payload() };
    if dynamic_add_throw != i64::MIN
        || dynamic_throw_kind != abi::COMPLETION_THROW
        || dynamic_throw_payload != i64::MIN
    {
        bail!("{name}: Symbol-conversion failure did not preserve OGM-19 Throw completion");
    }

    let call_boundary = unsafe { call(callee, 2, text_handle, 4, 4, 5.0) };
    if call_boundary != 8 {
        bail!("{name}: call-boundary string/result mismatch");
    }
    let call_arg_ptr = unsafe { last_call_argument() };
    if call_arg_ptr.is_null() {
        bail!("{name}: host callback did not record its string argument");
    }
    let call_argument = unsafe { std::ffi::CStr::from_ptr(call_arg_ptr) }
        .to_str()
        .context("call-boundary callback argument was not UTF-8")?;
    let call_receiver = unsafe { last_call_receiver() };
    if call_receiver != 2 || call_argument != "item=5" {
        bail!("{name}: call-boundary receiver/argument trace mismatch");
    }

    let host_call = unsafe { host(31) };
    let host_normal_kind = unsafe { last_completion_kind() };
    let host_normal_payload = unsafe { last_completion_payload() };
    if host_call != 42 || host_normal_kind != abi::COMPLETION_NORMAL || host_normal_payload != 42 {
        bail!("{name}: host capability did not travel through the OGM-19 Normal completion");
    }
    let host_repeat = unsafe { host(32) };
    if host_repeat != 43
        || unsafe { last_completion_kind() } != abi::COMPLETION_NORMAL
        || unsafe { last_completion_payload() } != 43
    {
        bail!("{name}: repeated OGM-19 host callback changed its completion");
    }
    let host_throw = unsafe { host(-7) };
    let host_throw_kind = unsafe { last_completion_kind() };
    let host_throw_payload = unsafe { last_completion_payload() };
    if host_throw != -7 || host_throw_kind != abi::COMPLETION_THROW || host_throw_payload != -7 {
        bail!("{name}: OGM-19 Throw completion did not survive the native host boundary");
    }

    // OGM-32 interpreter reference: this calls the same OGM-21 registry slots
    // through the interpreter-side dispatch path before exercising generated code.
    let aot_interpreter_typed = aot_table
        .invoke_interpreter(
            0,
            AOT_CAPABILITY_MATH,
            &[
                ogma_typed_host_dispatch_proof::DispatchValue::Number(31.0),
                ogma_typed_host_dispatch_proof::DispatchValue::Number(11.0),
            ],
            false,
        )
        .map_err(|error| anyhow!("{name}: OGM-32 typed interpreter reference: {error:?}"))?;
    let aot_interpreter_generic = aot_table
        .invoke_interpreter(
            1,
            AOT_CAPABILITY_MATH,
            &[
                ogma_typed_host_dispatch_proof::DispatchValue::Number(12.0),
                ogma_typed_host_dispatch_proof::DispatchValue::Number(30.0),
            ],
            true,
        )
        .map_err(|error| anyhow!("{name}: OGM-32 generic interpreter reference: {error:?}"))?;
    let aot_interpreter_throw = aot_table
        .invoke_interpreter(
            1,
            AOT_CAPABILITY_MATH,
            &[
                ogma_typed_host_dispatch_proof::DispatchValue::Number(-7.0),
                ogma_typed_host_dispatch_proof::DispatchValue::Number(1.0),
            ],
            true,
        )
        .map_err(|error| anyhow!("{name}: OGM-32 throw interpreter reference: {error:?}"))?;
    let aot_interpreter_typed_handle = aot_table
        .invoke_interpreter(
            2,
            AOT_CAPABILITY_MATH,
            &[
                ogma_typed_host_dispatch_proof::DispatchValue::Handle(aot_left),
                ogma_typed_host_dispatch_proof::DispatchValue::Handle(aot_right),
            ],
            false,
        )
        .map_err(|error| anyhow!("{name}: OGM-32 typed-handle interpreter reference: {error:?}"))?;
    let aot_interpreter_generic_handle = aot_table
        .invoke_interpreter(
            3,
            AOT_CAPABILITY_MATH,
            &[
                ogma_typed_host_dispatch_proof::DispatchValue::Handle(aot_left),
                ogma_typed_host_dispatch_proof::DispatchValue::Handle(aot_right),
            ],
            true,
        )
        .map_err(|error| {
            anyhow!("{name}: OGM-32 generic-handle interpreter reference: {error:?}")
        })?;
    if aot_interpreter_typed
        != ogma_typed_host_dispatch_proof::JsCompletion::Normal(
            ogma_typed_host_dispatch_proof::DispatchValue::Number(42.0),
        )
        || aot_interpreter_generic
            != ogma_typed_host_dispatch_proof::JsCompletion::Normal(
                ogma_typed_host_dispatch_proof::DispatchValue::Number(42.0),
            )
        || aot_interpreter_throw
            != ogma_typed_host_dispatch_proof::JsCompletion::Throw(
                ogma_typed_host_dispatch_proof::JsException::Thrown(
                    ogma_typed_host_dispatch_proof::DispatchValue::Number(-7.0),
                ),
            )
        || aot_interpreter_typed_handle
            != ogma_typed_host_dispatch_proof::JsCompletion::Normal(
                ogma_typed_host_dispatch_proof::DispatchValue::Number(42.0),
            )
        || aot_interpreter_generic_handle
            != ogma_typed_host_dispatch_proof::JsCompletion::Normal(
                ogma_typed_host_dispatch_proof::DispatchValue::Number(42.0),
            )
    {
        bail!("{name}: OGM-32 interpreter typed/generic/throw reference semantics changed");
    }

    // OGM-32: execute generated Cranelift/LLVM entry points that dispatch only
    // through the fixed C ABI table. Keep root checking enabled for these native
    // calls, then disable it before the OGM-31 workload timing loops.
    let aot_table_ptr = aot_table.as_abi() as *const AotHostFunctionTableV1 as usize as i64;
    let aot_left_ptr = &aot_left as *const abi::OgmaHandle as usize as i64;
    let aot_right_ptr = &aot_right as *const abi::OgmaHandle as usize as i64;
    let aot_boolean_ptr = &aot_boolean as *const abi::OgmaHandle as usize as i64;
    proof_state.set_aot_root_checks_enabled(true);

    let aot_typed_primitive_result =
        unsafe { aot_host_f64(aot_table_ptr, 0, 31.0, 11.0, AOT_CAPABILITY_MATH) };
    if aot_typed_primitive_result != 42
        || unsafe { aot_last_status() } != 0
        || unsafe { aot_last_completion_kind() } != 0
        || unsafe { aot_last_number() } != 42.0
    {
        bail!("{name}: OGM-32 typed primitive C ABI dispatch failed");
    }

    let aot_generic_reentrant_result =
        unsafe { aot_host_f64(aot_table_ptr, 1, 12.0, 30.0, AOT_CAPABILITY_MATH) };
    if aot_generic_reentrant_result != 42
        || unsafe { aot_last_status() } != 0
        || unsafe { aot_last_completion_kind() } != 0
    {
        bail!("{name}: OGM-32 generic fallback or nested callback reentrancy failed");
    }

    let aot_generic_throw_result =
        unsafe { aot_host_f64(aot_table_ptr, 1, -7.0, 1.0, AOT_CAPABILITY_MATH) };
    let aot_generic_throw_kind = unsafe { aot_last_completion_kind() };
    let aot_generic_throw_error = unsafe { aot_last_error_kind() };
    let aot_generic_throw_value = unsafe { aot_last_number() };
    if aot_generic_throw_result != i64::MIN
        || unsafe { aot_last_status() } != 0
        || aot_generic_throw_kind != 1
        || aot_generic_throw_error != AOT_ERROR_THROWN
        || aot_generic_throw_value != -7.0
    {
        bail!("{name}: OGM-32 generic Throw completion did not cross the C ABI");
    }

    let aot_typed_handle_result = unsafe {
        aot_host_handles(
            aot_table_ptr,
            2,
            aot_left_ptr,
            aot_right_ptr,
            AOT_CAPABILITY_MATH,
        )
    };
    if aot_typed_handle_result != 42
        || unsafe { aot_last_status() } != 0
        || unsafe { aot_last_completion_kind() } != 0
    {
        bail!("{name}: OGM-32 typed opaque-handle dispatch failed");
    }

    let aot_generic_handle_result = unsafe {
        aot_host_handles(
            aot_table_ptr,
            3,
            aot_left_ptr,
            aot_right_ptr,
            AOT_CAPABILITY_MATH,
        )
    };
    if aot_generic_handle_result != 42
        || unsafe { aot_last_status() } != 0
        || unsafe { aot_last_completion_kind() } != 0
    {
        bail!("{name}: OGM-32 generic opaque-handle fallback failed");
    }

    let aot_signature_mismatch_result = unsafe {
        aot_host_handles(
            aot_table_ptr,
            2,
            aot_boolean_ptr,
            aot_right_ptr,
            AOT_CAPABILITY_MATH,
        )
    };
    let aot_signature_mismatch_error = unsafe { aot_last_error_kind() };
    if aot_signature_mismatch_result != i64::MIN
        || unsafe { aot_last_status() } != 0
        || unsafe { aot_last_completion_kind() } != 1
        || aot_signature_mismatch_error != AOT_ERROR_TYPE
    {
        bail!("{name}: OGM-32 signature mismatch was not a JavaScript TypeError completion");
    }

    let aot_capability_result = unsafe { aot_host_f64(aot_table_ptr, 0, 1.0, 2.0, 0) };
    let aot_capability_error = unsafe { aot_last_error_kind() };
    let aot_capability_details = unsafe { aot_last_details() };
    if aot_capability_result != i64::MIN
        || unsafe { aot_last_status() } != 0
        || unsafe { aot_last_completion_kind() } != 1
        || aot_capability_error != AOT_ERROR_CAPABILITY
        || aot_capability_details != AOT_CAPABILITY_MATH
    {
        bail!("{name}: OGM-32 capability denial did not map to a Throw completion");
    }

    let mut incompatible_aot_table = *aot_table.as_abi();
    incompatible_aot_table.abi_major = AOT_ABI_MAJOR + 1;
    let incompatible_aot_table_ptr =
        &incompatible_aot_table as *const AotHostFunctionTableV1 as usize as i64;
    let aot_abi_mismatch_result =
        unsafe { aot_host_f64(incompatible_aot_table_ptr, 0, 1.0, 2.0, AOT_CAPABILITY_MATH) };
    let aot_abi_mismatch_status = unsafe { aot_last_status() };
    if aot_abi_mismatch_result != i64::MIN || aot_abi_mismatch_status != 2 {
        bail!("{name}: OGM-32 versioned C table did not reject an ABI major mismatch");
    }
    let (aot_generic_callback_invocations, aot_nested_callback_invocations) =
        aot_table.callback_stats();
    if aot_generic_callback_invocations < 5 || aot_nested_callback_invocations < 2 {
        bail!("{name}: OGM-32 typed registry callback/reentrancy evidence is incomplete");
    }
    let aot_pre_cancel_root_checks = proof_state.aot_root_checks;
    if aot_pre_cancel_root_checks == 0 {
        bail!("{name}: OGM-32 native calls did not validate opaque roots at safepoints");
    }
    proof_state.set_aot_root_checks_enabled(false);

    if unsafe { last_abi_status() } != 0 || unsafe { last_safepoint_status() } != 0 {
        bail!("{name}: an OGM-19 ABI operation or OGM-27 safepoint failed");
    }
    // Snapshot helper counts before the direct helper microbenchmarks below.
    let helper_names = [
        "js_add_number",
        "js_to_number",
        "js_add_completion",
        "js_throw",
        "js_add_mixed",
        "js_call",
        "host_call_target",
        "host_echo",
    ];
    let runtime_helper_calls = serde_json::json!({
        "js_add_number": unsafe { count(0) },
        "js_to_number": unsafe { count(1) },
        "js_add_completion": unsafe { count(2) },
        "js_throw": unsafe { count(3) },
        "js_add_mixed": unsafe { count(4) },
        "js_call": unsafe { count(5) },
        "host_call_target": unsafe { count(6) },
        "host_echo": unsafe { count(7) },
    });
    for helper in helper_names {
        if runtime_helper_calls[helper].as_u64().unwrap_or(0) == 0 {
            bail!("{name}: expected runtime helper {helper} was not exercised");
        }
    }

    let semantic_observations = serde_json::json!({
        "branch_equal": {"return": branch_equal, "host_trace": []},
        "branch_unequal": {"return": branch_unequal, "host_trace": []},
        "loop_integer": {"return": loop_integer, "host_trace": []},
        "loop_fractional": {"return": loop_fractional, "host_trace": []},
        "dynamic_add_normal": {"return": dynamic_add_normal, "host_trace": []},
        "dynamic_add_throw": {
            "throw": "TypeError: Cannot convert a Symbol value to a number",
            "host_trace": []
        },
        "call_boundary": {
            "return": call_boundary,
            "host_trace": [{
                "symbol": "benchmark.host.call_target",
                "args": [call_receiver, call_argument]
            }]
        },
        "host_call": {
            "return": host_call,
            "host_trace": [{
                "symbol": "benchmark.host.echo",
                "args": [31]
            }]
        }
    });

    let add_number: AddNumber = unsafe { *lib.get(b"ogma_js_add_number\0")? };
    let to_number: ToNumber = unsafe { *lib.get(b"ogma_js_to_number\0")? };
    let add_completion: BinaryI64 = unsafe { *lib.get(b"ogma_js_add_completion\0")? };
    let host_direct: UnaryI64 = unsafe { *lib.get(b"ogma_host_echo\0")? };
    for (left, right, expected) in [
        (1.25_f64, 2.75_f64, 4.0_f64),
        (-7.5_f64, 2.0_f64, -5.5_f64),
        (0.0_f64, -0.0_f64, 0.0_f64),
    ] {
        let boxed = unsafe { add_number(left, right) };
        let actual = unsafe { to_number(boxed) };
        if actual.to_bits() != expected.to_bits() {
            bail!("{name}: numeric add/to_number helper ABI mismatch");
        }
    }

    let workloads = serde_json::json!({
      "core_ir_ssa_branch":bench(o,|n|{black_box(unsafe{branch((n%17)as f64,(n%13)as f64)});}),
      "core_ir_loop_js_add_to_number":bench(o,|_|{black_box(unsafe{loop_fn(0.0,5.0,1.0)});}),
      "core_ir_dynamic_add_normal_completion":bench(o,|n|{black_box(unsafe{exception((n%100)as i64,7)});}),
      "core_ir_dynamic_add_throw_completion":bench(o,|_|{black_box(unsafe{exception(i64::MIN,7)});}),
      "core_ir_js_add_js_call_boundary":bench(o,|_|{black_box(unsafe{call(callee,2,text_handle,4,4,5.0)});}),
      "core_ir_host_call":bench(o,|n|{black_box(unsafe{host(n as i64)});}),
      "direct_js_add_number_helper":bench(o,|n|{black_box(unsafe{to_number(add_number(n as f64,1.0))});}),
      "direct_dynamic_completion_helper":bench(o,|n|{black_box(unsafe{add_completion(n as i64,7)});}),
      "direct_host_helper":bench(o,|n|{black_box(unsafe{host_direct(n as i64)});})
    });

    // Cancellation is deliberately the final generated native call: subsequent
    // OGM-27 budget polls must observe the cancelled token and reject execution.
    proof_state.set_aot_root_checks_enabled(true);
    let aot_cancel_result =
        unsafe { aot_host_f64(aot_table_ptr, 1, 99.0, 1.0, AOT_CAPABILITY_MATH) };
    let aot_cancel_status = unsafe { aot_last_status() };
    let aot_cancel_kind = unsafe { aot_last_completion_kind() };
    let aot_cancel_error = unsafe { aot_last_error_kind() };
    let aot_cancelled = proof_state.budget.cancellation_token().is_cancelled();
    if aot_cancel_result != i64::MIN
        || aot_cancel_status != 0
        || aot_cancel_kind != 1
        || aot_cancel_error != AOT_ERROR_CANCELLED
        || !aot_cancelled
    {
        bail!(
            "{name}: OGM-32 cancellation did not propagate through native completion and OGM-27 budget"
        );
    }
    proof_state.set_aot_root_checks_enabled(false);

    if unsafe { last_abi_status() } != 0 || unsafe { last_safepoint_status() } != 0 {
        bail!("{name}: an OGM-19 ABI operation or OGM-27 safepoint failed during measurements");
    }
    for site in 1..=5 {
        if unsafe { safepoint_count(site) } == 0 {
            bail!("{name}: required OGM-27 safepoint site {site} was not observed");
        }
    }
    let native_safepoints = serde_json::json!({
        "function_entry": unsafe { safepoint_count(1) },
        "loop_back_edge": unsafe { safepoint_count(2) },
        "allocation_boundary": unsafe { safepoint_count(3) },
        "call_boundary": unsafe { safepoint_count(4) },
        "host_call_boundary": unsafe { safepoint_count(5) }
    });
    let (aot_generic_callbacks, aot_nested_callbacks) = aot_table.callback_stats();
    let aot_evidence = serde_json::json!({
        "abi_version": {
            "major": aot_table.as_abi().abi_major,
            "minor": aot_table.as_abi().abi_minor
        },
        "table_version": aot_table.as_abi().table_version,
        "entry_count": aot_table.as_abi().entry_count,
        "interpreter_reference": {
            "typed": {
                "kind": "normal",
                "value": 42
            },
            "generic": {
                "kind": "normal",
                "value": 42
            },
            "throw": {
                "kind": "throw",
                "value": -7
            },
            "typed_handle": {
                "kind": "normal",
                "value": 42
            },
            "generic_handle": {
                "kind": "normal",
                "value": 42
            }
        },
        "typed_primitive": {
            "result": aot_typed_primitive_result,
            "completion_kind": 0,
            "error_kind": 0
        },
        "generic_reentrant": {
            "result": aot_generic_reentrant_result,
            "completion_kind": 0,
            "nested_callbacks": aot_nested_callback_invocations
        },
        "generic_throw": {
            "result": aot_generic_throw_result,
            "completion_kind": aot_generic_throw_kind,
            "error_kind": aot_generic_throw_error,
            "value": aot_generic_throw_value
        },
        "typed_handle": {
            "result": aot_typed_handle_result,
            "completion_kind": 0
        },
        "generic_handle_fallback": {
            "result": aot_generic_handle_result,
            "completion_kind": 0
        },
        "signature_mismatch": {
            "result": aot_signature_mismatch_result,
            "completion_kind": 1,
            "error_kind": aot_signature_mismatch_error
        },
        "capability_denied": {
            "result": aot_capability_result,
            "completion_kind": 1,
            "error_kind": aot_capability_error,
            "missing_capabilities": aot_capability_details
        },
        "abi_version_mismatch": {
            "result": aot_abi_mismatch_result,
            "status": aot_abi_mismatch_status,
            "expected_status": 2
        },
        "cancellation": {
            "result": aot_cancel_result,
            "status": aot_cancel_status,
            "completion_kind": aot_cancel_kind,
            "error_kind": aot_cancel_error,
            "budget_token_cancelled": aot_cancelled
        },
        "generic_callback_invocations": aot_generic_callbacks,
        "nested_callback_invocations": aot_nested_callbacks,
        "dispatch_counts": {
            "typed_calls": unsafe { aot_typed_calls() },
            "generic_calls": unsafe { aot_generic_calls() },
            "handle_calls": unsafe { aot_handle_calls() }
        },
        "opaque_roots": {
            "safepoint_checks": proof_state.aot_root_checks,
            "number_handles_survived_collection": aot_registry.read_number(aot_left).map_err(|error| anyhow!("{name}: read rooted left handle: {error:?}"))? == 19.0
                && aot_registry.read_number(aot_right).map_err(|error| anyhow!("{name}: read rooted right handle: {error:?}"))? == 23.0
        },
        "all_checks_passed": true
    });
    let model_evidence = proof_state.finish()?;
    let runtime_integration = serde_json::json!({
        "ogm19_version": {
            "major": abi_version.major,
            "minor": abi_version.minor,
            "patch": abi_version.patch,
            "features": abi_version.features,
            "negotiated_required_features": required_features
        },
        "ogm19_completions": {
            "dynamic_add_normal": {"kind": dynamic_normal_kind, "payload": dynamic_normal_payload},
            "dynamic_add_throw": {"kind": dynamic_throw_kind, "payload": dynamic_throw_payload},
            "host_echo_normal": {"kind": host_normal_kind, "payload": host_normal_payload},
            "host_echo_throw": {"kind": host_throw_kind, "payload": host_throw_payload}
        },
        "ogm19_last_status": unsafe { last_abi_status() },
        "ogm27_native_safepoints": native_safepoints,
        "ogm27_model": model_evidence,
        "ogm32_aot_host_calls": aot_evidence
    });

    // Release the mock Runtime only after every generated-code call has returned.
    for callback in [dynamic_callback, host_callback] {
        let status = unsafe { abi::ogma_callback_unregister(runtime, context, callback) };
        if status != abi::OgmaStatus::OK {
            bail!("{name}: unregister OGM-19 callback failed: {status:?}");
        }
    }
    for persistent in [dynamic_persistent, host_persistent] {
        let status = unsafe { abi::ogma_persistent_release(runtime, context, persistent) };
        if status != abi::OgmaStatus::OK {
            bail!("{name}: release OGM-19 persistent function root failed: {status:?}");
        }
    }
    let status = unsafe { abi::ogma_context_destroy(runtime, context) };
    if status != abi::OgmaStatus::OK {
        bail!("{name}: destroy OGM-19 context failed: {status:?}");
    }
    let status = unsafe { abi::ogma_runtime_destroy(&mut runtime) };
    if status != abi::OgmaStatus::OK || !runtime.is_null() {
        bail!("{name}: destroy OGM-19 runtime failed: {status:?}");
    }

    let native_object_path = if let Some(directory) = &o.artifact_dir {
        fs::create_dir_all(directory)?;
        let destination = directory.join(format!("{name}.o"));
        fs::copy(&object, &destination)?;
        Some(destination.display().to_string())
    } else {
        None
    };

    Ok(BackendReport {
        first_compile_ns,
        compile_time_ns: latency_distribution(compile),
        first_link_ns,
        link_time_ns: latency_distribution(link_samples),
        object_bytes: fs::metadata(&object)?.len(),
        shared_library_bytes: fs::metadata(&shared)?.len(),
        native_object_path,
        semantic_observations,
        runtime_helper_calls,
        runtime_integration,
        workloads,
    })
}

#[derive(Serialize)]
struct Report {
    schema_version: u32,
    task: &'static str,
    source_revision: String,
    tested_revision: String,
    rustc: String,
    llvm: String,
    llvm_target_cpu: String,
    llvm_target_features: String,
    clang: String,
    os: String,
    host_arch: &'static str,
    native_target_triple: String,
    cpu_model: String,
    build_configuration: &'static str,
    iterations_per_sample: u64,
    warmup_rounds: usize,
    measurement_samples: usize,
    target_lookup: JsonValue,
    llvm_package_footprint_kib: Option<u64>,
    llvm_package_sizes_kib: BTreeMap<String, u64>,
    llvm_package_footprint_packages: Vec<&'static str>,
    cargo_target_directory: String,
    cranelift_build_artifact_files: u64,
    llvm_binding_build_artifact_files: u64,
    cranelift_build_artifact_search_directory: String,
    llvm_binding_build_artifact_search_directory: String,
    cranelift_build_artifact_search_directory_exists: bool,
    llvm_binding_build_artifact_search_directory_exists: bool,
    cranelift_build_artifact_matching_files: Vec<String>,
    llvm_binding_build_artifact_matching_files: Vec<String>,
    cranelift_crate_source_bytes: u64,
    llvm_binding_source_bytes: u64,
    cranelift_build_artifact_bytes: u64,
    llvm_binding_build_artifact_bytes: u64,
    integration_notes: JsonValue,
    cranelift: BackendReport,
    llvm_backend: BackendReport,
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
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unavailable".into())
}

fn llvm_host_cpu_metadata() -> (String, String) {
    let cpu = TargetMachine::get_host_cpu_name();
    let features = TargetMachine::get_host_cpu_features();
    (
        cpu.to_str().unwrap_or("unavailable").to_owned(),
        features.to_str().unwrap_or("unavailable").to_owned(),
    )
}

fn command(p: &str, a: &[&str]) -> String {
    Command::new(p)
        .args(a)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_else(|| "unavailable".into())
}
fn target_lookup() -> JsonValue {
    Target::initialize_all(&InitializationConfig::default());
    let triples = [
        "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-gnu",
        "x86_64-pc-windows-msvc",
        "aarch64-apple-darwin",
    ];
    JsonValue::Array(triples.iter().map(|name|{let t:Triple=name.parse().unwrap();serde_json::json!({"triple":name,
 "cranelift":cranelift_codegen::isa::lookup(t).is_ok(),"llvm":Target::from_triple(&TargetTriple::create(name)).is_ok(),
 "scope":"target lookup only; no cross-link or ABI claim"})}).collect())
}
fn dir_size(p: &Path) -> u64 {
    let Ok(e) = fs::read_dir(p) else {
        return 0;
    };
    e.flatten()
        .map(|x| {
            if x.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                dir_size(&x.path())
            } else {
                x.metadata().map(|m| m.len()).unwrap_or(0)
            }
        })
        .sum()
}
fn cargo_source_size(prefixes: &[&str]) -> u64 {
    let home = env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".cargo")));
    let Some(home) = home else {
        return 0;
    };
    let Ok(indices) = fs::read_dir(home.join("registry").join("src")) else {
        return 0;
    };
    indices
        .flatten()
        .map(|i| {
            let Ok(pkgs) = fs::read_dir(i.path()) else {
                return 0;
            };
            pkgs.flatten()
                .filter_map(|p| {
                    let n = p.file_name().to_string_lossy().into_owned();
                    prefixes
                        .iter()
                        .any(|x| n.starts_with(x))
                        .then(|| dir_size(&p.path()))
                })
                .sum::<u64>()
        })
        .sum()
}
#[derive(Serialize)]
struct BuildArtifactSummary {
    files: u64,
    bytes: u64,
    search_directory: String,
    search_directory_exists: bool,
    matching_files: Vec<String>,
}

fn cargo_target_directory() -> PathBuf {
    env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            env::current_exe().ok().and_then(|executable| {
                executable
                    .parent()
                    .and_then(Path::parent)
                    .map(Path::to_path_buf)
            })
        })
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"))
}

fn cargo_artifact_summary(target_directory: &Path, crate_names: &[&str]) -> BuildArtifactSummary {
    let search_directory = target_directory.join("release");
    let mut summary = BuildArtifactSummary {
        files: 0,
        bytes: 0,
        search_directory: search_directory.display().to_string(),
        search_directory_exists: search_directory.is_dir(),
        matching_files: Vec::new(),
    };
    let mut pending = vec![search_directory.clone()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                pending.push(path);
                continue;
            }
            let Some(extension) = path.extension().and_then(|value| value.to_str()) else {
                continue;
            };
            if !matches!(extension, "rlib" | "rmeta" | "so" | "a" | "dylib" | "dll") {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if crate_names
                .iter()
                .any(|crate_name| name.contains(crate_name))
            {
                summary.files += 1;
                summary.bytes += entry.metadata().map(|metadata| metadata.len()).unwrap_or(0);
                summary.matching_files.push(
                    path.strip_prefix(&search_directory)
                        .unwrap_or(&path)
                        .display()
                        .to_string(),
                );
            }
        }
    }
    summary.matching_files.sort();
    summary
}
fn main() -> Result<()> {
    let o = options()?;
    let c = load_corpus()?;
    let root = env::temp_dir().join(format!("ogm30-{}", std::process::id()));
    fs::create_dir_all(&root)?;
    let cranelift = run_backend("cranelift", &o, &root, &c)?;
    let llvm_backend = run_backend("llvm", &o, &root, &c)?;
    let llvm_package_sizes_kib = Command::new("dpkg-query")
        .args([
            "-W",
            r"-f=${binary:Package}\t${Installed-Size}\n",
            "llvm-18-dev",
            "llvm-18",
            "clang-18",
        ])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter_map(|line| {
                    let (name, size) = line.split_once(char::from(9))?;
                    Some((name.to_owned(), size.parse::<u64>().ok()?))
                })
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    let footprint =
        (!llvm_package_sizes_kib.is_empty()).then(|| llvm_package_sizes_kib.values().sum::<u64>());
    let native_target_triple = command("rustc", &["-vV"])
        .lines()
        .find_map(|line| line.strip_prefix("host: ").map(str::to_owned))
        .context("rustc -vV did not report a host target triple")?;
    let cargo_target_directory = cargo_target_directory();
    let cranelift_artifacts = cargo_artifact_summary(&cargo_target_directory, &["cranelift"]);
    let llvm_artifacts = cargo_artifact_summary(&cargo_target_directory, &["inkwell", "llvm_sys"]);
    let report = Report {
        schema_version: 1,
        task: "OGM-30",
        source_revision: o.revision.clone(),
        tested_revision: o.revision.clone(),
        rustc: command("rustc", &["--version"]),
        llvm: command("llvm-config-18", &["--version"]),
        llvm_target_cpu: llvm_host_cpu_metadata().0,
        llvm_target_features: llvm_host_cpu_metadata().1,
        clang: command("clang-18", &["--version"]),
        os: command("uname", &["-sr"]),
        host_arch: env::consts::ARCH,
        native_target_triple,
        cpu_model: cpu_model(),
        build_configuration: "release; Cranelift opt_level=speed; LLVM OptimizationLevel::Default; host CPU/features matched; PIC",
        iterations_per_sample: o.iterations,
        warmup_rounds: o.warmups,
        measurement_samples: o.samples,
        target_lookup: target_lookup(),
        llvm_package_footprint_kib: footprint,
        llvm_package_sizes_kib,
        llvm_package_footprint_packages: vec!["llvm-18-dev", "llvm-18", "clang-18"],
        cargo_target_directory: cargo_target_directory.display().to_string(),
        cranelift_build_artifact_files: cranelift_artifacts.files,
        llvm_binding_build_artifact_files: llvm_artifacts.files,
        cranelift_build_artifact_search_directory: cranelift_artifacts.search_directory.clone(),
        llvm_binding_build_artifact_search_directory: llvm_artifacts.search_directory.clone(),
        cranelift_build_artifact_search_directory_exists: cranelift_artifacts
            .search_directory_exists,
        llvm_binding_build_artifact_search_directory_exists: llvm_artifacts.search_directory_exists,
        cranelift_build_artifact_matching_files: cranelift_artifacts.matching_files.clone(),
        llvm_binding_build_artifact_matching_files: llvm_artifacts.matching_files.clone(),
        cranelift_crate_source_bytes: cargo_source_size(&["cranelift-"]),
        llvm_binding_source_bytes: cargo_source_size(&["inkwell-", "llvm-sys-181"]),
        cranelift_build_artifact_bytes: cranelift_artifacts.bytes,
        llvm_binding_build_artifact_bytes: llvm_artifacts.bytes,
        integration_notes: serde_json::json!({
            "core_ir":"both backends lower the same canonical ogma.core-ir/1.0.0 fixture inputs",
            "cranelift":"Rust crates and in-process ObjectModule; no external LLVM toolchain",
            "llvm":"Inkwell/llvm-sys and system LLVM 18 development toolchain; in-process target-machine emission",
            "ogm19":"dynamic addition and host echo call through the versioned mock C ABI function table, opaque handles, registered Rust callbacks, root scopes and explicit completions",
            "ogm27":"generated code calls modeled shadow-frame safepoints for entry, loop, allocation, call and host-call boundaries; the model verifies roots, transitions and execution-budget polls",
            "fixture_adapters":"string/object call-boundary conversion and the reserved Symbol failure sentinel remain exact-fixture test adapters",
            "status":"M0 fixture proof; not a production source compiler, Runtime, ABI implementation or machine-register stack-map encoder"
        }),
        cranelift,
        llvm_backend,
    };
    if let Some(parent) = o.output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&o.output, serde_json::to_vec_pretty(&report)?)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    let _ = fs::remove_dir_all(root);
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lowering_profile_rejects_core_ir_operand_drift() {
        let mut document: JsonValue = serde_json::from_str(include_str!(
            "../../../tests/core-ir/fixtures/positive-call-boundary.json"
        ))
        .unwrap();
        document["function"]["blocks"][0]["instructions"][1]["operands"][0] =
            JsonValue::String("wrong_value".into());
        assert!(validate_lowering_profile("call_boundary", &document).is_err());
    }

    #[test]
    fn ogm31_composes_native_safepoints_and_completion_roots() {
        let mut state = NativeProofState::new().unwrap();
        for site in 1..=5 {
            state.on_safepoint(site).unwrap();
        }
        state.exercise_transition_model(false).unwrap();
        state.exercise_transition_model(true).unwrap();
        assert_eq!(state.safepoint_counts.len(), 5);
        assert_eq!(state.transition_paths.len(), 1);
        assert_eq!(
            state.transition_paths[0],
            vec!["Native", "Interpreter", "HostCallback"]
        );
        assert!(state.heap.is_alive(state.sentinel));
        assert_eq!(state.roots.native.depth(), 1);
        assert_eq!(state.budget.poll_trace().len(), 5);
    }

    #[test]
    fn build_artifact_scanner_counts_only_native_backend_crates() {
        let root = env::temp_dir().join(format!("ogm30-artifacts-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let deps = root.join("release/deps/nested");
        fs::create_dir_all(&deps).unwrap();
        fs::write(deps.join("libcranelift_codegen-test.rlib"), [0_u8; 13]).unwrap();
        fs::write(deps.join("libinkwell-test.rlib"), [0_u8; 17]).unwrap();
        fs::write(deps.join("libllvm_sys-test.so"), [0_u8; 19]).unwrap();
        fs::write(deps.join("libunrelated-test.rlib"), [0_u8; 23]).unwrap();

        let cranelift = cargo_artifact_summary(&root, &["cranelift"]);
        let llvm = cargo_artifact_summary(&root, &["inkwell", "llvm_sys"]);

        assert!(cranelift.search_directory_exists);
        assert_eq!((cranelift.files, cranelift.bytes), (1, 13));
        assert_eq!(
            cranelift.matching_files,
            vec!["deps/nested/libcranelift_codegen-test.rlib".to_owned()]
        );
        assert_eq!((llvm.files, llvm.bytes), (2, 36));
        assert_eq!(
            llvm.matching_files,
            vec![
                "deps/nested/libinkwell-test.rlib".to_owned(),
                "deps/nested/libllvm_sys-test.so".to_owned()
            ]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn benchmark_uses_canonical_core_ir_v1_fixtures() {
        let c = load_corpus().unwrap();
        assert_eq!(c.fixtures.len(), 5);
        assert!(c.get("dynamic_exception").unwrap().exception_edge);
        assert_eq!(
            c.get("host_call").unwrap().host_symbol.as_deref(),
            Some("benchmark.host.echo")
        );
        assert!(
            c.get("call_boundary")
                .unwrap()
                .operations
                .contains(&"js.call".into())
        );
    }
}
