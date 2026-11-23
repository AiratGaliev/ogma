# OGM-38 — Native Value and SSA evidence

**M0 native evidence and raw-data Git storage are complete; PR #32 remains open pending integration.** All four candidates are measured with generated native code, an OGM-24-backed OGM-26 heap, isolated peak-memory workers, a 31-case native semantic batch CFG and independent OGM-31 differential. The canonical raw dataset is committed as checksum-pinned Base64 fragments and reconstructed by a tested decoder. The full native report and additional execution artifacts remain available in CI.

## Question and candidates

OGM-38 independently evaluates two axes:

| Runtime Value layout | SSA policy | Candidate |
|---|---|---|
| Explicit tagged 16-byte value | Boxed | `tagged-16-boxed` |
| Explicit tagged 16-byte value | Representation-aware | `tagged-16-representation-aware` |
| 8-byte NaN-box word | Boxed | `nanbox-8-boxed` |
| 8-byte NaN-box word | Representation-aware | `nanbox-8-representation-aware` |

All four use the same Core IR v1 contract, the same five canonical OGM-31 fixtures, and the same semantic matrix. Runtime representation and internal SSA policy remain separate choices.

## Reproducible CI run and evidence provenance

- Snapshot-producing native-evidence run: [#45130525848](https://github.com/AiratGaliev/ogma/actions/runs/45130525848). The active raw measurement snapshot is pinned to this successful run.
- Measured source/tested revision: `69038fbbd6246f14466ab91821524f7cc0cfb04c`; the native and raw reports record identical source and tested revisions.
- Both `fast-checks` and `OGM-38 native Value and SSA representation evidence` passed on the snapshot-producing revision. The focused job completed Core IR/OGM-17 verification, formatting, Clippy with `-D warnings`, OGM-26/27/backend contract tests, all four native candidates, OGM-31 differential, raw export and validation/regression tests.
- Full evidence artifact: [`ogm38-native-value-ssa-evidence`, artifact 14798285147](https://github.com/AiratGaliev/ogma/actions/runs/45130525848/artifacts/14798285147). ZIP SHA-256: `3d6fceb2879bd1533f0e84333400b585f7b7f70a6768ada91cd34d3308a04b42`; size 629,148 bytes. It contains the full native JSON, OGM-31 differential, Core IR report, generated Cranelift/LLVM objects, portable .ogmb fixtures, lockfile and raw samples.
- Target: `x86_64-unknown-linux-gnu`; CPU Intel Xeon 6975P-C; Linux 7.0.0-1014-aws; Rust 1.100.0; Cranelift 0.137.2; regalloc2 0.15.3; Python 3.12.16.
- Sampling: three warmups, 101 measured samples per metric and 1,000 native calls per throughput sample.
- Report checksums: native JSON `1c797f3bd4a523ccb11c12803c43a9b9184d0a43aac8398308d3d245811c248f`; compressed native report `e1fd3bab0476f98eb48728a2916275eec386de337907e334e552fed0afbae73e`; differential `c8d2f9ac94993beed48a887e0e4896349a624c0a3f15906beeaddff6c260978b`; primary projection `77f499d4d8d8ff9ecfc13f4772220ddb7195a8dc477287ceabfdfdc6f9a69d70`; runtime projection gzip `270e1cbc059fc6fd37d14984c7ddf4979db24f7ee51e30998723091ebfaa7564`.
- Active compact XZ dataset: 13 Git-tracked fragments under `results/ogm38-raw-native-samples.delta.json.xz.b64.parts/` (12 × 4,000 bytes and one 3,984-byte fragment; 51,984 Base64 bytes). Reconstructed XZ SHA-256 `a6f4e0485194cda809af97d5ce84049b0a7ec257adc80d3ee9150dddfe52b5af`, size 38,988 bytes; decompressed JSON SHA-256 `3a165dc0e13c9bb92e7e773b4a39f51dc0fa875f255cd5411db9a364fe7bfe59`, size 163,793 bytes.
- The active compact XZ dataset contains 156 raw series × 101 samples and 36 worker records (24 collector configurations and 12 allocator configurations). The complete columnar sample export is also retained in artifact #14798285147.
- The previous source snapshot `74991baa54b346389b6193c1f7075954055be181` is preserved unchanged under `results/historical/`, including its 14 fragments, report and original artifact [#14762343441](https://github.com/AiratGaliev/ogma/actions/runs/45044214950/artifacts/14762343441). The historical decoder expectation validates it independently; current evidence does not relabel it.

## Native code generation and throughput

The generated Cranelift ObjectModule functions are loaded and invoked as native code. The principal throughput workload is a strict-equality-controlled, Number-only loop specialization from the canonical loop fixture (`js.add` + `js.to_number`). It is a deliberately narrow compiler/runtime probe, not representative JavaScript application throughput.

| Candidate | Value bytes | Compile p50 / p95 / p99 (ms) | Loop p50 / p95 / p99 (ns) | Handle dispatch p50 (ns) | Object bytes | Loop code bytes |
|---|---:|---:|---:|---:|---:|---:|
| tagged-16-boxed | 16 | 1.433 / 1.621 / 1.703 | 277.955 / 295.050 / 298.248 | 5.413 | 4,128 | 336 |
| tagged-16-representation-aware | 16 | 1.514 / 1.536 / 1.540 | 92.671 / 104.834 / 106.326 | 5.413 | 4,456 | 336 |
| nanbox-8-boxed | 8 | 1.222 / 1.261 / 1.445 | 277.306 / 291.897 / 298.524 | 5.163 | 3,720 | 392 |
| nanbox-8-representation-aware | 8 | 1.333 / 1.575 / 1.654 | 83.092 / 95.237 / 96.427 | 5.167 | 4,024 | 384 |

All four shared libraries are 15,304 bytes in this run. Complete object size includes helper functions in addition to the Number loop. Compile p50 is candidate Cranelift function compilation, not whole-program compile/startup time. The large p99 compile outliers are retained in the raw sample set and should not be treated as steady-state compile latency.

On this Number-only workload, representation-aware SSA reduces p50 latency by about 3.00× for Tagged16 and 3.34× for NaN-box compared with boxed SSA. The NaN-box representation-aware p50 is 10.3% lower than Tagged16 representation-aware, a small difference compared with the representation-policy gain. This supports representation-aware internal SSA as the compiler policy; it is not a universal application performance claim and does not establish NaN-box as a public or production ABI.

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

The separate OGM-31 portable/native comparison reports `all_candidates_match_portable_ogm31_oracle = true` for all four candidates and all five fixture IDs. The semantic suite additionally performs exactly 31 generated native batch-dispatch invocations per candidate (124 total); the validator rejects missing aggregate or per-case entries. It covers branch/loop/dynamic-number observations, 18 strict-equality cases, call-boundary, host echo, the Symbol TypeError observation and balanced OGM-27 roots.

**Execution boundary:** this does not mean the four candidates fully lower and execute every instruction and control-flow path of all five complete Core IR functions. They execute a generated native batch loop over the fixture Value corpus and a candidate-specific subset of branch, loop, Number dynamic addition, strict equality and related helpers. Call-boundary and host-call observations use candidate native dispatch plus M0 host/ABI adapters. OGM-31 is an independent portable/native oracle proof; the repository still does not contain a general production Core IR-to-native compiler.

## Value semantics and equality

Every candidate passes the same 31-case matrix. It covers Number/String/BigInt observations, Symbol and BigInt/Number errors, NaN, signed zero, infinities, undefined/null/booleans, object and Symbol identity, String and BigInt strict equality, type transitions, completion/exception passthrough, call-boundary and host-call behavior. The current report records 31 native semantic batch-dispatch invocations, 70 native callbacks and 39 forced collections per candidate. The generated dispatcher owns the candidate-layout addressing, decode/validation and callback loop; Rust enters it once for each semantic case. A supplemental Rust regression test (outside the fixed 31-case report matrix) also enters the generated native js.add helper for BigInt/String, Object-to-String/BigInt and BigInt/Object-to-String combinations on all four candidate layouts, protecting the required ToPrimitive and String-precedence ordering; it also verifies the explicitly bounded M0 signed-128-bit BigInt overflow completion.

The strict-equality regression is important: equal Strings and equal BigInts are allocated into distinct generation-checked handles, yet generated native strict equality compares their semantic values and returns true. Distinct Strings/BigInts compare false. Object and Symbol strict equality remains identity-based. The primitive comparison callback resolves both tokens through OGM-26, forces collection while handles remain retained, verifies the expected objects and compares the actual String/BigInt values.

**Boundary:** String concatenation, BigInt arithmetic, object ToPrimitive and some completion observations are still M0 oracle/adapter work rather than general generated native operator implementations. The 31-case pass is not a claim that the full ECMAScript operator set has been lowered.

## Native semantic helper measurements

The generated native candidate-layout callers enter shared M0 Rust semantic helpers for dynamic operations. These timings measure the real helper/ABI boundary and should not be interpreted as four separate Cranelift implementations of the full ECMAScript operators.

| Candidate | Number add | String concat | BigInt add | Object ToPrimitive add | Symbol error | BigInt/Number error |
|---|---:|---:|---:|---:|---:|---:|
| tagged-16-boxed | 39.401 | 4,783.544 | 8,842.780 | 4,851.693 | 181.875 | 177.903 |
| tagged-16-representation-aware | 39.397 | 4,806.324 | 8,868.014 | 4,806.631 | 181.809 | 178.015 |
| nanbox-8-boxed | 39.118 | 4,824.514 | 8,965.487 | 4,838.851 | 182.026 | 178.444 |
| nanbox-8-representation-aware | 39.124 | 4,771.434 | 8,840.482 | 4,858.873 | 182.139 | 178.520 |

<em>Values are p50 nanoseconds per call; every helper case has 101 raw samples.</em>

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
| tagged-16-boxed | 4.723–4.848 MiB |
| tagged-16-representation-aware | 4.777–4.828 MiB |
| nanbox-8-boxed | 4.469–4.527 MiB |
| nanbox-8-representation-aware | 4.484–4.527 MiB |

The worker report stores baseline/peak/delta for every combination, not only the range shown here. The exporter and regression tests verify that all 24 combinations exist, each has 101 raw samples, and `peak >= baseline` with `delta = peak - baseline`.

The direct OGM-24 allocator microbenchmark is also retained separately for each candidate. In this run the exact-reuse and size-class strategies each recorded 12,928 reuses and zero allocation failures across the samples. Maximum allocator-accounted internal fragmentation was 32,768 B for Tagged16 payloads and 16,384 B for NaN-box payloads; System reported zero. Allocator accounting and VmHWM are distinct measures and must not be conflated.

OGM-27 shadow-root evidence exercises entry/call-boundary roots, persistent handles, forced collections and cleanup. All modeled roots survive collection and all objects are reclaimed after release. This remains an executable M0 root-map/frame model, not compiler-generated production machine stack maps or a fully integrated production OGM-19 Runtime ABI.

## Register pressure, spill and conversion evidence

The `ogma_value_pressure` function keeps 32 candidate Values live across a native callback. It is generated by the same Cranelift 0.137.2 ObjectModule as the candidate code and is disassembled with GNU objdump. The function declares no explicit Cranelift stack slots or local arrays; the spill counters are derived from emitted x86-64 stack-memory operands, with callee-save `push`/`pop` bookkeeping excluded.

| Candidate | Pressure code bytes | Stack loads / stores | Cranelift spill loads / stores | Spill slots | Spill bytes |
|---|---:|---:|---:|---:|---:|
| tagged-16-boxed | 1,460 | 65 / 65 | 64 / 64 | 64 | 512 |
| tagged-16-representation-aware | 1,789 | 66 / 65 | 65 / 64 | 64 | 512 |
| nanbox-8-boxed | 727 | 33 / 33 | 32 / 32 | 32 | 256 |
| nanbox-8-representation-aware | 1,039 | 33 / 33 | 32 / 32 | 32 | 256 |

These spill counts are observations from the actual generated Cranelift native pressure function, not the separate regalloc2 model. The supplementary regalloc2 0.15.3 profile still maps canonical call-boundary Core IR values to VRegs under the documented System V AMD64 register budget; it is a liveness-model comparison and is not presented as Cranelift's private allocation dump. Stack-relative instruction totals are also kept separately from the classified allocator spill moves.

The candidate probe shows that reducing the Value storage width reduces pressure-function spill slots from 64 to 32 in this synthetic 32-live-Value workload. This is a measured property of this controlled probe, not evidence that every production workload will halve its spills.

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

The full report, OGM-31 differential, generated portable/native artifacts and every raw compile/throughput/GC/allocator sample remain available in the CI run artifact. The latest-evidence manifest records checksums, source/tested revision, toolchain, worker peak-memory metrics and reproduction metadata. The older primary projection is explicitly historical and is not represented as the current run.

## Acceptance-criteria status

| Criterion | Status | Evidence and remaining gap |
|---|---|---|
| All four candidates use the same accepted Core IR corpus/equivalent semantics | **Met for the M0 corpus proof** | Same five OGM-31 fixtures, 77 generated native Value dispatches per candidate, 31 generated native semantic batch CFG entries per candidate and independent OGM-31 differential match. This is not a general-purpose full-CFG compiler. |
| Actual native codegen, compile time, throughput, pressure/spills, helper/conversion costs | **Met for the M0 probes** | Cranelift emits and executes all four candidates; compile/throughput/helper metrics and a generated 32-live-Value pressure function are measured. GNU objdump identifies emitted spill moves/slots from that function. The separate regalloc2 profile remains supplemental liveness-model evidence, not Cranelift's private allocator dump. Full candidate-native lowering of every Core IR instruction remains covered by criterion 1. |
| Equivalent heap/allocator/GC and peak memory | **Met for this M0 proof heap** | OGM-24 allocator blocks back the actual OGM-26 object payloads; both collectors × all three allocator strategies run equivalent workloads. Isolated worker VmHWM baseline/peak/delta is recorded. This is still not a production Runtime heap or total application footprint. |
| Differential correctness for requested values and effects | **Met for the declared 31-case subset** | All 31 cases per candidate plus OGM-31 oracle match; NaN, signed zero, infinities, BigInt, undefined/null, Strings, Object/Symbol identity, type transitions, exceptions and host traces are covered. Shared helper/ABI boundaries are identified. |
| Rooting, handle lifetime, safepoints and ABI transitions | **Met for the M0 integration model** | 28 generation-checked candidate roots survive entry/call-boundary collections and are reclaimed; OGM-27 frame/safepoint proof and OGM-19/32 versioned ABI/host-call contracts are exercised. No production stack-map encoder is claimed. |
| Raw measurements, revisions, reproduction and machine-readable reports committed | **Met** | The active 156-series/101-sample lossless XZ delta dataset is Git-tracked as 13 Base64 fragments; the previous 14-fragment snapshot remains independently verified under `results/historical/`.  The decoder verifies exact XZ/JSON checksums, source/tested revision, four-candidate inventory and 36 worker records; the regression test reconstructs the archive. The complete source artifact and newer verification artifact remain available in CI. |
| Public Runtime ABI separated from internal SSA/layout decision | **Met as an M0 recommendation** | Keep the public embedding ABI opaque/versioned; use explicit tagged Value as the conservative internal M1 baseline; adopt representation-aware SSA; retain NaN-box as experimental, not selected from nominal width alone. |

OGM-38's bounded M0 native evidence and Value/SSA recommendation are complete on the active measured revision `69038fbb…`; the prior `74991baa…` archive is preserved as a separately verified historical snapshot. PR #32 remains open. Merge readiness requires successful Fast and focused OGM-38 native checks for the current PR head; Linear OGM-38 remains In Progress until the reviewed changes are integrated into `dev`. A production Runtime, general full-ECMAScript operator lowering and production machine stack-map emission remain outside this bounded M0 proof. The active measurement snapshot is pinned to `69038fbbd6246f14466ab91821524f7cc0cfb04c`; the earlier `74991baa…` snapshot is retained with independent provenance. The PR remains open until the current branch head passes both required CI jobs and is integrated into `dev`. PR #32 remains open; Linear OGM-38 stays In Progress until the reviewed changes are integrated into `dev`.


Historical projection provenance: `ogm38-primary-benchmark-samples.json` is retained as a separate reference snapshot from run #447304 / revision `dfea7eb`. It is not mixed with the current raw native/GC/allocator dataset pinned to run #451305 / revision `69038fbb`; the manifest records both provenance chains independently.
