# OGM-38 decision — Runtime Value ABI and internal SSA

## Status

M0 implementation recommendation based on successful CI run [#45042417728](https://github.com/AiratGaliev/ogma/actions/runs/45042417728), measured source revision `770bf5bdaa598473a49733d1053b4fff589091c1`, Rust 1.100.0 and Cranelift 0.137.2.

This decision establishes the conservative direction for the next Runtime/compiler implementation. It does not freeze a production binary layout and does not expose internal Value words through the embedding ABI.

## Decision

1. **Keep the public embedding ABI independent of the internal Value encoding.** The public C-compatible surface remains versioned and uses opaque generation-checked handles, explicit completions, capability checks and ownership rules. Do not expose a Tagged16 record, a NaN-box word, raw GC pointers or SSA values through the public ABI. Keep the existing OGM-19/32 ABI v1 contract.
2. **Use explicit tagged Value as the conservative internal Runtime baseline.** Keep the representation private and replaceable. This avoids choosing the more constrained NaN-box representation before the production heap, collector, root inventory and target matrix are implemented.
3. **Adopt representation-aware SSA inside the compiler.** Preserve verified Core IR representations for typed values within a function. Box only at genuinely dynamic semantic, heap-storage and external/host ABI boundaries. Keep effects, exceptions and safepoints explicit.
4. **Do not select NaN-box now.** Keep it as an experimental candidate in the evidence harness. Do not infer a public ABI or production internal representation from nominal word width or the Number-only loop benchmark.

## Measured trade-offs

| Candidate | Compile p50 | Loop p50 | Object bytes | Native pressure spills |
|---|---:|---:|---:|---:|
| Tagged16 + boxed SSA | 1.248 ms | 334.334 ns | 4,128 | 64 slots / 512 B |
| Tagged16 + representation-aware SSA | 1.385 ms | 102.113 ns | 4,456 | 64 slots / 512 B |
| NaN-box + boxed SSA | 1.164 ms | 336.747 ns | 3,720 | 32 slots / 256 B |
| NaN-box + representation-aware SSA | 1.211 ms | 84.818 ns | 4,024 | 32 slots / 256 B |

Representation-aware SSA improves p50 by about 3.27× for Tagged16 and 3.97× for NaN-box in this synthetic numeric loop. NaN-box reduces Value payload backing storage from 524,288 B to 262,144 B in the measured graph and halves the observed spills of the generated 32-live-Value pressure function. These are bounded M0 measurements, not whole-runtime performance claims. The NaN-box representation-aware candidate has more variable p95/p99 latency in this run.

## Why NaN-box is not selected

The eight-byte candidate wins on nominal Value width and the synthetic pressure test. It still adds constraints around reserved NaN bit patterns, canonical NaN handling, a 48-bit handle payload, target portability and collector/root integration. The current benchmark does not establish that these constraints are acceptable for a production Runtime across targets and workloads. The actual semantic helper path shows little consistent benefit from changing the candidate word width.

## Public ABI versus internal representation

The public ABI is the compatibility boundary for embedding applications and host modules. It must remain stable, opaque and versioned. The internal Value layout is a private Runtime implementation detail. Internal SSA representation is a compiler optimization policy, not a public data layout. These three decisions must not be collapsed into one choice.

## Re-evaluation triggers

Reopen the internal representation choice after the production heap/object model and collector exist, actual interpreter/native frame roots are wired into the Runtime, and representative application workloads are available. Re-run the comparison for x86-64 and ARM64 and include property access, calls, object allocation, GC, strings, BigInts and host calls. Measure total Runtime/application memory rather than only Value payload capacity. Reconsider NaN-box only if broader results justify the extra constraints.

## Raw evidence

The full report and lossless raw arrays are retained in [CI artifact #14762262101](https://github.com/AiratGaliev/ogma/actions/runs/45042417728/artifacts/14762262101). The committed latest-evidence manifest records exact checksums and source/toolchain revisions. The raw binary itself is not yet stored in Git; the raw-data-in-repository criterion remains Partial.

## Scope boundary

This decision does not implement a production Runtime or full native AOT compiler. It records the M0 evidence-based direction: opaque/versioned public ABI, conservative explicit tagged internal Value baseline and representation-aware internal SSA. See [native evidence](native-evidence-ogm38.md) and [latest evidence manifest](results/ogm38-latest-evidence.json).
