# Ogma portable bytecode container contract v1

- **Container ID:** `ogma.ogmb`
- **Container version:** `1.0`
- **Decoded verifier input:** `ogma.bytecode-verification` `1.0.0` ([JSON Schema](../contracts/ogmb/v1/verification-input.schema.json))
- **Reference verifier:** [`.github/scripts/verify_ogmb.py`](../.github/scripts/verify_ogmb.py)
- **Reference fixture generator:** [`tests/ogmb/build_fixtures.py`](../tests/ogmb/build_fixtures.py)
- **Status:** M0 container/verifier contract and dependency-free proof harness. This is not a production encoder, decoder, interpreter or runtime.
- **Authority:** this document refines architecture specification section 0.4 and OGM-36. Core IR semantics remain defined only by [Core IR Contract v1](core-ir-contract.md).

## 1. Purpose and security boundary

`.ogmb` is the portable, versioned artifact boundary for Ogma's bytecode interpreter. It is distinct from target-specific native AOT object files and from Core IR. Core IR describes backend-neutral JavaScript semantics; a future bytecode encoder maps verified Core IR into a selected VM instruction representation.

An artifact is untrusted input. Before an artifact can be accepted for execution, a loader must:

1. Validate its fixed header, format version, feature flags, directory entries, alignment and section bounds.
2. Strictly decode the manifest, constant pool and exception metadata using bounded parsers.
3. Verify the manifest SHA-256 against the exact code section.
4. Resolve `encoding_id` to a host-registered decoder. Unknown encodings fail closed.
5. Decode the code without executing it and produce the normalized v1 verification input bound to the code digest.
6. Verify opcode/operand constraints, constant references, control flow, execution state, exception regions and declared resource limits.
7. Only then make the verified instruction stream available to an interpreter.

The checked-in implementation is a reference proof for the binary envelope and common decoder-to-verifier contract. It does not implement a production bytecode decoder or interpreter. An artifact without decoder verification input is rejected for execution. `ogma.test-records.v1` is a fixture-only identifier and is not a production encoding selection.

The verifier is a pre-execution structural safety boundary, not a general sandbox. Runtime instruction/time budgets, heap quotas, cancellation, call-depth accounting and host capabilities must also be enforced dynamically. Native AOT code executes in the host process; these checks do not isolate arbitrary native code. Untrusted native execution requires a separate process or WASM boundary.

## 2. Fixed binary header

All integers are unsigned fixed-width little-endian. The v1 header is exactly 48 bytes and has no implicit padding.

| Offset | Width | Field | v1 rule |
|---:|---:|---|---|
| 0 | 4 | `magic` | ASCII bytes `OGMB` |
| 4 | 2 | `major` | `1` |
| 6 | 2 | `minor` | `0` |
| 8 | 4 | `header_size` | Exactly `48` |
| 12 | 8 | `feature_flags` | Zero in v1; every non-zero bit is unsupported |
| 20 | 8 | `file_size` | Exact byte length of the artifact |
| 28 | 8 | `directory_offset` | Exactly `48` |
| 36 | 4 | `section_count` | At most `64` |
| 40 | 4 | `directory_entry_size` | Exactly `32` |
| 44 | 4 | `reserved` | Must be zero |

A reader rejects unsupported major or minor versions. It must not attempt best-effort parsing or silently reinterpret a newer format. V1 defines no optional feature bits yet. A later minor or major revision requires an explicit compatibility policy and verifier support for its semantics; an older v1 reader rejects unknown versions and non-zero feature flags.

## 3. Section directory

The directory immediately follows the header. Each 32-byte entry is:

| Offset in entry | Width | Field | Rule |
|---:|---:|---|---|
| 0 | 4 | `type` | Section type identifier |
| 4 | 4 | `flags` | Bit 0 = optional; all other bits reserved and rejected |
| 8 | 8 | `offset` | Absolute byte offset from file start |
| 16 | 8 | `size` | Section byte length |
| 24 | 4 | `alignment` | Power of two from 1 through 64; offset satisfies it |
| 28 | 4 | `reserved` | Must be zero |

