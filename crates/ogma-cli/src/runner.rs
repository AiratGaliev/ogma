//! Execution of persisted, verified OGMB through the production Runtime.

use crate::artifact::{read_artifact, read_unit, verify_artifact};
use ogma_embedding::{
    CancellationToken, Engine, EngineConfig, ExecutionRequest, JobPumpOutcome,
    ModuleEvaluationCompletion, ModuleResolver, ModuleResolverError, PersistentCompletion,
    ResolvedModule,
};
use ogma_runtime::ValueType;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Default)]
pub(crate) struct RunOptions {
    pub(crate) heap_bytes: Option<usize>,
    pub(crate) stack_bytes: Option<usize>,
    pub(crate) call_depth: Option<u32>,
    pub(crate) fuel: Option<u64>,
    pub(crate) timeout_ms: Option<u64>,
    pub(crate) job_capacity: Option<usize>,
}

#[derive(Debug)]
pub(crate) struct RunFailure {
    pub(crate) exit_code: u8,
    pub(crate) message: String,
}

impl RunFailure {
    fn new(exit_code: u8, message: impl Into<String>) -> Self {
        Self {
            exit_code,
            message: message.into(),
        }
    }
}

fn interpreter_failure_exit_code(error: &ogma_embedding::InterpreterError) -> u8 {
    match error {
        ogma_embedding::InterpreterError::CallDepthExceeded { .. }
        | ogma_embedding::InterpreterError::StackQuotaExceeded { .. } => {
            crate::cli::EXIT_INTERRUPTED
        }
        _ => crate::cli::EXIT_RUNTIME,
    }
}

fn job_queue_failure_exit_code(error: &ogma_embedding::JobQueueError) -> u8 {
    match error {
        ogma_embedding::JobQueueError::Interpreter(error) => interpreter_failure_exit_code(error),
        _ => crate::cli::EXIT_RUNTIME,
    }
}

fn context_failure_exit_code(error: &ogma_embedding::ContextError) -> u8 {
    match error {
        ogma_embedding::ContextError::Interpreter(error) => interpreter_failure_exit_code(error),
        ogma_embedding::ContextError::JobQueue(error) => job_queue_failure_exit_code(error),
        _ => crate::cli::EXIT_RUNTIME,
    }
}

fn module_graph_failure_exit_code(error: &ogma_embedding::ModuleGraphError) -> u8 {
    match error {
        ogma_embedding::ModuleGraphError::Interpreter(error) => {
            interpreter_failure_exit_code(error)
        }
        ogma_embedding::ModuleGraphError::Context(error) => context_failure_exit_code(error),
        ogma_embedding::ModuleGraphError::Jobs(error) => job_queue_failure_exit_code(error),
        _ => crate::cli::EXIT_RUNTIME,
    }
}

