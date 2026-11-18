# OGM-30 — Cranelift ObjectModule vs LLVM

This is an M0 backend-selection benchmark, not a production Ogma compiler or Runtime.
Both lowerers read the same versioned benchmark Core IR subset manifest in
`fixtures/workloads.json`. The manifest supplies the function names, parameter arity and semantic effect annotations to both lowerers; each backend maps the same workload kinds to its own IR instructions. The subset is deliberately explicit: it covers integer
arithmetic, a real CFG branch, an internal function call, dynamic-operation helper
dispatch, completion/exception helper dispatch, and a host-call boundary. Dynamic
operations and completions are represented by explicit helpers; the proof does not
claim full ECMAScript value semantics.

## Backends

- **Cranelift:** `cranelift-object::ObjectModule`, native host ISA, speed optimization.
- **LLVM:** LLVM 18.1 through Inkwell, native target machine, default optimization level.
- Both emit native object files with the same exported C ABI signatures. A small C
  shim supplies deterministic helper implementations and is linked into each shared
  object. No Rust default ABI or Ogma Runtime layout is exposed.
- Runtime measurements load the emitted shared object and execute the generated
  functions. Compilation, linking, object/shared-library size and helper-call
  throughput are reported separately.

## Reproduction

The CI job pins Rust 1.100.0, Cranelift 0.137.1, Inkwell 0.11.0 and LLVM 18.1 (Inkwell dynamic LLVM linking).
On Ubuntu 24.04 with `llvm-18-dev`, `clang-18` and `cc` installed:

```sh
cargo fmt --manifest-path benchmarks/backend-codegen/Cargo.toml --all -- --check
cargo clippy --manifest-path benchmarks/backend-codegen/Cargo.toml --all-targets --all-features --locked -- -D warnings
cargo test --manifest-path benchmarks/backend-codegen/Cargo.toml --all-features --locked
cargo run --manifest-path benchmarks/backend-codegen/Cargo.toml --locked --release -- \
  --iterations 50000 --warmups 3 --samples 101 \
  --revision "$(git rev-parse HEAD)" --output /tmp/ogm30-report.json
```

The CI evidence contains the raw JSON report, generated object files, dependency
lockfile, toolchain versions and exact source/tested revisions. Results are
measurements from the named runner and must not be generalized to other targets.

## Metrics and interpretation

The harness records compile-time distributions, link time, object and linked image
sizes, per-workload p50/p95/p99 and throughput, helper-boundary call costs, target
lookup coverage, LLVM package footprint, and dependency/integration notes. Target
lookup is not a cross-link or ABI compatibility proof. Toolchain footprint is
reported as installed LLVM package size; Cranelift is a Rust dependency set and its
Cargo registry/build footprint is reported separately. Integration complexity is
qualitative and tied to the actual prototype rather than a synthetic score.

The C helpers model a narrow ABI boundary only. They do not prove production Ogma
host-call semantics, GC root maps, stack maps, callbacks, or interpreter/AOT
equivalence. Those remain governed by OGM-27 and OGM-32. External compiler benchmarks
are contextual references only; the backend decision is based on these Ogma-specific
measurements.

## Decision policy

Do not select a primary backend from a single arithmetic score. The decision must
weigh steady-state runtime, compilation latency, emitted size, target availability,
toolchain/deployment burden, helper-call overhead, and the cost of integrating the
backend with the shared Core IR and OGM-27 root/safepoint contract. Keep fallback or
re-evaluation triggers explicit. A benchmark result is not a production Runtime
implementation.
