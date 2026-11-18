# OGM-24 — allocator and compile-unit arena proof contract

## Scope and authority

This document records the M0 proof under benchmarks/isolate-allocator. The Ogma architecture specification remains authoritative, especially section 8.9.5. This is not a production Runtime implementation and does not freeze a public allocator ABI.

The proof uses raw byte allocations to evaluate the lifecycle and accounting contract before a JavaScript heap or compiler exists. It must not be described as a measurement of JS object allocation, GC, AST/Core IR construction or production Ogma throughput.

## Runtime allocation contract exercised

Each Isolate owns one allocator instance and one byte quota. The instance is owner-thread-only (!Send + !Sync); Isolate-local caches and counters are never shared across Isolates.

The allocation token stores the exact pointer, backing Layout, requested size and originating allocator identity. The allocator keeps an active-block registry. Deallocation through a different Isolate or with a stale token is rejected and returns ownership of the allocation token, so a caller can return it to the correct owner. On Isolate shutdown, active and cached backing blocks are reclaimed; allocation tokens are invalid after their allocator is destroyed.

The quota is enforced against bytes reserved from the underlying system allocator, including cached free blocks. This avoids a custom allocator bypassing a host memory limit by retaining capacity outside the accounting path. A new allocation may evict cached blocks before failing. The system baseline uses the same quota semantics but releases every block immediately.

| Candidate | Reuse policy | Backing allocation | Quota accounting |
|---|---|---|---|
| System | None | Requested size/alignment | Live System blocks |
| Per-Isolate exact reuse | Exact size and alignment | Requested size/alignment | Live and cached blocks |
| Per-Isolate size classes | Power-of-two capacity, 16–4096 bytes | Rounded class, 16-byte alignment | Live and cached backing capacity |

Size-class requests larger than 4096 bytes or aligned above 16 bytes use the direct System path. Cache retention is capped at 128 blocks per bucket. The custom allocator must remain correct when a cache is full, a quota is exhausted, a block is evicted or an allocation fails.

## Compile-unit lifetime contract

CompileUnitArena is separate from IsolateAllocator and has its own 2 MiB quota. It allocates aligned slices from 16 KiB-or-larger chunks and resets used offsets at the end of each compile unit. Reserved chunk capacity remains charged to the arena quota between resets and is released only when the arena is dropped.

The proof models POD-like compiler scratch with no per-node destructor. Types that own resources, require Drop, escape the compilation unit or participate in JS object/GC lifetimes are not suitable for this arena. Runtime heap allocations must never be mixed into compile-unit storage.

## Workload and metrics

Allocator candidates receive the same repeated mixed-size workload (128 requests per iteration, 16–4096 bytes, 16-byte alignment). One eighth of blocks are retained across iterations; the rest are churned and released. Every sample starts with a fresh allocator instance.

Compiler candidates receive the same 256-node size sequence for each compile unit. The System path individually allocates and frees nodes; the arena path allocates the same shapes and resets after each unit. Every sample starts with fresh storage.

Latency distributions use nearest-rank p50/p95/p99 over individual operations. Timing includes the two Instant reads. Throughput is measured over complete samples. Each of the five candidates executes in its own subprocess, making Linux VmHWM process high-water measurements candidate-specific.

Memory counters distinguish:
- requested live bytes;
- live backing bytes;
- allocator-reserved bytes, including retained cache/chunks;
- free cached bytes;
- allocator-visible internal slack, computed as reserved minus live requested bytes;
- compile-unit payload and alignment padding.

The System allocator does not expose its internal fragmentation to this proof. Its OS allocator behaviour cannot be inferred from the logical accounting counters. Peak RSS includes process and benchmark overhead, so it is corroborating context rather than allocator-only memory.

## Quota and lifecycle regressions

Unit tests verify:
- System allocations fail at quota and release reserved bytes on deallocation.
- Per-Isolate exact reuse retains free backing capacity in the quota and reuses an exact block.
- Size-class allocation rounds capacity correctly, preserves alignment and reuses a matching class.
- Distinct Isolates have independent quotas and reject cross-Isolate deallocation.
- Cached blocks are evicted before a new class is rejected, while the reserved-byte quota remains intact.
- Compile-unit allocations satisfy alignment, reuse chunk addresses after reset and cannot grow beyond the arena quota.
- Zero-sized Layout requests are represented by a one-byte backing allocation but account zero requested bytes.