pub(crate) fn run_artifact(path: &Path, options: &RunOptions) -> Result<(), RunFailure> {
    let artifact_bytes = read_artifact(path).map_err(|error| {
        let exit_code = if error.starts_with("invalid artifact size") {
            crate::cli::EXIT_ARTIFACT
        } else {
            crate::cli::EXIT_IO
        };
        RunFailure::new(exit_code, error)
    })?;
    let _verified = verify_artifact(&artifact_bytes).map_err(|error| RunFailure::new(5, error))?;
    let unit = read_unit(path, &artifact_bytes)
        .map_err(|error| RunFailure::new(5, format!("invalid compilation unit: {error}")))?;

    let mut config = EngineConfig::default();
    if let Some(heap) = options.heap_bytes {
        config.heap_quota_bytes = heap;
        config.max_allocation_bytes = config.max_allocation_bytes.min(heap);
    }
    if let Some(stack) = options.stack_bytes {
        config.max_stack_bytes = stack;
    }
    if let Some(depth) = options.call_depth {
        config.max_call_depth = depth;
    }
    if let Some(fuel) = options.fuel {
        config.instruction_budget = fuel;
    }
    if let Some(capacity) = options.job_capacity {
        config.max_pending_jobs = capacity;
        config.max_jobs_per_pump = config.max_jobs_per_pump.min(capacity.max(1));
    }
    if config.max_pending_jobs == 0 {
        return Err(RunFailure::new(
            2,
            "--job-capacity must be greater than zero",
        ));
    }
    config
        .validate()
        .map_err(|error| RunFailure::new(2, format!("invalid Runtime limits: {error:?}")))?;

    let timeout = options
        .timeout_ms
        .map(|millis| {
            if millis == 0 {
                Err(RunFailure::new(2, "--timeout-ms must be greater than zero"))
            } else {
                Instant::now()
                    .checked_add(Duration::from_millis(millis))
                    .ok_or_else(|| RunFailure::new(2, "--timeout-ms is too large"))
            }
        })
        .transpose()?;

    let engine = Engine::new(config)
        .map_err(|error| RunFailure::new(2, format!("invalid Runtime configuration: {error:?}")))?;
    let mut runtime = engine.create_runtime().map_err(|error| {
        RunFailure::new(
            crate::cli::EXIT_RUNTIME,
            format!("cannot create Runtime: {error:?}"),
        )
    })?;
    let mut context = runtime.create_context().map_err(|error| {
        RunFailure::new(
            crate::cli::EXIT_RUNTIME,
            format!("cannot create execution Context: {error:?}"),
        )
    })?;

    let cancellation = CancellationToken::new();
    let _cancellation_guard =
        CancellationGuard::install(cancellation.clone()).map_err(|error| {
            RunFailure::new(
                3,
                format!("cannot install Ctrl+C cancellation handler: {error}"),
            )
        })?;
    let request = ExecutionRequest {
        instruction_budget: options.fuel,
        max_stack_bytes: options.stack_bytes,
        max_call_depth: options.call_depth,
        cancellation: Some(cancellation),
        deadline: timeout,
        time_budget: None,
        global_this: None,
    };

    if let Some(unit) = unit {
        if let Some(module_data) = unit.manifest.module.clone() {
            let export_names = module_data
                .exports
                .iter()
                .filter(|item| item.kind != "star")
                .map(|item| item.export_name.clone())
                .collect::<Vec<_>>();
            let descriptor: ogma_bytecode::ModuleDescriptor =
                module_data.try_into().map_err(|error: String| {
                    RunFailure::new(5, format!("invalid saved module descriptor: {error}"))
                })?;
            let root_module = ResolvedModule {
                descriptor,
                artifacts: unit.artifacts,
            };
            let mut dependency_modules = Vec::with_capacity(unit.dependencies.len());
            for dependency in unit.dependencies {
                let descriptor: ogma_bytecode::ModuleDescriptor =
                    dependency.descriptor.try_into().map_err(|error: String| {
                        RunFailure::new(5, format!("invalid saved dependency descriptor: {error}"))
                    })?;
                dependency_modules.push(ResolvedModule {
                    descriptor,
                    artifacts: dependency.artifacts,
                });
            }
            let mut resolver = FileModuleResolver::new(
                root_module,
                dependency_modules,
                unit.manifest.resolved_requests,
            );
            let report = context
                .evaluate_module_graph(&unit.manifest.source.id, &mut resolver, request.clone())
                .map_err(|error| {
                    RunFailure::new(
                        module_graph_failure_exit_code(&error),
                        format!("module graph failed: {error:?}"),
                    )
                })?;

            let mut instruction_count = 0_u64;
            for evaluation in &report.evaluations {
                instruction_count =
                    instruction_count.saturating_add(evaluation.instructions_executed);
                match evaluation.completion {
                    ModuleEvaluationCompletion::Evaluated => {}
                    ModuleEvaluationCompletion::Threw(handle) => {
                        let value = context.resolve(handle).map_err(|error| {
                            RunFailure::new(
                                crate::cli::EXIT_RUNTIME,
                                format!("cannot read module exception: {error:?}"),
                            )
                        })?;
                        let description = value_to_string(&context, value)
                            .map_err(|error| RunFailure::new(crate::cli::EXIT_RUNTIME, error))?;
                        context.release(handle).map_err(|error| {
                            RunFailure::new(
                                crate::cli::EXIT_RUNTIME,
                                format!("cannot release exception root: {error:?}"),
                            )
                        })?;
                        return Err(RunFailure::new(
                            crate::cli::EXIT_RUNTIME,
                            format!(
                                "uncaught exception in {}: {description}",
                                evaluation.module_id
                            ),
                        ));
                    }
                    ModuleEvaluationCompletion::Interrupted(reason) => {
                        return Err(RunFailure::new(
                            crate::cli::EXIT_INTERRUPTED,
                            format!("module evaluation interrupted: {reason:?}"),
                        ));
                    }
                }
            }
            if let Some(failed_module) = report.failure {
                return Err(RunFailure::new(
                    crate::cli::EXIT_RUNTIME,
                    format!("module graph evaluation failed in {failed_module}"),
                ));
            }
            let job_report = context
                .run_promise_jobs_until_idle_with_request(
                    Some(
                        options
                            .job_capacity
                            .unwrap_or(EngineConfig::default().max_pending_jobs),
                    ),
                    request,
                )
                .map_err(|error| {
                    RunFailure::new(
                        job_queue_failure_exit_code(&error),
                        format!("PromiseJobs pump failed: {error:?}"),
                    )
                })?;
            handle_job_outcome(&mut context, &job_report)?;
            let mut exports = serde_json::Map::new();
            for export_name in export_names {
                let handle = context
                    .read_module_export(&unit.manifest.source.id, &export_name)
                    .map_err(|error| {
                        RunFailure::new(
                            crate::cli::EXIT_RUNTIME,
                            format!("cannot read module export {export_name:?}: {error:?}"),
                        )
                    })?;
                let value = context.resolve(handle).map_err(|error| {
                    RunFailure::new(
                        crate::cli::EXIT_RUNTIME,
                        format!("cannot resolve module export {export_name:?}: {error:?}"),
                    )
                })?;
                let rendered = value_to_json(&context, value)
                    .map_err(|error| RunFailure::new(crate::cli::EXIT_RUNTIME, error))?;
                context.release(handle).map_err(|error| {
                    RunFailure::new(
                        crate::cli::EXIT_RUNTIME,
                        format!("cannot release module export {export_name:?}: {error:?}"),
                    )
                })?;
                exports.insert(export_name, rendered);
            }
            println!(
                "{}",
                serde_json::json!({
                    "completion": "evaluated",
                    "entry_module": &report.entry_module_id,
                    "modules": &report.modules_instantiated,
                    "module_cache_reused": report.reused_module_cache,
                    "instructions": instruction_count,
                    "jobs_executed": job_report.jobs_executed,
                    "exports": exports,
                })
            );
            return Ok(());
        } else {
            context
                .load_module(unit.artifacts.iter())
                .map_err(|error| {
                    RunFailure::new(
                        crate::cli::EXIT_RUNTIME,
                        format!("Runtime module loading failed: {error:?}"),
                    )
                })?;
            execute_script(
                &mut context,
                &unit.manifest.entry_function,
                &request,
                options
                    .job_capacity
                    .unwrap_or(EngineConfig::default().max_pending_jobs),
            )?;
        }
    } else {
        let entry_function = _verified
            .manifest
            .pointer("/metadata/function/id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                RunFailure::new(
                    5,
                    "verified artifact is missing its entry function identity",
                )
            })?;
        context
            .load_module([artifact_bytes.as_slice()])
            .map_err(|error| {
                RunFailure::new(
                    crate::cli::EXIT_RUNTIME,
                    format!("Runtime artifact loading failed: {error:?}"),
                )
            })?;
        execute_script(
            &mut context,
            entry_function,
            &request,
            options
                .job_capacity
                .unwrap_or(EngineConfig::default().max_pending_jobs),
        )?;
    }

    Ok(())
}

