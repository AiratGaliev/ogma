//! Runtime-owned, host-driven ECMAScript job queue.
//!
//! This is the scheduling substrate for PromiseJobs. It deliberately does not
//! implement Promise objects or Node-specific queues; those remain separate
//! language/compatibility layers.

use std::collections::VecDeque;

use ogma_runtime::{HeapError, HostHandle};

use crate::{
    ConfigurationError, Context, ExecutableModule, ExecutionCompletion, ExecutionInterruption,
    ExecutionRequest, InterpreterError, PersistentCompletion, PersistentHandle,
};

#[derive(Default)]
pub(super) struct JobScheduler {
    queue: VecDeque<PendingJob>,
    state: JobQueueState,
    pumping: bool,
}

struct PendingJob {
    module: ExecutableModule,
    callback: HostHandle,
    arguments: Vec<HostHandle>,
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
    /// A pump budget must be positive.
    InvalidPumpBudget,
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

        let module = self
            .module
            .as_ref()
            .ok_or(JobQueueError::ModuleNotLoaded)?
            .clone();
        if !module.contains_callable(&self.runtime.heap, callback)? {
            return Err(JobQueueError::InvalidCallback);
        }

        self.runtime
            .jobs
            .queue
            .try_reserve(1)
            .map_err(|_| JobQueueError::AllocationFailed)?;
        let mut argument_roots = Vec::new();
        argument_roots
            .try_reserve_exact(arguments.len())
            .map_err(|_| JobQueueError::AllocationFailed)?;

        let callback_root = self.runtime.heap.retain_host_value(callback)?;
        for argument in arguments {
            match self.runtime.heap.retain_host_value(*argument) {
                Ok(root) => argument_roots.push(root),
                Err(error) => {
                    for root in argument_roots.drain(..) {
                        let _ = self.runtime.heap.release_host_handle(root);
                    }
                    let _ = self.runtime.heap.release_host_handle(callback_root);
                    return Err(JobQueueError::Heap(error));
                }
            }
        }

        self.runtime.jobs.queue.push_back(PendingJob {
            module,
            callback: callback_root,
            arguments: argument_roots,
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

        self.runtime.jobs.pumping = true;
        let result = self.pump_promise_jobs_inner(budget);
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
        let total_budget = match total_budget {
            Some(0) => return Err(JobQueueError::InvalidPumpBudget),
            Some(budget) => budget,
            None => self.runtime.config.max_pending_jobs.max(1),
        };

        let mut jobs_executed = 0;
        while jobs_executed < total_budget {
            let turn_budget =
                (total_budget - jobs_executed).min(self.runtime.config.max_jobs_per_pump);
            let mut turn = self.pump_promise_jobs(Some(turn_budget))?;
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

    fn pump_promise_jobs_inner(&mut self, budget: usize) -> Result<JobPumpReport, JobQueueError> {
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
            let options = match ExecutionRequest::default().options(&self.runtime.config) {
                Ok(options) => options,
                Err(error) => {
                    let _ = release_job_roots(&mut self.runtime.heap, job);
                    return Err(JobQueueError::Configuration(error));
                }
            };
            let mut host = self.host_functions.clone();
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
        while let Some(job) = self.runtime.jobs.queue.pop_front() {
            release_job_roots(&mut self.runtime.heap, job)?;
            cancelled += 1;
        }
        Ok(cancelled)
    }

    /// Irreversibly shuts down the queue and releases every pending job root.
    pub fn shutdown_promise_jobs(&mut self) -> Result<(), JobQueueError> {
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
            "function factory() { return function callback() { return 1; }; }",
        );

        context.enqueue_promise_job(callback, &[]).expect("enqueue");
        assert!(matches!(
            context.enqueue_promise_job(callback, &[]),
            Err(JobQueueError::QueueFull { capacity: 1 })
        ));
        let live_before_cancel = context.heap_stats().live_objects;
        assert_eq!(context.cancel_pending_promise_jobs().expect("cancel"), 1);
        assert_eq!(context.pending_promise_jobs(), 0);
        context.collect().expect("collect after releasing roots");
        assert!(context.heap_stats().live_objects < live_before_cancel);
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
            "function factory() { return function callback() { return 1; }; }",
        );
        context.enqueue_promise_job(callback, &[]).expect("first");
        context.enqueue_promise_job(callback, &[]).expect("second");
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
            "function factory() { return function callback() { return 1; }; }",
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
            "function factory() { return function callback() { return 1; }; }",
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
    fn thrown_job_completion_is_persisted_for_host_observation() {
        let engine = Engine::new(EngineConfig::default()).expect("valid Engine");
        let mut runtime = engine.create_runtime().expect("Runtime");
        let mut context = runtime.create_context().expect("Context");
        let callback = load_callback(
            &mut context,
            "function factory() { return function callback() { throw 99; }; }",
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