V1 section identifiers:

| Type | Name | Required | Payload |
|---:|---|---|---|
| 1 | `manifest` | Yes | Strict UTF-8 JSON object |
| 2 | `code` | Yes | Opaque bytes interpreted only by the registered decoder |
| 3 | `constants` | Yes | Strict UTF-8 JSON array of bounded scalar values in the M0 contract |
| 4 | `exceptions` | Yes | Strict UTF-8 JSON array of exception-region records |
| 5 | `source_map` | No | Optional strict UTF-8 JSON source metadata |

Section type identifiers are unique. Required sections cannot be marked optional. `source_map` must be marked optional. Unknown sections are skipped only when their optional flag is set; an unknown required section is rejected. Directory entries cannot overlap the header, directory or each other. Every section must fit in the declared file size. Empty required sections, duplicate sections, invalid alignment, unknown flag bits and non-zero reserved fields are rejected. All range checks occur before a section slice is read.

The reference verifier caps an artifact at 64 MiB and the directory at 64 entries. These are verifier policy ceilings, not a promise that every host must expose those amounts. An embedder may impose stricter context-specific limits; artifact metadata cannot increase the hard ceilings.

## 4. Manifest, constants and integrity

The manifest has exactly these fields:

```json
{
  "format": "ogma.ogmb",
  "version": "1.0",
  "encoding_id": "vendor.codec-name.v1",
  "code_sha256": "<lowercase SHA-256 of the complete code section>",
  "resource_limits": {
    "max_code_bytes": 1048576,
    "max_instructions": 100000,
    "max_constants": 10000,
    "max_stack": 4096,
    "max_registers": 4096,
    "max_locals": 4096,
    "max_call_depth": 256,
    "max_exception_regions": 1000
  },
  "metadata": {}
}
```

`encoding_id` is a stable ASCII identifier, not a memory address or runtime-loaded plugin path. Decoder registration is controlled by the host/build, never by an artifact. `code_sha256` binds decoder output to the exact code bytes. JSON parsers reject duplicate object keys, non-finite numbers, invalid UTF-8, oversized structures and unknown manifest fields.

All eight resource limits are mandatory positive integers bounded by hard verifier ceilings:

| Limit | Reference hard ceiling | Enforcement |
|---|---:|---|
| `max_code_bytes` | 64 MiB | Container/verifier before program analysis |
| `max_instructions` | 1,000,000 | Decoder output verification |
| `max_constants` | 262,144 | Constant-pool validation |
| `max_stack` | 65,536 | Stack dataflow and runtime |
| `max_registers` | 65,536 | Register dataflow and runtime |
| `max_locals` | 65,536 | Local-slot operand validation and runtime |
| `max_call_depth` | 4,096 | Declared runtime ceiling; runtime must enforce dynamically |
| `max_exception_regions` | 65,536 | Exception metadata validation |

The host may impose lower limits. The verifier checks resource declarations as well as the actual section/instruction/constant/region sizes. `max_call_depth` cannot be proven from a single function's bytecode in the general case, so runtime call entry must enforce the declared and host-specific depth budget. The M0 constant pool is an array of bounded JSON scalar values; richer ECMAScript constant encodings require an explicit contract extension rather than host-specific serialization.

## 5. Decoder-to-verifier contract

The container layer keeps the code section opaque. A host-registered decoder parses it without executing instructions and returns `ogma.bytecode-verification` version `1.0.0`, conforming to [`verification-input.schema.json`](../contracts/ogmb/v1/verification-input.schema.json).

The normalized input contains:

