# OGM-16 — portable bytecode stack/register proof

This directory contains an M0 proof, not a production compiler or VM. It lowers the same verified Core IR v1 workload corpus into two independent portable bytecode encodings, decodes both encodings, packages each function in the existing .ogmb v1 container, and validates the decoded facts with the repository's ContainerReader and ProgramVerifier.

## Workload corpus

```text
dynamic-add          JS + coercion (string + number)
control-flow         two-way CFG branch, value block arguments and both paths
mutable-loop-locals  back-edge, lexical load/store, loop-carried state
function-calls       closure creation, receiver and nested call
host-calls           capability call with observable side-effect trace
property-access      dynamic named-property reads/writes on a host-provided object
exceptions           typed thrown values propagated across a nested JS call
```

The corpus is generated deterministically by corpus.py into fixtures/. Every generated function is a standalone ogma.core-ir v1 document. The fixture manifest is consumed by the existing Core IR verifier; no parallel IR schema or semantic dialect is introduced. The property workload covers plain own data properties only; it does not claim accessor, proxy or prototype-chain semantics.

## Candidate encodings

- **Stack:** SSA values are assigned frame-local slots. Inputs are loaded to an operand stack, the operation consumes/pushes stack values, and results are stored back to frame locals.
- **Register:** SSA values and lexical bindings are assigned virtual registers. Operations read their sources and write their destination directly. The interpreter does not synthesize a separate operand stack.

Both encodings use explicit, fixed-width little-endian operands and byte offsets. They contain no host pointers, Rust object layouts or target addresses. The encoding_id distinguishes the prototypes while the .ogmb v1 envelope remains unchanged. Opcode numbers and prototype widths are experiment details, not a frozen production ISA.

The decoder emits the exact normalized ogma.bytecode-verification v1 shape. The actual repository container reader validates the bytes and the actual .ogmb program verifier validates instruction boundaries, CFG, operand bounds, stack/register effects, definite register initialization and metadata summary. The two interpreters execute decoded bytes, not the pre-encoding IR or plan objects.

## Reproduction

Run from the repository root with the pinned Fast CI Python 3.12 toolchain:

```bash
python benchmarks/portable-bytecode/corpus.py
python .github/scripts/verify_core_ir.py \
  --manifest benchmarks/portable-bytecode/fixtures/manifest.json \
  --report benchmarks/portable-bytecode/results/core-ir-verifier-report.json
python -m unittest discover -s .github/scripts -p "test_*.py" -v
python benchmarks/portable-bytecode/run.py \
  --iterations 1000 \
  --warmups 3 \
  --compile-rounds 15 \
  --verifier-rounds 15 \
  --measurement-rounds 9
```

The benchmark writes results/benchmark-results.json. It records the environment, command parameters, raw timing samples, instruction and dispatch counts, code/container/constant/debug-sidecar sizes, isolated decoder timings, closure and host-object allocation counters, verifier input size and structural work proxies, verifier timings, and Python tracemalloc peak memory. Interpreter throughput is measured over nine independent timed rounds after warm-up; the report retains every round and uses median rates. Compile and verifier costs likewise retain fifteen samples each. The task-scoped GitHub Actions job uploads the raw JSON reports and generated Core IR fixtures as an artifact.

## Measurement interpretation

- Instruction and dispatch counts are deterministic for a given workload and encoding.
- Code bytes measure the encoded .ogmb code section. Container bytes include the .ogmb header, directory and metadata/constant sections.
- `compile_cost_median_ns` measures Core IR lowering, layout, bytecode decoding and container generation up to (but not including) the first verification pass. `decode_cost_median_ns` isolates the decoder subset of that compile path; it must not be added to compile cost. `compile_plus_validation_pipeline_wall_median_ns` additionally includes the immediate container/program verification performed by `compile_function`; the repeated standalone verifier timing is measured separately.
- Throughput is reported for the Python proof interpreters. It is a comparative prototype measurement, not a prediction of Rust-native production throughput.
- Memory uses Python tracemalloc peak allocations during one invocation, not process RSS or native VM heap usage. Allocation counters distinguish closure objects created by the proof interpreter from objects returned by the test host factory. They do not measure an Ogma GC: the repository has no runtime heap or collector yet.
- Verifier complexity reports measured time and deterministic structural proxies (instructions, CFG edges, operands, stack-effect checks, and register dataflow reads/writes). These proxies expose the different state-analysis work required by each candidate; they are not asymptotic proofs.
- The supported semantic subset is intentionally limited to the operations used by this corpus. It is not an ECMAScript conformance claim, a general Core IR backend, or a production interpreter.

Any optimization or encoding change must keep both candidates on the same input corpus and preserve the expected observable results, exceptions and host traces.

## Measured result and decision — 2026-11-17

