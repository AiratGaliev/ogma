//! M0 proof comparing application-level locking with owner-thread execution scopes.
//!
//! This crate models host-call dispatch only. It does not embed an ECMAScript engine
//! or a third-party binding library. The optional runtime-sync mutex is a synthetic
//! control used to keep non-application synchronization visible in measurements.

use std::cell::{Cell, RefCell};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::thread::{self, JoinHandle, ThreadId};

const MAX_SCOPE_DEPTH: u32 = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LibrarySyncMode {
    None,
    SyntheticControl,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LockSnapshot {
    pub acquisitions: u64,
    pub contentions: u64,
    pub wait_ns: u64,
}

#[derive(Default)]
struct LockCounters {
    acquisitions: std::sync::atomic::AtomicU64,
    contentions: std::sync::atomic::AtomicU64,
    wait_ns: std::sync::atomic::AtomicU64,
}

impl LockCounters {
    fn snapshot(&self) -> LockSnapshot {
        LockSnapshot {
            acquisitions: self.acquisitions.load(Ordering::Relaxed),
            contentions: self.contentions.load(Ordering::Relaxed),
            wait_ns: self.wait_ns.load(Ordering::Relaxed),
        }
    }
}

struct InstrumentedMutex<T> {
    inner: Mutex<T>,
    counters: Arc<LockCounters>,
}

impl<T> InstrumentedMutex<T> {
    fn new(value: T) -> Self {
        Self {
            inner: Mutex::new(value),
            counters: Arc::new(LockCounters::default()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, T> {
        self.counters.acquisitions.fetch_add(1, Ordering::Relaxed);
        let started = std::time::Instant::now();
        match self.inner.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::Poisoned(error)) => error.into_inner(),
            Err(TryLockError::WouldBlock) => {
                self.counters.contentions.fetch_add(1, Ordering::Relaxed);
                let guard = self.inner.lock().unwrap_or_else(|error| error.into_inner());
                let waited = started.elapsed().as_nanos().min(u64::MAX as u128) as u64;
                self.counters.wait_ns.fetch_add(waited, Ordering::Relaxed);
                guard
            }
        }
    }

    fn snapshot(&self) -> LockSnapshot {
        self.counters.snapshot()
    }
}

#[derive(Default)]
struct HostState {
    value: f64,
    typed_calls: u64,
}

fn runtime_sync(mode: LibrarySyncMode) -> Option<Arc<InstrumentedMutex<()>>> {
    match mode {
        LibrarySyncMode::None => None,
        LibrarySyncMode::SyntheticControl => Some(Arc::new(InstrumentedMutex::new(()))),
    }
}

/// Baseline: a shared HostState protected by an application-level mutex.
/// Every host callback acquires that mutex independently.
pub struct SharedLockHost {
    state: Arc<InstrumentedMutex<HostState>>,
    runtime_sync: Option<Arc<InstrumentedMutex<()>>>,
}

impl SharedLockHost {
    pub fn new(mode: LibrarySyncMode) -> Self {
        Self {
            state: Arc::new(InstrumentedMutex::new(HostState::default())),
            runtime_sync: runtime_sync(mode),
        }
    }

    fn with_callback<R>(&self, callback: impl FnOnce(&mut HostState) -> R) -> R {
        let _runtime_guard = self.runtime_sync.as_ref().map(|lock| lock.lock());
        let mut state = self.state.lock();
        callback(&mut state)
    }

    pub fn noop(&self) -> f64 {
        self.with_callback(|state| std::hint::black_box(state.value))
    }

    pub fn typed_add(&self, delta: f64) -> f64 {
        self.with_callback(|state| {
            state.value += delta;
            state.typed_calls += 1;
            std::hint::black_box(state.value)
        })
    }

    /// Models a host callback that re-enters script execution and invokes two more
    /// host callbacks. The mutex is released between callbacks, so nested execution
    /// never runs while the application state lock is held.
    pub fn nested(&self, delta: f64) -> f64 {
        let before = self.noop();
        let _ = self.typed_add(delta);
        let after = self.noop();
        std::hint::black_box(after - before)
    }

    pub fn run_callback<R>(&self, callback: impl FnOnce() -> R) -> Result<R, HostError> {
        catch_unwind(AssertUnwindSafe(callback)).map_err(|_| HostError::CallbackPanicked)
    }

    pub fn application_lock_metrics(&self) -> LockSnapshot {
        self.state.snapshot()
    }

    pub fn runtime_sync_metrics(&self) -> LockSnapshot {
        self.runtime_sync
            .as_ref()
            .map_or(LockSnapshot::default(), |lock| lock.snapshot())
    }

    pub fn snapshot(&self) -> HostSnapshot {
        let state = self.state.lock();
        HostSnapshot {
            value: state.value,
            typed_calls: state.typed_calls,
        }
    }
}

/// Errors are deterministic and deliberately small: the proof models the host
/// execution contract, not production Runtime error taxonomy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostError {
    WrongThread,
    ScopeAlreadyActive,
    NoActiveScope,
    ScopeDepthExceeded,
    BorrowConflict,
    CallbackPanicked,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HostSnapshot {
    pub value: f64,
    pub typed_calls: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScopeMetrics {
    pub scope_enters: u64,
    pub state_borrows: u64,
    pub active_scopes: u32,
}

/// Owner-thread state. The type is intentionally !Send + !Sync: its mutable host
/// state is accessed on one owner thread through short, checked RefCell borrows.
///
/// It must not be moved to or shared with another thread:
///
/// ```compile_fail
/// use ogma_host_execution_scope_proof::HostExecutionContext;
/// fn require_send<T: Send>() {}
/// fn main() {
///     require_send::<HostExecutionContext>();
/// }
/// ```
///
/// ```compile_fail
/// use ogma_host_execution_scope_proof::HostExecutionContext;
/// fn require_sync<T: Sync>() {}
/// fn main() {
///     require_sync::<HostExecutionContext>();
/// }
/// ```
pub struct HostExecutionContext {
    owner_thread: ThreadId,
    state: RefCell<HostState>,
    active_scopes: Cell<u32>,
    scope_enters: Cell<u64>,
    state_borrows: Cell<u64>,
    runtime_sync: Option<Arc<InstrumentedMutex<()>>>,
    _not_send_or_sync: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl HostExecutionContext {
    pub fn new(mode: LibrarySyncMode) -> Self {
        Self {
            owner_thread: thread::current().id(),
            state: RefCell::new(HostState::default()),
            active_scopes: Cell::new(0),
            scope_enters: Cell::new(0),
            state_borrows: Cell::new(0),
            runtime_sync: runtime_sync(mode),
            _not_send_or_sync: std::marker::PhantomData,
        }
    }

    pub fn enter_scope(&self) -> Result<HostExecutionScope<'_>, HostError> {
        self.enter_scope_inner(false)
    }

    fn enter_scope_inner(&self, nested: bool) -> Result<HostExecutionScope<'_>, HostError> {
        if thread::current().id() != self.owner_thread {
            return Err(HostError::WrongThread);
        }
        let active = self.active_scopes.get();
        if nested && active == 0 {
            return Err(HostError::NoActiveScope);
        }
        if !nested && active != 0 {
            return Err(HostError::ScopeAlreadyActive);
        }
        if active >= MAX_SCOPE_DEPTH {
            return Err(HostError::ScopeDepthExceeded);
        }
        self.active_scopes.set(active + 1);
        self.scope_enters
            .set(self.scope_enters.get().saturating_add(1));
        Ok(HostExecutionScope { context: self })
    }

    pub fn metrics(&self) -> ScopeMetrics {
        ScopeMetrics {
            scope_enters: self.scope_enters.get(),
            state_borrows: self.state_borrows.get(),
            active_scopes: self.active_scopes.get(),
        }
    }

    pub fn runtime_sync_metrics(&self) -> LockSnapshot {
        self.runtime_sync
            .as_ref()
            .map_or(LockSnapshot::default(), |lock| lock.snapshot())
    }

    pub fn snapshot(&self) -> Result<HostSnapshot, HostError> {
        if thread::current().id() != self.owner_thread {
            return Err(HostError::WrongThread);
        }
        let state = self
            .state
            .try_borrow()
            .map_err(|_| HostError::BorrowConflict)?;
        Ok(HostSnapshot {
            value: state.value,
            typed_calls: state.typed_calls,
        })
    }
}

/// A scope binds host execution to the current owner thread. It deliberately stores
/// no reference to HostState: each callback takes and releases a short borrow before
/// arbitrary script code or nested execution can run.
pub struct HostExecutionScope<'a> {
    context: &'a HostExecutionContext,
}

impl HostExecutionScope<'_> {
    fn with_callback_state<R>(
        &self,
        callback: impl FnOnce(&mut HostState) -> R,
    ) -> Result<R, HostError> {
        let mut state = self
            .context
            .state
            .try_borrow_mut()
            .map_err(|_| HostError::BorrowConflict)?;
        let _runtime_guard = self.context.runtime_sync.as_ref().map(|lock| lock.lock());
        self.context
            .state_borrows
            .set(self.context.state_borrows.get().saturating_add(1));
        Ok(callback(&mut state))
    }

    pub fn noop(&self) -> Result<f64, HostError> {
        self.with_callback_state(|state| std::hint::black_box(state.value))
    }

    pub fn typed_add(&self, delta: f64) -> Result<f64, HostError> {
        self.with_callback_state(|state| {
            state.value += delta;
            state.typed_calls += 1;
            std::hint::black_box(state.value)
        })
    }

    /// Opens an explicit nested scope. No host-state borrow is held while user code
    /// runs, so a non-conflicting nested callback succeeds; an attempted re-entry
    /// while a mutable borrow is live is rejected by try_borrow_mut.
    pub fn nested_scope(&self) -> Result<HostExecutionScope<'_>, HostError> {
        self.context.enter_scope_inner(true)
    }

    pub fn nested(&self, delta: f64) -> Result<f64, HostError> {
        let before = self.noop()?;
        let nested = self.nested_scope()?;
        let _ = nested.typed_add(delta)?;
        let after = nested.noop()?;
        Ok(std::hint::black_box(after - before))
    }

    pub fn run_callback<R>(
        &self,
        callback: impl FnOnce(&HostExecutionScope<'_>) -> R,
    ) -> Result<R, HostError> {
        catch_unwind(AssertUnwindSafe(|| callback(self))).map_err(|_| HostError::CallbackPanicked)
    }
}