fn execute_script(
    context: &mut ogma_embedding::Context<'_>,
    entry_function: &str,
    request: &ExecutionRequest,
    job_budget: usize,
) -> Result<(), RunFailure> {
    let report = context
        .execute_persistent(entry_function, &[], request.clone())
        .map_err(|error| {
            RunFailure::new(
                context_failure_exit_code(&error),
                format!("execution failed: {error:?}"),
            )
        })?;
    let value_text = match report.completion {
        PersistentCompletion::Return(handle) => {
            let value = context.resolve(handle).map_err(|error| {
                RunFailure::new(
                    crate::cli::EXIT_RUNTIME,
                    format!("cannot resolve return value: {error:?}"),
                )
            })?;
            let result = value_to_string(context, value)
                .map_err(|error| RunFailure::new(crate::cli::EXIT_RUNTIME, error))?;
            context.release(handle).map_err(|error| {
                RunFailure::new(
                    crate::cli::EXIT_RUNTIME,
                    format!("cannot release return value: {error:?}"),
                )
            })?;
            result
        }
        PersistentCompletion::Throw(handle) => {
            let value = context.resolve(handle).map_err(|error| {
                RunFailure::new(
                    crate::cli::EXIT_RUNTIME,
                    format!("cannot resolve thrown value: {error:?}"),
                )
            })?;
            let description = value_to_string(context, value)
                .map_err(|error| RunFailure::new(crate::cli::EXIT_RUNTIME, error))?;
            context.release(handle).map_err(|error| {
                RunFailure::new(
                    crate::cli::EXIT_RUNTIME,
                    format!("cannot release thrown value: {error:?}"),
                )
            })?;
            return Err(RunFailure::new(
                crate::cli::EXIT_RUNTIME,
                format!("uncaught exception: {description}"),
            ));
        }
        PersistentCompletion::Interrupted(reason) => {
            return Err(RunFailure::new(
                crate::cli::EXIT_INTERRUPTED,
                format!("execution interrupted: {reason:?}"),
            ));
        }
    };
    let job_report = context
        .run_promise_jobs_until_idle_with_request(Some(job_budget), request.clone())
        .map_err(|error| {
            RunFailure::new(
                crate::cli::EXIT_RUNTIME,
                format!("PromiseJobs pump failed: {error:?}"),
            )
        })?;
    handle_job_outcome(context, &job_report)?;
    println!(
        "{}",
        serde_json::json!({
            "completion": "return",
            "value": value_text,
            "instructions": report.instructions_executed,
            "fuel_remaining": report.fuel_remaining,
            "max_call_depth": report.max_call_depth_reached,
            "max_stack_bytes": report.max_stack_bytes_reached,
            "jobs_executed": job_report.jobs_executed,
        })
    );
    Ok(())
}

