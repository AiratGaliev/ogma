# OGM-38 — Native Value and SSA evidence

**Status:** M0 executable evidence slice verified; OGM-38 acceptance remains partially open. No production Runtime ABI or NaN-boxing decision is made.

## Question and candidate matrix

Revalidate the two independent OGM-17 axes using native execution evidence:

| Runtime Value | SSA | Candidate |
|---|---|---|
| Explicit tagged 16-byte value | Boxed | `tagged-16-boxed` |
| Explicit tagged 16-byte value | Representation-aware | `tagged-16-representation-aware` |
| 8-byte NaN-box word | Boxed | `nanbox-8-boxed` |
| 8-byte NaN-box word | Representation-aware | `nanbox-8-representation-aware` |

The current Cranelift ObjectModule prototype compiles and runs all four candidates from the same canonical loop fixture, `tests/core-ir/fixtures/positive-loop-carried-values.json`. Tagged values cross the fixed C entry signature as pointers to a `repr(C)` two-word structure; NaN-box values cross as 64-bit words. Boxed SSA carries the encoded Value through the loop and decodes at numeric operations. Representation-aware SSA converts its numeric inputs at the Value boundary and carries `f64` through the loop.

This is an exact-fixture native adapter, not a general Core IR compiler. It does not parse JavaScript source, and it does not execute a production Ogma Runtime. The other canonical call-boundary fixture is read separately to construct the allocator-pressure profile; it is not lowered and executed by these four loop candidates.

## Latest verified run

