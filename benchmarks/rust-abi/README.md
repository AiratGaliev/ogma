# Versioned Rust ABI proof (OGM-19)

This directory is a scoped M0 engineering proof of the Rust embedding boundary. It is deliberately a standalone Rust crate with no third-party dependencies; it is not the production Ogma workspace, a stable published ABI, or a JavaScript interpreter.

## What is exercised

- C-compatible fixed-width version descriptor and major/minor/feature negotiation.
- Opaque 40-byte tokens for context, value, root-scope, persistent-root and callback identities. Tokens contain no exposed Rust object, GC-cell or compiler layout and are not pointers.
- Runtime and context lifecycle with owner-thread checks, independent Runtime identities and generation invalidation.
- Explicit LIFO root scopes, host-owned persistent roots, collection of unrooted mock values and persistent promotion from a borrowed callback argument.
- Checked f64/boolean and f64-to-i32 conversions, including type and range failures.
- Bidirectional embedding boundary prototype: host calls a mock callable, which enters a registered Rust callback and returns an explicit Normal/Throw completion.
- Callback registration/unregistration, scoped arguments, nested ABI re-entry, deterministic recursion limit and panic containment.

The callable is a test fixture that dispatches directly to the registered host callback. No JS source is parsed or executed. This makes the callback and ownership contract testable before the Runtime exists, but it does not prove ECMAScript call semantics or production GC behaviour.

## Build and test

From the repository root, using Rust 1.100.0 (the version pinned in the M0 CI proof):

    cargo fmt --manifest-path benchmarks/rust-abi/Cargo.toml --all -- --check
    cargo clippy --manifest-path benchmarks/rust-abi/Cargo.toml --all-targets --all-features --locked -- -D warnings
    cargo test --manifest-path benchmarks/rust-abi/Cargo.toml --all-features --locked

The crate uses only the Rust standard library. The C header is benchmarks/rust-abi/include/ogma_abi.h; `tests/c_abi_smoke.c` is a real C11 consumer that checks the fixed layouts, ABI negotiation, Runtime/Context lifecycle, roots, callback dispatch and stale-handle behaviour. The Fast CI job builds the shared library, compiles this consumer with warnings-as-errors and executes it.

## Reproduce the measurements

    cargo run --manifest-path benchmarks/rust-abi/Cargo.toml --locked --release --bin ogma-abi-bench -- --iterations 10000 --warmups 3 --samples 101 --revision <source-sha> --tested-revision <tested-sha> --output ogma-abi-report.json

The JSON report records source and tested revisions separately, rustc/OS/architecture/profile, iteration/warmup/sample configuration, the negotiated ABI and min/median/p95/p99/max nanoseconds per call. It uses nearest-rank percentiles over per-sample mean latency and requires at least 101 measurement samples. Workloads cover primitive f64 dispatch, checked handle conversion, a 64-handle bulk read and a host-call/callback roundtrip.

The benchmark has no timing pass/fail threshold. Numbers are host- and toolchain-specific measurements of this mock proof, not production engine performance. Compare only reports with equivalent workloads and configurations.

## Contract and limits

The detailed proof contract and acceptance map are in the Rust ABI contract (../../docs/rust-abi-contract.md). The authoritative architecture remains the Ogma Notion specification, sections 0.5 and 8. This proof does not replace or independently redefine that specification.

Explicitly out of scope: production Runtime/Context implementation, real JS evaluation, GC algorithm selection, typed host-function tables, HostExecutionScope performance comparison, JS buffer/zero-copy interop, frame snapshots/batch host APIs, async Future/Waker bridge, bindgen and TypeScript declarations. These belong to the Runtime implementation or separately tracked M0 issues.