fn handle_job_outcome(
    context: &mut ogma_embedding::Context<'_>,
    report: &ogma_embedding::JobPumpReport,
) -> Result<(), RunFailure> {
    match report.outcome {
        JobPumpOutcome::Progress | JobPumpOutcome::Idle => Ok(()),
        JobPumpOutcome::BudgetExhausted => Err(RunFailure::new(
            crate::cli::EXIT_INTERRUPTED,
            "PromiseJobs work budget exhausted",
        )),
        JobPumpOutcome::Interrupted => Err(RunFailure::new(
            crate::cli::EXIT_INTERRUPTED,
            format!("PromiseJob interrupted: {:?}", report.interruption),
        )),
        JobPumpOutcome::JobThrew => {
            if let Some(PersistentCompletion::Throw(handle)) = report.completion {
                let value = context.resolve(handle).map_err(|error| {
                    RunFailure::new(
                        crate::cli::EXIT_RUNTIME,
                        format!("cannot resolve PromiseJob exception: {error:?}"),
                    )
                })?;
                let description = value_to_string(context, value)
                    .map_err(|error| RunFailure::new(crate::cli::EXIT_RUNTIME, error))?;
                context.release(handle).map_err(|error| {
                    RunFailure::new(
                        crate::cli::EXIT_RUNTIME,
                        format!("cannot release PromiseJob exception: {error:?}"),
                    )
                })?;
                Err(RunFailure::new(
                    crate::cli::EXIT_RUNTIME,
                    format!("uncaught PromiseJob exception: {description}"),
                ))
            } else {
                Err(RunFailure::new(
                    crate::cli::EXIT_RUNTIME,
                    "PromiseJob threw without a persistent exception value",
                ))
            }
        }
        JobPumpOutcome::Paused => Err(RunFailure::new(
            crate::cli::EXIT_INTERRUPTED,
            "Runtime PromiseJobs queue is paused",
        )),
        JobPumpOutcome::Shutdown => Err(RunFailure::new(
            crate::cli::EXIT_INTERRUPTED,
            "Runtime PromiseJobs queue is shut down",
        )),
    }
}