The successful GitHub Actions run [44051174210](https://github.com/AiratGaliev/ogma/actions/runs/44051174210) measured seven workloads with 1,000 iterations per test case, three warm-up executions, fifteen compile rounds, fifteen verifier rounds and nine interpreter measurement rounds. Both decoded interpreters matched every expected return value, thrown value and host-call trace. The Core IR verifier accepted all nine generated function fixtures.

| Metric (whole corpus) | Stack | Register |
|---|---:|---:|
| Decoded instructions | 131 | 62 |
| Encoded code bytes | 717 | 485 |
| Dispatches per measurement round | 231,000 | 105,000 |
| Compile/codegen median samples summed by workload | 1,161,056 ns | 812,369 ns |
| Decoder median samples summed by workload | 390,401 ns | 184,595 ns |
| Compile + in-band validation pipeline medians | 3,529,709 ns | 2,809,548 ns |
| Standalone verifier median samples summed by workload | 2,096,507 ns | 1,776,142 ns |
| Verifier structural work units | 493 | 250 |
| Python interpreter executions/s, derived from sum of workload medians | 114,353 | 159,921 |
| Mean per-workload max Python tracemalloc peak | 1,477 bytes | 1,181 bytes |
| Runtime closure allocation events per round | 3,000 | 3,000 |
| Host-factory object allocations per round | 1,000 | 1,000 |
| Estimated debug sidecar JSON bytes | 32,157 | 15,113 |
| .ogmb container bytes | 13,810 | 13,666 |

Register lowering reduced instruction count by 52.7%, code bytes by 32.4%, dispatches by 54.5% and verifier structural work units by 49.3%. Its summed codegen medians were 30.0% lower, isolated decode medians 52.7% lower, compile-plus-in-band-validation medians 20.4% lower and standalone verifier medians 15.3% lower. Derived aggregate Python proof-interpreter throughput was 39.8% higher. Mean per-workload maximum tracemalloc peak was 20.0% lower. Both candidates emitted the same number of measured closure and host-factory allocation events; no collector exists in this M0 proof, so GC pause/throughput is not measured. The container itself was only 1.0% smaller because fixed metadata and JSON sections dominate this small code corpus.

**Decision:** select the register-oriented execution model as Ogma's primary candidate for the next Rust-native interpreter proof. The expanded corpus now includes dynamic named-property access and writes, as well as thrown completions propagated across a nested call. The register candidate still leads on code size, instruction/dispatch count, decoder, compile and verifier metrics. Keep the stack candidate as comparative evidence, but do not freeze prototype opcode numbers, operand widths or a final binary ISA. Keep the OGM-36 portable .ogmb v1 container and normalized verifier boundary unchanged; later Rust-native measurements and broader semantics must validate the model before production adoption.

## Limits of this decision

- These throughput figures compare two Python proof interpreters, not Rust-native VM performance. Register dispatches/s itself is not the decision metric; the register candidate executes fewer dispatches and completes more whole workload cases per second in this prototype.
- Python tracemalloc peak is an allocation proxy, not native process RSS, Rust heap size or production VM memory. Allocation counters cover proof-interpreter closure creation and separately reported host-factory object creation; no Ogma GC/heap exists in this M0 repository, so collection cost, pause time and collector throughput remain unmeasured.
- The property workload exercises named own data-property get/set over a small host-provided object representation. It does not cover prototype-chain lookup, accessors, proxies, computed element keys or user-defined getters/setters.
- The corpus is a bounded M0 decision workload, not Test262, fuzzing, broad JS compatibility or a production backend proof. It tests typed explicit throw propagation across a function call, but does not compare local catch/finally handler lowering or suspension.
- The debug map is retained and measured as an in-memory sidecar estimate. Workload source records are generated provenance only (no user file/line spans or lexical scopes), and the map is not serialized into the optional .ogmb source_map section in this prototype. The call workload carries an explicit thisArg but does not assert observable this binding semantics; closure captures are not exercised.
- Summed per-workload medians are comparative aggregates, not a single uninterrupted whole-suite benchmark run. Raw per-workload samples and environment metadata remain in the committed compressed JSON evidence.
## Committed evidence

The raw benchmark report (including all per-round samples) and Core IR verifier report from the same successful CI run are committed under results/:

- `results/benchmark-results.json.gz` — lossless gzip of the complete machine-readable benchmark report. Decompress with `gzip -dc benchmarks/portable-bytecode/results/benchmark-results.json.gz`.
- `results/core-ir-verifier-report.json` — all nine generated Core IR v1 function fixtures accepted, zero failures.

The SHA-256 of the decompressed benchmark JSON is `8870419ef4d0be36733330324a54b11baddc3697801ae4d4700c9799222efff1`. It was produced by CI run [44051174210](https://github.com/AiratGaliev/ogma/actions/runs/44051174210) for topic HEAD `dd55955b512a0196f837ee5480ba302717a157ab`; the report's `source_revision` records the PR merge-checkout commit `4ae87a7d894937d4a0edf15bb85929547f98e77c`. Environment: CPython 3.12.16, Linux x86_64, AMD EPYC 9V74 80-Core Processor.
