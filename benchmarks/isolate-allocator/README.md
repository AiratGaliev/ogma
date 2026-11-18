# OGM-24 — per-Isolate allocator and compile-unit arena proof

This standalone M0 Rust crate compares three quota-enforced allocation strategies and separately compares per-node system allocation with a resettable compile-unit bump arena. It is a raw-memory proof harness, not a production Ogma Runtime, JavaScript heap, garbage collector, AST or Core IR implementation.

## Compared runtime allocation strategies

- System allocator: platform Rust System allocator, with a per-candidate committed-byte quota wrapper. Every deallocation returns memory to System.
- Per-Isolate exact reuse: an owner-thread allocator instance with an exact size/alignment free list. Free blocks remain committed and count against the Isolate quota.
- Per-Isolate size-class pool: an owner-thread allocator with power-of-two buckets from 16 through 4096 bytes. Requests outside that size/alignment range fall back to System through the same quota/accounting path.

Every allocator instance owns its own counters, live-block registry and cache; it is deliberately not Send or Sync. The quota limits allocator-reserved backing bytes, not just live requested bytes. Before rejecting a new request, custom allocators evict cached blocks until the request fits or no cache can be reclaimed. Allocation tokens are tied to the allocator instance that created them; cross-Isolate or stale deallocation is rejected and returns the token to the caller. Isolate shutdown frees both still-live and cached backing blocks; allocation tokens are invalid after their owning allocator is destroyed.

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


## Recorded result (CI run 44300297357)

The final pinned run used Rust 1.100.0 on Linux x86_64 (AMD EPYC 7763 64-Core Processor), with 101 samples, three warmups and 100 iterations per sample. The proof includes active-allocation tracking and Isolate-shutdown cleanup. The machine-readable report is attached to [CI run 44300297357](https://github.com/AiratGaliev/ogma/actions/runs/44300297357) as [`ogm-24-allocator-arena-evidence`](https://github.com/AiratGaliev/ogma/actions/runs/44300297357/artifacts/14425239185). It records source revision `d3da74b55e022d9bae4bab733e4e6747eb3ee4a1` and tested merge revision `98dcfbbdc2aa5c146416b633792810510600f2bd`.

| Runtime allocator candidate | Throughput (ops/s) | Allocation p50/p95/p99 (ns) | Deallocation p50/p95/p99 (ns) | Peak reserved (bytes) |
|---|---:|---:|---:|---:|
| System + quota wrapper | 9,950,260 | 50 / 91 / 130 | 50 / 70 / 80 | 156,808 |
| Per-Isolate exact reuse | 9,365,320 | 60 / 80 / 130 | 51 / 120 / 150 | 157,168 |
| Per-Isolate size classes | 9,537,768 | 60 / 71 / 110 | 50 / 110 / 140 | 158,304 |

The exact-reuse and size-class variants reduced overall throughput by 5.9% and 4.1% respectively versus the System baseline. Size classes reduced allocation p95/p99 from 91/130 ns to 71/110 ns, but deallocation p95/p99 increased from 70/80 ns to 110/140 ns. Exact reuse had no p99 allocation improvement (130 ns) and a larger deallocation-tail penalty (120/150 ns). Both custom strategies retained approximately 157–158 KiB of free backing blocks. This workload does **not** justify a custom pool as the default Isolate allocator. Keep System as the baseline; consider a size-class pool only for a separately measured, allocation-tail-sensitive workload where the free-path regression and retained capacity are acceptable.

| Compile-unit candidate | Throughput (units/s) | Allocation p50/p95/p99 (ns) | Release/reset p50/p95/p99 (ns) | Peak reserved / payload (bytes) |
|---|---:|---:|---:|---:|
| System per-node allocation | 24,552 | 30 / 50 / 89 | 30 / 41 / 50 | 26,368 / 26,368 |
| Compile-unit bump arena | 48,795 | 31 / 40 / 41 | 40 / 41 / 41 | 32,768 / 26,368 |

For this compile-unit POD workload, the bump arena increased throughput by 98.7%; allocation p95/p99 improved from 50/89 ns to 40/41 ns. Allocation p50 moved from 30 to 31 ns, and reset p50 from 30 to 40 ns. The arena retained two 16 KiB chunks with 6,400 bytes of slack at the measured peak (24.3% of reserved capacity). This supports the arena for bounded, homogeneous compiler scratch with a strict compile-unit lifetime, not for JavaScript heap objects or data requiring destructors.

These are synthetic raw-memory results from one CI host, not production Runtime timings. Peak RSS is a per-process high-water metric and includes benchmark-process overhead; System allocator internal/external fragmentation cannot be introspected. The quota covers backing blocks, including live and cached blocks and arena chunks, but excludes allocator bookkeeping containers and general process/runtime overhead. It is therefore not a process-wide memory cap.
