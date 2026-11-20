# OGM-38 — Native Value and SSA evidence

**Status: In Progress.** The committed M0 evidence now measures all four candidates with generated native code, an allocator-backed OGM-26 heap, isolated process peak-memory workers and an independent OGM-31 differential. It does not close every OGM-38 acceptance criterion and is not a production Runtime/ABI decision.

## Question and candidates

OGM-38 independently evaluates two axes:

| Runtime Value layout | SSA policy | Candidate |
|---|---|---|
| Explicit tagged 16-byte value | Boxed | `tagged-16-boxed` |
| Explicit tagged 16-byte value | Representation-aware | `tagged-16-representation-aware` |
| 8-byte NaN-box word | Boxed | `nanbox-8-boxed` |
| 8-byte NaN-box word | Representation-aware | `nanbox-8-representation-aware` |

All four use the same Core IR v1 contract, the same five canonical OGM-31 fixtures, and the same semantic matrix. Runtime representation and internal SSA policy remain separate choices.

## Reproducible CI run and committed data

- Successful CI run: [#44766826425](https://github.com/AiratGaliev/ogma/actions/runs/44766826425).
- Tested source revision: `c7da33dc00e0d588ba36b809f17fc06a17559a6e`.
- Both `OGM-38 native Value and SSA representation evidence` and `fast-checks` passed. The native job completed formatting, Clippy (`-D warnings`), OGM-26/27/backend Rust tests, all four candidate executions, OGM-31 differential validation and evidence upload.
- Full run artifact: [ogm38-native-value-ssa-evidence, artifact 14644153664](https://github.com/AiratGaliev/ogma/actions/runs/44766826425/artifacts/14644153664). It preserves the native JSON, OGM-31 differential, Core IR reports, generated portable/native objects and lockfile. The artifact expires on 2027-02-18.
- Target: `x86_64-unknown-linux-gnu`; Rust 1.100.0; Cranelift 0.137.2; Linux x86-64 hosted runner; Intel Xeon Platinum 6975P-C.
- Sampling: three warmups, 101 measured samples per metric and 1,000 native calls per throughput sample.
- The repository commits the raw primary compile/throughput samples, the complete GC/allocator sample matrix, the complete machine-readable native report and the OGM-31 differential report under `benchmarks/value-representations/results/`. Every committed projection pins source/tested revision `c7da33dc00e0d588ba36b809f17fc06a17559a6e` and the SHA-256 of the complete report/differential.
- `export_ogm38_raw_samples.py` validates the CI native/differential reports and regenerates the committed-format JSON/gzip projections. The focused CI workflow executes this exporter on every OGM-38 run and uploads its output with the authoritative report.
- The older tracked report from earlier OGM-38 attempts is not the source of the current measurements.

The full report has a small post-run correction to two descriptive boundary strings: the raw values and samples are unchanged, but the report now describes the actual isolated VmHWM workers and the integrated heap allocator accurately.

## Native code generation and throughput

The generated Cranelift ObjectModule functions are loaded and invoked as native code. The principal throughput workload is a strict-equality-controlled, Number-only loop specialization from the canonical loop fixture (`js.add` + `js.to_number`). It is a deliberately narrow compiler/runtime probe, not representative JavaScript application throughput.

| Candidate | Value bytes | Compile p50 (ms) | Compile p95 (ms) | Loop p50 / p95 / p99 (ns) | Object bytes | Loop code bytes |
|---|---:|---:|---:|---:|---:|---:|
| tagged-16-boxed | 16 | 1.372 | 1.574 | 282.339 / 294.212 / 296.498 | 4,040 | 336 |
| tagged-16-representation-aware | 16 | 1.519 | 1.666 | 92.865 / 102.077 / 103.491 | 4,368 | 336 |
| nanbox-8-boxed | 8 | 1.268 | 1.446 | 282.077 / 292.020 / 293.274 | 3,600 | 392 |
| nanbox-8-representation-aware | 8 | 1.387 | 1.546 | 85.526 / 94.528 / 97.133 | 3,904 | 384 |

All four shared libraries are 15,256 bytes in this run. Complete object size includes helper functions in addition to the Number loop. Compile p50 is candidate Cranelift function compilation, not whole-program compile/startup time.

On this Number-only workload, representation-aware SSA reduces p50 latency by about 3.04× for Tagged16 and 3.30× for NaN-box compared with boxed SSA. This supports representation-aware internal SSA as a direction for further compiler work; it is not a universal runtime performance claim. NaN-box representation-aware is faster in this particular loop, but that does not establish that NaN-box should be the public Runtime ABI.

## Canonical Core IR corpus and differential

For every candidate the generated native batch dispatcher executes five batches, one for each canonical fixture, covering all 77 Value representations extracted from the five OGM-31 documents. All 77 inputs round-trip through the candidate codec and are dispatched by a generated Cranelift loop. Twenty reference Values are resolved through generation-checked OGM-26 handles after forced GC.

| Canonical fixture | Native Value dispatches | Reference Values resolved after forced GC |
|---|---:|---:|
| positive-ssa-branch | 7 | 0 |
| positive-loop-carried-values | 20 | 4 |
| positive-dynamic-exception | 14 | 5 |
| positive-call-boundary | 23 | 7 |
| positive-host-call | 13 | 4 |
| **Total per candidate** | **77** | **20** |

The separate OGM-31 portable/native comparison reports `all_candidates_match_portable_ogm31_oracle = true` for all four candidates and all five fixture IDs. It covers branch/loop/dynamic-number observations, 18 strict-equality cases, call-boundary, host echo, the Symbol TypeError observation and balanced OGM-27 roots.

**Execution boundary:** this does not mean the four candidates fully lower and execute every instruction and control-flow path of all five complete Core IR functions. They execute a generated native batch loop over the fixture Value corpus and a candidate-specific subset of branch, loop, Number dynamic addition, strict equality and related helpers. Call-boundary and host-call observations use candidate native dispatch plus M0 host/ABI adapters. OGM-31 is an independent portable/native oracle proof; the repository still does not contain a general production Core IR-to-native compiler.

## Value semantics and equality

Every candidate passes the same 31-case matrix. It covers Number/String/BigInt observations, Symbol and BigInt/Number errors, NaN, signed zero, infinities, undefined/null/booleans, object and Symbol identity, String and BigInt strict equality, type transitions, completion/exception passthrough, call-boundary and host-call behavior. Each candidate records 63 native callbacks and 33 forced collections.

The strict-equality regression is important: equal Strings and equal BigInts are allocated into distinct generation-checked handles, yet generated native strict equality compares their semantic values and returns true. Distinct Strings/BigInts compare false. Object and Symbol strict equality remains identity-based. The primitive comparison callback resolves both tokens through OGM-26, forces collection while handles remain retained, verifies the expected objects and compares the actual String/BigInt values.

**Boundary:** String concatenation, BigInt arithmetic, object ToPrimitive and some completion observations are still M0 oracle/adapter work rather than general generated native operator implementations. The 31-case pass is not a claim that the full ECMAScript operator set has been lowered.

## Heap, allocator, GC, roots and peak process memory

The OGM-26 heap uses the OGM-24 `IsolateAllocator` for actual object payload backing blocks. Each object cell owns its allocation; its candidate Value slots are stored in that block and are exposed through checked word-slice accessors. When RC-cycle-removal or MarkSweep reclaims an object, its backing allocation is returned to the selected allocator strategy. Unit tests exercise System, exact-reuse and size-class strategies under both collectors, including payload contents surviving roots/collection and storage reclamation after roots are released.

Each candidate is measured with the same OGM-26 object graph under both collectors and all three allocator strategies: 2,048 objects, 2,121 edges, 544 encoded object-reference Values and the same root topology (171 interpreter-frame roots, 171 native-frame roots, 170 persistent handles and 32 temporary-scope roots).

| Value layout | Value payload capacity | Peak logical heap |
|---|---:|---:|
| Tagged16 (both SSA policies) | 524,288 B | 787,600 B |
| NaN-box (both SSA policies) | 262,144 B | 525,456 B |

Each of the 24 candidate × collector × allocator combinations runs in its own child process. The worker reads Linux `/proc/self/status` `VmHWM` before warmups and after three warmups plus 101 measured graph lifecycles. The baseline, absolute peak and delta are recorded per worker. The metric includes the worker executable, code mappings, Rust runtime and allocator state; it is peak process memory, not a heap-only RSS measurement.

| Candidate | Peak worker VmHWM across its six GC/allocator combinations |
|---|---:|
| tagged-16-boxed | 4.633–4.863 MiB |
| tagged-16-representation-aware | 4.672–4.836 MiB |
| nanbox-8-boxed | 4.398–4.566 MiB |
| nanbox-8-representation-aware | 4.465–4.598 MiB |

The worker report stores baseline/peak/delta for every combination, not only the range shown here. The exporter and regression tests verify that all 24 combinations exist, each has 101 raw samples, and `peak >= baseline` with `delta = peak - baseline`.

The direct OGM-24 allocator microbenchmark is also retained separately for each candidate. In this run the exact-reuse and size-class strategies each recorded 12,928 reuses and zero allocation failures across the samples. Maximum allocator-accounted internal fragmentation was 32,768 B for Tagged16 payloads and 16,384 B for NaN-box payloads; System reported zero. Allocator accounting and VmHWM are distinct measures and must not be conflated.

OGM-27 shadow-root evidence exercises entry/call-boundary roots, persistent handles, forced collections and cleanup. All modeled roots survive collection and all objects are reclaimed after release. This remains an executable M0 root-map/frame model, not compiler-generated production machine stack maps or a fully integrated production OGM-19 Runtime ABI.

## Register pressure, spill and conversion evidence

The Cranelift-generated 32-live-Value pressure function is disassembled with GNU objdump. The following are actual stack-relative memory instructions in the generated native function, not inferred allocator spill counts:

| Candidate | Pressure function code bytes | Stack-relative loads | Stack-relative stores |
|---|---:|---:|---:|
| tagged-16-boxed | 1,460 | 65 | 65 |
| tagged-16-representation-aware | 1,789 | 66 | 65 |
| nanbox-8-boxed | 727 | 33 | 33 |
| nanbox-8-representation-aware | 1,039 | 33 | 33 |

A separate regalloc2 0.15.3 liveness profile maps canonical call-boundary Core IR parameter/operand/result representations to VRegs under a documented System V AMD64 register budget (14 non-reserved GPRs and 16 XMM/FPR registers, plus 16 vector registers). Its reported spill slots/moves are a controlled liveness-model result, **not Cranelift's private physical register allocation or spill dump**. The pinned Cranelift 0.137.2 public API does not expose its private register mask; the report states that boundary explicitly. The objdump stack-relative counter also does not classify every stack access as a spill.

## Reproduction

The OGM-38 focused CI workflow runs the same sequence. From the repository root on the documented x86-64 Linux target with Rust 1.100.0, LLVM/Clang 18, GNU binutils and Python 3.12:

~~~sh
python .github/scripts/verify_core_ir.py --manifest tests/core-ir/manifest.json
cargo fmt --manifest-path benchmarks/gc-comparison/Cargo.toml --all -- --check
cargo fmt --manifest-path benchmarks/native-runtime-boundary/Cargo.toml --all -- --check
cargo fmt --manifest-path benchmarks/backend-codegen/Cargo.toml --all -- --check
cargo clippy --manifest-path benchmarks/gc-comparison/Cargo.toml --all-targets --all-features --locked -- -D warnings
cargo clippy --manifest-path benchmarks/native-runtime-boundary/Cargo.toml --all-targets --all-features --locked -- -D warnings
cargo clippy --manifest-path benchmarks/backend-codegen/Cargo.toml --all-targets --all-features --locked -- -D warnings
cargo test --manifest-path benchmarks/gc-comparison/Cargo.toml --all-features --locked
cargo test --manifest-path benchmarks/native-runtime-boundary/Cargo.toml --all-features --locked
cargo test --manifest-path benchmarks/backend-codegen/Cargo.toml --all-features --locked
cargo run --manifest-path benchmarks/backend-codegen/Cargo.toml --locked --release --bin ogm38-value-ssa -- --iterations 1000 --warmups 3 --samples 101 --revision "$(git rev-parse HEAD)" --output /tmp/ogm38-native-value-ssa-report.json
python -m unittest discover -s benchmarks/dual-backend -p "test_*.py" -v
python benchmarks/dual-backend/run.py --output /tmp/ogm38-dual-backend-report.json --artifact-dir /tmp/ogm38-dual-backend-artifacts --iterations 1000
python benchmarks/value-representations/validate_ogm38_ogm31.py --native-report /tmp/ogm38-native-value-ssa-report.json --ogm31-report /tmp/ogm38-dual-backend-report.json --output /tmp/ogm38-ogm31-differential-report.json
python benchmarks/value-representations/export_ogm38_raw_samples.py --native-report /tmp/ogm38-native-value-ssa-report.json --differential-report /tmp/ogm38-ogm31-differential-report.json --run-url "$GITHUB_SERVER_URL/$GITHUB_REPOSITORY/actions/runs/$GITHUB_RUN_ID" --artifact-url "$GITHUB_SERVER_URL/$GITHUB_REPOSITORY/actions/runs/$GITHUB_RUN_ID/artifacts" --output-dir /tmp/ogm38-results
~~~

The full report, OGM-31 differential and generated portable/native artifacts remain available in the CI run artifact. Committed projections retain every raw compile/throughput sample and every GC/allocator sample, plus source/tested revision, toolchain, worker peak-memory metrics and reproducible validation.

## Acceptance-criteria status

| Criterion | Status | Evidence and remaining gap |
|---|---|---|
| All four candidates on the same accepted Core IR corpus | **Partial** | All four execute five generated native batch loops covering 77 Values from the same five fixtures and match OGM-31 observations. Full candidate-specific lowering/execution of every complete fixture CFG is not implemented. |
| Actual native codegen, compile time, throughput, pressure/spills, helper/conversion costs | **Partial** | Native code/object/compile/loop/helper metrics and objdump pressure-function accesses are measured. The regalloc2 spill profile is still a separate liveness model, not Cranelift's private spill report. |
| Equivalent heap/allocator/GC and peak memory | **Met for this M0 proof heap** | OGM-24 allocator blocks back the actual OGM-26 object payloads; both collectors × all three allocator strategies run equivalent workloads. Isolated worker VmHWM baseline/peak/delta is recorded. This is still not a production Runtime heap or total application footprint. |
| Differential correctness for requested values and effects | **Partial** | 31 semantic cases per candidate, generated native strict equality, distinct String/BigInt handle value comparisons, object/Symbol identity and OGM-31 differential pass. Some operator implementations remain oracle/adapter work rather than candidate-native lowering. |
| Rooting, handle lifetime, safepoints and ABI transitions | **Partial** | OGM-26 generation-checked handles survive forced GC; OGM-27 shadow roots and M0 host/ABI adapters balance. Production machine stack maps and full Runtime ABI integration are not implemented. |
| Raw measurements, revisions, reproduction and machine-readable reports committed | **Met for the M0 corpus** | The repository commits all 101 compile/loop raw samples, all 101 GC samples for 24 candidate × collector × allocator combinations, all allocator raw samples, full native JSON projection and OGM-31 differential. Revisions/toolchain and reproduction/exporter scripts are pinned. |
| Separate public Runtime ABI vs internal SSA decision | **Partial** | Retain explicit tagged, architecture-neutral public Value ABI as the conservative baseline; prefer representation-aware internal SSA under Core IR v1 contracts and box at dynamic/external boundaries. No production ABI selection follows from width alone. |

This is a verified M0 evidence slice, **not completion of every OGM-38 acceptance criterion**. Full Core IR CFG lowering, complete native operator implementation, true Cranelift spill metadata, production machine stack maps and production Runtime ABI integration remain M0 engineering gaps. They must not be mislabeled as browser QA or silently deferred to M1. OGM-38 therefore remains In Progress.
