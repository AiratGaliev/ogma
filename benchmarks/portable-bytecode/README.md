# OGM-16 — portable bytecode stack/register proof

This directory contains an M0 proof, not a production compiler or VM. It lowers the same verified Core IR v1 workload corpus into two independent portable bytecode encodings, decodes both encodings, packages each function in the existing .ogmb v1 container, and validates the decoded facts with the repository's ContainerReader and ProgramVerifier.

## Workload corpus

```text
dynamic-add          JS + coercion (string + number)
control-flow         two-way CFG branch, value block arguments and both paths
mutable-loop-locals  back-edge, lexical load/store, loop-carried state
function-calls       closure creation, receiver and nested call
host-calls           capability call with observable side-effect trace
exceptions           explicit JS throw and exceptional completion
```

The corpus is generated deterministically by corpus.py into fixtures/. Every generated function is a standalone ogma.core-ir v1 document. The fixture manifest is consumed by the existing Core IR verifier; no parallel IR schema or semantic dialect is introduced.

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

The benchmark writes results/benchmark-results.json. It records the environment, command parameters, raw timing samples, instruction and dispatch counts, code/container/constant/debug-sidecar sizes, verifier input size and structural work proxies, verifier timings, and Python tracemalloc peak memory. Interpreter throughput is measured over nine independent timed rounds after warm-up; the report retains every round and uses median rates. Compile and verifier costs likewise retain fifteen samples each. The task-scoped GitHub Actions job uploads the raw JSON reports and generated Core IR fixtures as an artifact.

## Measurement interpretation

- Instruction and dispatch counts are deterministic for a given workload and encoding.
- Code bytes measure the encoded .ogmb code section. Container bytes include the .ogmb header, directory and metadata/constant sections.
- Compile cost measures Core IR lowering, layout and bytecode/container generation. Container/program verifier time is reported separately.
- Throughput is reported for the Python proof interpreters. It is a comparative prototype measurement, not a prediction of Rust-native production throughput.
- Memory uses Python tracemalloc peak allocations during one invocation, not process RSS or native VM heap usage.
- Verifier complexity reports measured time and deterministic structural proxies (instructions, CFG edges, operands, stack-effect checks, and register dataflow reads/writes). These proxies expose the different state-analysis work required by each candidate; they are not asymptotic proofs.
- The supported semantic subset is intentionally limited to the operations used by this corpus. It is not an ECMAScript conformance claim, a general Core IR backend, or a production interpreter.

Any optimization or encoding change must keep both candidates on the same input corpus and preserve the expected observable results, exceptions and host traces.

## Measured result and decision — 2026-11-17

The successful GitHub Actions run on the task branch measured the six workloads with 1,000 iterations per test case, three warm-up executions, fifteen compile rounds, fifteen verifier rounds and nine interpreter measurement rounds. The same expected outcomes, thrown values and host-call traces were observed for both candidates in every case.

| Metric (whole corpus) | Stack | Register |
|---|---:|---:|
| Decoded instructions | 83 | 39 |
| Encoded code bytes | 455 | 283 |
| Dispatches per measurement round | 175,000 | 78,000 |
| Compile/codegen median samples summed by workload | 1,074,164 ns | 811,898 ns |
| Verifier median samples summed by workload | 2,139,452 ns | 1,874,432 ns |
| Python interpreter executions/s, derived from sum of workload medians | 99,897 | 154,162 |
| Mean per-workload max Python tracemalloc peak | 1,137 bytes | 925 bytes |
| Estimated debug sidecar JSON bytes | 20,463 | 9,570 |
| .ogmb container bytes | 10,510 | 10,406 |

Register lowering reduced instruction count by 53.0%, code bytes by 37.8% and dispatches per measurement round by 55.4%. Its summed compile/codegen medians were 24.4% lower and summed verifier medians 12.4% lower. The derived aggregate Python proof-interpreter throughput was 54.3% higher. Mean per-workload maximum tracemalloc peak was 18.7% lower, but remains only a Python allocation proxy. The container itself was only 1.0% smaller because fixed metadata and JSON sections dominate this small code corpus.

**Decision:** select the register-oriented execution model as Ogma's primary candidate for the next Rust-native interpreter proof. The proof consistently favours it across arithmetic/coercion, control flow, mutable locals/loops, calls, host calls and exceptions. Keep the stack candidate as comparative evidence, but do not freeze the prototype opcode numbers, operand widths or a final binary ISA. Keep the OGM-36 portable .ogmb v1 container and normalized verifier boundary unchanged; later Rust-native measurements and broader semantics must validate the model before production adoption.

## Limits of this decision

- These throughput figures compare two Python proof interpreters, not Rust-native VM performance. Register dispatches/s itself is not the decision metric; the register candidate executes substantially fewer dispatches and therefore completes more whole workload cases per second in this prototype.
- Python tracemalloc peak is an allocation proxy, not native process RSS, Rust heap size or production VM memory.
- The corpus is a bounded M0 decision workload, not Test262, fuzzing, broad JS compatibility or a production backend proof. It tests explicit exception propagation but does not yet compare local catch/finally handler lowering or suspension.
- The debug map is retained and measured as an in-memory sidecar estimate. It is not serialized into the optional .ogmb source_map section in this prototype. The call workload carries an explicit thisArg but does not assert observable this binding semantics.
- Summed per-workload medians are useful comparative aggregates, not a single uninterrupted whole-suite benchmark run. Raw per-workload samples and environment metadata remain in the JSON evidence.

## Committed evidence

The raw benchmark report (including all per-round samples) and the Core IR verifier report from the successful CI run are committed under `results/`:

- `results/benchmark-results.json.gz` — lossless gzip of the complete machine-readable benchmark report. Decompress with `gzip -dc benchmarks/portable-bytecode/results/benchmark-results.json.gz`.
- `results/core-ir-verifier-report.json` — all seven generated Core IR v1 fixtures accepted, zero failures.

The SHA-256 of the decompressed benchmark JSON is `e3f14e713ba3d6722c751fc71b8ef25c02498ab60a02716b1b3c5fc70618e91f`. It was produced by CI run [44045955802](https://github.com/AiratGaliev/ogma/actions/runs/44045955802) and records source revision `ea63d9512be445fcc2d62cbb8f2951de42ccad5e`, runner environment and the exact measurement protocol.
