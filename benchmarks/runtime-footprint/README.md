# OGM-29 — interpreter and hybrid-AOT footprint baseline

This directory contains the OGM-29 M0 footprint harness. It uses the same five canonical ogma.core-ir/1.0.0 fixtures already consumed by OGM-31's portable interpreter and OGM-30's native Cranelift/LLVM fixture adapters.

The harness is deliberately explicit about proof versus production measurements. The repository still has no production Rust Runtime, source parser, root Cargo workspace or profile-linked application executable. Therefore the report includes measured proof artifacts and process/toolchain costs, while true production-runtime startup, binary size and RSS remain not_implemented. It does not fill those gaps with estimates.

## What is measured

| Area | Portable interpreter proof | Hybrid/native proof |
|---|---|---|
| Workload | Five shared Core IR fixtures; register bytecode plus stack/register differential check | Same five fixture paths from the OGM-30 native workload manifest |
| Startup | 101-process start-to-ready distribution for the Python proof worker | 101-process start-to-ready distribution for the native benchmark executable |
| Binary/object size | Per-fixture .ogmb container and encoded code bytes | Benchmark tool executable, Cranelift/LLVM object and linked fixture-library sizes |
| Peak memory | Python worker ru_maxrss; source read, Core IR JSON parse and bytecode compilation tracemalloc peaks | GNU time process high-water RSS for both compiler backends and proof workloads |
| Compile cost | Core IR fixture to register .ogmb: wall, codegen, decode and separate verifier samples | First and repeated Cranelift/LLVM object compile and link distributions |
| Host-call overhead | Core IR host capability dispatch versus direct Python function baseline | Generated AOT host call versus direct native helper baseline |
| Dependencies | No production Rust interpreter Cargo graph exists | Full Cargo metadata graph of the benchmark executable and proof crates; compiler packages are identified |
| Shipped runtime graph | OGM-28 logical profile closure only | OGM-28 logical profile closure only; actual package graph remains unavailable |

Memory and startup fields name their measurement boundary. The Python proof worker is not a Rust VM; the native compiler process is not an Ogma production Runtime. Cross-language throughput is not presented as a production interpreter-versus-AOT speedup.

### Source, AST and Core IR memory

The runner measures source fixture read allocation and materialization of the canonical Core IR JSON documents. It also measures the tracemalloc peak while compiling those fixtures into the existing Python bytecode prototype.

AST peak memory is not_implemented: these JavaScript files are traceable source references, not inputs to a real parser. The repository has no production parser or source-to-Core-IR frontend. The report does not relabel JSON object materialization as AST memory.

### Dependency separation

benchmark_compile_time_graph is collected from cargo metadata --locked for benchmarks/backend-codegen/Cargo.toml. It covers the native benchmark executable and its proof crates, including Cranelift, Inkwell and LLVM binding dependencies. These are build-time dependencies of this measurement tool.

logical_shipped_runtime_graphs resolves the existing OGM-28 profile dependency graph for portable-interpreter-release and hybrid-native-release. These are logical architecture components, not Cargo packages. actual_shipped_runtime_package_graph remains not_implemented until production Runtime crates and profile build artifacts exist.

## Reproduction

Target: Ubuntu 24.04, x86_64-unknown-linux-gnu, Python 3.12, Rust 1.100.0, LLVM 18 and Clang 18. Install the same LLVM packages used by OGM-30 CI (llvm-18-dev, llvm-18, clang-18, build-essential) and GNU /usr/bin/time.

From the repository root:

~~~bash
python -m unittest discover -s benchmarks/runtime-footprint -p "test_*.py" -v

python benchmarks/runtime-footprint/run.py \
  --iterations 1000 \
  --warmups 3 \
  --samples 101 \
  --revision "$(git rev-parse HEAD)" \
  --output /tmp/ogm29-footprint-report.json \
  --artifact-dir /tmp/ogm29-footprint-artifacts
~~~

The run first measures portable Python and native executable process startup with the same flushed-readiness-marker protocol (3 warmups and 101 launches for each tool). It then builds the pinned OGM-30 backend proof executable with cargo build --release --locked, starts the isolated portable proof worker, executes the native benchmark binary under GNU time, and emits a JSON report plus .ogmb and native-object evidence. The CI job uploads the complete report, Cargo target output summary, timing report and proof artifacts. The tested source revision, full toolchain versions, CPU and target are recorded.

The native OGM-30 benchmark requires at least 101 measurement samples, so the unified runner enforces that minimum for both paths. Use a reduced iteration count for a faster local smoke run; accepted baseline evidence should retain the same workload/target and at least 101 samples. The report retains raw samples, p50/p95/p99, warm-up/sample counts and the exact revision.

## Interpretation and limitations

