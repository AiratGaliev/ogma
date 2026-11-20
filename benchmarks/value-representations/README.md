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

OGM-38 now provides a reproducible M0 native-evidence slice for all four Runtime Value × SSA-policy candidates. Each candidate processes all 77 Value representations extracted from the same five canonical OGM-31 fixtures through generated native batch loops, passes the 31-case semantic matrix, and matches the independent portable/native OGM-31 observations. These batch adapters are not a general lowering of each complete fixture CFG or a production ECMAScript Runtime.

The current measurement run is [#44766826425](https://github.com/AiratGaliev/ogma/actions/runs/44766826425) (source revision `c7da33dc00e0d588ba36b809f17fc06a17559a6e`). Both the OGM-38 native job and fast-checks succeeded. The [CI evidence artifact](https://github.com/AiratGaliev/ogma/actions/runs/44766826425/artifacts/14644153664) contains the full JSON report, OGM-31 differential, verifier reports, generated objects and lockfile.

Unlike the initial M0 estimates, the OGM-24 `IsolateAllocator` now backs the actual candidate Value payload blocks stored in OGM-26 heap objects. The selected RC-cycle-removal or MarkSweep collector returns those backing allocations when objects are reclaimed. Every candidate is measured across the same 2,048-object graph and the six collector × allocator combinations. Each combination runs in an isolated worker process and records Linux `VmHWM` baseline, peak and delta after three warmups and 101 measured lifecycles. The metric includes the worker executable, code mappings, Rust runtime and allocator state; it is not a heap-only counter.

The repository commits:
- `results/ogm38-primary-benchmark-samples.json`: all 101 compile-time and native-loop latency samples per candidate.
- `results/ogm38-runtime-raw-samples.json.gz`: all 101 GC and allocator samples for every candidate × collector × allocator combination, including worker VmHWM baseline/peak/delta.
- `results/ogm38-native-value-ssa-report.json.gz`: complete native machine-readable report.
- `results/ogm38-ogm31-differential-report.json`: portable/native differential evidence.

The exporter `export_ogm38_raw_samples.py` validates the full native and OGM-31 reports before producing these projections. The OGM-38 CI workflow executes the exporter and uploads its outputs alongside the original artifacts. Regression tests validate revisions, candidate matrices, all 24 worker combinations, 101 sample counts, positive peak-memory values and `peak - baseline = delta`.

The evidence and recommendation are documented in [OGM-38 native Value and SSA evidence](native-evidence-ogm38.md). OGM-38 remains In Progress: the four adapters do not fully lower every Core IR fixture CFG; some operator semantics remain M0 oracle/adapter work; regalloc2 spill estimates are not Cranelift's private spill report; OGM-27 shadow-root proof is not compiler-generated production machine stack maps; and the benchmark heap is not a production Runtime/object model.
