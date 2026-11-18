# OGM-24 — per-Isolate allocator and compile-unit arena proof

This standalone M0 Rust crate compares three quota-enforced allocation strategies and separately compares per-node system allocation with a resettable compile-unit bump arena. It is a raw-memory proof harness, not a production Ogma Runtime, JavaScript heap, garbage collector, AST or Core IR implementation.

## Compared runtime allocation strategies

- System allocator: platform Rust System allocator, with a per-candidate committed-byte quota wrapper. Every deallocation returns memory to System.
- Per-Isolate exact reuse: an owner-thread allocator instance with an exact size/alignment free list. Free blocks remain committed and count against the Isolate quota.
- Per-Isolate size-class pool: an owner-thread allocator with power-of-two buckets from 16 through 4096 bytes. Requests outside that size/alignment range fall back to System through the same quota/accounting path.

Every allocator instance owns its own counters and cache; it is deliberately not Send or Sync. The quota limits allocator-reserved backing bytes, not just live requested bytes. Before rejecting a new request, custom allocators evict cached blocks until the request fits or no cache can be reclaimed. Allocation tokens are tied to the allocator instance that created them; cross-Isolate deallocation is rejected and returns the token to the caller.

## Compile-unit arena comparison

The compiler-side benchmark compares individual System allocations/deallocations with a resettable bump arena. Both receive the same 256-node allocation sequence and a 2 MiB quota. Arena chunks remain committed and quota-accounted across compile-unit resets. The model is intended only for trivially reclaimable AST, semantic and Core IR scratch data whose lifetime ends with the compile unit. It does not model values with destructors, JS objects, closures, GC cells or cross-Isolate objects.

## Metrics and reproduction

Each candidate runs in its own subprocess so peak RSS high-water marks are isolated. Reports include:

- allocation and deallocation/reset p50, p95 and p99 latency;
- throughput (allocation operations/second for heap candidates, compile units/second for arena candidates);
- peak requested-live bytes, reserved bytes, free cached bytes, pool reuse/eviction counts and quota failures;
- arena payload, alignment padding, retained chunk capacity and chunk count;
- process peak RSS where the host exposes Linux /proc/self/status VmHWM;
- source/tested revisions, Rust compiler, OS, architecture, CPU and exact workload configuration.

Percentiles use nearest-rank samples of individually timed operations. Two Instant reads are included in every observation. No machine-independent timing threshold is used.

From the repository root, with the pinned Rust toolchain:

    cargo fmt --manifest-path benchmarks/isolate-allocator/Cargo.toml --all -- --check
    cargo clippy --manifest-path benchmarks/isolate-allocator/Cargo.toml --all-targets --all-features --locked -- -D warnings
    cargo test --manifest-path benchmarks/isolate-allocator/Cargo.toml --all-features --locked
    cargo run --manifest-path benchmarks/isolate-allocator/Cargo.toml --locked --release --bin ogma-isolate-allocator-bench -- --iterations 100 --warmups 3 --samples 101 --revision <source-sha> --tested-revision <tested-sha> --cpu "<cpu model>" --output ogm-24-allocator-arena-report.json

The feature-PR CI job executes these checks and uploads a machine-readable JSON evidence artifact.

## Interpretation limits

The System candidate exposes logical allocator accounting but cannot introspect the platform allocator's internal or external fragmentation. The reported internal-fragmentation proxy is reserved bytes minus live requested bytes; it includes pool-retained capacity and size-class slack, not OS allocator fragmentation. Peak RSS is a process-level high-water mark, including benchmark harness overhead, and is not attributed wholly to the allocator. These measurements do not establish Ogma Runtime or JS heap performance; they inform a future Runtime allocator contract only.
