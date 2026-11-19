# OGM-31 — M0 dual-backend execution proof

This directory contains a reproducible, fixture-bounded architecture proof. It is not a production JavaScript/TypeScript compiler or execution Runtime.

## What is compared

The portable and native paths consume the same five canonical ogma.core-ir/1.0.0 fixture files. Their paths are listed in manifest.json and in the existing native backend workload manifest.

| Subset | Core IR fixture | Observable behaviour |
|---|---|---|
| Branch / strict equality | positive-ssa-branch.json | equal and unequal numeric values |
| Loop-carried values | positive-loop-carried-values.json | loop CFG, dynamic addition and js.to_number |
| Dynamic completion | positive-dynamic-exception.json | normal numeric result and propagated TypeError |
| Call boundary | positive-call-boundary.json | string/number addition, receiver, indirect host callback |
| Host capability | positive-host-call.json | benchmark.host.echo result and host trace |

The matching JavaScript files under sources/ are readable source references and source-map inputs for the fixtures. The Core IR documents are pre-authored canonical fixtures: this repository does not yet have a source parser or source-to-Core-IR compiler, so this task does not claim that these files are parsed or compiled from JavaScript text.

### Portable path

The existing OGM-16 prototype is extended with js.to_number and a narrow, explicit rule for the dynamic-exception fixture. It creates versioned .ogmb containers, decodes the code, and runs the repository ContainerReader and ProgramVerifier before execution. The proof repeats verification at the execution boundary.

Both stack and register encodings are executed. The selected portable baseline in this report is the register interpreter; stack/register observations must agree.

The exceptional edge in the canonical dynamic-exception fixture targets a handler whose only instruction rethrows the original exception value and whose terminator is unreachable/after_throw. For this one shape the portable prototype models the edge as direct exception propagation, which preserves the observable completion. It rejects exception handlers with additional instructions or effects. This is not general exception-table, catch, or finally lowering.

### Native path

The existing OGM-30 exact-fixture adapter emits and links both Cranelift and LLVM native objects using the same Core IR fixture documents and the pinned lowering profiles. The proof runner stores the resulting target-specific .o objects separately from portable .ogmb containers. The documented CI target is Ubuntu 24.04 / x86_64-unknown-linux-gnu.

The native backend remains an exact-fixture adapter, not a general Core IR compiler. Native dynamic-add and host-capability calls go through the existing OGM-19 versioned mock C ABI: the runner negotiates ABI v1/features, registers Rust callbacks, creates opaque mock-function handles, persists those handles, and passes a C-compatible function table into the generated object. The C adapter marshals numeric arguments into opaque handles, opens a root scope, calls the registered function through `ogma_context_call`, validates the explicit `OgmaCompletion` kind/value and closes the scope. Callback code forces collection while the ABI's borrowed argument roots are active, then verifies that every argument remains readable. Both Normal and Throw completions are exercised for dynamic addition and host echo. This proves the OGM-19 contract is actually used by generated code; it is still a mock ABI/value store rather than the production Ogma Runtime.

The OGM-19 mock value store supports Number, Boolean and Function values only. The native representation of the portable Symbol-to-number error uses the reserved `i64::MIN` numeric sentinel to request a Throw completion; it is not a production JS Symbol value. The call-boundary fixture also remains an exact-fixture C shim for its string/object receiver arguments because those types are not represented by this mock ABI. Its receiver, string argument and return value are compared with the portable interpreter.

Cranelift and LLVM generated objects now import and call five modeled OGM-27 safepoint sites: function entry, loop back-edge, allocation boundary, call boundary and host-call boundary. Each callback updates the active native shadow frame's root-map ID, polls the execution budget and runs the OGM-27 root-provider collector. A live managed sentinel must survive every collection. During real OGM-19 callbacks, the OGM-27 activation model exercises the Native → Interpreter → HostCallback transition, completion propagation, pending/live roots and promotion to persistent handles before release. This is a composition proof of the existing OGM-27 model with generated calls, not a production machine-register stack-map encoder or production collector.

