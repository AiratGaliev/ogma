# OGM-30 — M0 native backend selection decision

**Status:** Decision for the M0 backend prototype; production AOT integration remains future work  
**Date:** 2026-11-18  
**Measured source revision:** `459305265372347befba90a1e3b3db2c0b4d7dfb`  
**CI run:** [PR #26 benchmark run](https://github.com/AiratGaliev/ogma/actions/runs/44362880422)  
**Raw evidence:** [`results/ogm30-report.json.gz`](results/ogm30-report.json.gz)

## Decision

Select **Cranelift ObjectModule as the primary M0 native code-generation candidate**. Keep LLVM 18/Inkwell as the alternate backend and comparative reference behind the same backend-neutral Core IR boundary.

This is a backend-selection decision based on the measured prototype. It does not claim that the production Ogma compiler or Runtime is implemented, that Cranelift is universally faster, or that this harness proves production ECMAScript semantics.

## Evidence and interpretation

The report was produced on Ubuntu 24.04, Linux x86_64, Intel Xeon 6975P-C, Rust 1.100.0 and LLVM 18.1.3. Both backends received the same five canonical `ogma.core-ir/1.0.0` fixture workloads. LLVM's target machine used the same host CPU name and feature set it detected; Cranelift used its native host ISA builder. The harness validated exact fixture profiles, operands, result IDs and CFG/exception edges before lowering.

Steady-state compile and link measurements use three discarded warmups and 101 samples. Runtime workloads use 50,000 calls per sample, three warmups and 101 samples. Percentiles use the nearest-rank method.

### Compiler and emitted-size results

| Metric | Cranelift | LLVM 18 | Interpretation |
|---|---:|---:|---|
| Compile p50 | 199,704 ns | 3,580,092 ns | Cranelift is about 17.9× faster |
| Compile p95 | 225,506 ns | 3,643,641 ns | Same direction as p50 |
| Compile p99 | 236,944 ns | 3,657,161 ns | Same direction as p50 |
| Link p50 | 23,060,382 ns | 22,985,812 ns | Effectively equal; LLVM is about 0.3% lower |
| Link p95 | 23,942,351 ns | 23,814,979 ns | Effectively equal |
| Object file | 1,632 B | 1,888 B | Cranelift output is about 13.6% smaller |
| Linked shared library | 16,224 B | 16,272 B | Effectively equal |

The first compiler/linker invocation is also recorded, but it is a single diagnostic observation per backend. The backends are invoked sequentially in one process, with Cranelift first; the first-link timings (413.8 ms for Cranelift and 27.3 ms for LLVM) are consequently not treated as a fair steady-state comparison. LLVM's first compile invocation also includes LLVM target initialization. The decision is based on the 101-sample steady-state measurements, not those single observations.

### Runtime workload results

Values below are median nanoseconds per generated function call. Lower is better.

| Workload | Cranelift | LLVM | Result |
|---|---:|---:|---|
| SSA strict-equality branch | 1.660 | 1.583 | LLVM ~4.6% lower |
| Loop-carried numeric add + to-number | 18.179 | 40.835 | Cranelift ~2.25× faster |
| Dynamic add, normal completion | 3.589 | 3.589 | Equal within measurement precision |
| Dynamic add, Throw completion | 4.102 | 4.358 | Cranelift ~5.9% lower |
| String/number add + indirect call boundary | 64.808 | 64.544 | LLVM ~0.4% lower |
| Host capability call | 2.310 | 2.564 | Cranelift ~9.9% lower |

Direct helper-call baselines are effectively equal because both generated backends call the same deterministic C helper shim. The call-boundary fixture contains a pure `js.strict_equal` result that is unused and may be eliminated by either optimizer; the measured call-boundary cost is attributed to string/number addition and indirect invocation, not to that equality operation. Observable strict equality is measured in the branch and loop workloads.

The runtime evidence is mixed rather than a universal Cranelift win. Cranelift is substantially faster on the loop workload and modestly faster on exception/host paths; LLVM is slightly faster on the branch and call-boundary workloads. Link latency is effectively the same. This is acceptable for selecting the M0 candidate because the strongest consistent difference is compile-time latency, while steady-state runtime is generally close and the emitted object is smaller.

### Build and toolchain footprint

| Metric | Cranelift | LLVM bindings / explicit packages |
|---|---:|---:|
| Cargo source directories measured | 9,648,102 B | 1,681,006 B |
| Matched native build artifacts | 32 files / 165,061,396 B | 5 files / 14,081,590 B |
| Explicit system-package installed size | — | 446,670 KiB (436.2 MiB) |

The Cranelift artifact scan covers files matching Cranelift crate names in the benchmark's release target directory, including Rust `.rlib`/`.rmeta` and build outputs. The LLVM-side scan covers Inkwell and llvm-sys artifacts; it does not include LLVM's system libraries. The 436.2 MiB subtotal is only for the three explicitly requested packages (`llvm-18-dev`, `llvm-18`, `clang-18`); it excludes their complete transitive package closure and is not a deployment-size measurement. These are build-environment indicators, not like-for-like final binary sizes.

Both backends resolved the target triples `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `x86_64-pc-windows-msvc` and `aarch64-apple-darwin`. This is target lookup only. No cross-target object emission, linker execution or ABI validation was performed for those non-host targets.

## Why Cranelift is the M0 primary candidate

1. Its steady-state compile median is about 17.9× lower than LLVM's on the same validated workload corpus.
2. Its emitted object is smaller, while linked library sizes are practically identical.
3. Runtime measurements do not reveal a broad LLVM advantage; most paths are close, and Cranelift leads on the loop and host/exception paths.
4. The Cranelift path uses Rust-native crates and the host ISA builder rather than requiring an LLVM 18 development toolchain in the build environment. The benchmark's package measurements are not a complete deployment-cost comparison, but LLVM clearly adds a separate native toolchain requirement.

The larger set of Cranelift Rust build artifacts is a real trade-off. It should be monitored when the production workspace and build profiles are integrated.

## Scope, fallback and re-evaluation

- **Scope:** use Cranelift as the default M0 native-codegen candidate for future backend integration planning. Keep the Core IR contract independent of Cranelift and LLVM; do not expose backend-specific types or semantics in the frontend contract.
- **Fallback:** retain the LLVM prototype and backend adapter as an alternate implementation/reference. Re-evaluate LLVM for a target or feature where Cranelift cannot meet required code-generation, object-format, ABI or optimization requirements.
- **OGM-31:** re-evaluate after both backends consume the same production-produced Core IR through a real execution path, rather than fixture-profile adapters.
- **OGM-27 / GC integration:** re-evaluate after native root maps, safepoints, stack maps, exception transitions and interpreter/AOT boundaries are implemented and measured together. This benchmark does not prove those contracts.
- **OGM-32 / host ABI:** re-evaluate after production host-call lowering and the real Rust Runtime ABI are integrated. The current C helpers are deterministic stubs and do not model production allocation, GC, re-entrancy, ownership or full ECMAScript coercion.
- **Conformance and realistic workloads:** repeat the comparison with representative JavaScript/TypeScript programs, closures, dynamic values, property access, exceptions, host callbacks and the actual helper/Runtime implementations.
- **Platform and packaging:** require real cross-compilation/linking/ABI checks on supported targets and measure release package size, runtime shared-library requirements, build time and peak compiler memory before making a final product/deployment decision.

## Acceptance boundary

This decision closes the scoped M0 comparison only. It does not mark the production AOT compiler, JavaScript Runtime, interpreter/native equivalence, complete ECMAScript conformance or cross-platform native deployment as implemented.
