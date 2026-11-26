# Host Pump and Node-Compatible Scheduler Contract

- **Contract ID:** ogma-host-pump-contract
- **Contract version:** 1.0.0
- **Status:** normative scheduling contract and M0 reference proof; OGM-48 implements the bounded M1 callback-queue substrate, not full Promise or Node scheduler conformance
- **Node reference:** 26.13.0, source commit 18b32a3b1552e199a01f1a8d9fd64aa756d3b75b
- **Authority:** this document defines the host-pump and scheduling contract; [Compatibility Profile](compatibility-profile.md) remains the source of truth for compatibility claims and feature status.

The contract follows the authoritative [Ogma architecture specification](https://app.notion.com/p/4167ef18f18181bd9b34f9dda764c92d#b5ca462a10c841279d724b74c44b239a), especially sections 8.8, 11 and 20.6. It is an implementation-facing semantic decision. The production M1 Runtime and Rust embedding API are implemented by OGM-47. The OGM-48 scheduler substrate does not claim that Promise objects/reactions, the ESM module loader or the Node adapter already conform to this contract.

## 1. Goals and non-goals

Ogma has one host-controlled execution model. The Rust host owns the application event loop and decides when an Isolate is advanced. The Runtime must not create a second event loop, spawn a private scheduler thread, or require Tokio or libuv.

The same contract must work when the host is a game loop, native UI loop, desktop application or headless Rust service. A host may supply different clock, timer and I/O adapters, but the observable Promise and declared Node scheduling behaviour must not change merely because the host changes.

This contract covers the initial Node compatibility scheduling surface: PromiseJobs, queueMicrotask, process.nextTick, setTimeout, setInterval, setImmediate, their cancellation APIs and asynchronous host/I/O completions. It does not imply universal Node.js process or libuv compatibility.

Names such as pump, run-until-idle, pause and shutdown describe required operations and outcomes. They are semantic contracts, not frozen public Rust method signatures; the exact API surface belongs to the M1 Runtime implementation.

## 2. Ownership and dependency direction

| Concern | Owner | Contract |
|---|---|---|
| ECMAScript PromiseJobs | Shared Ogma Runtime core | One FIFO job queue per Runtime/Isolate. Promise reactions and queueMicrotask use this queue. |
| process.nextTick | Optional Node compatibility adapter | A distinct FIFO queue. It must never be merged into the PromiseJobs queue. |
| setTimeout / setInterval | Node compatibility adapter plus host timer provider | The adapter owns handles and callback state; the host provides a monotonic clock and wakes the owner when a deadline becomes due. |
| setImmediate | Node compatibility adapter | A distinct check-phase queue, dispatched on the appropriate host turn. |
| I/O and Rust async completion | Host I/O/executor adapter | Completion is transferred to the Runtime owner thread as an owned result. A completion thread must never execute JavaScript. |
| Callback errors | Runtime error boundary and Node adapter policy | JavaScript exceptions and rejected promises follow the declared Node-facing error policy; host failures are converted at the adapter boundary. |
| Pump lifecycle | Rust host | The host starts, polls, pauses, resumes and shuts down the Runtime. |

The Runtime core exposes generic job and host-callback primitives. It does not import or depend on process.nextTick, Node timers, setImmediate, Node process state, libuv or Tokio. The Node adapter depends on those generic Runtime/host contracts; the dependency must not point from the ECMAScript core back into the Node adapter.

Each Runtime/Isolate is thread-affine. Its mutable heap, execution frames and queues are accessed only on its owner thread. Other threads may submit bounded, owned commands or completion messages. They may not retain JS references or call into the Isolate directly. Rust Future and Waker layouts are not part of the public ABI.

## 3. Node callback checkpoints and observable ordering

A checkpoint is the scheduler safe point at which the currently eligible Node and ECMAScript job queues are drained before another host callback is dispatched. Synchronous JavaScript execution is never interrupted by a checkpoint.

The initial ordering policy is context-sensitive. A single global priority rule for nextTick versus PromiseJobs is incorrect because Node CommonJS entry execution and ESM module evaluation have different observable checkpoints.

### 3.1 CommonJS entry

After a CommonJS entry's synchronous body returns:

1. Drain the nextTick queue in FIFO order, including nextTick callbacks added by nextTick callbacks.
2. Drain the Runtime PromiseJobs/microtask queue in FIFO order. A nextTick scheduled by one of these PromiseJobs does not interrupt the current microtask drain.
3. If either queue received more work, repeat from step 1.
4. Dispatch no timer, immediate or I/O callback until the checkpoint is quiescent or the host pump yields at a documented budget boundary.

### 3.2 ESM evaluation and top-level await

ESM module evaluation and its top-level-await continuations run through the ECMAScript job machinery. At the evaluation checkpoint:

1. Drain PromiseJobs/microtasks created by module evaluation and its queued continuations.
2. Drain nextTick callbacks queued by that evaluation.
3. Drain PromiseJobs created by those nextTick callbacks.
4. Repeat while either queue has work, then allow the next host callback.

The reference fixture deliberately includes top-level await. Its result is pinned in the Node baseline; it prevents an implementation from applying the CommonJS entry priority to every module context.

### 3.3 Timer, immediate and I/O callbacks

After a Node timer callback, setImmediate callback or delivered I/O completion callback returns:

1. Drain nextTick callbacks in FIFO order, including re-entrant nextTick work.
2. Drain PromiseJobs/microtasks in FIFO order.
3. Repeat if either queue received work, without interrupting a PromiseJobs drain to run a newly queued nextTick.
4. Only then dispatch another host callback.

A Promise reaction or queueMicrotask callback is itself a job, not a new host callback. Jobs queued by a running microtask remain in the current microtask drain; nextTick work queued from that microtask is serviced at the next checkpoint boundary before another timer, immediate or I/O callback.

### 3.4 Timers and immediates

The adapter must preserve the observable Node contract for timer and immediate callbacks, including cancellation and scheduling from inside I/O callbacks.

The pinned I/O fixture asserts the observable case in which setImmediate scheduled from an I/O callback runs before a zero-delay timer scheduled by that same callback. An immediate scheduled from an immediate callback is deferred to a subsequent check turn.

There is intentionally no universal ordering assertion between setTimeout(0) and setImmediate when both are scheduled from top-level code. Their order depends on the event-loop phase and timing; the contract must not invent a global priority.

Timer deadlines use a monotonic clock and are eligibility thresholds, not promises of exact wall-clock execution. Do not infer a total ordering for timers with effectively equal deadlines beyond behaviour explicitly captured by the pinned fixtures. A delayed host poll must not cause an interval to enqueue an unbounded catch-up burst.

### 3.5 I/O completion handoff

A host adapter observes completion using its own executor or I/O provider, then posts an owned completion record to the Isolate's owner thread. Posting may wake the host event loop. JavaScript runs only when the host next drives the owner-thread pump.

The adapter dispatches ready completions in the order they are accepted by the owner-thread queue. No submission-order guarantee is made for independent asynchronous operations whose completion order is controlled by the host or operating system.

Each delivered completion is a host callback and receives the callback checkpoint in section 3.3. Completion errors are passed as values to the Node adapter; they do not unwind a Rust stack or cross the ABI as a panic.

## 4. Host pump contract

The following operations are required semantically. Their final Rust types and names may be refined during M1, but their behaviour may not be weakened.

### 4.1 pump and poll

- Pumping is non-blocking and runs on the Isolate owner thread.
- A pump processes work that is already eligible: posted host commands/completions, due timers, immediate callbacks, and the required nextTick/PromiseJobs checkpoints.
- The host may call pump from its own event loop, frame update or UI dispatch. Ogma does not take ownership of that loop.
- Nested/re-entrant pump calls for the same Isolate are rejected deterministically. A host callback may request future work, but it may not recursively start a second event loop.
- The host supplies a callback/job/time budget. If the budget is exhausted while eligible work remains, the result is BudgetExhausted, not Idle. Queue position and FIFO order are preserved for the next pump.
- A budget boundary may return control to the host, but it must not allow a lower-priority host callback to overtake a pending nextTick or PromiseJobs checkpoint.

### 4.2 run-until-idle

run-until-idle repeatedly performs bounded, non-blocking pump work until there is no immediately runnable work or its budget is exhausted.

It does not wait for a future timer deadline, an unresolved I/O operation, a Rust Future or an external message. If such work is pending, it returns PendingExternalWork and enough scheduling information for the host to arrange the next wake-up (for example, the next timer deadline). Idle is returned only when there is no runnable work and no registered external work requiring a future wake-up. Budget exhaustion must never be reported as idle.

The host remains responsible for waiting or polling its own executor and for invoking the next pump when work becomes ready.

### 4.3 Outcomes

The semantic outcome set is:

| Outcome | Meaning |
|---|---|
| Progress | At least one eligible callback or job was executed. Counts may be returned for diagnostics and budgeting. |
| Idle | No runnable work and no pending external work. |
| PendingExternalWork | No work is currently runnable, but a timer, I/O operation or host message may wake the Isolate later. |
| BudgetExhausted | Eligible work remains but the current host-supplied budget was consumed. |
| Paused | The Runtime is paused; no user callback was dispatched. |
| Shutdown | The Runtime is terminal and cannot accept work. |

The exact Rust enum layout is deliberately not frozen here.

### 4.4 Current M1 implementation boundary (OGM-48)

The current Rust implementation is a bounded, host-driven callback-queue substrate in `ogma-embedding`:

- `EngineConfig.max_loaded_modules` bounds the number of verified modules retained by one Context; `max_pending_jobs` limits admitted jobs and `max_jobs_per_pump` caps each pump turn.
- Repeated `Context::load_module` calls register independent verified modules in the same Context. The most recently loaded module is the direct execution target; `load_module_with_id` also returns a stable identity scoped to its owning Runtime and Context so earlier modules can be introspected with `functions_in_module` and explicitly executed through `execute_in_module` without replacing the active target. `function_count_in_module`, `loaded_module_count` and `latest_loaded_module_id` expose registry state. The persistent execution counterpart promotes returned values to Runtime roots.
- `Context::enqueue_promise_job` accepts a verified callable Function owned by any module registered in that Context. It retains the exact originating `ExecutableModule`, host-function registry, callback and argument roots until dispatch or cancellation. Module execution and queued callback dispatch share the same Runtime heap; this does not yet implement ESM linking or shared module environments.
- `Context::pump_promise_jobs` dispatches one bounded FIFO turn. `pump_promise_jobs_with_request` applies the existing interpreter cancellation/deadline/fuel policy at ordinary VM safepoints.
- `Context::run_promise_jobs_until_idle` performs multiple bounded turns under a separate total-work budget. Its request-aware variant applies the same execution policy to each job.
- Pause/resume retains pending jobs. Queue cancellation and shutdown release persistent roots and external-memory accounting exactly once. A thrown completion is returned as a persistent handle owned by the caller.
- Function identities are unique across executable modules loaded in the process. Queue admission resolves the owner by that identity across the Context's registered modules, so a callback from an earlier module remains valid after a later module is loaded; a function from an unregistered module is still rejected.
- An invalid execution request is rejected before queue mutation. A deadline/cancellation observed before a job's first VM instruction leaves that job at the front of the queue with its roots retained; an interruption after execution has started is not retried because user code may already have produced side effects.
- A relative time budget supplied to a pump is converted once to an absolute monotonic deadline and is shared by every job in that pump/run-until-idle call.

This is intentionally **not yet the ECMAScript Promise implementation**. The Runtime does not yet expose Promise objects/reactions or `queueMicrotask`; JavaScript Promise settlement does not automatically enqueue through this API. The ESM graph loader, import/export linking and live-binding evaluation are also not implemented by this scheduler substrate. The callback queue validates the ownership, boundedness, host-pump and lifecycle primitives needed by those integrations, but it is not evidence of PromiseJobs or ESM conformance. Do not promote those features to SUPPORTED in the compatibility matrix based on these APIs alone.

## 5. Fairness and lifecycle

### 5.1 Fairness

The host budget prevents one Isolate from monopolising a game/UI loop or headless service thread. When the budget is reached, pump returns control to the host without changing queue priority or callback order. On the next pump, the pending checkpoint resumes before lower-priority host callbacks.

Node queue priority is observable behaviour, not a fairness hint. In particular, an endlessly replenished nextTick queue can starve PromiseJobs in Node. Ogma must not silently reorder Node queues to hide this behaviour. A host may regain control at budget boundaries, but it may not execute a later timer/I/O/immediate callback ahead of the pending checkpoint.

### 5.2 Pause and resume

Pause is a request to stop dispatching new user callbacks at the next safe point. It never interrupts a synchronous JavaScript callback or a currently executing job. Once Paused is reported:

- no user callback or PromiseJob is dispatched;
- queued nextTick and PromiseJobs, timers, immediates and delivered I/O completions are retained;
- host adapters may continue receiving external completions and enqueue them for the owner thread;
- resume returns the Runtime to Running and pending work follows the normal checkpoint and queue-order rules.

Timer deadlines continue to use the host's monotonic clock while paused. On resume, an overdue one-shot timer becomes eligible once. Missed interval periods are coalesced rather than replayed as an unbounded callback burst. This is an explicit host-pause policy, not a claim that Node exposes a pause API.

### 5.3 Cancellation

Cancellation is idempotent.

- clearTimeout, clearInterval and clearImmediate suppress a callback if cancellation wins before callback dispatch.
- An already executing JavaScript callback cannot be pre-empted by cancellation.
- I/O cancellation requests cancellation from the host provider. Completion-versus-cancellation races are resolved on the owner thread; a completion whose cancellation token is already cancelled is suppressed.
- A cancellation request does not reorder unrelated jobs and does not discard other callbacks.

The adapter must release timer, immediate and I/O handles exactly once, including shutdown paths.

### 5.4 Shutdown

Shutdown is terminal and idempotent.

- New evaluate/call/pump requests are rejected after shutdown begins.
- If requested from inside a callback, shutdown takes effect at the next safe point after that synchronous callback returns; it does not unwind or interrupt the running JavaScript stack.
- Pending timers, intervals and immediates are cancelled. Outstanding host I/O subscriptions receive cancellation requests.
- Queued user callbacks, nextTick callbacks and PromiseJobs are discarded when the Runtime becomes Closed. No user callback is dispatched after Closed.
- Runtime-owned handles and adapter registrations are released exactly once.

Shutdown is a host lifecycle operation, not a promise to reproduce Node process.exit or process-wide teardown behaviour.

## 6. Error handling

A synchronous exception thrown by a timer, immediate or I/O callback is reported through the Runtime error boundary and then handled by the Node adapter's declared uncaught-exception policy. It must not unwind through Rust host frames.

A rejected Promise is observed through the microtask checkpoint and the Node adapter's unhandled-rejection policy. The reference fixture pins the order observed for this combined scenario on Node.js 26.13.0: the nextTick exception is reported, the immediate callback runs, and the unhandledRejection event is emitted afterward. This is a version-specific interaction, not a universal ordering rule that unhandled rejections always follow immediates. The small fixture does not prove every Node process-exit or rejectionHandled edge case.

Host I/O/executor failures are delivered as owned completion errors and mapped by the compatibility adapter. Rust panics must not cross the public ABI. Error reporting must preserve the original callback/job identity sufficiently for deterministic diagnostics.

## 7. Reference proof and explicit limits

The focused reference corpus is [tests/node-scheduling](../tests/node-scheduling/README.md). It executes small black-box fixtures with the pinned Node.js 26.13.0 runtime and compares exact observable event traces with the checked-in baseline. CI invokes it after verifying the repository's .node-version.

This corpus proves the Node reference observations only. There is no Ogma Runtime or Rust workspace in the current M0 repository, so it cannot claim that an Ogma implementation already conforms. When the Runtime and Node adapter are implemented, the same scenarios must be run against Ogma and their traces compared with this baseline.

The proof intentionally does not reproduce libuv internals, its thread pool or phase implementation. It does not define universal top-level timer-versus-immediate ordering, exact timer deadlines, or submission ordering for independent host I/O operations. These behaviours are either phase-dependent or host-provided and must not be asserted more strongly than the pinned observable contract.

The Node adapter remains optional and separate from ECMAScript core. Runtime core must remain usable without Node APIs, Tokio or libuv.

## 8. Reference semantics

The compatibility rules above are grounded in the pinned Node.js 26.13.0 process and event-loop reference. The public Node documentation provides the corresponding semantic explanation:

- [process.nextTick and the Promise microtask queue](https://nodejs.org/api/process.html#when-to-use-queuemicrotask-vs-processnexttick) — CJS and ESM expose different nextTick/microtask ordering because ESM evaluation already runs in the microtask queue.
- [Event loop, timers and nextTick](https://nodejs.org/learn/asynchronous-work/event-loop-timers-and-nexttick) — I/O callback and check-phase ordering, including setImmediate versus timers.
- [setImmediate](https://nodejs.org/api/timers.html#setimmediatecallback-args) — callbacks scheduled from an executing immediate callback are deferred until the next event-loop iteration.

The pinned corpus asserts only ordering that the reference fixtures can establish without relying on wall-clock timing. The top-level timer/immediate cancellation fixture checks callback presence and cancellation, not a total order between those two top-level callbacks.

