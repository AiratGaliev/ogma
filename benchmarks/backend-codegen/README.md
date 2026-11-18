# OGM-30 — Cranelift ObjectModule vs LLVM

This is an M0 backend-selection benchmark. It consumes the project's canonical
`ogma.core-ir/1.0.0` positive fixtures rather than inventing a second IR schema.
The workload manifest points to exact fixtures under `tests/core-ir/fixtures`;
the Rust harness parses and checks their contract version, operations, CFG shape,
parameter representations, exception edge and host capability symbol. Both backend lowerers receive the same parsed corpus and use the fixture identity to name their
generated functions. The current M0 adapter is deliberately fixture-specific: it
lowers the selected operation/control-flow patterns into equivalent native functions,
not arbitrary Core IR programs. It is not the OGM-31 production interpreter/AOT
dual-consumer proof.

## Canonical Core IR workloads

| Workload | Canonical fixture | Measured behavior |
|---|---|---|
| SSA branch | `positive-ssa-branch.json` | `js.strict_equal`, branch and return |
| Loop-carried values | `positive-loop-carried-values.json` | loop CFG, `js.strict_equal`, `js.add`, `js.to_number` |
| Dynamic exception | `positive-dynamic-exception.json` | `js.add`, explicit exception edge and `js.throw` completion |
| Call boundary | `positive-call-boundary.json` | `js.strict_equal`, `js.add`, `js.call` |
| Host capability | `positive-host-call.json` | `host.call` with stable symbol `benchmark.host.echo` |

The host-call fixture is also added to the canonical Core IR fixture manifest and is
validated by the project's existing Core IR Fast checks.

## Backend prototypes

- **Cranelift:** `cranelift-object::ObjectModule`, native host ISA, speed optimization.
- **LLVM:** LLVM 18.1 through Inkwell, native target machine, default optimization.
- Both emit PIC object files using the same exported C ABI signatures and link the
  same helper shim. Generated functions are loaded and exercised through their native
  symbols; semantic checks run before timing.
- The loop fixture lowers numeric `js.add` to a boxed helper result followed by
  `js.to_number`. The call-boundary fixture exercises string-plus-number concatenation and an indirect function-reference call. The dynamic-exception fixture lowers the exception edge to an
  explicit tagged completion and exercises the `js.throw` propagation helper.
- Helpers are deterministic benchmark stubs, not the production Ogma Runtime. They
  exercise helper dispatch and call overhead but do not implement full ECMAScript
  coercion, allocation, GC roots, stack maps, safepoints, re-entrancy or host ABI
  ownership. In particular, the string/number `js.add` path is only a narrow
  specialization. OGM-27's root/safepoint contract and OGM-32's AOT host-call proof
  remain separate production integration requirements.

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

The report records source/tested revisions, Rust/LLVM/Clang and OS versions, CPU,
compile/link latency distributions and raw samples, object/shared-library sizes,
per-fixture p50/p95/p99 and throughput, direct helper baselines, target lookup,
LLVM package footprint, Cargo source/build artifact footprints and integration notes.
Target lookup is not a cross-link or target ABI proof. Measurements are specific to
the recorded runner and must not be generalized to other targets.

## Decision policy

Do not choose a primary backend from one arithmetic score. Weigh steady-state runtime,
compile latency, emitted size, target availability, helper-call overhead,
toolchain/deployment burden, and the cost of integrating with the shared Core IR and
OGM-27 root/safepoint contract. Record the measured trade-offs and explicit
re-evaluation triggers. This benchmark does not itself select or integrate a
production backend.