- `encoding_id` and `code_sha256` matching the loaded container manifest and actual code section;
- `execution_model`, currently `stack` or `register`, describing the abstract execution state;
- entry byte offset and a non-empty ordered instruction list;
- each instruction's byte offset, encoded size, uint32 opcode identifier, semantic operation class, bounded uint32 operands, constant references, normal successor offsets, `may_throw`, and exactly one applicable execution-state effect;
- `entry_registers` for register-model parameters; stack mode requires this list to be empty.

The operation class is a verifier-facing semantic category (`constant`, `binary`, `call`, `jump`, etc.), not an encoded opcode number. The numeric opcode space, operand bit packing and actual instruction decoder are owned by each registered encoding. OGM-36 does not assign numeric opcode IDs, freeze binary operand layout, choose a stack/register winner or define a final VM ISA.

The reference semantic operation vocabulary is `nop`, `constant`, `unary`, `binary`, `call`, `load_local`, `store_local`, `jump`, `branch`, `return` and `throw`. Immediate operand arity is checked by operation: zero for ordinary arithmetic/call/terminal operations, one for local loads/stores and unconditional jumps, and two for conditional branches. A local slot operand must be below `max_locals`. A constant operation references exactly one constant-pool entry; other operations in this v1 vocabulary do not carry constant references.

Instructions exactly and contiguously cover the code section from byte offset zero to `code_size`. Offsets, sizes, opcode IDs, operands, local indexes, constant indexes and branch targets are uint32 values; booleans are not integers for this purpose. Instruction offsets are unique and sizes are positive. Branch targets and exception boundaries resolve to instruction boundaries (the exclusive end of an exception range may equal `code_size`). Decoder output must not contain host pointers, Rust references, Rust type/layout identifiers, native addresses, target calling-convention values or target object offsets.

## 6. Opcode, operand and constant-pool invariants

- A decoder identifies the semantic operation class; unsupported classes fail closed.
- Opcode IDs, offsets, sizes, operands, constant indexes and targets must fit the uint32 verification model.
- Operand arrays are bounded to at most 64 uint32 values. Call arity is bounded to 0..1024.
- Every constant reference is checked against the decoded constant-pool length; `constant` requires exactly one existing entry.
- Local slot operands must be below `max_locals`.
- Stack and register effects are not trusted as free-form numbers: they must match the reference operation-class contract. A decoder cannot claim a zero-pop `binary` or a multi-result `constant` to bypass state checks.
- Unknown semantic operations, malformed operand arrays, overflow and out-of-range references produce stable diagnostics.

Adding semantic operation classes or changing their effect rules requires an explicit versioned review and focused positive/negative fixtures. This vocabulary does not freeze encoded instruction IDs or a final VM instruction set.

## 7. Control-flow verification

Decoded instructions form a complete, non-overlapping byte-range partition. Normal control flow is explicit:

- ordinary operations have exactly one successor at the immediately following instruction;
- `jump` has one successor matching its target operand;
- `branch` has two ordered successors matching its target operands;
- `return` and `throw` have no normal successors;
- every target names an instruction boundary;
- fallthrough beyond the code section, duplicate successors and a non-boundary entry point are rejected;
- every instruction is reachable from the entry through normal or valid exceptional control flow. V1 rejects dead/unreachable code instead of leaving it to backend-specific interpretation.

The verifier does not infer control flow from opcode bytes or guess instruction lengths. The registered decoder supplies decoded sizes and edges; the common verifier checks them against the original code size and digest.

## 8. Stack and register state

V1 supports both candidate execution models at the verifier boundary so OGM-16 can compare them without leaking an encoding decision into the container format.

### Stack candidate

Each instruction declares `{ "pop": n, "push": m }`. The verifier checks the exact operation-class effect, rejects underflow, enforces `max_stack`, and propagates stack depth over every normal edge. All paths merging at an instruction must have the same incoming depth. A mismatch is rejected; no implicit stack repair is attempted. The runtime enforces the same declared and host-specific stack ceilings during execution.

### Register candidate

