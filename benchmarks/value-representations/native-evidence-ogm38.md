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

## Reproducible CI run and evidence provenance

- Latest successful CI run: [#45023749338](https://github.com/AiratGaliev/ogma/actions/runs/45023749338).
- Tested source revision: `439f74c945a1acd93b816b33982fa103bb72d7bc`.
- Both `fast-checks` and `OGM-38 native Value and SSA representation evidence` passed. The focused job ran the canonical Core IR/OGM-17 checks, OGM-26/27/backend format and Clippy checks, Rust contract tests, all four native candidates, OGM-31 differential validation and evidence export.
- Full run artifact: [`ogm38-native-value-ssa-evidence`, artifact 14755470848](https://github.com/AiratGaliev/ogma/actions/runs/45023749338/artifacts/14755470848). It contains the authoritative native JSON, OGM-31 differential, Core IR report, generated portable/native artifacts and lockfile.
- Target: `x86_64-unknown-linux-gnu`; Rust 1.100.0; Cranelift 0.137.2; Linux 7.0.0-1017-azure; AMD EPYC 9V74 80-Core Processor.
- Sampling: three warmups, 101 measured samples per metric and 1,000 native calls per throughput sample.
- The machine-readable snapshot `results/ogm38-latest-evidence.json` pins this run, the measured source revision and SHA-256 checksums for the full native report, differential report, primary sample projection and GC/allocator projection. The exact unprojected reports and all raw samples are retained in the CI artifact.
- The older tracked `ogm38-primary-benchmark-samples.json`, `ogm38-native-value-ssa-report.json.gz`, `ogm38-runtime-raw-samples.json.gz` and `ogm38-ogm31-differential-report.json` files still refer to an earlier run; they are historical snapshots and must not be used as evidence for revision `439f74c`. The latest raw files are available from the linked Actions artifact and are checksum-addressed by the manifest.
- `export_ogm38_raw_samples.py` validates the native and differential reports and creates deterministic primary/runtime projections. CI runs the exporter before uploading the artifact. The manifest deliberately distinguishes source/tested revision from later evidence-documentation commits.

## Native code generation and throughput

The generated Cranelift ObjectModule functions are loaded and invoked as native code. The principal throughput workload is a strict-equality-controlled, Number-only loop specialization from the canonical loop fixture (`js.add` + `js.to_number`). It is a deliberately narrow compiler/runtime probe, not representative JavaScript application throughput.

| Candidate | Value bytes | Compile p50 / p95 / p99 (ms) | Loop p50 / p95 / p99 (ns) | Object bytes | Loop code bytes |
|---|---:|---:|---:|---:|---:|
| tagged-16-boxed | 16 | 1.622 / 1.849 / 17.139 | 342.735 / 355.074 / 356.505 | 4,128 | 336 |
| tagged-16-representation-aware | 16 | 1.822 / 1.860 / 2.020 | 109.371 / 123.642 / 127.227 | 4,456 | 336 |
| nanbox-8-boxed | 8 | 1.522 / 1.572 / 82.611 | 342.434 / 354.593 / 357.657 | 3,720 | 392 |
| nanbox-8-representation-aware | 8 | 1.625 / 1.698 / 20.710 | 108.831 / 117.453 / 124.624 | 4,024 | 384 |

All four shared libraries are 15,304 bytes in this run. Complete object size includes helper functions in addition to the Number loop. Compile p50 is candidate Cranelift function compilation, not whole-program compile/startup time. The large p99 compile outliers are retained in the raw sample set and should not be treated as steady-state compile latency.

On this Number-only workload, representation-aware SSA reduces p50 latency by about 3.13× for Tagged16 and 3.15× for NaN-box compared with boxed SSA. This supports representation-aware internal SSA as a direction for further compiler work; it is not a universal runtime performance claim. NaN-box representation-aware is faster in this particular loop, but that does not establish that NaN-box should be the public Runtime ABI.

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

Every candidate passes the same 31-case matrix. It covers Number/String/BigInt observations, Symbol and BigInt/Number errors, NaN, signed zero, infinities, undefined/null/booleans, object and Symbol identity, String and BigInt strict equality, type transitions, completion/exception passthrough, call-boundary and host-call behavior. The current report records 70 native callbacks and 39 forced collections per candidate.

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
| tagged-16-boxed | 4.680–4.797 MiB |
| tagged-16-representation-aware | 4.684–4.867 MiB |
| nanbox-8-boxed | 4.465–4.676 MiB |
| nanbox-8-representation-aware | 4.434–4.578 MiB |

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

The full report, OGM-31 differential, generated portable/native artifacts and every raw compile/throughput/GC/allocator sample remain available in the CI run artifact. The latest-evidence manifest records checksums, source/tested revision, toolchain, worker peak-memory metrics and reproduction metadata. The older checked-in projections are explicitly historical and are not represented as the current run.

## Acceptance-criteria status

| Criterion | Status | Evidence and remaining gap |
|---|---|---|
| All four candidates on the same accepted Core IR corpus | **Partial** | All four execute five generated native batch loops covering 77 Values from the same five fixtures and match OGM-31 observations. Full candidate-specific lowering/execution of every complete fixture CFG is not implemented. |
| Actual native codegen, compile time, throughput, pressure/spills, helper/conversion costs | **Met for the M0 probes** | Cranelift emits and executes all four candidates; compile/throughput/helper metrics and a generated 32-live-Value pressure function are measured. GNU objdump identifies emitted spill moves/slots from that function. The separate regalloc2 profile remains supplemental liveness-model evidence, not Cranelift's private allocator dump. Full candidate-native lowering of every Core IR instruction remains covered by criterion 1. |
| Equivalent heap/allocator/GC and peak memory | **Met for this M0 proof heap** | OGM-24 allocator blocks back the actual OGM-26 object payloads; both collectors × all three allocator strategies run equivalent workloads. Isolated worker VmHWM baseline/peak/delta is recorded. This is still not a production Runtime heap or total application footprint. |
| Differential correctness for requested values and effects | **Partial** | 31 semantic cases per candidate, generated native strict equality, distinct String/BigInt handle value comparisons, object/Symbol identity and OGM-31 differential pass. Some operator implementations remain oracle/adapter work rather than candidate-native lowering. |
| Rooting, handle lifetime, safepoints and ABI transitions | **Partial** | OGM-26 generation-checked handles survive forced GC; OGM-27 shadow roots and M0 host/ABI adapters balance. Production machine stack maps and full Runtime ABI integration are not implemented. |
| Raw measurements, revisions, reproduction and machine-readable reports committed | **Partial** | The latest CI artifact contains the full raw samples, native report and OGM-31 differential; the committed latest-evidence manifest pins the run and checksums. The checked-in raw projection files are from an earlier revision and have not yet been refreshed from this run. |
| Separate public Runtime ABI vs internal SSA decision | **Partial** | Retain explicit tagged, architecture-neutral public Value ABI as the conservative baseline; prefer representation-aware internal SSA under Core IR v1 contracts and box at dynamic/external boundaries. No production ABI selection follows from width alone. |

This is a verified M0 evidence slice, **not completion of every OGM-38 acceptance criterion**. Full Core IR CFG lowering, complete native operator implementation, true Cranelift spill metadata, production machine stack maps and production Runtime ABI integration remain M0 engineering gaps. They must not be mislabeled as browser QA or silently deferred to M1. OGM-38 therefore remains In Progress.