fn value_to_json(
    context: &ogma_embedding::Context<'_>,
    value: ogma_embedding::Value,
) -> Result<serde_json::Value, String> {
    Ok(match value.value_type() {
        ValueType::Undefined => serde_json::Value::String("undefined".to_owned()),
        ValueType::Null => serde_json::Value::Null,
        ValueType::Boolean => serde_json::json!(value.as_boolean().unwrap_or(false)),
        ValueType::Number => {
            let number = value.as_number().unwrap_or(f64::NAN);
            if number.is_finite() {
                serde_json::json!(number)
            } else {
                serde_json::Value::String(if number.is_nan() {
                    "NaN".to_owned()
                } else if number.is_sign_positive() {
                    "Infinity".to_owned()
                } else {
                    "-Infinity".to_owned()
                })
            }
        }
        ValueType::String => serde_json::Value::String(value_to_string(context, value)?),
        ValueType::Object => serde_json::Value::String("[object Object]".to_owned()),
        ValueType::Function => serde_json::Value::String("[function]".to_owned()),
    })
}

fn value_to_string(
    context: &ogma_embedding::Context<'_>,
    value: ogma_embedding::Value,
) -> Result<String, String> {
    Ok(match value.value_type() {
        ValueType::Undefined => "undefined".to_owned(),
        ValueType::Null => "null".to_owned(),
        ValueType::Boolean => value
            .as_boolean()
            .map(|item| item.to_string())
            .unwrap_or_else(|| "boolean".to_owned()),
        ValueType::Number => {
            let number = value.as_number().unwrap_or(f64::NAN);
            if number.is_nan() {
                "NaN".to_owned()
            } else if number == f64::INFINITY {
                "Infinity".to_owned()
            } else if number == f64::NEG_INFINITY {
                "-Infinity".to_owned()
            } else {
                number.to_string()
            }
        }
        ValueType::String => {
            let units = context
                .read_string_utf16(value)
                .map_err(|error| format!("cannot read Runtime string: {error:?}"))?;
            char::decode_utf16(units)
                .map(|unit| unit.unwrap_or(char::REPLACEMENT_CHARACTER))
                .collect()
        }
        ValueType::Object => "[object Object]".to_owned(),
        ValueType::Function => "[function]".to_owned(),
    })
}

type ModuleRequestMap = HashMap<(String, String), String>;

struct FileModuleResolver {
    cache: HashMap<String, ResolvedModule>,
    resolved_requests: ModuleRequestMap,
}

impl FileModuleResolver {
    fn new(
        entry: ResolvedModule,
        dependencies: Vec<ResolvedModule>,
        requests: Vec<crate::artifact::ResolvedModuleRequest>,
    ) -> Self {
        let mut cache = HashMap::new();
        cache.insert(entry.descriptor.id.clone(), entry);
        for dependency in dependencies {
            cache.insert(dependency.descriptor.id.clone(), dependency);
        }
        let resolved_requests = requests
            .into_iter()
            .map(|item| ((item.referrer, item.specifier), item.target))
            .collect();
        Self {
            cache,
            resolved_requests,
        }
    }
}

