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
- **Physical allocation probe:** the pinned regalloc2 0.15.3 API used by Cranelift receives a register-liveness profile derived from the six parameter representations and operations in positive-call-boundary.json. It reports physical register assignments, spill-slot count/bytes, and stack spill-store/reload move edits from `regalloc2::Output.edits` under an explicit controlled budget (6 GPR, 6 FPR and 6 vector registers). `spillslot_size` is expressed in logical 64-bit slots per value (Int/Float = 1, Vector = 2); reported spill bytes are logical slots × 8 on the measured x86_64 target. Store/reload edit counts are not counts of distinct spilled SSA values. This is an allocator evidence model, not an internal dump of Cranelift's private allocation for the generated loop and not a host-ISA register map.
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
## Measured M0 results
**Authoritative run:** [OGM-38 CI run #44569693384](https://github.com/AiratGaliev/ogma/actions/runs/44569693384)
**Measured source/tested revision:** `d9b76aae822c87ce66902fb460dcc3d588f7f700`
**Environment:** Ubuntu 24.04, Rust 1.100.0, Cranelift 0.137.2, regalloc2 0.15.3, x86_64-unknown-linux-gnu, AMD EPYC 9V74 80-Core Processor
**Sampling:** 3 warmups, 101 samples, 1,000 native calls per throughput sample.
| Candidate | Value bytes | Compile p50 (ns) | Native call p50 / p95 / p99 (ns) | Object bytes | Spill slots / logical bytes | Spill stores / reloads |
|---|---:|---:|---:|---:|---:|---:|
| tagged-16-boxed | 16 | 347,592 | 340.722 / 354.903 / 365.439 | 776 | 6 / 48 B | 6 / 6 |
| tagged-16-representation-aware | 16 | 334,072 | 128.189 / 138.894 / 143.351 | 768 | 0 / 0 B | 0 / 0 |
| nanbox-8-boxed | 8 | 308,685 | 337.126 / 348.604 / 353.781 | 704 | 0 / 0 B | 0 / 0 |
| nanbox-8-representation-aware | 8 | 292,902 | 141.308 / 149.680 / 265.231 | 696 | 0 / 0 B | 0 / 0 |
All four generated native functions returned the expected value `128`. The linked shared library was 14,856 bytes for each candidate.
### Interpretation
- Representation-aware SSA reduced median native-call latency by **62.4% (2.66×)** versus boxed SSA with the tagged layout, and by **58.1% (2.39×)** with the NaN-box layout. This is the result for the measured Number-only loop adapter, not a general ECMAScript workload.
- NaN-box reduced the generated object file by about **9.3%** versus the corresponding tagged candidate, but did not improve the representation-aware loop latency here: 141.308 ns versus 128.189 ns for tagged representation-aware (about 10.2% higher). These measurements do not justify selecting NaN-boxing for the production Value ABI.
- The controlled call-boundary allocator profile produced 6 logical spill slots (48 B) and six spill-store/six spill-reload edits for tagged boxed SSA. The other three profiles had no spill slots or spill edits under this 6-register-per-class budget. This is the regalloc2 profile derived from canonical Core IR value flow, not Cranelift's private allocation result for the generated loop or the host ISA's real register-pressure map.
- For each candidate, the OGM-26 proof heap started with two objects and 224 logical heap bytes. One native-frame root and one persistent host handle kept both objects alive during forced MarkSweep collection; after releasing both roots, two objects were reclaimed and zero remained live. This validates the existing M0 heap/handle/root API path, not production Runtime GC.
The raw machine-readable report is committed as `results/ogm38-native-value-ssa-report.json.gz`. SHA-256 of the uncompressed JSON: `498dae3c5849ff146b9c16c87b06ae9dabb22afff0c9e31b4f0b69627f13ab5a`. Reproduce the report with the command in the section above and inspect the committed samples with:
~~~sh
gzip -dc benchmarks/value-representations/results/ogm38-native-value-ssa-report.json.gz
~~~
This is the completed M0 measurement slice only. It does not satisfy the full OGM-38 criteria for four-candidate execution over the accepted corpus, native differential semantics across all supported JS types/exceptions/side effects/host traces, production heap/allocator/peak-memory metrics, or real interpreter/native ABI and machine-stack-map integration.
## Semantic and architecture limits
The generated native workload is numeric-only. It verifies that the four candidates produce the same numeric loop result for equivalent inputs. The OGM-17 semantic fixture suite remains the source of coverage for NaN, signed zero, infinities, BigInt, undefined/null, strings, object identity and symbol identity; those cases are not claimed as native differential execution in OGM-38.
This proof does not provide native execution of BigInt arithmetic, string concatenation, object ToPrimitive, Symbol semantics, dynamic property access, general host calls or exceptions. It does not implement a JavaScript interpreter, parser, source-to-Core-IR lowering, production Value/object model, production GC integration, machine stack-map emitter, write barriers or full Runtime. OGM-19's ABI and OGM-27's shadow-frame proof remain explicit M0 models; the OGM-26 GC check uses the actual existing prototype heap API, not a shipped Runtime heap.
Process RSS and allocator-specific peak memory are not measured. The register-pressure probe uses a controlled MachineEnv; it must not be confused with a physical register map for the runner's host CPU or a direct count of spills in the generated shared library. The report retains those boundaries alongside the raw values.
No production Runtime ABI or NaN-boxing decision is made solely from these M0 measurements. The final Value ABI and internal SSA decision must still account for the real Rust Runtime object model, roots/write barriers, helper calls, general dynamic semantics, interpreter/AOT equivalence, target architectures and production workload profiles.
