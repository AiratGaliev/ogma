# OGM-21 — typed host dispatch proof

This standalone Rust crate compares dynamic name lookup, pre-resolved function-table
slots, fixed-signature typed thunks and the generic callback fallback. It reuses the
OGM-19 versioned mock ABI and OGM-20 owner-thread HostExecutionScope; it does not
embed the production ECMAScript runtime.

## Contract and implementation

- The registration descriptor and C-compatible slot layout are specified in
  [docs/typed-host-dispatch-contract.md](../../docs/typed-host-dispatch-contract.md).
- src/lib.rs implements ABI negotiation, descriptor validation, a thread-affine
  function table, slot generations, capability checks, typed dispatch, generic fallback,
  and an ordinary-object/Proxy call-site guard proof.
- include/ogma_host_dispatch.h is the C11 layout contract for descriptors and slots.
- src/bin/benchmark.rs contains representative generated-style typed thunks and a
  reproducible benchmark harness. It compares dynamic property/name lookup, pre-resolved
  slots, typed primitive/handle calls, generic fallback and guarded call-site hits.
- tests/c_abi_layout.c checks the C layout independently of Rust.

Typed thunks are fixed extern "C-unwind" function pointers using the negotiated ABI
major/minor. They never use the default Rust ABI and never expose runtime/object layouts.
The generic OgmaHostCallback remains the semantic fallback through ogma_context_call.

## Local checks

Run from the repository root:

~~~sh
cargo fmt --manifest-path benchmarks/typed-host-dispatch/Cargo.toml --all -- --check
cargo clippy --manifest-path benchmarks/typed-host-dispatch/Cargo.toml --all-targets --all-features --locked -- -D warnings
cargo test --manifest-path benchmarks/typed-host-dispatch/Cargo.toml --all-features --locked
cc -std=c11 -Wall -Wextra -Werror -pedantic \
  -I benchmarks/typed-host-dispatch/include \
  -I benchmarks/rust-abi/include \
  benchmarks/typed-host-dispatch/tests/c_abi_layout.c -o /tmp/ogm21-abi-layout
/tmp/ogm21-abi-layout
cargo run --manifest-path benchmarks/typed-host-dispatch/Cargo.toml --locked --release --bin ogma-typed-dispatch-bench -- \
  --iterations 10000 --warmups 3 --samples 101 --output /tmp/ogm21-report.json
~~~

## Proof boundary

The included property model is intentionally narrow: it proves the guard decision for
ordinary function-valued properties and Proxy get traps. It is not a substitute for
interpreter-level ECMAScript tests. The OGM-19 ABI still stores mock Number/Boolean/
Function values; the OGM-21 crate does not claim a production Runtime integration,
full ECMAScript completion values, asynchronous host calls, or a complete binding
code generator. MAY_SUSPEND descriptors are rejected because the current ABI has no
suspension contract. Complex effects route through the generic callback path.

Generic benchmark cases periodically collect temporary OGM-19 mock values every 128
operations between timed chunks. Collection time is excluded from latency/throughput
samples; collection cycles are reported separately, while allocation counters include
collector allocations.

Host values that must survive explicit collection can be kept alive with the
registry's `root_handle` RAII guard.


## OGM-25 — Static Host Modules

src/static_modules.rs layers declarative host module registration, profile-aware TypeScript declaration generation and static import-to-slot resolution on top of this OGM-21 table. The contract and boundaries are documented in docs/static-host-modules-contract.md. JSON descriptor/profile schemas and checked-in fixtures live under contracts/host-modules/v1 and tests/fixtures.

The proof intentionally resolves only immutable named ESM host-function bindings to slots. Namespace/dynamic/CommonJS/property paths and mutable/dynamic exports remain generic. Profile planning computes transitive module/native dependencies and fails closed when a runtime or native reference would be stripped.
