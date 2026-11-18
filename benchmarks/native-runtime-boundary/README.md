# OGM-27 — native Runtime boundary proof

This is a dependency-free M0 model of the cross-backend root and execution-boundary contract. It is not a JavaScript Runtime, interpreter, AOT compiler or production GC.

## Covered

- non-moving mark/sweep root enumeration across interpreter locals, operand stack, closure captures, native shadow-frame maps and persistent host handles;
- exact per-safepoint native root-map validation and strict LIFO frame lifecycle;
- Native → Interpreter → Host callback → reentrant Interpreter activation transitions with pending completion roots, managed caller-local roots and host-result promotion verified against the matching persistent handle;
- explicit Normal/Return/Break/Continue/Throw completion propagation, captured throw activation traces and source/closure context;
- a shared deterministic budget/cancellation meter with bounded poll windows and maximum work-unit overshoot; chunked built-ins respect the stricter execution-level window;
- explicit RegExp, comparator sort, JSON and string interrupt policies.

## Reproduce

Using the repository-pinned Rust 1.100.0 toolchain:

```sh
cargo fmt --manifest-path benchmarks/native-runtime-boundary/Cargo.toml --all -- --check
cargo clippy --manifest-path benchmarks/native-runtime-boundary/Cargo.toml --all-targets --all-features --locked -- -D warnings
cargo test --manifest-path benchmarks/native-runtime-boundary/Cargo.toml --all-features --locked
```

The CI job runs these focused checks on the OGM-27 topic PR. No full Test262/E2E run is added to feature CI.

See [the OGM-27 boundary contract](../../docs/native-runtime-boundary-contract.md) for decisions, invariants, policy limits, security caveats, acceptance evidence and explicit non-goals.
