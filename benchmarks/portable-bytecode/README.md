# OGM-16 — portable bytecode stack/register proof

This directory contains an M0 proof, not a production compiler or VM. It lowers the same verified Core IR v1 workload corpus into two independent portable bytecode encodings, decodes both encodings, packages each function in the existing .ogmb v1 container, and validates the decoded facts with the repository's ContainerReader and ProgramVerifier.

## Workload corpus

```text
dynamic-add          JS + coercion (string + number)
control-flow         two-way CFG branch, both paths
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
  --verifier-rounds 15
```

The benchmark writes results/benchmark-results.json. It records the environment, command parameters, raw timing samples, instruction and dispatch counts, code/container/constant/debug-map sizes, verifier input size and structural work proxies, verifier timings, and Python tracemalloc peak memory. The task-scoped GitHub Actions job uploads both JSON reports as an artifact.

## Measurement interpretation

- Instruction and dispatch counts are deterministic for a given workload and encoding.
- Code bytes measure the encoded .ogmb code section. Container bytes include the .ogmb header, directory and metadata/constant sections.
- Compile cost measures Core IR lowering, layout and bytecode/container generation. Container/program verifier time is reported separately.
- Throughput is reported for the Python proof interpreters. It is a comparative prototype measurement, not a prediction of Rust-native production throughput.
- Memory uses Python tracemalloc peak allocations during one invocation, not process RSS or native VM heap usage.
- Verifier complexity reports measured time and deterministic structural proxies (instructions, CFG edges, operands and register dataflow reads/writes). These proxies expose the additional state analysis required by register code; they are not asymptotic proofs.
- The supported semantic subset is intentionally limited to the operations used by this corpus. It is not an ECMAScript conformance claim, a general Core IR backend, or a production interpreter.

Any optimization or encoding change must keep both candidates on the same input corpus and preserve the expected observable results, exceptions and host traces.