Each instruction declares `register_effects.reads` and `register_effects.writes`. Every index must be below `max_registers`; duplicates within a read or write list are rejected. Read/write arity must match the operation class. A forward must-analysis intersects initialized-register sets at CFG merges; a read is valid only if it is definitely initialized on every incoming path. Entry registers represent initialized parameters.

For an exceptional edge, handler live-ins must already be initialized in the state *before* the throwing instruction. The result written by a potentially throwing instruction is not assumed to exist on its exceptional path. Handler registers are checked against `max_registers` and the throw-site state.

Neither model is selected by OGM-36. OGM-16 owns the same-Core-IR stack/register comparison and encoding recommendation. OGM-36 defines the invariant each candidate must satisfy.

## 9. Exception metadata

The exception section contains exact records:

- stack mode: `{ "start", "end", "handler", "kind", "handler_stack_depth" }`;
- register mode: `{ "start", "end", "handler", "kind", "handler_registers" }`.

`start` and `handler` point to instruction boundaries. `end` is exclusive and may equal `code_size`. The protected range is non-empty, the handler is outside its own protected range, and the range covers at least one instruction marked `may_throw`. Regions may be disjoint or nested but cannot partially overlap. Handler stack depth/register state must fit declared limits; register live-ins must be initialized at every covered throwing site. Exception edges participate in reachability and execution-state verification.

Uncaught exceptions may propagate out of a function; a local handler is required only where the artifact declares a protected region. Malformed metadata is rejected, never treated as “no handler”. Future typed-catch/finally or unwind details require an explicit versioned extension.

## 10. Deterministic diagnostics and evidence

From the repository root:

```bash
python tests/ogmb/build_fixtures.py
python .github/scripts/verify_ogmb.py --manifest tests/ogmb/.generated/manifest.json
python .github/scripts/verify_ogmb.py --manifest tests/ogmb/.generated/manifest.json --report .artifacts/ogmb-verifier-report.json
python .github/scripts/verify_ogmb.py --container path/to/artifact.ogmb --program path/to/decoded-verification-input.json
```

Diagnostics are deterministic and contain stable `code`, JSONPath-like `path` and human-readable `message` fields. A corpus report records contract/verifier versions, expected and actual outcomes, diagnostics and summary totals. Exit code 0 means the complete corpus matched its declared expectations; 1 means an artifact failed or a fixture expectation did not match; 2 means the invocation or corpus could not be loaded.

The checked-in corpus covers valid stack/register candidates and exception flow, truncated headers, unknown versions/features/required sections, optional unknown sections, overlapping sections, digest mismatch, invalid jumps, uint32 operand overflow, bad constant references, local-slot bounds, inconsistent stack effects, stack underflow and merge mismatch, malformed exception boundaries, uninitialized register reads and missing decoder output. The generator writes only to the ignored `tests/ogmb/.generated/` directory. These are container/verifier contract tests, not runtime execution, fuzzing, Test262, interpreter equivalence or proof that a production decoder exists.

## 11. Compatibility and exclusions

- `.ogmb` has its own version lifecycle, independent of Core IR, Rust ABI and native AOT artifact versions.
- Unknown versions and unknown mandatory features fail closed. V1 accepts only 1.0. A future minor version is accepted by an older reader only when an explicit compatibility policy and safe extension rules are defined.
- Native AOT object files are separate target-specific artifacts, not native objects embedded in `.ogmb`.
- No binary compatibility with QuickJS `qjsc` bytecode is promised or implied. qjsc encoding, pointer/layout assumptions and runtime internals are outside this contract.
- OGM-36 does not implement a production encoder, decoder, interpreter, Core IR lowering, native AOT backend or sandbox.
- OGM-36 does not decide stack versus register encoding. OGM-16 owns that comparison.
- Core IR v1 remains the sole source of Core IR semantics; this bytecode contract does not duplicate or redefine those semantics.