- .ogmb bytes are portable proof-container sizes. Native .o and .so bytes are target-specific fixture proof artifacts. Neither is the size of a production runtime executable.
- The native process RSS includes Cranelift and LLVM compilation/linking, mock Runtime setup and timed fixture workloads. It is not isolated AOT compiler peak memory or steady-state application RSS.
- The Python and native process startup distributions are readiness-marker measurements of proof tools, not production Ogma Runtime startup. Python worker ru_maxrss and tracemalloc are tool/prototype memory metrics, not Ogma VM heap or production RSS.
- Native AOT measurements use the existing exact-fixture adapters, mock C ABI and modeled Runtime boundary. They do not imply a full ECMAScript backend or production host ABI.
- LLVM package installed sizes cover only the explicitly listed packages, not the complete transitive operating-system deployment closure.
- No performance objective is selected by this task. The report establishes a reproducible baseline for later milestones and labels all unavailable production metrics.

The full ogma.runtime-footprint-report/1.0.0 JSON and raw proof outputs are retained as the downloadable CI artifact for the exact measured source revision. A compact, machine-readable benchmark summary is committed under results/ogm29-footprint-summary.json; the complete raw report is not duplicated in Git.


## Measured M0 baseline — 2026-11-19

The CI measurement completed successfully on source revision
\`a23fce801a71e910f3fce0c3804e75e1f70655f3\` (run #478). Target: Ubuntu 24.04,
\`x86_64-unknown-linux-gnu\`, AMD EPYC 9V45 96-Core Processor; Python 3.12.16,
Rust/Cargo 1.100.0, LLVM/Clang 18.1.3. The run used 3 warmups, 1,000
iterations per workload sample and 101 native measurement samples for each
proof-tool startup, compile and link distribution. Portable Core IR-to-bytecode
compilation and standalone verification each used 15 rounds.

| Measurement boundary | Portable interpreter proof | Hybrid/native proof |
|---|---:|---:|
| Startup (proof tools) | Python: p50 61.732 ms / p95 75.585 ms / p99 85.320 ms; 101 launches | Native benchmark executable: p50 10.179 ms / p95 13.790 ms / p99 15.879 ms; 101 launches |
| Peak process RSS | Python proof worker: 24,535,040 B (23.40 MiB) | Native compile/link/proof process: 80,384,000 B (76.66 MiB) |
| Compile memory | Bytecode compilation tracemalloc peak: 162,461 B | Covered by combined native process RSS; isolated per-backend peak not available |
| Core IR input memory | JSON fixture materialization peak: 95,635 B | Same canonical fixtures; no native frontend parser |
| Compile latency | Core IR fixture to register .ogmb, wall p50 1.371 ms / p95 1.881 ms (15 rounds) | Cranelift p50 0.288 ms / p95 0.470 ms; LLVM p50 3.739 ms / p95 4.395 ms (101 samples each) |
| Link latency | Not applicable to portable bytecode | Cranelift p50 53.364 ms / p95 62.528 ms; LLVM p50 53.860 ms / p95 65.364 ms (101 samples each) |
| Binary/object size | Five .ogmb containers: 1,482–2,426 B each | Benchmark tooling executable 10,022,656 B; Cranelift .o 2,944 B / .so 26,488 B; LLVM .o 3,112 B / .so 26,576 B |
| Host-call overhead proxy | 1,942.2 ns over direct Python callback; Python prototype only | Cranelift 163.3 ns (1.174× direct native helper); LLVM 184.3 ns (1.191×); exact-fixture mock ABI |

The portable and native paths consumed the same five Core IR v1 fixtures,
and all report acceptance assertions passed. Cross-language execution
throughput is not compared: the portable interpreter is a Python proof
implementation, while the native path is a Rust exact-fixture adapter with
mock ABI/runtime helpers.

The 80.4 MB native RSS includes both compiler backends, object linking,
mock Runtime setup and timed workloads. It is not a production hybrid
Runtime memory figure or an isolated AOT compiler peak. Likewise,
10,022,656 B is the native benchmark tooling executable, not a production
Runtime binary. The startup distributions are measured from process launch
to flushed readiness markers in the Python and native proof tools; they are
not Ogma Runtime startup. Python RSS is proof-worker memory, not VM RSS.

Actual production interpreter/hybrid startup, shipped executable sizes,
steady-state Runtime RSS, JavaScript AST peak memory, production Core IR
frontend memory and final shipped Cargo dependency graph remain
\`not_implemented\`. These unavailable values are null/status-labelled in
the machine-readable report rather than estimated.

The committed machine-readable summary is
[`results/ogm29-footprint-summary.json`](results/ogm29-footprint-summary.json).
It contains the measured p50/p95 values, memory and footprint boundaries,
profile closures, dependency-graph status, acceptance flags and the SHA-256
of the full raw report.

The full raw JSON (193,919 bytes, including all raw samples) is retained in
the successful GitHub Actions artifact
[`ogm29-interpreter-hybrid-footprint`](https://github.com/AiratGaliev/ogma/actions/runs/44538873406/artifacts/14537516721).
Raw report JSON SHA-256:
`a1e2521af8b3ecffd15c68aef09152015c5aca4e91a26c528cf7df102864c760`.
The artifact ZIP is 3,195,095 bytes; SHA-256:
`1a7634e1696b43020715f21e60887fe60af86a5fa6ad59bc2fdf50c0f6d8234a`.

The report's `source_revision` and `tested_revision` are both
`a23fce801a71e910f3fce0c3804e75e1f70655f3`. The subsequent commits update
only this measured summary and README; they do not change benchmark code.