impl ModuleResolver for FileModuleResolver {
    fn resolve(
        &mut self,
        referrer_module_id: &str,
        specifier: &str,
    ) -> Result<String, ModuleResolverError> {
        self.resolved_requests
            .get(&(referrer_module_id.to_owned(), specifier.to_owned()))
            .cloned()
            .ok_or_else(|| ModuleResolverError::new(format!(
                "module request {specifier:?} from {referrer_module_id:?} is not present in the verified compilation-unit index"
            )))
    }

    fn load(&mut self, module_id: &str) -> Result<ResolvedModule, ModuleResolverError> {
        self.cache.get(module_id).cloned().ok_or_else(|| {
            ModuleResolverError::new(format!(
                "module {module_id:?} was not present in the compiled artifact graph"
            ))
        })
    }
}

static SIGNAL_RECEIVED: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
unsafe extern "C" {
    fn signal(signal: i32, handler: usize) -> usize;
}

#[cfg(unix)]
extern "C" fn handle_unix_signal(_: i32) {
    SIGNAL_RECEIVED.store(true, Ordering::Relaxed);
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn SetConsoleCtrlHandler(
        handler: Option<unsafe extern "system" fn(u32) -> i32>,
        add: i32,
    ) -> i32;
}

#[cfg(windows)]
unsafe extern "system" fn handle_windows_signal(_: u32) -> i32 {
    SIGNAL_RECEIVED.store(true, Ordering::Relaxed);
    1
}

struct CancellationGuard {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    #[cfg(unix)]
    previous_sigint: usize,
    #[cfg(unix)]
    previous_sigterm: usize,
    #[cfg(windows)]
    handler_registered: bool,
}

impl CancellationGuard {
    fn install(token: CancellationToken) -> Result<Self, String> {
        SIGNAL_RECEIVED.store(false, Ordering::SeqCst);
        #[cfg(unix)]
        let (previous_sigint, previous_sigterm) = {
            // SIGINT and SIGTERM handlers only set an atomic flag; Runtime cancellation
            // is requested by the ordinary worker thread, never from signal context.
            let previous_sigint = unsafe { signal(2, handle_unix_signal as *const () as usize) };
            if previous_sigint == usize::MAX {
                return Err("failed to register SIGINT".to_owned());
            }
            let previous_sigterm = unsafe { signal(15, handle_unix_signal as *const () as usize) };
            if previous_sigterm == usize::MAX {
                unsafe {
                    signal(2, previous_sigint);
                }
                return Err("failed to register SIGTERM".to_owned());
            }
            (previous_sigint, previous_sigterm)
        };
        #[cfg(windows)]
        let handler_registered = {
            let installed = unsafe { SetConsoleCtrlHandler(Some(handle_windows_signal), 1) };
            if installed == 0 {
                return Err("SetConsoleCtrlHandler failed".to_owned());
            }
            true
        };
        #[cfg(not(any(unix, windows)))]
        return Err("Ctrl+C cancellation is unsupported on this platform".to_owned());

        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            while !worker_stop.load(Ordering::Acquire) {
                if SIGNAL_RECEIVED.swap(false, Ordering::AcqRel) {
                    token.cancel();
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
        });
        Ok(Self {
            stop,
            thread: Some(worker),
            #[cfg(unix)]
            previous_sigint,
            #[cfg(unix)]
            previous_sigterm,
            #[cfg(windows)]
            handler_registered,
        })
    }
}

impl Drop for CancellationGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.thread.take() {
            let _ = worker.join();
        }
        #[cfg(unix)]
        unsafe {
            signal(2, self.previous_sigint);
            signal(15, self.previous_sigterm);
        }
        #[cfg(windows)]
        if self.handler_registered {
            unsafe {
                SetConsoleCtrlHandler(Some(handle_windows_signal), 0);
            }
        }
    }
}
