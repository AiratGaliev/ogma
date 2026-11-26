//! Runtime-owned, host-driven ECMAScript job queue.
//!
//! This is the scheduling substrate for PromiseJobs. It deliberately does not
//! implement Promise objects or Node-specific queues; those remain separate
//! language/compatibility layers.

use std::collections::VecDeque;
use std::rc::Rc;
use std::time::Instant;

use ogma_runtime::{ExternalMemoryCharge, HeapError, HostHandle};

use crate::{
    ConfigurationError, Context, ExecutableModule, ExecutionCompletion, ExecutionInterruption,
    ExecutionRequest, HostFunctionRegistry, InterpreterError, PersistentCompletion,
    PersistentHandle,
};

#[derive(Default)]
pub(super) struct JobScheduler {
    queue: VecDeque<PendingJob>,
    state: JobQueueState,
    pumping: bool,
}

struct PendingJob {
    module: Rc<ExecutableModule>,
    callback: HostHandle,
    arguments: Vec<HostHandle>,
    host_functions: HostFunctionRegistry,
    accounting: ExternalMemoryCharge,
}

/// Lifecycle state of the Runtime's ECMAScript job queue.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum JobQueueState {
    /// New jobs are accepted and the host may pump queued jobs.
    #[default]
    Running,
    /// Jobs are retained, but no queued callback is dispatched.
    Paused,
    /// The queue is terminal; pending roots have been released.
    Shutdown,
}

/// Observable outcome of one bounded job-pump call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobPumpOutcome {
    /// At least one callback ran and the queue is now empty.
    Progress,
    /// No callback was runnable.
    Idle,
    /// The per-pump work budget was consumed while jobs remain.
    BudgetExhausted,
    /// The queue is paused.
    Paused,
    /// The queue has been shut down.
    Shutdown,
    /// A callback completed with a JavaScript Throw completion.
    JobThrew,
    /// A callback reached an execution cancellation/deadline/fuel boundary.
    Interrupted,
}

/// Result of one host-driven bounded job-pump call.
#[derive(Clone, Debug)]
pub struct JobPumpReport {
    /// Observable pump outcome.
    pub outcome: JobPumpOutcome,
    /// Number of callbacks removed from the queue and dispatched.
    pub jobs_executed: usize,
    /// A thrown completion, promoted to a persistent Runtime root.
    ///
    /// The caller owns this handle and must release it through Context::release.
    pub completion: Option<PersistentCompletion>,
    /// Execution interruption, when a callback did not complete normally.
    pub interruption: Option<ExecutionInterruption>,
}

/// Failure to enqueue, pump or manage the Runtime job queue.
#[derive(Debug)]
pub enum JobQueueError {
    /// The configured pending-job capacity has been reached.
    QueueFull {
        /// Configured maximum pending jobs.
        capacity: usize,
    },
    /// The queue has entered terminal shutdown.
    ShuttingDown,
    /// A Context must load a verified module before it can enqueue callbacks.
    ModuleNotLoaded,
    /// The callback is not a function owned by the Context's verified module.
    InvalidCallback,
    /// A queue/root allocation could not be reserved.
    AllocationFailed,
    /// A callback exceeds the supported argument limit.
    TooManyArguments {
        /// Actual callback argument count.
        count: usize,
        /// Maximum accepted arguments.
        maximum: usize,
    },
    /// A pump budget must be positive.
    InvalidPumpBudget,
    /// The relative time budget cannot be represented as a monotonic deadline.
    TimeBudgetOutOfRange,
    /// A pump call was re-entered for the same Runtime.
    ReentrantPump,
    /// A heap/root operation failed.
    Heap(HeapError),
    /// The interpreter rejected a callback execution.
    Interpreter(InterpreterError),
    /// Runtime execution options were invalid.
    Configuration(ConfigurationError),
}

impl std::fmt::Display for JobQueueError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for JobQueueError {}

impl From<HeapError> for JobQueueError {
    fn from(error: HeapError) -> Self {
        Self::Heap(error)
    }
}

impl From<InterpreterError> for JobQueueError {
    fn from(error: InterpreterError) -> Self {
        Self::Interpreter(error)
    }
}

