# OGM-30 — Cranelift ObjectModule vs LLVM

This is an M0 backend-selection benchmark. It consumes the project's canonical
`ogma.core-ir/1.0.0` positive fixtures rather than inventing a second IR schema.
The backend recommendation and its re-evaluation criteria are recorded in
[decision-record.md](decision-record.md). The workload manifest points to exact fixtures under `tests/core-ir/fixtures`.
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
- The original OGM-31 helper fixtures remain deterministic stubs, not the production
  Ogma Runtime. Their string/number `js.add` path is a narrow specialization.
- OGM-32 adds a separate, versioned C-compatible AOT host-function table over the
  existing OGM-21 `HostFunctionRegistry`. Generated Cranelift and LLVM functions call
  native C dispatch helpers; those helpers validate ABI/table/descriptor versions and
  choose the registered typed thunk or generic fallback. Native code receives only
  fixed-layout metadata, opaque OGM handles, C function pointers and host-owned
  `user_data`; it does not depend on Rust's default ABI or expose internal Runtime
  layouts.
- The OGM-32 proof compares typed, generic, opaque-handle and Throw results with the
  interpreter-side OGM-21 dispatch path. It also exercises re-entrant callbacks,
  root preservation during OGM-27 safepoints, capability denial, signature mismatch,
  cancellation and incompatible ABI versions. The generic callback forces an
  OGM-19 unrooted collection while callback argument handles are borrowed.
- This remains an M0 integration proof, not a production JavaScript/TypeScript Runtime:
  the C helper adapts its fixed numeric benchmark result to an `int64_t` return with a
  separate completion/status snapshot; it is not a general ECMAScript Value ABI,
  production stack-map encoder or collector. OGM-27's modeled shadow-frame contract
  is still distinct from production machine-code stack maps and GC.

## OGM-32 AOT host-call ABI

The focused OGM-31/32 CI job builds the OGM-32 table and its C layout assertions
alongside the OGM-19 ABI, OGM-20 execution scopes, OGM-21 typed dispatch and
OGM-27 runtime-boundary crates. It then executes the same host-call cases through
both native backends and validates the emitted JSON evidence. The AOT table is an
adapter over OGM-21's registered slots, not a second host-function registry.

The public C contract is defined in `include/ogma_aot_host_calls.h`; Rust's
`repr(C)` mirror and dispatch adapters are in `src/aot_host_abi.rs`. The ABI is
major/minor versioned, carries structure sizes and descriptor versions, bounds table
and argument counts, rejects unsupported suspension and unknown effects, and keeps
internal registry slots behind opaque host-owned bindings. The C helper is linked
only into the M0 proof harness.

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
and target features, first-invocation compile/link timings (single diagnostic samples;
the backend order is sequential, so they are not used as steady-state comparisons),
three discarded compile/link warmups, and 101-sample steady-state compile/link
distributions with raw samples,
object/shared-library sizes, per-fixture p50/p95/p99 and throughput, direct helper
baselines, target lookup, installed size for the explicitly listed LLVM/Clang packages
(not the full transitive apt dependency closure or runtime deployment size), Cargo
source size and recursively scanned native build artifacts (including the explicitly
configured Cargo target directory, matched filenames, file counts and byte totals), and
integration notes. Target lookup is not a cross-link or target ABI
proof. Peak compiler RSS, cross-target linking and deployment-package measurements
are not collected by this M0 harness and must not be inferred from the reported
metrics. Results are specific to the recorded runner.

## Committed evidence

The raw machine-readable report is committed as
`results/ogm30-report.json.gz` (gzip-compressed JSON, including all measurement samples).
Inspect it with `gzip -dc benchmarks/backend-codegen/results/ogm30-report.json.gz`.
The report's `tested_revision` identifies the exact source revision measured; the later
commit that adds the evidence file and decision record does not alter that measured code.
The CI artifact also retains the resolved `Cargo.lock` used for the run.

## Decision policy

The [M0 decision record](decision-record.md) selects the current primary backend,
records the measured trade-offs and preserves explicit fallback/re-evaluation triggers.
The benchmark does not itself implement a production compiler or Runtime.
