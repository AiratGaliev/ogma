# OGM-29 — interpreter and hybrid-AOT footprint baseline

This directory contains the OGM-29 M0 footprint harness. It uses the same five canonical ogma.core-ir/1.0.0 fixtures already consumed by OGM-31's portable interpreter and OGM-30's native Cranelift/LLVM fixture adapters.

The harness is deliberately explicit about proof versus production measurements. The repository still has no production Rust Runtime, source parser, root Cargo workspace or profile-linked application executable. Therefore the report includes measured proof artifacts and process/toolchain costs, while true production-runtime startup, binary size and RSS remain not_implemented. It does not fill those gaps with estimates.

## What is measured

| Area | Portable interpreter proof | Hybrid/native proof |
|---|---|---|
| Workload | Five shared Core IR fixtures; register bytecode plus stack/register differential check | Same five fixture paths from the OGM-30 native workload manifest |
| Startup | Parent-to-ready latency of the isolated Python proof worker | Native benchmark process total wall time; isolated startup latency is explicitly unavailable |
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

The run builds the pinned OGM-30 backend proof executable with cargo build --release --locked, starts the isolated portable proof worker, executes the native benchmark binary under GNU time, and emits a JSON report plus .ogmb and native-object evidence. The CI job uploads the complete report, Cargo target output summary, timing report and proof artifacts. The tested source revision, full toolchain versions, CPU and target are recorded.

The native OGM-30 benchmark requires at least 101 measurement samples, so the unified runner enforces that minimum for both paths. Use a reduced iteration count for a faster local smoke run; accepted baseline evidence should retain the same workload/target and at least 101 samples. The report retains raw samples, p50/p95/p99, warm-up/sample counts and the exact revision.

## Interpretation and limitations

- .ogmb bytes are portable proof-container sizes. Native .o and .so bytes are target-specific fixture proof artifacts. Neither is the size of a production runtime executable.
- The native process RSS includes Cranelift and LLVM compilation/linking, mock Runtime setup and timed fixture workloads. It is not isolated AOT compiler peak memory or steady-state application RSS.
- The Python worker ru_maxrss and tracemalloc are tool/prototype memory metrics, not Ogma VM heap or production RSS.
- Native AOT measurements use the existing exact-fixture adapters, mock C ABI and modeled Runtime boundary. They do not imply a full ECMAScript backend or production host ABI.
- LLVM package installed sizes cover only the explicitly listed packages, not the complete transitive operating-system deployment closure.
- No performance objective is selected by this task. The report establishes a reproducible baseline for later milestones and labels all unavailable production metrics.

The machine-readable evidence is emitted as ogma.runtime-footprint-report/1.0.0; the CI artifact is retained with the PR run. The report is also intended to be committed under benchmarks/runtime-footprint/results/ after the CI measurement completes for the exact reviewed source revision.