## Recommendation policy

Do not select an allocator based on throughput alone. Compare results alongside p95/p99 latency, reserved-versus-live memory, peak RSS, cache retention and lifecycle complexity under the same quota.

A custom per-Isolate strategy is justified only for workloads where reuse materially improves latency/throughput without unacceptable retained memory, quota pressure or maintenance cost. If gains are negligible or narrow, prefer the System baseline. Enable size classes only for measured, recurring size distributions. Keep compile-unit arenas only for bounded compiler data whose lifetime is genuinely compile-unit-scoped.

No production allocator, quota API, GC interaction, JS object semantics, compiler allocation path, multi-thread Runtime behaviour or cross-platform allocator comparison is proved here.


## Final measured result and recommendation (2026-11-18)

Evidence: [CI run 44300297357](https://github.com/AiratGaliev/ogma/actions/runs/44300297357), artifact [`ogm-24-allocator-arena-evidence`](https://github.com/AiratGaliev/ogma/actions/runs/44300297357/artifacts/14425239185). Environment: Rust 1.100.0, Linux x86_64, AMD EPYC 7763 64-Core Processor; three warmups, 101 samples, 100 iterations/sample. Source revision: `d3da74b55e022d9bae4bab733e4e6747eb3ee4a1`; tested merge revision: `98dcfbbdc2aa5c146416b633792810510600f2bd`.

| Candidate | Throughput | Allocation p50/p95/p99 | Deallocation or reset p50/p95/p99 | Peak reserved |
|---|---:|---:|---:|---:|
| System + quota wrapper | 9.950 M ops/s | 50 / 91 / 130 ns | 50 / 70 / 80 ns | 156,808 B |
| Per-Isolate exact reuse | 9.365 M ops/s | 60 / 80 / 130 ns | 51 / 120 / 150 ns | 157,168 B |
| Per-Isolate size-class pool | 9.538 M ops/s | 60 / 71 / 110 ns | 50 / 110 / 140 ns | 158,304 B |
| System per-node compile allocation | 24,552 units/s | 30 / 50 / 89 ns | 30 / 41 / 50 ns | 26,368 B |
| Compile-unit bump arena | 48,795 units/s | 31 / 40 / 41 ns | 40 / 41 / 41 ns | 32,768 B |

### Recommendation

- **Runtime heap:** retain System as the default baseline. Under this mixed-size churn workload, exact reuse and size classes reduced total throughput by 5.9% and 4.1%. Size classes improved allocation p95/p99 (91/130 → 71/110 ns), but worsened deallocation p95/p99 (70/80 → 110/140 ns) and retained 158,304 B. Exact reuse provided no p99 allocation improvement and had even higher deallocation tail latency. Evidence does not support enabling either custom pool by default. A size-class allocator is only a possible targeted option where allocation-tail latency outweighs free-path latency and retained memory.
- **Compiler scratch:** a compile-unit bump arena is promising for homogeneous POD-like AST/semantic/Core IR scratch with a strict compile-unit lifetime. It improved measured throughput by 98.7% and reduced allocation p95/p99 from 50/89 to 40/41 ns, while retaining 6,400 B of slack across two chunks. Reset p50 was higher (40 vs 30 ns); keep objects requiring Drop, escaping data and JS heap objects outside this arena.
- These decisions are provisional M0 evidence, not production policy. A real Ogma heap/compiler workload and additional controlled runner/platform measurements are required before a production allocator selection.

### Lifecycle, quota and measurement caveats

The allocator now tracks active blocks by address and backing layout, rejects stale/cross-Isolate deallocation, and reclaims still-live plus cached blocks at Isolate shutdown. A regression test proves shutdown cleanup for an outstanding allocation token. The token must not be used after its allocator owner is destroyed.

The quota enforced here covers allocation backing bytes, including live blocks, cache-resident blocks and arena chunks. It does **not** count the Rust allocator bookkeeping structures (bucket maps, vectors and live-block registry) or process/runtime overhead; the result proves that the custom block allocation path cannot bypass its configured backing-byte quota, not that the entire Isolate process is capped at that value. Peak RSS includes benchmark scratch buffers and is only a process-level corroborating metric. System allocator internal/external fragmentation is not introspectable. This was one synthetic workload on one CI host; it cannot establish JS object allocation or GC performance.
