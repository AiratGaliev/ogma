# OGM-38 — Native Value and SSA evidence

**Status:** M0 native proof; production Runtime decision remains open until a real interpreter/Runtime and general Core IR compiler exist.

## Question

Revalidate the two independent OGM-17 axes using native execution evidence:

| Runtime Value | SSA | Candidate |
|---|---|---|
| Explicit tagged 16-byte value | Boxed | tagged-16-boxed |
| Explicit tagged 16-byte value | Representation-aware | tagged-16-representation-aware |
| 8-byte NaN-box word | Boxed | nanbox-8-boxed |
| 8-byte NaN-box word | Representation-aware | nanbox-8-representation-aware |

All four candidates are compiled from one Cranelift ObjectModule harness using the same numeric loop semantics from tests/core-ir/fixtures/positive-loop-carried-values.json. The C entry signature remains fixed; tagged values cross as pointers to a repr(C) two-word structure, while NaN-box values cross as 64-bit words. Boxed SSA keeps the encoded value in loop-carried SSA values and decodes at each numeric operation. Representation-aware SSA converts the numeric inputs once and carries f64 through the loop, re-encoding only at the Value boundary.

This is an exact-fixture native adapter, not a general Core IR compiler. The code path intentionally exercises the loop-carried representation distinction; it does not claim that JavaScript source is parsed or that a production Ogma Runtime executes the generated function.

## Measurement layers

- **Native compile/object/runtime:** Cranelift 0.137.2 host ObjectModule builds, links and executes all four candidates. The JSON report preserves the first compile observation, steady-state raw compile samples, object/shared-library sizes and raw native-call throughput samples (p50/p95/p99).
- **Physical allocation probe:** the pinned regalloc2 0.15.3 API used by Cranelift receives a register-liveness profile derived from the six parameter representations and operations in positive-call-boundary.json. It reports actual physical register assignments and spill slots under an explicit controlled budget (6 GPR, 6 FPR and 6 vector registers). This is an allocator evidence model, not an internal dump of Cranelift's private allocation for the generated loop and not a host-ISA register map.
- **Heap and roots:** each candidate uses the real OGM-26 GcHeap::MarkSweep, generation-checked GcRef, FrameKind::NativePrototype root frame and persistent HostHandle. The encoded reference payload is an opaque token into a registry containing the actual OGM-26 persistent handle; collection is forced while both frame and persistent roots are live, then the roots are released and the unreachable cycle must be reclaimed.
- **Value storage:** the report records the encoded Value width and OGM-26 logical heap/peak logical heap accounting. Logical heap bytes are not process RSS, allocator fragmentation or total process memory.

## Verification

The OGM-38 CI job verifies the full Core IR manifest and runs the OGM-17 semantic fixture tests before building this native proof. It then runs the OGM-26 GC tests, OGM-27 native-boundary tests, backend crate formatting/Clippy/tests and the four-candidate native benchmark. Raw samples and the resolved Cargo lock are uploaded as the ogm38-native-value-ssa-evidence artifact.

Reproduce the focused native runner on Ubuntu 24.04 with Rust 1.100.0, LLVM 18 development packages and Clang 18:

~~~sh
cargo fmt --manifest-path benchmarks/backend-codegen/Cargo.toml --all -- --check
cargo clippy --manifest-path benchmarks/backend-codegen/Cargo.toml --all-targets --all-features --locked -- -D warnings
cargo test --manifest-path benchmarks/backend-codegen/Cargo.toml --all-features --locked
cargo run --manifest-path benchmarks/backend-codegen/Cargo.toml --locked --release --bin ogm38-value-ssa -- \
  --iterations 1000 --warmups 3 --samples 101 \
  --revision "$(git rev-parse HEAD)" \
  --output /tmp/ogm38-native-value-ssa-report.json
~~~

## Semantic and architecture limits

The generated native workload is numeric-only. It verifies that the four candidates produce the same numeric loop result for equivalent inputs. The OGM-17 semantic fixture suite remains the source of coverage for NaN, signed zero, infinities, BigInt, undefined/null, strings, object identity and symbol identity; those cases are not claimed as native differential execution in OGM-38.

This proof does not provide native execution of BigInt arithmetic, string concatenation, object ToPrimitive, Symbol semantics, dynamic property access, general host calls or exceptions. It does not implement a JavaScript interpreter, parser, source-to-Core-IR lowering, production Value/object model, production GC integration, machine stack-map emitter, write barriers or full Runtime. OGM-19's ABI and OGM-27's shadow-frame proof remain explicit M0 models; the OGM-26 GC check uses the actual existing prototype heap API, not a shipped Runtime heap.

Process RSS and allocator-specific peak memory are not measured. The register-pressure probe uses a controlled MachineEnv; it must not be confused with a physical register map for the runner's host CPU or a direct count of spills in the generated shared library. The report retains those boundaries alongside the raw values.

No production Runtime ABI or NaN-boxing decision is made solely from these M0 measurements. The final Value ABI and internal SSA decision must still account for the real Rust Runtime object model, roots/write barriers, helper calls, general dynamic semantics, interpreter/AOT equivalence, target architectures and production workload profiles.