### Unsupported operations and fallback

The native profile manifest deliberately does not include dynamic property access. The OGM-16 property-access workload uses js.get_property and js.set_property; the proof routes it to the verified portable register interpreter and checks that its property mutation and record.create host trace match the expected result. No native code is emitted for that workload. Literal js.constant nodes are compile-time constant-pool materialization, not runtime operations; they are excluded from the unsupported-operation calculation just as they are from the OGM-30 native operation lists. The proof validates their side-effect-free literal shape before classifying the remaining runtime operations.

The negative proof mutates a byte in a valid .ogmb code section and requires the container reader to reject it by the code digest. AOT support is closed over the native workload manifest; adding an unsupported operation to a profile does not make it silently executable.

## Run

The workflow uses Python 3.12, Rust 1.100.0, LLVM 18, Clang 18 and the pinned Rust dependency graphs for the backend codegen, OGM-19 ABI and OGM-27 native runtime-boundary proof crates. CI creates each crate lockfile if it is absent, then all build/test commands use `--locked`.

From the repository root, after installing the same toolchain and LLVM packages:

    for crate in rust-abi native-runtime-boundary backend-codegen; do
      test -f "benchmarks/$crate/Cargo.lock" || cargo generate-lockfile --manifest-path "benchmarks/$crate/Cargo.toml"
    done
    python -m unittest discover -s benchmarks/dual-backend -p "test_*.py" -v
    for crate in rust-abi native-runtime-boundary backend-codegen; do
      cargo fmt --manifest-path "benchmarks/$crate/Cargo.toml" --all -- --check
      cargo clippy --manifest-path "benchmarks/$crate/Cargo.toml" --all-targets --all-features --locked -- -D warnings
      cargo test --manifest-path "benchmarks/$crate/Cargo.toml" --all-features --locked
    done
    python benchmarks/dual-backend/run.py --output /tmp/ogm31-dual-backend-report.json --artifact-dir /tmp/ogm31-artifacts

The runner compares every returned/raised value and host trace across the portable register interpreter, portable stack interpreter, Cranelift AOT and LLVM AOT; checks runtime-helper counts and ELF object headers; validates the OGM-19 ABI version/features, callback argument rooting, explicit Normal/Throw completions and cleanup; and validates OGM-27 generated safepoint counts, active shadow-frame roots, activation/completion transitions, persistent promotion and execution-budget polls. It also tests the unsupported-operation fallback and invalid-bytecode rejection, then emits the machine-readable report. Portable .ogmb, native .o and all three generated Cargo.lock evidence files are stored as CI artifacts.

## Explicit M0 exclusions

- No full ECMAScript or TypeScript parser, source compiler, module loader, or general-purpose Core IR lowering.
- No production JS Value representation, object model, property semantics, Proxy/accessor/prototype handling or general dynamic-call machinery.
- No general exception regions, nested catch/finally, cancellation or suspension in this subset.
- No production Runtime implementation or general-purpose ABI table for arbitrary Core IR host calls. The native fixture subset uses the existing OGM-19 mock ABI and a test C adapter; strings/objects in the call-boundary fixture still use an exact-fixture callback shim.
- No production JS `Value`/Symbol representation. The ABI mock carries only Number/Boolean/Function; the native Symbol-conversion throw is represented by the documented `i64::MIN` test sentinel.
- No machine-register liveness analysis or production stack-map emission. The generated objects call the OGM-27 model at explicit, fixture-profile safepoints; its single managed shadow-frame slot is a synthetic proof root. This is not a production collector or compiler-generated per-value stack map.
- No cross-compilation or ABI compatibility claim for other targets. Target lookup in OGM-30 is not a cross-link proof.
- No Test262 claim and no full engine/browser runtime acceptance.

A passing report demonstrates only the stated M0 fixture subset on the documented target.
