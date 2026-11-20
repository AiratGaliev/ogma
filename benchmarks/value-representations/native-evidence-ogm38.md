# OGM-38 — Native Value and SSA evidence

**Status:** verified M0 evidence slice; OGM-38 remains In Progress because several acceptance criteria are only partially covered. No production Runtime ABI or NaN-boxing decision is made.

## Question and candidates

OGM-38 revalidates two independent choices using native execution evidence:

| Runtime Value layout | SSA policy | Candidate |
|---|---|---|
| Explicit tagged 16-byte value | Boxed | tagged-16-boxed |
| Explicit tagged 16-byte value | Representation-aware | tagged-16-representation-aware |
| 8-byte NaN-box word | Boxed | nanbox-8-boxed |
| 8-byte NaN-box word | Representation-aware | nanbox-8-representation-aware |

All four candidates use the same accepted Core IR v1 contract and the five-fixture OGM-31 corpus. Runtime layout and SSA policy remain independent axes. This report does not select a public Runtime ABI based on width alone.

## Reproducible CI run

- CI run: [OGM-38 run #44726681350](https://github.com/AiratGaliev/ogma/actions/runs/44726681350) — success.
- OGM-38 native job: success (format, Clippy with warnings denied, OGM-26/27/backend tests, all four native candidates and OGM-31 differential proof).
- Repository fast-checks: success.
- Raw CI artifact: [ogm38-native-value-ssa-evidence, artifact 14623748867](https://github.com/AiratGaliev/ogma/actions/runs/44726681350/artifacts/14623748867).
- Source revision and tested revision: e145f6b4cd8a175d43cb8fb96bc8667f315a3c7d.
- Environment: GitHub-hosted Linux x86_64; Rust 1.100.0; Cranelift 0.137.2; target x86_64-unknown-linux-gnu; Intel Xeon Platinum 8573C.
- Sampling: 3 warmups and 101 measurements per candidate; 1,000 native calls per throughput sample.
- Full CI JSON: 1,657,812 bytes; SHA-256 7487fb6c393a6f2a24763f6d091b02e0d5de58ba7721e4a52713bbb01cd51783 (preserved in artifact 14623748867).
- The repository's previously tracked `results/ogm38-native-value-ssa-report.json.gz` is an older evidence run, not the report for revision e145f6b4cd8a175d43cb8fb96bc8667f315a3c7d. It is not used for any table or conclusion in this document.
- A lossless compact projection of the current run was generated and locally checksum-verified (expanded JSON 274,526 bytes; gzip 43,422 bytes). It retains all 101 raw sample values for compile time, native throughput, handle helper, both collectors (build/collection/root-scan/cleanup and per-sample topology) and all three allocator strategies. GC and allocator sample records use an ordered field-name array plus positional sample rows; semantic cases, fixture dispatches, generated-code metrics, root evidence and aggregate percentiles are retained. This compact projection was not committed to the PR; the full authoritative report remains in the linked CI artifact, whose retention currently expires on 2027-02-18. Consequently, the raw-measurements-committed criterion is still open.

The report stores all compile, native-loop, callback, GC and allocator samples, not just aggregate percentiles. The CI artifact additionally preserves the complete OGM-31 differential report, Core IR verifier report, generated portable/native artifacts and resolved dependency lock.

## Native code generation and throughput

The current Cranelift ObjectModule prototype compiles and links candidate-specific host-native functions. The primary throughput workload is a strict-equality-controlled Number loop specialized from the canonical loop fixture (js.add and js.to_number). It is deliberately narrow and must not be interpreted as representative JavaScript application throughput.

| Candidate | Value bytes | Compile p50 (ms) | Native loop p50 / p95 / p99 (ns) | Object bytes | Loop code bytes | Modeled conversions |
|---|---:|---:|---:|---:|---:|---|
| tagged-16-boxed | 16 | 4.076 | 346.397 / 367.117 / 378.112 | 3,720 | 336 | 5 per iteration |
| tagged-16-representation-aware | 16 | 3.948 | 110.520 / 120.532 / 131.800 | 4,048 | 336 | 4 at invocation boundary |
| nanbox-8-boxed | 8 | 3.054 | 345.160 / 353.427 / 356.208 | 3,280 | 392 | 5 per iteration |
| nanbox-8-representation-aware | 8 | 3.026 | 123.657 / 144.154 / 149.841 | 3,584 | 384 | 4 at invocation boundary |

All four generated shared libraries are 15,128 bytes in this run. Loop function code sizes and instruction counts are measured from the generated object; complete object sizes also include the other generated helpers. Compile p50 is the Cranelift candidate function compilation measurement, not whole-program or production compiler startup.

On this Number-only workload, representation-aware SSA lowers p50 loop latency by about 3.13× for Tagged16 and 2.79× for NaN-box compared with boxed SSA. This is useful evidence for keeping representation-aware SSA as the compiler direction, not a universal runtime performance claim. NaN-box representation-aware is slower than Tagged16 representation-aware in this sample (123.657 ns versus 110.520 ns p50), despite its nominally smaller Value word.

## Same-corpus dispatch and differential observations

Every candidate consumes all Value representations extracted from the five canonical OGM-31 fixtures:

| Canonical fixture | Native Value dispatches per candidate | Reference Values resolved after forced GC |
|---|---:|---:|
| positive-ssa-branch | 7 | 0 |
| positive-loop-carried-values | 20 | 4 |
| positive-dynamic-exception | 14 | 5 |
| positive-call-boundary | 23 | 7 |
| positive-host-call | 13 | 4 |
| **Total** | **77** | **20** |

For every candidate the report verifies that all 77 Value inputs round-trip through its candidate codec, are dispatched through the generated native Value helper and that the 20 reference Values resolve to their expected generation-checked OGM-26 objects after forced collection.

The candidate-specific Cranelift functions execute branch equal/unequal, integer and fractional loop cases, Number dynamic addition, and the Symbol-add TypeError path. Call-boundary and host-call observations use the candidate native Value dispatcher and the existing M0 host/ABI adapters. The independent OGM-31 differential run reports all_candidates_match_portable_ogm31_oracle = true for all four candidates and all five fixture IDs; each candidate passes 18 strict-equality checks, call-boundary and host-echo checks, the Symbol TypeError observation and OGM-27 root-balance checks.

**Important boundary:** this does not mean each of the four candidate backends fully lowers and executes every instruction and control-flow path from all five complete Core IR functions. The candidate adapters execute a native subset and dispatch the complete fixture Value representation corpus. The OGM-31 portable/native fixture oracle is a separate, narrower M0 backend proof. The repository still does not have a general production Core IR-to-native compiler.

## Value semantics and strict equality regression

Each candidate passes the same 31-case semantic matrix. The matrix includes Number, String and BigInt addition observations; Symbol and BigInt/Number error cases; NaN, signed zero and infinities; undefined, null and booleans; object and Symbol identity; String and BigInt strict equality; type transition; exception passthrough; call-boundary and host-call cases. Each candidate records 63 native callbacks and 33 forced collections in this run.

The matrix exercises generated native strict equality for the strict-equality cases. The regression suite specifically verifies JavaScript primitive semantics that a handle-identity comparison would get wrong:

- Equal String values held in two different generation-checked handles compare true.
- Distinct String values held in different handles compare false.
- Equal BigInt values held in two different handles compare true.
- Distinct BigInt values held in different handles compare false.
- Object and Symbol strict equality continues to use identity.

The generated Tagged16 and NaN-box strict-equality functions call a checked native host callback only for String and BigInt payload comparison. The callback resolves both HostHandle tokens through OGM-26, forces collection while they remain retained, verifies both handles still resolve to their expected objects, and then compares their semantic String/BigInt values. The native report asserts that this callback path was actually executed for all four primitive cases. A Rust regression test verifies that equal String and BigInt inputs are allocated into distinct handles and are fully reclaimed after release.

The 31-case matrix is not a claim that all ECMAScript operators are lowered to machine code. In particular, String concatenation, BigInt arithmetic, object ToPrimitive and some completion observations are evaluated by the M0 semantic oracle around native Value dispatch; they are not production-native operator implementations. The native Number path, strict equality, native helper dispatch and AOT host/ABI adapter paths are exercised independently.

## Native register-pressure observations

The Cranelift prototype emits a 32-live-Value pressure function per candidate and the CI runner disassembles the actual object with GNU objdump. The table reports observed stack-relative memory traffic in that generated pressure function, not an inferred spill count:

| Candidate | Pressure function code bytes | Stack-relative loads | Stack-relative stores |
|---|---:|---:|---:|
| tagged-16-boxed | 1,460 | 65 | 65 |
| tagged-16-representation-aware | 1,789 | 66 | 65 |
| nanbox-8-boxed | 727 | 33 | 33 |
| nanbox-8-representation-aware | 1,039 | 33 | 33 |

A separate regalloc2 0.15.3 profile maps the canonical call-boundary Core IR parameter/operand/result liveness to VRegs under a controlled 6 GPR / 6 FPR / 6 vector-register budget:

| Candidate | Register allocations | Modeled spill slots | Modeled spill bytes | Spill-store / reload edits |
|---|---:|---:|---:|---:|
| tagged-16-boxed | 34 | 6 | 48 | 6 / 6 |
| tagged-16-representation-aware | 23 | 0 | 0 | 0 / 0 |
| nanbox-8-boxed | 17 | 0 | 0 | 0 / 0 |
| nanbox-8-representation-aware | 17 | 0 | 0 | 0 / 0 |

These regalloc2 spill figures are a controlled liveness-model result, not Cranelift's private register-allocation dump, not the host ISA's physical spill-slot map and not a substitute for a general backend register-pressure benchmark. Similarly, objdump stack-relative loads/stores are actual generated machine instructions, but the counter does not classify every such access as a compiler spill.

## Heap, GC, allocator and roots

The OGM-26 workload compares RC cycle-removal and non-moving MarkSweep on equivalent graphs: 2,048 objects, 2,121 edges, 544 encoded object-reference Values and the same root topology for all candidates (171 interpreter-frame roots, 171 native-frame roots, 170 persistent handles and 32 temporary-scope roots). Both collectors preserve the reachable graph and reclaim the objects after all roots and handles are released.

| Value layout | Value storage capacity | Peak logical heap bytes | MarkSweep collection p50 (ns) | RC cycle-removal collection p50 (ns) |
|---|---:|---:|---:|---:|
| tagged-16 (both SSA policies) | 524,288 | 656,528 | 20,829–20,992 | 37,809–37,929 |
| nanbox-8 (both SSA policies) | 262,144 | 394,384 | 20,435–20,989 | 37,722–37,805 |

The OGM-24 allocator proof also performs real backing allocations for a matching number and size of per-object Value payload blocks, under System, per-isolate exact-reuse and per-isolate size-class-pool strategies. It measures allocation/deallocation time, peak reserved/live requested bytes, allocator-accounted internal fragmentation, reuse, evictions and failures. In this run the reuse pools performed 12,928 reused allocations per candidate across the samples, with zero allocation failures; peak allocator-accounted internal fragmentation was 32,768 bytes for Tagged16 payloads and 16,384 bytes for NaN-box payloads, while System reported zero.

**Boundary:** the OGM-24 allocator is a measured sidecar with payload dimensions matching OGM-26's candidate heap. It does not back OGM-26's object cells and is not a production Runtime allocator integration. Peak logical heap and the allocator's reserved/live/fragmentation counters are not OS process RSS, external fragmentation or total process memory. Thus the evidence is stronger than a logical Value-width estimate, but it does not close the integrated allocator/GC/peak-process-memory criterion.

OGM-27 shadow-root evidence also executes entry and call-boundary safepoints with 28 managed roots, interpreter/native frame providers, persistent handles and cleanup. All roots survive collection and all objects are reclaimed after release. This is an executable M0 root-map/frame model; it is not a production compiler-generated machine stack-map encoder or a full OGM-19 ABI integration.

## Reproduction

Run on the documented x86_64 Linux target with Rust 1.100.0, LLVM 18 development packages, Clang 18, GNU binutils and Python 3.12:

~~~sh
python .github/scripts/verify_core_ir.py --manifest tests/core-ir/manifest.json
python -m unittest discover -s benchmarks/value-representations -p "test_*.py" -v
cargo fmt --manifest-path benchmarks/backend-codegen/Cargo.toml --all -- --check
cargo clippy --manifest-path benchmarks/backend-codegen/Cargo.toml --all-targets --all-features --locked -- -D warnings
cargo test --manifest-path benchmarks/backend-codegen/Cargo.toml --all-features --locked
cargo run --manifest-path benchmarks/backend-codegen/Cargo.toml --locked --release --bin ogm38-value-ssa --   --iterations 1000 --warmups 3 --samples 101   --revision "$(git rev-parse HEAD)"   --output /tmp/ogm38-native-value-ssa-report.json
python -m unittest discover -s benchmarks/dual-backend -p "test_*.py" -v
python benchmarks/dual-backend/run.py   --output /tmp/ogm38-dual-backend-report.json   --artifact-dir /tmp/ogm38-dual-backend-artifacts   --iterations 1000
python benchmarks/value-representations/validate_ogm38_ogm31.py   --native-report /tmp/ogm38-native-value-ssa-report.json   --ogm31-report /tmp/ogm38-dual-backend-report.json   --output /tmp/ogm38-ogm31-differential-report.json
~~~

The focused GitHub Actions workflow runs these checks in the repository's CI Fast / engineering-delivery lane. Browser/runtime acceptance remains a separate promotion-candidate activity under the repository workflow and is not claimed by this PR.

## Acceptance-criteria status

| Criterion | Status | Evidence and remaining gap |
|---|---|---|
| Four candidates on the same accepted Core IR corpus | **Partial** | All four process 77 Value representations from the same five canonical fixtures and match the OGM-31 observations. Candidate-specific native lowering does not yet execute each complete fixture CFG end-to-end. |
| Actual codegen, throughput, register pressure/spills, helper and conversion costs | **Partial** | Native compile/object/loop throughput, helper callbacks, conversion counts, generated pressure-function disassembly and stack-relative traffic are measured. regalloc2 remains a separate controlled spill model; complete Cranelift spill metadata is not exposed. |
| Equivalent heap/allocator/GC and peak-memory metrics | **Partial** | OGM-26 collectors and OGM-24 backing allocator run equivalent candidate-shaped workloads with root and fragmentation metrics. Allocator is a sidecar, not integrated into GcHeap; process RSS is not measured. |
| Native differential semantics for requested values and effects | **Partial** | 31 cases pass for each candidate, including native strict equality, distinct String/BigInt handles, object/Symbol identity and ABI/host adapter observations. Some operator completions remain M0 oracle work rather than generated native operator implementations. |
| Rooting, handle lifetime, safepoints and ABI transitions | **Partial** | Real OGM-26 generation-checked handles survive forced collections; OGM-27 shadow roots and call-boundary models balance; OGM-31 host adapters match. No production machine stack maps or integrated Runtime ABI are claimed. |
| Raw measurements, toolchain/revision metadata and reproduction | **Partial** | The full current-run report is preserved in the CI artifact with SHA-256, tested revision, environment and reproduction steps. The repository's tracked gzip report is an older run; the current-run report is not committed, so this acceptance criterion remains open. |
| Documented Value ABI / internal SSA direction | **Partial** | Keep the public Value ABI explicitly tagged and architecture-neutral; preserve representation-aware SSA behind verified Core IR representations, boxing at dynamic/external boundaries. Do not select NaN-box from nominal width. A production ABI decision remains open. |

This is a verified and reproducible M0 evidence slice, **not completion of every OGM-38 acceptance criterion**. The Linear task remains In Progress. Remaining engineering gaps are still M0 proof gaps: they must not be mislabeled as browser QA, silently deferred to M1, or used as justification to start an unauthorized production Runtime implementation.

## Decision supported by current evidence

Keep two design decisions separate:

1. **Public Runtime Value ABI:** retain the explicit, architecture-neutral tagged representation as the baseline until broader object, GC, ABI, memory and integration evidence justifies a change.
2. **Internal compiler SSA:** prefer representation-aware SSA under the canonical Core IR v1 representation/phi contracts, and box only at dynamic semantic and external ABI boundaries.

The Number-loop measurements favour representation-aware SSA in this prototype. They do not prove that NaN-box is the better public ABI; the 8-byte representation did not produce the fastest representation-aware loop in this run. No final production Value layout is selected.
