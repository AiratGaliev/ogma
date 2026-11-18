# OGM-20 — Host Execution Scope M0 proof

This standalone, dependency-free Rust crate compares two equivalent mock host-call paths:

1. **Per-callback shared locking** — each host callback acquires an application-level Arc<Mutex<HostState>>.
2. **Owner-thread execution scope** — one owner-thread HostExecutionScope is entered for a logical execution; callbacks access state through short checked borrows. Nested execution requires an explicit nested scope.

The workload set is deliberately narrow: no-op callback, typed numeric callback and a nested callback sequence. It isolates the dispatch/host-state boundary without pulling in a production JavaScript engine or changing the CGE consumer.

## Architecture invariants proved

- A mutable host execution context belongs to one owner thread and is intentionally !Send + !Sync.
- A top-level execution enters exactly one scope; nested execution enters a distinct nested scope.
- The scope stores no &mut HostState. Each host callback takes and releases a short RefCell borrow before arbitrary script/user code may execute.
- A conflicting nested mutable borrow returns BorrowConflict deterministically rather than creating aliasing or retaining a mutable borrow across re-entry.
- Cross-thread callers submit owned commands through a bounded dispatcher. Only its worker/owner thread touches the context.
- The dispatcher has explicit backpressure, queued-command cancellation, close-admission and shutdown/join behaviour.
- Rust callback panics are contained at the mock callback boundary; scope guards unwind normally.
- Application host-state locking and the optional synthetic runtime synchronization control have separate counters.

## Measurements

The benchmark runs both strategies with identical no-op, typed and nested workloads in two configurations:

- no additional runtime synchronization;
- a separate synthetic mutex acquired once per callback.

Each workload records p50/p95/p99/min/max from 10,000 individually timed workload iterations. A workload iteration is one no-op, one typed callback or a three-callback nested sequence. Two `Instant` reads are included in every single-iteration observation; an independent empty timer-pair baseline is reported and not subtracted. Throughput is measured separately from 101 batches of 10,000 iterations, and batch p50/p95/p99 are retained in the JSON report. This avoids mislabelling percentiles of batch means as individual-operation tail latency.

Reports also include allocation/reallocation/deallocation counts and bytes, application lock acquisitions/contentions/wait time, synthetic-control lock acquisitions/contentions/wait time, scope entries, state borrows and a final-state equivalence check.

A separate four-thread shared-lock probe reports contention and per-operation tail latency. The owner-thread context is not accessed concurrently: cross-thread use is represented by owned commands and tested through the bounded dispatcher.

The counting global allocator is enabled only in this standalone proof executable. Sample storage is preallocated before the measured interval, so allocation deltas cover allocations performed by the measured workload rather than the benchmark's sample buffer. Counters are process-wide and include any process activity that occurs during the measured interval. Instrumented mutexes use try_lock to distinguish uncontended and contended acquisitions, so the instrumentation itself adds a small probe cost to mutex paths.

## Reproduction

From the repository root, using the pinned Rust toolchain from the project workflow:

    cargo fmt --manifest-path benchmarks/host-execution-scope/Cargo.toml --all -- --check
    cargo clippy --manifest-path benchmarks/host-execution-scope/Cargo.toml --all-targets --all-features --locked -- -D warnings
    cargo test --manifest-path benchmarks/host-execution-scope/Cargo.toml --all-features --locked
    cargo run --manifest-path benchmarks/host-execution-scope/Cargo.toml --locked --release --bin ogma-host-scope-bench -- --iterations 10000 --warmups 3 --samples 101 --revision <source-sha> --tested-revision <tested-sha> --cpu "<cpu model>" --output ogm-20-host-execution-scope-report.json

The runner rejects fewer than 101 measurement samples. The report includes source revision, tested revision, OS, architecture, CPU model, Rust compiler version, build profile, iterations, warmups and sample count.

## Interpretation and limitations

This is an M0 mock host-dispatch proof, not an ECMAScript parser, interpreter, production Ogma Runtime or production binding integration. It does not claim QuickJS/rquickjs or another third-party library has no internal locks.

The exact binding-library synchronization profile used by the existing CGE reference consumer is source-audited and reported separately from the mock timings:

