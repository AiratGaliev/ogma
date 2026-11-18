# OGM-22 — buffer interoperability proof

This standalone M0 crate is a semantic and safety harness, not a production Ogma interpreter. It models ownership transitions, escape/invalidation rules and the scoped Rust borrow boundary required by Notion architecture section 8.9.3.

Contents:
- src/lib.rs: aligned allocation, owned transfer, external owner, escaped TypedArray lifetime, scoped Rust borrows, revocable lease, detach/resize and deterministic finalizer probe.
- src/bin/benchmark.rs: copy versus scoped zero-copy checksum benchmark at 64 B, 4 KiB and 1 MiB.
- docs/buffer-interop-contract.md: ownership modes, safety invariants, acceptance evidence and proof limitations.

Run from repository root:
cargo fmt --manifest-path benchmarks/buffer-interop/Cargo.toml --all -- --check
cargo clippy --manifest-path benchmarks/buffer-interop/Cargo.toml --all-targets --all-features --locked -- -D warnings
cargo test --manifest-path benchmarks/buffer-interop/Cargo.toml --all-features --locked
cargo run --manifest-path benchmarks/buffer-interop/Cargo.toml --locked --release --bin ogma-buffer-interop-bench -- --iterations 2000 --warmups 3 --samples 101 --revision <source-sha> --tested-revision <tested-sha> --cpu <cpu-model> --output /tmp/ogm-22-buffer-interop-report.json

The task-specific CI job executes these checks and uploads JSON evidence. No machine-independent performance threshold is used.

Limitations: finalizer counting is deterministic Rust drop behavior, not tracing GC; detach/resize are checked by the model Runtime, not real JS intrinsics; the benchmark excludes JS dispatch, production allocator/GC integration and consumer workloads. Zero-copy remains opt-in until a real Runtime proves correctness and end-to-end benefit.