impl Context<'_> {
    /// Enqueues a callable Function value as a FIFO Runtime job.
    ///
    /// The callback and arguments are promoted to persistent roots before the
    /// job becomes visible in the queue. The module's verified function table
    /// is retained with the job, so dropping this Context does not invalidate
    /// accepted callbacks while the parent Runtime remains alive.
    pub fn enqueue_promise_job(
        &mut self,
        callback: crate::Value,
        arguments: &[crate::Value],
    ) -> Result<(), JobQueueError> {
        if self.runtime.jobs.state == JobQueueState::Shutdown {
            return Err(JobQueueError::ShuttingDown);
        }
        let capacity = self.runtime.config.max_pending_jobs;
        if self.runtime.jobs.queue.len() >= capacity {
            return Err(JobQueueError::QueueFull { capacity });
        }
        if arguments.len() > crate::MAX_HOST_ARGUMENTS {
            return Err(JobQueueError::TooManyArguments {
                count: arguments.len(),
                maximum: crate::MAX_HOST_ARGUMENTS,
            });
        }

        if self.loaded_modules.is_empty() {
            return Err(JobQueueError::ModuleNotLoaded);
        }
        let mut owner = None;
        for candidate in self.loaded_modules.iter().rev() {
            if candidate
                .executable
                .contains_callable(&self.runtime.heap, callback)?
            {
                owner = Some(Rc::clone(&candidate.executable));
                break;
            }
        }
        let module = owner.ok_or(JobQueueError::InvalidCallback)?;

        self.runtime
            .jobs
            .queue
            .try_reserve(1)
            .map_err(|_| JobQueueError::AllocationFailed)?;
        let mut argument_roots = Vec::new();
        argument_roots
            .try_reserve_exact(arguments.len())
            .map_err(|_| JobQueueError::AllocationFailed)?;

        let accounted_bytes = std::mem::size_of::<PendingJob>()
            .checked_add(
                argument_roots
                    .capacity()
                    .checked_mul(std::mem::size_of::<HostHandle>())
                    .ok_or(JobQueueError::AllocationFailed)?,
            )
            .ok_or(JobQueueError::AllocationFailed)?;
        let accounting = self.runtime.heap.charge_external_memory(accounted_bytes)?;
        let callback_root = match self.runtime.heap.retain_host_value(callback) {
            Ok(root) => root,
            Err(error) => {
                let _ = self.runtime.heap.release_external_memory(accounting);
                return Err(JobQueueError::Heap(error));
            }
        };
        for argument in arguments {
            match self.runtime.heap.retain_host_value(*argument) {
                Ok(root) => argument_roots.push(root),
                Err(error) => {
                    for root in argument_roots.drain(..) {
                        let _ = self.runtime.heap.release_host_handle(root);
                    }
                    let _ = self.runtime.heap.release_host_handle(callback_root);
                    let _ = self.runtime.heap.release_external_memory(accounting);
                    return Err(JobQueueError::Heap(error));
                }
            }
        }

        self.runtime.jobs.queue.push_back(PendingJob {
            module,
            callback: callback_root,
            arguments: argument_roots,
            host_functions: self.host_functions.clone(),
            accounting,
        });
        Ok(())
    }

    /// Dispatches up to max_jobs callbacks on the Runtime owner thread.
    ///
    /// None uses EngineConfig.max_jobs_per_pump. Budget exhaustion is reported
    /// distinctly from Idle, and remaining jobs retain FIFO order.
    pub fn pump_promise_jobs(
        &mut self,
        max_jobs: Option<usize>,
    ) -> Result<JobPumpReport, JobQueueError> {
        self.pump_promise_jobs_with_request(max_jobs, ExecutionRequest::default())
    }

    /// Dispatches queued callbacks with host-supplied cancellation and time limits.
    ///
    /// Cancellation is observed by the existing interpreter at its normal
    /// safepoints. A job that completes as Interrupted is removed and its
    /// retained roots are released; later FIFO jobs remain queued.
    pub fn pump_promise_jobs_with_request(
        &mut self,
        max_jobs: Option<usize>,
        request: ExecutionRequest,
    ) -> Result<JobPumpReport, JobQueueError> {
        let configured_budget = self.runtime.config.max_jobs_per_pump;
        let budget = max_jobs.unwrap_or(configured_budget);
        if budget == 0 || budget > configured_budget {
            return Err(JobQueueError::InvalidPumpBudget);
        }
        match self.runtime.jobs.state {
            JobQueueState::Paused => {
                return Ok(JobPumpReport::new(JobPumpOutcome::Paused, 0));
            }
            JobQueueState::Shutdown => {
                return Ok(JobPumpReport::new(JobPumpOutcome::Shutdown, 0));
            }
            JobQueueState::Running => {}
        }
        if self.runtime.jobs.pumping {
            return Err(JobQueueError::ReentrantPump);
        }
        let request = normalize_execution_request(request)?;
        request
            .options(&self.runtime.config)
            .map_err(JobQueueError::Configuration)?;

        self.runtime.jobs.pumping = true;
        let result = self.pump_promise_jobs_inner(budget, &request);
        self.runtime.jobs.pumping = false;
        result
    }

    /// Repeatedly pumps bounded turns until the queue is idle or total work is exhausted.
    ///
    /// Each turn is capped by EngineConfig.max_jobs_per_pump. None uses the
    /// configured pending-job capacity as the total-work budget.
    pub fn run_promise_jobs_until_idle(
        &mut self,
        total_budget: Option<usize>,
    ) -> Result<JobPumpReport, JobQueueError> {
        self.run_promise_jobs_until_idle_with_request(total_budget, ExecutionRequest::default())
    }

    /// Drains bounded turns while applying the same cancellation/deadline policy to each job.
    pub fn run_promise_jobs_until_idle_with_request(
        &mut self,
        total_budget: Option<usize>,
        request: ExecutionRequest,
    ) -> Result<JobPumpReport, JobQueueError> {
        let total_budget = match total_budget {
            Some(0) => return Err(JobQueueError::InvalidPumpBudget),
            Some(budget) => budget,
            None => self.runtime.config.max_pending_jobs.max(1),
        };
        let request = normalize_execution_request(request)?;
        request
            .options(&self.runtime.config)
            .map_err(JobQueueError::Configuration)?;

        let mut jobs_executed = 0;
        while jobs_executed < total_budget {
            let turn_budget =
                (total_budget - jobs_executed).min(self.runtime.config.max_jobs_per_pump);
            let mut turn =
                self.pump_promise_jobs_with_request(Some(turn_budget), request.clone())?;
            jobs_executed += turn.jobs_executed;
            turn.jobs_executed = jobs_executed;
            if turn.outcome != JobPumpOutcome::BudgetExhausted {
                return Ok(turn);
            }
        }

        let outcome = if self.runtime.jobs.queue.is_empty() {
            JobPumpOutcome::Progress
        } else {
            JobPumpOutcome::BudgetExhausted
        };
        Ok(JobPumpReport::new(outcome, jobs_executed))
    }

    fn pump_promise_jobs_inner(
        &mut self,
        budget: usize,
        request: &ExecutionRequest,
    ) -> Result<JobPumpReport, JobQueueError> {
        let mut jobs_executed = 0;
        while jobs_executed < budget {
            let Some(job) = self.runtime.jobs.queue.pop_front() else {
                let outcome = if jobs_executed == 0 {
                    JobPumpOutcome::Idle
                } else {
                    JobPumpOutcome::Progress
                };
                return Ok(JobPumpReport::new(outcome, jobs_executed));
            };

            let callback = match self.runtime.heap.resolve_host_handle(job.callback) {
                Ok(value) => value,
                Err(error) => {
                    let _ = release_job_roots(&mut self.runtime.heap, job);
                    return Err(JobQueueError::Heap(error));
                }
            };
            let arguments = match job
                .arguments
                .iter()
                .map(|root| self.runtime.heap.resolve_host_handle(*root))
                .collect::<Result<Vec<_>, _>>()
            {
                Ok(arguments) => arguments,
                Err(error) => {
                    let _ = release_job_roots(&mut self.runtime.heap, job);
                    return Err(JobQueueError::Heap(error));
                }
            };
            let options = request
                .options(&self.runtime.config)
                .map_err(JobQueueError::Configuration)?;
            let mut host = job.host_functions.clone();
            let execution = job.module.execute_callable(
                &mut self.runtime.heap,
                callback,
                &arguments,
                &mut host,
                options,
            );
            let report = match execution {
                Ok(report) => report,
                Err(error) => {
                    let _ = release_job_roots(&mut self.runtime.heap, job);
                    return Err(JobQueueError::Interpreter(error));
                }
            };

            match report.completion {
                ExecutionCompletion::Return(_) => {
                    release_job_roots(&mut self.runtime.heap, job)?;
                    jobs_executed += 1;
                }
                ExecutionCompletion::Throw(value) => {
                    let persistent = match self.runtime.heap.retain_host_value(value) {
                        Ok(handle) => PersistentHandle(handle),
                        Err(error) => {
                            let _ = release_job_roots(&mut self.runtime.heap, job);
                            return Err(JobQueueError::Heap(error));
                        }
                    };
                    release_job_roots(&mut self.runtime.heap, job)?;
                    jobs_executed += 1;
                    return Ok(JobPumpReport {
                        outcome: JobPumpOutcome::JobThrew,
                        jobs_executed,
                        completion: Some(PersistentCompletion::Throw(persistent)),
                        interruption: None,
                    });
                }
                ExecutionCompletion::Interrupted(reason) => {
                    if report.instructions_executed == 0 {
                        // Interruption before the first instruction leaves the
                        // callback pending; preserve its FIFO position and roots.
                        self.runtime.jobs.queue.push_front(job);
                        return Ok(JobPumpReport {
                            outcome: JobPumpOutcome::Interrupted,
                            jobs_executed,
                            completion: None,
                            interruption: Some(reason),
                        });
                    }
                    release_job_roots(&mut self.runtime.heap, job)?;
                    jobs_executed += 1;
                    return Ok(JobPumpReport {
                        outcome: JobPumpOutcome::Interrupted,
                        jobs_executed,
                        completion: None,
                        interruption: Some(reason),
                    });
                }
            }
        }

        let outcome = if self.runtime.jobs.queue.is_empty() {
            JobPumpOutcome::Progress
        } else {
            JobPumpOutcome::BudgetExhausted
        };
        Ok(JobPumpReport::new(outcome, jobs_executed))
    }

    /// Pauses dispatch while retaining pending jobs and their Runtime roots.
    pub fn pause_promise_jobs(&mut self) {
        if self.runtime.jobs.state == JobQueueState::Running {
            self.runtime.jobs.state = JobQueueState::Paused;
        }
    }

    /// Resumes a paused queue; a shut-down queue cannot be restarted.
    pub fn resume_promise_jobs(&mut self) {
        if self.runtime.jobs.state == JobQueueState::Paused {
            self.runtime.jobs.state = JobQueueState::Running;
        }
    }

    /// Cancels all pending jobs and releases their callback/argument roots.
    pub fn cancel_pending_promise_jobs(&mut self) -> Result<usize, JobQueueError> {
        let mut cancelled = 0;
        let mut first_error = None;
        while let Some(job) = self.runtime.jobs.queue.pop_front() {
            if let Err(error) = release_job_roots(&mut self.runtime.heap, job) {
                first_error.get_or_insert(error);
            }
            cancelled += 1;
        }
        first_error.map_or(Ok(cancelled), |error| Err(JobQueueError::Heap(error)))
    }

    /// Irreversibly shuts down the queue and releases every pending job root.
    pub fn shutdown_promise_jobs(&mut self) -> Result<(), JobQueueError> {
        if self.runtime.jobs.pumping {
            return Err(JobQueueError::ReentrantPump);
        }
        self.runtime.jobs.state = JobQueueState::Shutdown;
        self.cancel_pending_promise_jobs()?;
        Ok(())
    }

    /// Returns the number of callbacks currently waiting in the FIFO queue.
    #[must_use]
    pub fn pending_promise_jobs(&self) -> usize {
        self.runtime.jobs.queue.len()
    }

    /// Returns the current job-queue lifecycle state.
    #[must_use]
    pub const fn promise_job_state(&self) -> JobQueueState {
        self.runtime.jobs.state
    }
}

