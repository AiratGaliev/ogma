# OGM-30 — Cranelift ObjectModule vs LLVM

This is an M0 backend-selection benchmark. It consumes the project's canonical
`ogma.core-ir/1.0.0` positive fixtures rather than inventing a second IR schema.
The workload manifest points to exact fixtures under `tests/core-ir/fixtures`.
Before lowering, the harness validates each fixture's exact block order, instruction
order and operands, result identifiers, branch/loop edges, exception edge and host
capability symbol. The lowerers are fixture-profile adapters, not a general-purpose
Core IR compiler; a fixture drift fails validation rather than silently benchmarking
a stale handwritten lowering.

## Canonical Core IR workloads

| Workload | Canonical fixture | Measured behavior |
|---|---|---|
| SSA branch | `positive-ssa-branch.json` | `js.strict_equal`, branch and return |
| Loop-carried values | `positive-loop-carried-values.json` | loop CFG, `js.strict_equal`, `js.add`, `js.to_number` |
| Dynamic exception | `positive-dynamic-exception.json` | `js.add`, explicit exception edge and `js.throw` completion |
| Call boundary | `positive-call-boundary.json` | string/number `js.add` and indirect `js.call`; the fixture's unused pure equality result may be eliminated by either optimizer |
| Host capability | `positive-host-call.json` | `host.call` with stable symbol `benchmark.host.echo` |

The host-call fixture is also added to the canonical Core IR fixture manifest and is
validated by the project's existing Core IR Fast checks.

## Backend prototypes

- **Cranelift:** `cranelift-object::ObjectModule`, native host ISA, speed optimization.
  `cranelift_native::builder()` detects the host ISA and native feature flags.
- **LLVM:** LLVM 18.1 through Inkwell, native target machine, default optimization.
  It receives the same host CPU name and host feature set reported by LLVM, so the
  comparison does not pit Cranelift host-specific code against LLVM's generic target.
- Both emit PIC object files with matching exported C ABI signatures and link the
  same C helper shim. Generated functions are loaded and exercised through their native
  symbols; semantic checks run before timing.
- The loop fixture lowers numeric `js.add` to a boxed helper result followed by
  `js.to_number`. The call-boundary fixture exercises string-plus-number concatenation
  (the C helper checks the exact expected string) and an indirect function-reference
  call. Its `js.strict_equal` result is unused and can be dead-code-eliminated, so its
  runtime cost is not attributed to that workload; observable strict equality is
  measured in the branch/loop workloads. The dynamic-exception fixture lowers the
  explicit exception edge to a tagged completion and exercises `js.throw` propagation.
- Helpers are deterministic benchmark stubs, not the production Ogma Runtime. They
  exercise helper dispatch and call overhead but do not implement full ECMAScript
  coercion, allocation, GC roots, stack maps, safepoints, re-entrancy or host ABI
  ownership. The string/number `js.add` path is a narrow specialization. OGM-27's
  root/safepoint contract and OGM-32's AOT host-call proof remain separate production
  integration requirements.

## Reproduction

The focused CI job pins Rust 1.100.0, Cranelift 0.137.2, Inkwell 0.11.0 and LLVM 18.1.
On Ubuntu 24.04 with `llvm-18-dev`, `clang-18` and `build-essential`:

```sh
cargo fmt --manifest-path benchmarks/backend-codegen/Cargo.toml --all -- --check
cargo clippy --manifest-path benchmarks/backend-codegen/Cargo.toml --all-targets --all-features --locked -- -D warnings
cargo test --manifest-path benchmarks/backend-codegen/Cargo.toml --all-features --locked
cargo run --manifest-path benchmarks/backend-codegen/Cargo.toml --locked --release -- \
  --iterations 50000 --warmups 3 --samples 101 \
  --revision "$(git rev-parse HEAD)" --output /tmp/ogm30-report.json
```

The report records source/tested revisions, Rust/LLVM/Clang and OS versions, CPU model
and target features, cold first-invocation compile/link latency, three discarded compile/link
warmups, and 101-sample steady-state compile/link distributions with raw samples,
object/shared-library sizes, per-fixture p50/p95/p99 and throughput, direct helper
baselines, target lookup, installed size for each selected LLVM toolchain package, Cargo
source size and recursively scanned native build artifacts (including the explicitly
configured Cargo target directory, matched filenames, file counts and byte totals), and
integration notes. Target lookup is not a cross-link or target ABI
proof. Peak compiler RSS, cross-target linking and deployment-package measurements
are not collected by this M0 harness and must not be inferred from the reported
metrics. Results are specific to the recorded runner.

## Decision policy

Use the committed M0 report as the project-specific evidence; do not substitute
external compiler benchmarks. Do not choose a backend from one arithmetic score.
Weigh steady-state runtime, compile latency, emitted size, target availability,
helper-call overhead, toolchain/deployment burden, and the cost of integrating with
the shared Core IR and OGM-27 root/safepoint contract. The M0 decision record must
state the chosen backend (or explain why the data are inconclusive), the scope of
that choice, trade-offs, fallback path and explicit re-evaluation triggers. This
benchmark does not itself implement a production compiler or Runtime.