- CGE Experiments pins `rquickjs` 0.14.0 with default features and `std`; it does not enable `parallel` or `futures`.
- In this configuration, rquickjs stores `Runtime` through `Rc<Mut<RawRuntime>>`; `Mut` wraps `core::cell::RefCell`, and `Mut::lock()` calls `borrow_mut()`. This is single-thread borrow exclusion, not a cross-thread OS mutex.
- `Context::with()` acquires that runtime borrow once and keeps it for the closure/JS execution. The CGE `call_hook()` enters one `Context::with()` around the lifecycle function call, so the library borrow is one per hook/frame execution, not one additional borrow per host callback.
- Enabling rquickjs `parallel` changes the holder to `Arc<Mutex<...>>`; the `futures` path exposes `AsyncRuntime` backed by `async_lock::Mutex`. Neither feature is enabled by the inspected CGE dependency configuration.

This is a source audit and exact lock/entry-count report, not a timed rquickjs benchmark: the Ogma proof crate intentionally has no rquickjs runtime dependency. The independent synthetic mutex remains only a control for demonstrating a separate synchronization cost; it is not attributed to rquickjs.

The contention probe characterizes the shared-lock baseline under concurrent producers; it is not a claim that mutable owner-thread Runtime state should be shared across threads. Such work must be transferred as owned commands/tasks and applied by the Runtime owner. Benchmark timings have no machine-independent pass/fail threshold.

The experiment does not implement a production HostExecutionScope, typed host function table, FrameSnapshot, zero-copy buffers, ECS/physics adapters or a production async task bridge. It does not modify the CGE consumer or OGM-19 ABI proof.

## M0 recommendation

Use an owner-thread HostExecutionScope as the default path for host callbacks whose mutable state belongs to the Runtime owner thread. Do not make an application-level Arc<Mutex<HostState>> acquisition mandatory for every callback when no cross-thread sharing is required.

Keep synchronization where the ownership model actually requires it. Calls arriving from other host threads must cross the boundary as owned commands/tasks through a bounded dispatcher with explicit backpressure, cancellation and shutdown. The Runtime owner applies those commands; mutable Runtime state is never shared between threads.

A scope must not retain an exclusive mutable borrow, raw reference or lock across arbitrary JavaScript execution, callback re-entry, await, yield or suspension. Nested execution receives an explicit nested scope; conflicting access fails deterministically. Any synchronization internal to a selected binding/runtime library remains a separate cost and must be measured for that concrete library/version.

This is an architecture recommendation, not a production performance claim. The mock benchmark compares dispatch mechanics and host-state locking; the current rquickjs consumer's internal borrow/lock profile is reported separately from those mock timings. End-to-end Ogma performance remains unmeasured until Ogma has a real Runtime/binding integration.

## Normative references

The architecture invariants follow the Ogma [architecture specification](https://app.notion.com/p/4167ef18f18181bd9b34f9dda764c92d#b5ca462a10c841279d724b74c44b239a), especially sections 8.9.1, 8.9.7 and 8.9.8. The motivating per-callback mutex pattern and concrete `rquickjs` dependency profile were inspected in [CGE Experiments scripting.rs](https://github.com/AiratGaliev/cge-experiments/blob/c47ad08c3da9a3857f8429cebbed6ce88969715d/rust-brick-breaker/src/scripting.rs) and [Cargo.toml](https://github.com/AiratGaliev/cge-experiments/blob/c47ad08c3da9a3857f8429cebbed6ce88969715d/rust-brick-breaker/Cargo.toml) at commit c47ad08c3da9a3857f8429cebbed6ce88969715d. The binding implementation was checked in [rquickjs `safe_ref.rs`](https://github.com/DelSkayn/rquickjs/blob/5a9560dd821d5498ded9e0745250d42fcadf8608/core/src/safe_ref.rs), [runtime/base.rs](https://github.com/DelSkayn/rquickjs/blob/5a9560dd821d5498ded9e0745250d42fcadf8608/core/src/runtime/base.rs) and [context/base.rs](https://github.com/DelSkayn/rquickjs/blob/5a9560dd821d5498ded9e0745250d42fcadf8608/core/src/context/base.rs). OGM-20 does not add rquickjs as an Ogma dependency or modify the CGE consumer.
