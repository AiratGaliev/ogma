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

Each measurement records nearest-rank p50/p95/p99 and min/max latency, workload and callback throughput, allocation/reallocation/deallocation counts and bytes, application lock acquisitions/contentions/wait time, synthetic runtime-sync acquisitions/contentions/wait time, scope entries, state borrows and a final-state equivalence check.

A separate four-thread shared-lock probe reports contention and per-operation tail latency. The owner-thread context is not accessed concurrently: cross-thread use is represented by owned commands and tested through the bounded dispatcher.

The counting global allocator is enabled only in this standalone proof executable. Its process-wide counters include harness allocations occurring inside the measured interval. Instrumented mutexes use try_lock to distinguish uncontended and contended acquisitions, so the instrumentation itself adds a small and symmetric probe cost to mutex paths.

## Reproduction

From the repository root, using the pinned Rust toolchain from the project workflow:

    cargo fmt --manifest-path benchmarks/host-execution-scope/Cargo.toml --all -- --check
    cargo clippy --manifest-path benchmarks/host-execution-scope/Cargo.toml --all-targets --all-features --locked -- -D warnings
    cargo test --manifest-path benchmarks/host-execution-scope/Cargo.toml --all-features --locked
    cargo run --manifest-path benchmarks/host-execution-scope/Cargo.toml --locked --release --bin ogma-host-scope-bench -- --iterations 10000 --warmups 3 --samples 101 --revision <source-sha> --tested-revision <tested-sha> --cpu "<cpu model>" --output ogm-20-host-execution-scope-report.json

The runner rejects fewer than 101 measurement samples. The report includes source revision, tested revision, OS, architecture, CPU model, Rust compiler version, build profile, iterations, warmups and sample count.

## Interpretation and limitations

This is an M0 mock host-dispatch proof, not an ECMAScript parser, interpreter, production Ogma Runtime or production binding integration. It does not claim QuickJS/rquickjs or another third-party library has no internal locks.

No third-party binding/runtime library is linked into this proof. Therefore actual binding-library synchronization cannot be measured here. The additional mutex is an explicitly labelled synthetic control to keep independent runtime/library synchronization visible in the accounting; it must not be cited as a measurement of any real library.

The contention probe characterizes the shared-lock baseline under concurrent producers; it is not a claim that mutable owner-thread Runtime state should be shared across threads. Such work must be transferred as owned commands/tasks and applied by the Runtime owner. Benchmark timings have no machine-independent pass/fail threshold.

The experiment does not implement a production HostExecutionScope, typed host function table, FrameSnapshot, zero-copy buffers, ECS/physics adapters or a production async task bridge. It does not modify the CGE consumer or OGM-19 ABI proof.
