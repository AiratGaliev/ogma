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

Not measured by the original OGM-17 structural model (the subsequent OGM-38 native follow-up adds a separate bounded native proof):
- native object-code size;
- native execution throughput;
- physical register pressure and spill counts;
- actual allocator/GC costs and process RSS;
- differential execution of the candidate representations.

The instruction-count proxy is not code size. SSA liveness is not physical register pressure. Logical payload bytes are not runtime memory usage. Python execution times are not reported as Ogma throughput.

## NaN-box model boundary

The executable codec uses a deterministic synthetic heap-handle table to test round-trip encoding of strings, arbitrarily large BigInts, objects and symbols. It checks 48-bit handle exhaustion rather than truncating IDs or using non-reversible hashes. This table is a proof fixture, not a production pointer representation. It does not prove pointer canonicalization, GC roots, write barriers, moving-GC compatibility or host ABI safety. OGM-38's separate M0 native integration proof exercises real generation-checked host handles and OGM-26/27 root/GC paths; it still does not claim a production Runtime heap or production stack-map encoder.

## Semantic subset

Correctness fixtures cover NaN, signed zero, infinities, BigInt, undefined/null, strings, object identity, symbol identity and BigInt/Number addition errors. Object ToPrimitive, complete ECMAScript number/string formatting and full property semantics remain outside this M0 semantic subset.

## Reproduce

From the repository root with Python 3.12+:

    python .github/scripts/verify_core_ir.py --manifest tests/core-ir/manifest.json
    python -m unittest discover -s benchmarks/value-representations -p "test_*.py" -v
    python benchmarks/value-representations/run.py

The report is written to benchmarks/value-representations/results/value-ssa-report.json. For the focused native follow-up reproduction commands and toolchain, see [native-evidence-ogm38.md](native-evidence-ogm38.md).

## Recommendation

Keep the public Runtime Value ABI explicitly tagged and architecture-neutral. Adopt representation-aware SSA and box at dynamic semantic and external ABI boundaries. OGM-38 supplies bounded native, heap/GC and ABI evidence; NaN-box remains experimental because the M0 measurements do not justify selecting it as the production internal layout across targets and representative workloads.

## Native follow-up (OGM-38)

OGM-38 adds a bounded native execution and runtime-evidence follow-up to the original OGM-17 structural model. Its active Git-tracked measurement snapshot is pinned to tested revision `69038fbbd6246f14466ab91821524f7cc0cfb04c`, produced by [successful CI run #45130525848](https://github.com/AiratGaliev/ogma/actions/runs/45130525848). [Artifact #14798285147](https://github.com/AiratGaliev/ogma/actions/runs/45130525848/artifacts/14798285147) retains the full native/differential reports, Core IR report, generated native objects, portable .ogmb fixtures, Cargo.lock and raw samples.

The active lossless compact raw dataset is committed in `results/ogm38-raw-native-samples.delta.json.xz.b64.parts/` as 13 Base64 fragments (12 × 4,000 bytes and one 3,984-byte fragment). It reconstructs to 38,988 bytes of XZ containing 156 measurement series × 101 samples and 36 worker records (24 collector and 12 allocator configurations). `decode_ogm38_raw_archive.py` verifies exact XZ/JSON checksums, tested revision, candidate inventory and sample counts. The former 14-fragment snapshot from revision `74991baa…` remains preserved and independently checked under `results/historical/`. The manifest's Base64-file checksum includes the exporter's terminal newline; Git fragments are concatenated without separators before decoding.

The snapshot-producing native-evidence run [#45130525848](https://github.com/AiratGaliev/ogma/actions/runs/45130525848) passed both `fast-checks` and the focused OGM-38 job and generated artifact [#14798285147](https://github.com/AiratGaliev/ogma/actions/runs/45130525848/artifacts/14798285147). Its raw dataset is pinned to `69038fbb…`; the earlier 74991 snapshot remains a separate historical record. The primary projection from run #447304 is also separately provenance-pinned in the manifest. Current-head verification after the snapshot rotation is required before PR #32 is ready to integrate.

All four candidates use the same five canonical OGM-31 fixtures. Each dispatches 77 Value inputs through generated native batch loops, passes 31 semantic cases with one generated native CFG entry per case, and matches the independent OGM-31 portable oracle. Each candidate records 70 callbacks, 39 forced collections and 28 candidate roots that survive entry/call-boundary collections and are reclaimed after release.

For the active committed snapshot, the Number-only native loop p50 is 277.955 ns (Tagged16 boxed), 92.671 ns (Tagged16 representation-aware), 277.306 ns (NaN-box boxed) and 83.092 ns (NaN-box representation-aware). Representation-aware SSA accounts for most of the speedup; the measured 10.3% p50 difference between the representation-aware Value layouts is not sufficient to select a production layout. A generated 32-live-Value Cranelift pressure function records 64 spill slots / 512 B for Tagged16 and 32 / 256 B for NaN-box.

The OGM-24 IsolateAllocator backs actual OGM-26 candidate Value payload storage. Both collectors and all three allocator strategies use the same 2,048-object graph. There are 24 isolated collector workers and 12 direct allocator workers; each records 101 raw samples. Linux VmHWM is measured separately from logical heap and allocator accounting.

The M0 decision is recorded in [decision-record-ogm38.md](decision-record-ogm38.md): keep the public embedding ABI opaque and versioned, use explicit tagged Value as the conservative internal baseline, adopt representation-aware SSA and retain NaN-box as experimental. Exact measurement boundaries and acceptance status are in [native-evidence-ogm38.md](native-evidence-ogm38.md).
