# Value representation and SSA lowering proof (OGM-17)

This M0 proof evaluates two independent design axes:

1. Runtime Value layout: explicit tagged 16-byte candidate versus candidate 8-byte NaN-box word.
2. Compiler SSA representation: all-boxed SSA versus representation-aware SSA preserving verified Core IR representations.

## Canonical input corpus

The benchmark loads accepted function documents listed by tests/core-ir/manifest.json. It does not invent a separate workload table. Negative fixtures remain Core IR verifier regressions and are not compiler workloads. Each candidate receives the same fixture bytes; the report records manifest SHA-256, fixture SHA-256 values and a combined corpus SHA-256.

The canonical corpus contains numeric branch and block-argument values, dynamic addition and exception flow, closure/environment access, async suspension, string/object/function call boundaries, and loop-carried numeric values. Workload labels are derived from actual parameter/result representations, operations, block arguments and CFG back-edges.

The focused CI job first invokes the repository Core IR verifier against the full manifest. The benchmark then consumes the accepted entries from that exact manifest.

## Candidate matrix and lowering model

Four combinations are reported:

- tagged-16 plus boxed SSA
- nanbox-8 plus boxed SSA
- tagged-16 plus representation-aware SSA
- nanbox-8 plus representation-aware SSA

Representation-aware SSA preserves the representations declared in Core IR v1. The report breaks conversions down by function parameters, returns, dynamic semantic operands, exception operands, lexical environment locals, phi/block arguments and loop-carried edges. Typed parameters are unboxed at the modeled public Value ABI boundary; typed returns and operands passed to generic dynamic helpers are boxed. Lexical environment load/store, phi and loop transitions report zero implicit conversions because Core IR v1 requires the stored, incoming and outgoing representations to match. Their slot, edge-argument and back-edge counts are reported alongside the conversion counts.

The helper-call model distinguishes generic dynamic operations from a small set of representation-specializable operations. It is a documented compiler-cost model, not generated machine code.

Logical payload estimates include SSA definitions, lexical environment/capture slots and conversion temporaries. Abstract reference representations are assumed to use eight bytes for this estimate. A backward CFG liveness pass includes normal and exceptional successors and reports peak simultaneously live SSA values and their abstract payload size.

## What is and is not measured

Derived from canonical Core IR:
- SSA definitions, function parameters, block parameters and edge arguments;
- normal/exception CFG edges and loop back-edges;
- modeled boxing/unboxing at public ABI and dynamic semantic boundaries;
- helper-call sites and operation counts;
- peak live SSA values and abstract payload bytes;
- logical SSA/environment payload estimates and an instruction-count proxy.

Not measured because this repository has no Rust workspace, runtime, interpreter, AOT code generator or register allocator:
- native object-code size;
- native execution throughput;
- physical register pressure and spill counts;
- actual allocator/GC costs and process RSS;
- differential execution of the candidate representations.

The instruction-count proxy is not code size. SSA liveness is not physical register pressure. Logical payload bytes are not runtime memory usage. Python execution times are not reported as Ogma throughput.

## NaN-box model boundary

The executable codec uses a deterministic synthetic heap-handle table to test round-trip encoding of strings, arbitrarily large BigInts, objects and symbols. It checks 48-bit handle exhaustion rather than truncating IDs or using non-reversible hashes. This table is a proof fixture, not a production pointer representation. It does not prove pointer canonicalization, GC roots, write barriers, moving-GC compatibility or host ABI safety.

## Semantic subset

Correctness fixtures cover NaN, signed zero, infinities, BigInt, undefined/null, strings, object identity, symbol identity and BigInt/Number addition errors. Object ToPrimitive, complete ECMAScript number/string formatting and full property semantics remain outside this M0 semantic subset.

## Reproduce

From the repository root with Python 3.12+:

    python .github/scripts/verify_core_ir.py --manifest tests/core-ir/manifest.json
    python -m unittest discover -s benchmarks/value-representations -p "test_*.py" -v
    python benchmarks/value-representations/run.py

The report is written to benchmarks/value-representations/results/value-ssa-report.json.

## Recommendation

Keep the public Runtime Value ABI explicitly tagged and architecture-neutral. Preserve representation-aware SSA as the compiler direction and box only at dynamic semantic and external ABI boundaries. Keep NaN-boxing unselected until the Rust heap, GC, ABI and native benchmark prototypes exist.


## Native follow-up (OGM-38)

OGM-38 is the M0 native-evidence and Value/SSA decision follow-up. The latest measured source revision is `770bf5bdaa598473a49733d1053b4fff589091c1`, from [successful CI run #45042417728](https://github.com/AiratGaliev/ogma/actions/runs/45042417728). Both Fast CI and the focused OGM-38 native job passed. [Artifact #14762262101](https://github.com/AiratGaliev/ogma/actions/runs/45042417728/artifacts/14762262101) contains the full native/differential JSON, Core IR report, generated native objects, portable .ogmb fixtures, Cargo.lock and raw measurements.

All four candidates use the same five canonical OGM-31 fixtures. Each dispatches 77 Value inputs through generated native batch loops, passes 31 semantic cases with one generated native CFG entry per case, and matches the independent OGM-31 portable oracle. Each candidate records 70 callbacks, 39 forced collections and 28 candidate roots that survive entry/call-boundary collections and are reclaimed after release.

The Number-only native loop p50 is 334.334 ns (Tagged16 boxed), 102.113 ns (Tagged16 representation-aware), 336.747 ns (NaN-box boxed) and 84.818 ns (NaN-box representation-aware). A generated 32-live-Value Cranelift pressure function records 64 spill slots / 512 B for Tagged16 and 32 / 256 B for NaN-box. These results are bounded native probes, not whole-runtime performance claims.

The OGM-24 IsolateAllocator backs actual OGM-26 candidate Value payload storage. Both collectors and all three allocator strategies use the same 2,048-object graph. There are 24 isolated GC workers and 12 direct allocator workers; each records 101 raw samples. Linux VmHWM is measured separately from logical heap and allocator accounting.

The complete raw measurement arrays and compact XZ delta snapshot are retained in [CI artifact #14762262101](https://github.com/AiratGaliev/ogma/actions/runs/45042417728/artifacts/14762262101). The manifest pins the full report, differential, projections, XZ snapshot and decoded JSON checksums. Exporter, validator and regression tests are committed, but the raw arrays/XZ snapshot are not yet stored in Git itself; criterion 6 therefore remains Partial.

The M0 decision is recorded in [decision-record-ogm38.md](decision-record-ogm38.md): keep the public embedding ABI opaque and versioned, use explicit tagged Value as the conservative internal M1 baseline, adopt representation-aware SSA and retain NaN-box as experimental. Exact measurements and acceptance boundaries are in [native-evidence-ogm38.md](native-evidence-ogm38.md).