impl JobPumpReport {
    fn new(outcome: JobPumpOutcome, jobs_executed: usize) -> Self {
        Self {
            outcome,
            jobs_executed,
            completion: None,
            interruption: None,
        }
    }
}

fn normalize_execution_request(
    mut request: ExecutionRequest,
) -> Result<ExecutionRequest, JobQueueError> {
    if let Some(time_budget) = request.time_budget.take() {
        let budget_deadline = Instant::now()
            .checked_add(time_budget)
            .ok_or(JobQueueError::TimeBudgetOutOfRange)?;
        request.deadline = Some(
            request
                .deadline
                .map_or(budget_deadline, |deadline| deadline.min(budget_deadline)),
        );
    }
    Ok(request)
}

fn release_job_roots(heap: &mut crate::Heap, job: PendingJob) -> Result<(), HeapError> {
    let mut first_error = None;
    for root in job.arguments {
        if let Err(error) = heap.release_host_handle(root) {
            first_error.get_or_insert(error);
        }
    }
    if let Err(error) = heap.release_host_handle(job.callback) {
        first_error.get_or_insert(error);
    }
    if let Err(error) = heap.release_external_memory(job.accounting) {
        first_error.get_or_insert(error);
    }
    first_error.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Engine, EngineConfig, ExecutionCompletion};
    use ogma_compiler::compile_to_bytecode;
    use ogma_frontend::{ParseRequestOptions, SourceFile, parse};
    use oxc_allocator::Allocator;

    fn compile_source(source_text: &str) -> Vec<Vec<u8>> {
        let allocator = Allocator::default();
        let source = SourceFile::new("job-test", "memory:///job-test.js", None, source_text)
            .expect("valid source");
        let parsed =
            parse(&allocator, &source, ParseRequestOptions::default()).expect("source parses");
        compile_to_bytecode(&parsed)
            .expect("source compiles")
            .functions
            .into_iter()
            .map(|function| function.artifact.bytes)
            .collect()
    }

    fn load_callback(context: &mut crate::Context<'_>, source: &str) -> crate::Value {
        context
            .load_module(compile_source(source))
            .expect("load OGMB");
        let factory = context
            .functions()
            .expect("function metadata")
            .into_iter()
            .find(|function| function.name == "factory")
            .expect("factory entry");
        let report = context
            .execute(&factory.id, &[], ExecutionRequest::default())
            .expect("execute factory");
        match report.completion {
            ExecutionCompletion::Return(value)
                if value.value_type() == ogma_runtime::ValueType::Function =>
            {
                value
            }
            completion => panic!("expected callback function, got {completion:?}"),
        }
    }

    #[test]
    fn queue_is_bounded_and_releases_roots_on_cancellation() {
        let engine = Engine::new(EngineConfig {
            max_pending_jobs: 1,
            ..EngineConfig::default()
        })
        .expect("valid Engine");
        let mut runtime = engine.create_runtime().expect("Runtime");
        let mut context = runtime.create_context().expect("Context");
        let callback = load_callback(
            &mut context,
            "function factory() { return function() { return 1; }; }",
        );

        context.enqueue_promise_job(callback, &[]).expect("enqueue");
        assert!(matches!(
            context.enqueue_promise_job(callback, &[]),
            Err(JobQueueError::QueueFull { capacity: 1 })
        ));
        assert!(context.heap_stats().external_memory_bytes > 0);
        let live_before_cancel = context.heap_stats().live_objects;
        assert_eq!(context.cancel_pending_promise_jobs().expect("cancel"), 1);
        assert_eq!(context.pending_promise_jobs(), 0);
        assert_eq!(context.heap_stats().external_memory_bytes, 0);
        context.collect().expect("collect after releasing roots");
        assert!(context.heap_stats().live_objects < live_before_cancel);
    }

    #[test]
    fn enqueue_rejects_oversized_argument_vectors_before_rooting() {
        let engine = Engine::new(EngineConfig::default()).expect("valid Engine");
        let mut runtime = engine.create_runtime().expect("Runtime");
        let mut context = runtime.create_context().expect("Context");
        let callback = load_callback(
            &mut context,
            "function factory() { return function() { return 1; }; }",
        );
        let arguments = vec![crate::Value::undefined(); crate::MAX_HOST_ARGUMENTS + 1];

        assert!(matches!(
            context.enqueue_promise_job(callback, &arguments),
            Err(JobQueueError::TooManyArguments {
                count,
                maximum
            }) if count == crate::MAX_HOST_ARGUMENTS + 1
                && maximum == crate::MAX_HOST_ARGUMENTS
        ));
        assert_eq!(context.pending_promise_jobs(), 0);
        assert_eq!(context.heap_stats().external_memory_bytes, 0);
    }

    #[test]
    fn pump_preserves_fifo_budget_and_roots_callbacks_across_collection() {
        let engine = Engine::new(EngineConfig {
            max_jobs_per_pump: 1,
            ..EngineConfig::default()
        })
        .expect("valid Engine");
        let mut runtime = engine.create_runtime().expect("Runtime");
        let mut context = runtime.create_context().expect("Context");
        let callback = load_callback(
            &mut context,
            "function factory() { return function() { return 1; }; }",
        );
        context.enqueue_promise_job(callback, &[]).expect("first");
        context.enqueue_promise_job(callback, &[]).expect("second");
        assert!(matches!(
            context.pump_promise_jobs(Some(2)),
            Err(JobQueueError::InvalidPumpBudget)
        ));
        assert_eq!(context.pending_promise_jobs(), 2);
        context.collect().expect("queued callback roots survive GC");

        let first = context.pump_promise_jobs(None).expect("first pump");
        assert_eq!(first.outcome, JobPumpOutcome::BudgetExhausted);
        assert_eq!(first.jobs_executed, 1);
        assert_eq!(context.pending_promise_jobs(), 1);

        let second = context.pump_promise_jobs(None).expect("second pump");
        assert_eq!(second.outcome, JobPumpOutcome::Progress);
        assert_eq!(second.jobs_executed, 1);
        assert_eq!(context.pending_promise_jobs(), 0);

        assert_eq!(
            context.pump_promise_jobs(None).expect("idle pump").outcome,
            JobPumpOutcome::Idle
        );
    }

    #[test]
    fn run_until_idle_respects_total_budget_across_bounded_turns() {
        let engine = Engine::new(EngineConfig {
            max_jobs_per_pump: 1,
            ..EngineConfig::default()
        })
        .expect("valid Engine");
        let mut runtime = engine.create_runtime().expect("Runtime");
        let mut context = runtime.create_context().expect("Context");
        let callback = load_callback(
            &mut context,
            "function factory() { return function() { return 1; }; }",
        );
        context.enqueue_promise_job(callback, &[]).expect("first");
        context.enqueue_promise_job(callback, &[]).expect("second");

        let report = context
            .run_promise_jobs_until_idle(Some(2))
            .expect("drain queue");
        assert_eq!(report.outcome, JobPumpOutcome::Progress);
        assert_eq!(report.jobs_executed, 2);
        assert_eq!(context.pending_promise_jobs(), 0);
    }

    #[test]
    fn pause_resume_and_shutdown_have_distinct_terminal_behavior() {
        let engine = Engine::new(EngineConfig::default()).expect("valid Engine");
        let mut runtime = engine.create_runtime().expect("Runtime");
        let mut context = runtime.create_context().expect("Context");
        let callback = load_callback(
            &mut context,
            "function factory() { return function() { return 1; }; }",
        );
        context.enqueue_promise_job(callback, &[]).expect("enqueue");

        context.pause_promise_jobs();
        assert_eq!(
            context.pump_promise_jobs(None).unwrap().outcome,
            JobPumpOutcome::Paused
        );
        assert_eq!(context.pending_promise_jobs(), 1);
        context.resume_promise_jobs();
        assert_eq!(
            context.pump_promise_jobs(None).unwrap().outcome,
            JobPumpOutcome::Progress
        );

        context
            .enqueue_promise_job(callback, &[])
            .expect("enqueue again");
        context.shutdown_promise_jobs().expect("shutdown");
        assert_eq!(context.promise_job_state(), JobQueueState::Shutdown);
        assert_eq!(context.pending_promise_jobs(), 0);
        assert_eq!(context.heap_stats().external_memory_bytes, 0);
        assert!(matches!(
            context.enqueue_promise_job(callback, &[]),
            Err(JobQueueError::ShuttingDown)
        ));
        assert_eq!(
            context.pump_promise_jobs(None).unwrap().outcome,
            JobPumpOutcome::Shutdown
        );
    }

    #[test]
    fn queued_job_keeps_its_module_and_registry_after_context_recreation() {
        let engine = Engine::new(EngineConfig::default()).expect("valid Engine");
        let mut runtime = engine.create_runtime().expect("Runtime");
        let mut first = runtime.create_context().expect("first Context");
        let callback = load_callback(
            &mut first,
            "function factory() { return function() { return 7; }; }",
        );
        first.enqueue_promise_job(callback, &[]).expect("enqueue");
        drop(first);

        let mut second = runtime.create_context().expect("second Context");
        let report = second
            .pump_promise_jobs(None)
            .expect("job uses its originating module");
        assert_eq!(report.outcome, JobPumpOutcome::Progress);
        assert_eq!(report.jobs_executed, 1);
        assert!(report.completion.is_none());
        assert_eq!(second.pending_promise_jobs(), 0);
        assert_eq!(second.heap_stats().external_memory_bytes, 0);
    }

    #[test]
    fn callbacks_from_earlier_loaded_modules_remain_schedulable() {
        let engine = Engine::new(EngineConfig::default()).expect("valid Engine");
        let mut runtime = engine.create_runtime().expect("Runtime");
        let mut context = runtime.create_context().expect("Context");
        let callback = load_callback(
            &mut context,
            "function factory() { return function() { return 7; }; }",
        );
        assert_eq!(context.loaded_module_count(), 1);

        context
            .load_module(compile_source("function unrelated() { return 9; }"))
            .expect("load a second verified module");
        assert_eq!(context.loaded_module_count(), 2);

        context
            .enqueue_promise_job(callback, &[])
            .expect("callback owner is the first loaded module");
        let report = context
            .pump_promise_jobs(None)
            .expect("dispatch callback from first module");
        assert_eq!(report.outcome, JobPumpOutcome::Progress);
        assert_eq!(report.jobs_executed, 1);
        assert_eq!(context.pending_promise_jobs(), 0);
        assert_eq!(context.heap_stats().external_memory_bytes, 0);
    }

    #[test]
    fn module_registry_limit_rejects_extra_modules_without_replacing_active_module() {
        let engine = Engine::new(EngineConfig {
            max_loaded_modules: 1,
            ..EngineConfig::default()
        })
        .expect("valid Engine");
        let mut runtime = engine.create_runtime().expect("Runtime");
        let mut context = runtime.create_context().expect("Context");
        let _callback = load_callback(
            &mut context,
            "function factory() { return function() { return 7; }; }",
        );
        assert_eq!(context.loaded_module_count(), 1);
        let error = context
            .load_module(compile_source("function unrelated() { return 9; }"))
            .expect_err("module capacity is enforced");
        assert!(matches!(
            error,
            crate::InterpreterError::ModuleLimitExceeded { limit: 1 }
        ));
        assert_eq!(context.loaded_module_count(), 1);
        assert!(
            context
                .functions()
                .expect("active module remains available")
                .iter()
                .any(|function| function.name == "factory")
        );
    }

    #[test]
    fn pump_observes_cancellation_at_interpreter_entry_and_releases_the_job() {
        let engine = Engine::new(EngineConfig::default()).expect("valid Engine");
        let mut runtime = engine.create_runtime().expect("Runtime");
        let mut context = runtime.create_context().expect("Context");
        let callback = load_callback(
            &mut context,
            "function factory() { return function() { return 1; }; }",
        );
        context.enqueue_promise_job(callback, &[]).expect("enqueue");

        let cancellation = crate::CancellationToken::new();
        cancellation.cancel();
        let report = context
            .pump_promise_jobs_with_request(
                None,
                ExecutionRequest {
                    cancellation: Some(cancellation),
                    ..ExecutionRequest::default()
                },
            )
            .expect("cancellation is an execution completion");
        assert_eq!(report.outcome, JobPumpOutcome::Interrupted);
        assert_eq!(
            report.interruption,
            Some(crate::ExecutionInterruption::Cancelled)
        );
        assert_eq!(report.jobs_executed, 0);
        assert_eq!(context.pending_promise_jobs(), 1);
        assert!(context.heap_stats().external_memory_bytes > 0);

        let resumed = context.pump_promise_jobs(None).expect("resume pending job");
        assert_eq!(resumed.outcome, JobPumpOutcome::Progress);
        assert_eq!(resumed.jobs_executed, 1);
        assert_eq!(context.pending_promise_jobs(), 0);
        assert_eq!(context.heap_stats().external_memory_bytes, 0);
    }

    #[test]
    fn invalid_execution_request_does_not_consume_a_queued_job() {
        let engine = Engine::new(EngineConfig::default()).expect("valid Engine");
        let mut runtime = engine.create_runtime().expect("Runtime");
        let mut context = runtime.create_context().expect("Context");
        let callback = load_callback(
            &mut context,
            "function factory() { return function() { return 1; }; }",
        );
        context.enqueue_promise_job(callback, &[]).expect("enqueue");

        let error = context
            .pump_promise_jobs_with_request(
                None,
                ExecutionRequest {
                    instruction_budget: Some(0),
                    ..ExecutionRequest::default()
                },
            )
            .expect_err("zero fuel must be rejected before queue mutation");
        assert!(matches!(
            error,
            JobQueueError::Configuration(crate::ConfigurationError::InvalidExecutionFuel)
        ));
        assert_eq!(context.pending_promise_jobs(), 1);
        assert!(context.heap_stats().external_memory_bytes > 0);
    }

    #[test]
    fn unrepresentable_time_budget_does_not_consume_a_queued_job() {
        let engine = Engine::new(EngineConfig::default()).expect("valid Engine");
        let mut runtime = engine.create_runtime().expect("Runtime");
        let mut context = runtime.create_context().expect("Context");
        let callback = load_callback(
            &mut context,
            "function factory() { return function() { return 1; }; }",
        );
        context.enqueue_promise_job(callback, &[]).expect("enqueue");

        assert!(matches!(
            context.pump_promise_jobs_with_request(
                None,
                ExecutionRequest {
                    time_budget: Some(std::time::Duration::MAX),
                    ..ExecutionRequest::default()
                },
            ),
            Err(JobQueueError::TimeBudgetOutOfRange)
        ));
        assert_eq!(context.pending_promise_jobs(), 1);
        assert!(context.heap_stats().external_memory_bytes > 0);
    }

    #[test]
    fn zero_time_budget_keeps_the_unstarted_job_for_a_later_pump() {
        let engine = Engine::new(EngineConfig::default()).expect("valid Engine");
        let mut runtime = engine.create_runtime().expect("Runtime");
        let mut context = runtime.create_context().expect("Context");
        let callback = load_callback(
            &mut context,
            "function factory() { return function() { return 1; }; }",
        );
        context.enqueue_promise_job(callback, &[]).expect("enqueue");

        let report = context
            .run_promise_jobs_until_idle_with_request(
                Some(1),
                ExecutionRequest {
                    time_budget: Some(std::time::Duration::ZERO),
                    ..ExecutionRequest::default()
                },
            )
            .expect("deadline is a normal interruption");
        assert_eq!(report.outcome, JobPumpOutcome::Interrupted);
        assert_eq!(report.jobs_executed, 0);
        assert_eq!(context.pending_promise_jobs(), 1);

        let resumed = context.pump_promise_jobs(None).expect("resume pending job");
        assert_eq!(resumed.outcome, JobPumpOutcome::Progress);
        assert_eq!(resumed.jobs_executed, 1);
    }

    #[test]
    fn queued_callable_from_another_module_is_rejected_even_when_ids_overlap() {
        let engine = Engine::new(EngineConfig::default()).expect("valid Engine");
        let mut runtime = engine.create_runtime().expect("Runtime");
        let mut first = runtime.create_context().expect("first Context");
        let foreign_callback = load_callback(
            &mut first,
            "function factory() { return function() { return 1; }; }",
        );
        let mut second = runtime.create_context().expect("second Context");
        second
            .load_module(compile_source(
                "function factory() { return function() { return 2; }; }",
            ))
            .expect("load a distinct module");

        assert!(matches!(
            second.enqueue_promise_job(foreign_callback, &[]),
            Err(JobQueueError::InvalidCallback)
        ));
        assert_eq!(second.pending_promise_jobs(), 0);
    }

    #[test]
    fn thrown_job_completion_is_persisted_for_host_observation() {
        let engine = Engine::new(EngineConfig::default()).expect("valid Engine");
        let mut runtime = engine.create_runtime().expect("Runtime");
        let mut context = runtime.create_context().expect("Context");
        let callback = load_callback(
            &mut context,
            "function factory() { return function() { throw 99; }; }",
        );
        context.enqueue_promise_job(callback, &[]).expect("enqueue");

        let report = context.pump_promise_jobs(None).expect("pump");
        assert_eq!(report.outcome, JobPumpOutcome::JobThrew);
        let Some(PersistentCompletion::Throw(handle)) = report.completion else {
            panic!("thrown job must expose a persistent Throw completion");
        };
        assert_eq!(context.resolve(handle).unwrap().as_number(), Some(99.0));
        context.release(handle).expect("release exception root");
    }
}