impl Drop for HostExecutionScope<'_> {
    fn drop(&mut self) {
        self.context
            .active_scopes
            .set(self.context.active_scopes.get().saturating_sub(1));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DispatchError {
    Closed,
    Backpressure,
    Cancelled,
    WorkerTerminated,
    Host(HostError),
}

#[derive(Clone, Debug)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

#[derive(Clone, Copy)]
enum Lifecycle {
    Open,
    Closing,
    Closed,
}

enum Message {
    Execute {
        delta: f64,
        cancellation: CancellationToken,
        reply: Sender<Result<f64, DispatchError>>,
    },
    #[cfg(test)]
    Block {
        entered: Sender<()>,
        release: Receiver<()>,
        cancellation: CancellationToken,
        reply: Sender<Result<f64, DispatchError>>,
    },
    Shutdown,
}

/// Cloneable producer handle. Cross-thread callers submit owned numeric commands;
/// the owner thread alone enters HostExecutionScope and mutates host state.
#[derive(Clone)]
pub struct DispatchHandle {
    sender: SyncSender<Message>,
    lifecycle: Arc<InstrumentedMutex<Lifecycle>>,
}

pub struct DispatchTicket {
    receiver: Receiver<Result<f64, DispatchError>>,
    cancellation: CancellationToken,
}

impl DispatchTicket {
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    pub fn wait(self) -> Result<f64, DispatchError> {
        self.receiver
            .recv()
            .map_err(|_| DispatchError::WorkerTerminated)?
    }
}

impl DispatchHandle {
    pub fn submit(&self, delta: f64) -> Result<DispatchTicket, DispatchError> {
        let lifecycle = self.lifecycle.lock();
        if !matches!(*lifecycle, Lifecycle::Open) {
            return Err(DispatchError::Closed);
        }
        let cancellation = CancellationToken {
            cancelled: Arc::new(AtomicBool::new(false)),
        };
        let (reply, receiver) = mpsc::channel();
        let message = Message::Execute {
            delta,
            cancellation: cancellation.clone(),
            reply,
        };
        match self.sender.try_send(message) {
            Ok(()) => Ok(DispatchTicket {
                receiver,
                cancellation,
            }),
            Err(TrySendError::Full(_)) => Err(DispatchError::Backpressure),
            Err(TrySendError::Disconnected(_)) => Err(DispatchError::WorkerTerminated),
        }
    }

    pub fn lifecycle_lock_metrics(&self) -> LockSnapshot {
        self.lifecycle.snapshot()
    }

    #[cfg(test)]
    fn submit_block(
        &self,
        entered: Sender<()>,
        release: Receiver<()>,
    ) -> Result<DispatchTicket, DispatchError> {
        let lifecycle = self.lifecycle.lock();
        if !matches!(*lifecycle, Lifecycle::Open) {
            return Err(DispatchError::Closed);
        }
        let cancellation = CancellationToken {
            cancelled: Arc::new(AtomicBool::new(false)),
        };
        let (reply, receiver) = mpsc::channel();
        let message = Message::Block {
            entered,
            release,
            cancellation: cancellation.clone(),
            reply,
        };
        match self.sender.try_send(message) {
            Ok(()) => Ok(DispatchTicket {
                receiver,
                cancellation,
            }),
            Err(TrySendError::Full(_)) => Err(DispatchError::Backpressure),
            Err(TrySendError::Disconnected(_)) => Err(DispatchError::WorkerTerminated),
        }
    }
}

/// Bounded owner-thread dispatcher. Shutdown closes admission, enqueues a terminal
/// message after all accepted commands, drains/cancels queued work according to each
/// ticket's cancellation token, then joins the owner thread.
pub struct HostDispatcher {
    handle: DispatchHandle,
    worker: Option<JoinHandle<()>>,
}

impl HostDispatcher {
    pub fn new(queue_capacity: usize) -> Self {
        let (sender, receiver) = mpsc::sync_channel(queue_capacity.max(1));
        let lifecycle = Arc::new(InstrumentedMutex::new(Lifecycle::Open));
        let worker = thread::spawn(move || worker_loop(receiver));
        Self {
            handle: DispatchHandle { sender, lifecycle },
            worker: Some(worker),
        }
    }

    pub fn handle(&self) -> DispatchHandle {
        self.handle.clone()
    }

    pub fn shutdown(&mut self) -> Result<(), DispatchError> {
        {
            let mut lifecycle = self.handle.lifecycle.lock();
            match *lifecycle {
                Lifecycle::Open => {
                    *lifecycle = Lifecycle::Closing;
                    self.handle
                        .sender
                        .send(Message::Shutdown)
                        .map_err(|_| DispatchError::WorkerTerminated)?;
                }
                Lifecycle::Closing | Lifecycle::Closed => {}
            }
        }

        if let Some(worker) = self.worker.take() {
            worker.join().map_err(|_| DispatchError::WorkerTerminated)?;
        }
        let mut lifecycle = self.handle.lifecycle.lock();
        *lifecycle = Lifecycle::Closed;
        Ok(())
    }
}

impl Drop for HostDispatcher {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn worker_loop(receiver: Receiver<Message>) {
    let context = HostExecutionContext::new(LibrarySyncMode::None);
    while let Ok(message) = receiver.recv() {
        match message {
            Message::Execute {
                delta,
                cancellation,
                reply,
            } => {
                let result = if cancellation.is_cancelled() {
                    Err(DispatchError::Cancelled)
                } else {
                    let result = context
                        .enter_scope()
                        .map_err(DispatchError::Host)
                        .and_then(|scope| scope.typed_add(delta).map_err(DispatchError::Host));
                    result
                };
                let _ = reply.send(result);
            }
            #[cfg(test)]
            Message::Block {
                entered,
                release,
                cancellation,
                reply,
            } => {
                let _ = entered.send(());
                let _ = release.recv();
                let result = if cancellation.is_cancelled() {
                    Err(DispatchError::Cancelled)
                } else {
                    Ok(0.0)
                };
                let _ = reply.send(result);
            }
            Message::Shutdown => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_strategies_produce_equivalent_noop_typed_and_nested_results() {
        for mode in [LibrarySyncMode::None, LibrarySyncMode::SyntheticControl] {
            let shared = SharedLockHost::new(mode);
            let owner = HostExecutionContext::new(mode);

            assert_eq!(shared.noop(), 0.0);
            let shared_typed = shared.typed_add(2.5);
            let shared_nested = shared.nested(1.25);
            let shared_snapshot = shared.snapshot();

            let scope = owner.enter_scope().unwrap();
            assert_eq!(scope.noop().unwrap(), 0.0);
            let owner_typed = scope.typed_add(2.5).unwrap();
            let owner_nested = scope.nested(1.25).unwrap();
            drop(scope);
            let owner_snapshot = owner.snapshot().unwrap();

            assert_eq!(shared_typed, owner_typed);
            assert_eq!(shared_nested, owner_nested);
            assert_eq!(shared_snapshot.value, owner_snapshot.value);
            assert_eq!(shared_snapshot.typed_calls, owner_snapshot.typed_calls);

            let shared_locks = shared.application_lock_metrics();
            // The explicit snapshot below is also a host-state access.
            assert_eq!(shared_locks.acquisitions, 6);
            assert_eq!(shared_locks.contentions, 0);
            let owner_metrics = owner.metrics();
            assert_eq!(owner_metrics.active_scopes, 0);
            assert_eq!(owner_metrics.scope_enters, 2);
            assert_eq!(owner_metrics.state_borrows, 5);
            assert_eq!(owner_metrics.active_scopes, 0);
            assert_eq!(
                owner.runtime_sync_metrics().acquisitions,
                shared.runtime_sync_metrics().acquisitions
            );
        }
    }

    #[test]
    fn a_conflicting_reentrant_borrow_is_rejected_deterministically() {
        let context = HostExecutionContext::new(LibrarySyncMode::None);
        let scope = context.enter_scope().unwrap();
        let result = scope
            .with_callback_state(|_| {
                let nested = scope.nested_scope().unwrap();
                nested.with_callback_state(|state| state.value += 1.0)
            })
            .unwrap();
        assert_eq!(result, Err(HostError::BorrowConflict));
        drop(scope);
        assert_eq!(context.metrics().active_scopes, 0);
    }

    #[test]
    fn top_level_scope_is_non_reentrant_and_nested_depth_is_bounded() {
        let context = HostExecutionContext::new(LibrarySyncMode::None);
        let root = context.enter_scope().unwrap();
        assert_eq!(
            context.enter_scope().err(),
            Some(HostError::ScopeAlreadyActive)
        );

        let nested = (0..MAX_SCOPE_DEPTH - 1)
            .map(|_| root.nested_scope().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            root.nested_scope().err(),
            Some(HostError::ScopeDepthExceeded)
        );
        assert_eq!(context.metrics().active_scopes, MAX_SCOPE_DEPTH);

        drop(nested);
        drop(root);
        assert_eq!(context.metrics().active_scopes, 0);
    }

    #[test]
    fn callback_panic_is_contained_and_scope_guards_unwind() {
        let shared = SharedLockHost::new(LibrarySyncMode::None);
        assert_eq!(
            shared.run_callback(|| panic!("synthetic callback failure")),
            Err(HostError::CallbackPanicked)
        );
        assert_eq!(shared.noop(), 0.0);

        let context = HostExecutionContext::new(LibrarySyncMode::None);
        {
            let scope = context.enter_scope().unwrap();
            assert_eq!(
                scope.run_callback(|_| panic!("synthetic callback failure")),
                Err(HostError::CallbackPanicked)
            );
            let nested = scope.nested_scope().unwrap();
            assert_eq!(
                nested.run_callback(|_| panic!("nested callback failure")),
                Err(HostError::CallbackPanicked)
            );
        }
        assert_eq!(context.metrics().active_scopes, 0);
    }

    #[test]
    fn owner_context_rejects_calls_from_non_owner_thread() {
        let mut context = HostExecutionContext::new(LibrarySyncMode::None);
        let other_thread_id = thread::spawn(|| thread::current().id()).join().unwrap();
        context.owner_thread = other_thread_id;
        assert_eq!(context.enter_scope().err(), Some(HostError::WrongThread));
    }

    #[test]
    fn bounded_dispatcher_cancels_queued_work_and_shuts_down_cleanly() {
        let mut dispatcher = HostDispatcher::new(1);
        let handle = dispatcher.handle();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();

        let blocking = handle.submit_block(entered_tx, release_rx).unwrap();
        entered_rx.recv().unwrap();

        let queued = handle.submit(7.0).unwrap();
        assert_eq!(handle.submit(8.0).err(), Some(DispatchError::Backpressure));
        queued.cancel();
        release_tx.send(()).unwrap();

        assert_eq!(blocking.wait(), Ok(0.0));
        assert_eq!(queued.wait(), Err(DispatchError::Cancelled));
        dispatcher.shutdown().unwrap();
        assert_eq!(handle.submit(9.0).err(), Some(DispatchError::Closed));
    }

    #[test]
    fn dispatcher_applies_owned_commands_on_its_owner_thread() {
        let mut dispatcher = HostDispatcher::new(8);
        let handle = dispatcher.handle();
        let first = handle.submit(2.0).unwrap();
        let second = handle.submit(3.0).unwrap();
        assert_eq!(first.wait(), Ok(2.0));
        assert_eq!(second.wait(), Ok(5.0));
        dispatcher.shutdown().unwrap();
        assert_eq!(handle.lifecycle_lock_metrics().acquisitions, 4);
    }

    #[test]
    fn concurrent_shared_lock_contention_is_counted() {
        let host = Arc::new(SharedLockHost::new(LibrarySyncMode::None));
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let holder = Arc::clone(&host);
        let holder_thread = thread::spawn(move || {
            holder.with_callback(|_| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            });
        });
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();

        let caller = Arc::clone(&host);
        let (attempt_tx, attempt_rx) = mpsc::channel();
        let caller_thread = thread::spawn(move || {
            attempt_tx.send(()).unwrap();
            caller.typed_add(1.0)
        });
        attempt_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let contention_observed = loop {
            if host.application_lock_metrics().contentions > 0 {
                break true;
            }
            if std::time::Instant::now() >= deadline {
                break false;
            }
            thread::yield_now();
        };

        release_tx.send(()).unwrap();
        holder_thread.join().unwrap();
        assert_eq!(caller_thread.join().unwrap(), 1.0);

        let metrics = host.application_lock_metrics();
        assert!(
            contention_observed,
            "the caller did not observe a held mutex"
        );
        assert_eq!(metrics.acquisitions, 2);
        assert_eq!(metrics.contentions, 1);
        assert!(metrics.wait_ns > 0);
    }
}