- **Workflow:** [OGM-38 CI run #44683606257](https://github.com/AiratGaliev/ogma/actions/runs/44683606257) — success, including the focused OGM-38 job and repository `fast-checks`.
- **Tested/source revision:** `b98ef1ed2441306803d7c537ca59a6d9696a7e25`.
- **Environment:** GitHub-hosted Linux x86_64; Rust 1.100.0; Cranelift 0.137.2; regalloc2 0.15.3; target `x86_64-unknown-linux-gnu`; AMD EPYC 9V45 96-Core Processor.
- **Sampling:** 3 warmups, 101 compile samples and 101 native-call samples per candidate; each throughput sample executes 1,000 native calls.
- **Artifact:** `ogm38-native-value-ssa-evidence` from the workflow run, containing `ogm38-native-value-ssa-report.json` (820,962 bytes), the OGM-31 differential report, verifier report, resolved dependency lock and generated proof artifacts.
- **Raw report SHA-256:** `8e3efb549d418ef94cc59b84e88033d8a09548cc594eae235cb47ce0d7aed820` (uncompressed JSON). The repository's existing `results/ogm38-native-value-ssa-report.json.gz` is an earlier snapshot and does **not** contain this latest `b98ef1e` run; the new raw artifact still needs to be added to Git.

## Native loop measurements

The table reports the p50 compile time and p50/p95/p99 native-call latency from the actual Cranelift-generated and linked host-native functions.

| Candidate | Value bytes | Compile p50 (ns) | Native loop p50 / p95 / p99 (ns) | Object bytes | Shared library bytes | Value conversions |
|---|---:|---:|---:|---:|---:|---|
| tagged-16-boxed | 16 | 526,168 | 334.335 / 342.317 / 348.315 | 1,312 | 14,912 | 5 per loop iteration |
| tagged-16-representation-aware | 16 | 499,209 | 81.060 / 86.749 / 89.732 | 1,312 | 14,912 | 4 at invocation boundary |
| nanbox-8-boxed | 8 | 485,890 | 331.440 / 340.655 / 346.142 | 1,408 | 14,912 | 5 per loop iteration |
| nanbox-8-representation-aware | 8 | 516,755 | 83.744 / 96.423 / 104.415 | 1,400 | 14,912 | 4 at invocation boundary |

All four native functions returned the expected loop result, `128`, for the primary workload. The report also contains the first compile observation and all raw compile and throughput samples, rather than only aggregates.

### Interpretation of this workload

- Representation-aware SSA reduced p50 loop latency by approximately **4.12×** for the tagged layout (334.335 ns → 81.060 ns) and **3.96×** for NaN-box (331.440 ns → 83.744 ns). This is a result for this Number-only loop adapter; it is not a general JavaScript workload result.
- NaN-box did **not** improve representation-aware loop throughput here: 83.744 ns versus 81.060 ns for tagged representation-aware, about **3.3% slower**.
- The NaN-box object files were larger in this prototype: 1,408 versus 1,312 bytes for boxed (about **7.3% larger**) and 1,400 versus 1,312 bytes for representation-aware (about **6.7% larger**). The current helper validation and encoding paths are part of the measured prototype and affect this result.
- These measurements support further evaluation of representation-aware SSA. They do not establish a final public Runtime Value layout. In particular, they do not justify NaN-boxing from its nominal 8-byte width.

## Helper, codec and malformed-value evidence

The generated native helper is exercised independently from the numeric loop:

- Each candidate successfully round-trips and dispatches `undefined`, `null`, both booleans, negative zero, canonical NaN and both infinities.
- Each candidate resolves String, BigInt, Object and Symbol reference-tag families through the real generation-checked OGM-26 `HostHandle` registry. The native callback forces collection before resolving the exact expected `GcRef`.
- Five malformed encodings per candidate are rejected before the host callback is entered. Cases include malformed immediate payloads, a zero reference token, an out-of-range tagged handle and positive/negative non-canonical NaN inputs where applicable.
- The latest CI run caught and fixed a real NanBox helper defect: `undefined(payload=1)` could fall through to the Number path and invoke the host callback. Validation is now an explicit Cranelift basic-block decision tree; the malformed-value regression is exercised by the native runner.

This proves the candidate codecs and this helper boundary for the listed cases. It does not amount to complete native ECMAScript semantics or a production Value implementation.

## Register-allocation evidence boundary

The pinned regalloc2 0.15.3 API receives a register-liveness profile derived from the parameters and operand/result flow in `positive-call-boundary.json`, under a controlled budget of 6 GPR, 6 FPR and 6 vector registers.

| Candidate | regalloc2 allocations | GPR / FPR used | Spill slots | Logical spill bytes | Spill-store / reload edits |
|---|---:|---:|---:|---:|---:|
| tagged-16-boxed | 34 | 6 / 0 | 6 | 48 | 6 / 6 |
| tagged-16-representation-aware | 23 | 6 / 1 | 0 | 0 | 0 / 0 |
| nanbox-8-boxed | 17 | 6 / 0 | 0 | 0 | 0 / 0 |
| nanbox-8-representation-aware | 17 | 5 / 1 | 0 | 0 | 0 / 0 |

Spill bytes are logical 64-bit slots (`spillslot_size`: Int/Float = 1 slot, Vector = 2 slots). Store/reload edit counts are not counts of distinct spilled SSA values.

**This is a controlled regalloc2 evidence model, not Cranelift's private register-allocation output for the generated functions and not a physical register map for the runner's host CPU.** Therefore these values are still a proxy for criterion 2, not actual generated-loop spill evidence.

## Heap, GC and roots evidence boundary

All four layouts run an equivalent separate OGM-26 graph workload using the existing `GcHeap` API, with `HostHandle` tokens, generation-checked `GcRef` resolution, interpreter/native frame roots, temporary scopes and persistent host handles.

| Candidate layout | Objects | Edges | Encoded Value storage capacity | Peak logical heap bytes |
|---|---:|---:|---:|---:|
| tagged-16 (both SSA policies) | 2,048 | 2,121 | 524,288 | 656,528 |
| nanbox-8 (both SSA policies) | 2,048 | 2,121 | 262,144 | 394,384 |

Each measured graph sample resolved 544 object-reference Values. The root topology was equivalent across the candidates and collectors: 171 interpreter-frame roots, 171 native-frame roots, 170 persistent host handles and 32 temporary-scope roots. Each collector preserved all reachable objects during collection; after the roots and handles were released, the full graph was reclaimed.

The report compares OGM-26 RC cycle-removal and non-moving MarkSweep on these same graph shapes. For example, the MarkSweep collection p50 was approximately 11.45–11.65 μs across candidates; RC cycle-removal collection p50 was approximately 20.59–21.10 μs.

**These are logical heap and Value-storage measurements on a separate prototype graph, not peak process RSS, allocator-internal fragmentation, or a candidate-integrated native execution/GC workload.** They do not close production allocator or peak-memory requirements.

## Reproduction

On the documented Linux x86_64 target with Rust 1.100.0, LLVM 18 development packages and Clang 18:

~~~sh
cargo fmt --manifest-path benchmarks/backend-codegen/Cargo.toml --all -- --check
cargo clippy --manifest-path benchmarks/backend-codegen/Cargo.toml --all-targets --all-features --locked -- -D warnings
cargo test --manifest-path benchmarks/backend-codegen/Cargo.toml --all-features --locked
cargo run --manifest-path benchmarks/backend-codegen/Cargo.toml --locked --release --bin ogm38-value-ssa -- \
  --iterations 1000 --warmups 3 --samples 101 \
  --revision "$(git rev-parse HEAD)" \
  --output /tmp/ogm38-native-value-ssa-report.json
gzip -dc benchmarks/value-representations/results/ogm38-native-value-ssa-report.json.gz
~~~

The focused workflow additionally verifies the canonical Core IR manifest and OGM-17 semantic fixture suite, then runs OGM-26/27/backend tests, the four-candidate native benchmark and the separate OGM-31 portable/native proof.

## OGM-38 acceptance status after this M0 slice

| Acceptance criterion | Status | Evidence / remaining gap |
|---|---|---|
| Four candidates on the same accepted Core IR corpus | **Not met** | All four execute one canonical Number-only loop fixture. The call-boundary fixture only feeds a separate allocator profile; the accepted corpus is not natively lowered and executed by each candidate. |
| Actual codegen, throughput, register pressure/spills and helper/conversion costs | **Partial** | Cranelift object size, compile time, native loop throughput, helper dispatch and conversion counts are measured. Register pressure/spills still come from a separate controlled regalloc2 model, not Cranelift's allocation of these functions. |
| Equivalent heap/allocator/GC and peak-memory metrics | **Partial** | Equivalent OGM-26 graph/root/handle workloads and logical heap/Value capacity are measured. The GC workload is separate from candidate execution; process RSS and allocator fragmentation are not measured. |
| Native differential semantics across requested JS values and effects | **Partial** | Native numeric edge cases, primitive codec/helper round-trips, malformed values and reference handle families are tested. BigInt arithmetic, strings, identity semantics, type transitions, exceptions, side effects and host-call traces are not differentially executed through all four candidate layouts. OGM-31 separately covers its narrower five-fixture portable/native subset. |
| Rooting, handle lifetime, safepoints and ABI transitions | **Partial** | Candidate encodings cross the native helper and real OGM-26 handles/roots survive forced GC. Production OGM-19 ABI and candidate-integrated OGM-27 safepoints/stack maps and interpreter/native transitions are not implemented by this benchmark. |
| Raw measurements, target/toolchain/runtime revisions and reproduction | **Partial** | The latest complete raw JSON, dependency lock and reports are available in successful CI artifact #14604484603, with target/toolchain/revision metadata and reproduction instructions. The committed gzip report is still an older snapshot; latest raw measurements are not yet committed to the PR branch. |
| Separate public Value ABI and internal SSA decision | **Partial** | The results support representation-aware SSA as a promising compiler direction and do not support choosing NaN-box by width. A final production Value ABI decision still needs broader Runtime/GC/ABI evidence. |

This is a completed and reproducible M0 evidence slice, **not completion of all OGM-38 acceptance criteria**. The Linear task remains In Progress. Per the latest OGM-38 correction, remaining M0 proof gaps must be addressed with M0-fidelity prototypes; they must not be dismissed as browser QA or silently deferred to an unauthorized M1 production Runtime implementation.

## Architecture and semantic limits

The generated native workload remains numeric-only. Native helper checks cover a deliberately bounded set of immediate/reference encodings, not BigInt arithmetic, string concatenation, object ToPrimitive, Symbol operations, dynamic property access, general host calls or exception propagation. The OGM-17 semantic fixture suite and OGM-31 dual-backend proof are separate adjacent M0 proofs; they are not a four-layout native differential test.

This proof does not implement a JavaScript interpreter, parser, source-to-Core-IR compiler, production Value/object model, production allocator/GC integration, machine stack-map emitter, write barriers or full Runtime. Process RSS and allocator-specific peak memory are not measured. No production Runtime ABI or NaN-boxing decision is made from these measurements alone.
