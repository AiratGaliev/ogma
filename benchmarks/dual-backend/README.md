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

The native backend remains an exact-fixture adapter, not a general Core IR compiler. The additional C functions are deterministic test shims. Counters prove that arithmetic, conversion, completion/throw, indirect call and host-call helpers were actually reached. The call-boundary and host-capability traces are compared with the portable interpreter. These C shims are not the production Ogma Runtime and do not replace the versioned production host ABI contract.

### Unsupported operations and fallback

The native profile manifest deliberately does not include dynamic property access. The OGM-16 property-access workload uses js.get_property and js.set_property; the proof routes it to the verified portable register interpreter and checks that its property mutation and record.create host trace match the expected result. No native code is emitted for that workload. Literal js.constant nodes are compile-time constant-pool materialization, not runtime operations; they are excluded from the unsupported-operation calculation just as they are from the OGM-30 native operation lists. The proof validates their side-effect-free literal shape before classifying the remaining runtime operations.

The negative proof mutates a byte in a valid .ogmb code section and requires the container reader to reject it by the code digest. AOT support is closed over the native workload manifest; adding an unsupported operation to a profile does not make it silently executable.

## Run

The workflow uses Python 3.12, Rust 1.100.0, LLVM 18, Clang 18 and the existing pinned Rust crate dependencies. The CI job creates benchmarks/backend-codegen/Cargo.lock when it is absent and then uses --locked.

From the repository root, after installing the same toolchain and LLVM packages:

    test -f benchmarks/backend-codegen/Cargo.lock || cargo generate-lockfile --manifest-path benchmarks/backend-codegen/Cargo.toml
    python -m unittest discover -s benchmarks/dual-backend -p "test_*.py" -v
    cargo fmt --manifest-path benchmarks/backend-codegen/Cargo.toml --all -- --check
    cargo clippy --manifest-path benchmarks/backend-codegen/Cargo.toml --all-targets --all-features --locked -- -D warnings
    cargo test --manifest-path benchmarks/backend-codegen/Cargo.toml --all-features --locked
    python benchmarks/dual-backend/run.py --output /tmp/ogm31-dual-backend-report.json --artifact-dir /tmp/ogm31-artifacts

The runner compares every returned/raised value and host trace across the portable register interpreter, portable stack interpreter, Cranelift AOT and LLVM AOT; checks runtime-helper counts; tests the unsupported-operation fallback and invalid-bytecode rejection; then emits a JSON report. It also stores portable .ogmb and native .o evidence files under the artifact directory.

## Explicit M0 exclusions

- No full ECMAScript or TypeScript parser, source compiler, module loader, or general-purpose Core IR lowering.
- No production JS Value representation, object model, property semantics, Proxy/accessor/prototype handling or general dynamic-call machinery.
- No general exception regions, nested catch/finally, cancellation or suspension in this subset.
- No production Runtime linkage, versioned host function table, opaque-handle ownership implementation or panic-safe host integration; native C helpers are test shims.
- No generated-code GC stack maps, safepoint integration or production collector. OGM-27 remains a separate model proof; this task does not claim that its root protocol is wired into these objects.
- No cross-compilation or ABI compatibility claim for other targets. Target lookup in OGM-30 is not a cross-link proof.
- No Test262 claim and no full engine/browser runtime acceptance.

A passing report demonstrates only the stated M0 fixture subset on the documented target.
