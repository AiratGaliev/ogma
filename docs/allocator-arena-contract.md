# OGM-24 — allocator and compile-unit arena proof contract

## Scope and authority

This document records the M0 proof under benchmarks/isolate-allocator. The Ogma architecture specification remains authoritative, especially section 8.9.5. This is not a production Runtime implementation and does not freeze a public allocator ABI.

The proof uses raw byte allocations to evaluate the lifecycle and accounting contract before a JavaScript heap or compiler exists. It must not be described as a measurement of JS object allocation, GC, AST/Core IR construction or production Ogma throughput.

## Runtime allocation contract exercised

Each Isolate owns one allocator instance and one byte quota. The instance is owner-thread-only (!Send + !Sync); Isolate-local caches and counters are never shared across Isolates.

The allocation token stores the exact pointer, backing Layout, requested size and originating allocator identity. Deallocation through a different Isolate is rejected and returns ownership of the allocation token, so a caller can return it to the correct owner.

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
