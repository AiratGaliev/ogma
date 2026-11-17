# Backend-neutral Debugger API v1

- **Contract:** ogma.debugger-api
- **Version:** 1.0.0
- **Machine-readable contract:** [API schema](../contracts/debugger/v1/api.schema.json)
- **Public facade:** [debugger_api.py](../benchmarks/debugger/debugger_api.py)
- **M0 proof:** [prototype.py](../benchmarks/debugger/prototype.py)
- **DAP boundary:** [dap_adapter.py](../benchmarks/debugger/dap_adapter.py)
- **Architecture authority:** Notion specification, section 19 (Developer Tooling & Debugging Architecture).

## 1. Status and boundaries

This is the M0 contract and a deterministic proof harness. The Python fixture interpreter executes a deliberately small instruction set to validate source-level control flow and adapter seams. It is **not** an ECMAScript interpreter, production VM, Rust Runtime, real GC, native AOT backend, network DAP server or complete IDE integration.

The contract fixes the data and ownership boundary before M1 VM implementation. It does not prescribe interpreter internals, registers, operand stacks, object layouts, heap pointers, code addresses, native ABI or a particular backend.

## 2. Source identity and position conventions

A source file has a stable SourceId, normalized repository-relative POSIX path, language and optional SHA-256 of the exact source bytes. Paths are descriptive metadata, not identities: a rename or content change must not cause an old breakpoint to bind to unrelated source.

- Lines are **1-based**.
- Columns are **0-based UTF-16 code units**, matching JavaScript/TypeScript source semantics and common editor/DAP conventions.
- Source spans are half-open: start is included and end is excluded.
- Source offsets in .ogmb are byte offsets into the encoded bytecode section, not source columns.
- User and generated locations are distinct. Generated operations carry a bounded reason and must not invent a user file/line.

A code-generation identity is the pair (id, version) from the function manifest. Breakpoints/frames are associated with that generation and with the source digest when supplied. A stale generation or digest must be reported as an unverified breakpoint, never silently rebound.

## 3. Frontend → Core IR → bytecode propagation

The frontend owns stable source identity, source spans and semantic binding IDs. Core IR v1 remains the semantic source of truth for function-local bindings, captures and instruction/terminator source provenance. A lowering pass must preserve or explicitly remap this provenance; it must not assign a nearby user line to generated code.

The portable bytecode side reuses the existing optional .ogmb v1 source_map section:

- source files and content hashes;
- per-instruction user/generated locations and inline provenance;
- function-local lexical scopes;
- optional variable storage ranges;
- bytecode instruction-boundary checks performed by the existing .ogmb verifier.

The M0 proof's lower_core_ir_debug_metadata takes explicit Core IR operation → bytecode offsets and explicit backend/encoder-owned variable storage locations. It does not guess a local slot from the position of a binding descriptor. Every mapped offset/range must be an encoded instruction boundary; synthetic operations remain synthetic.

### Capture identities

A Core IR capture descriptor has a stable capture ID distinct from the binding ID in the enclosing function. Within the captured function, source-level metadata references that capture ID. In .ogmb v1 the corresponding canonical storage location is kind: "capture" and index equal to that capture descriptor's index in environment.captures. It is not a slot in environment.bindings. The verifier checks capture IDs against the descriptor array and rejects conflicting/unknown identifiers or mismatched location indexes.

A lexical scope in one function artifact is function-local. References to an outer function's lexical environment are represented through capture descriptors; they are not dangling cross-artifact scope-parent references.

## 4. Backend-neutral API

The stable facade is DebuggerPort. Public results are immutable snapshots:

| DTO | Responsibility |
|---|---|
| CodeGeneration | Stable code-generation identity/version. |
| SourceLocation | User/generated provenance, source digest, span, scope, binding IDs and inline chain. |
| Breakpoint | Requested source position, generation/digest binding, verified state and diagnostic. |
| StackFrameSnapshot | Opaque frame ID, function identity, generation, source location, scope IDs and backend label. |
| ScopeSnapshot, VariableSnapshot | Lexical scope and immutable variable values/availability. |
| DebugValue | Bounded preview, optional opaque object ID and explicit availability. |
| StoppedEvent, ExceptionEvent | Pause reason and structured exception data. |
| InspectedObject, InspectedProperty | Bounded descriptor-level object view. |

Operations include launch/attach lifecycle, source breakpoints, pause/resume, step into/over/out, call stack, scopes, variables, explicit identifier evaluation, object inspection and detach. Each backend reports real capabilities; a DAP adapter must not advertise operations unsupported by the active backend/profile.

A value optimized away or unavailable in the selected frame is returned as optimized_out or unavailable. The debugger must not synthesize a value from a type annotation or infer it from unrelated state.

## 5. Thread ownership and safe points

Runtime/Isolate remains owner-thread-affine. Transport or host-control threads may enqueue a pause request; they may not inspect or mutate VM frames/heap directly. The owner thread applies commands at defined interpreter safepoints. Resume and stepping commands operate only on a paused session and are serialized with execution and GC.

A pause request is not an arbitrary asynchronous interruption of native code. The M0 proof checks requests at interpreter instruction boundaries. Native AOT pause latency and safepoint strategy require a separate backend design and capability declaration.

## 6. Runtime Inspector and GC roots

The Inspector is separate from the source debugger and consumes opaque object IDs only while the target is paused. Ordinary inspection is descriptor-only:

- it never invokes getters, setters, Proxy traps, toString, user repr or arbitrary JavaScript;
- accessors are shown as not evaluated;
- Proxy values are identified without querying the target or invoking traps;
- result count and string previews are bounded;
- unsupported values are rendered as unavailable, not stringified by executing user code.

An inspected object is held by a temporary root for the duration of the inspection operation. The root is released in a finally/scope guard on success and failure. The Inspector does not create a persistent root or extend object lifetime after the request. The proof harness injects collection at the inspection safepoint to test this lifetime rule.

Expression evaluation is a separate explicit operation and may have side effects. The M0 proof implements identifier-only evaluation; it does not claim general ECMAScript expression evaluation.

## 7. DAP adapter boundary

DAP is an IDE transport adapter over DebuggerPort, not a VM/ECMAScript Runtime dependency. The in-process M0 adapter validates a limited command set and translates normalized requests/results. It has no socket, stdio Content-Length framing, authentication, launch/attach process management or full DAP event loop. Production transport and protocol-conformance tests remain separate implementation work.

The adapter advertises capabilities from the selected backend. It does not advertise unsupported conditional breakpoints, logpoints, step-back, hover evaluation or exceptionInfo.

## 8. Metadata profiles and stripping

- **Debug:** retain source maps, scope/binding locations, breakpoints and inspection metadata for the selected backend.
- **Development:** retain available metadata and generation identities; optimized-out values are explicit.
- **Release:** may omit the optional source-map section, source text, IDE transport and interactive Inspector unless explicitly requested by the host.

Stripping debug metadata must not change executable bytecode, Core IR semantics, exceptions, effects, module identity or runtime behaviour. Do not strip metadata required by the bytecode verifier or Runtime contract: .ogmb function identity, code-generation/cache identity, frame layout, environment/capture descriptors, suspension state and verifier summary remain in the existing manifest when required by that contract. The proof helper strips only source/debug additions and verifies the executable instruction payload is unchanged.

## 9. M0 proof capabilities and limitations

| Capability | Proof status | Limitation |
|---|---|---|
| Source digest/generation-bound line breakpoints | Implemented in fixture harness | No actual TS frontend or parser. |
| Pause/resume at safe points | Implemented | Only the fixture interpreter. |
| Step into/over/out | Implemented using source locations and frame depth | Not verified against production statement lowering. |
| Call stack and lexical locals/captures | Immutable DTO proof | No optimized native variables/async stacks. |
| Exception event | Uncaught fixture exception event | No complete caught/uncaught runtime policy. |
| Safe object inspection | Descriptor-only heap model and temporary roots | Not the selected production GC/object model. |
| Core IR → .ogmb source metadata projection | Fixture proof using existing Core IR and source-map contracts | Encoder supplies instruction offsets and physical locations. |
| DAP boundary | In-process command adapter proof | Not a DAP server or client compatibility claim. |
| Release stripping | Proof that source/debug metadata can be removed without changing fixture execution | Production build profiles are not implemented. |

These boundaries are intentional for M0. Native AOT/mixed-stack debugging, async/generator continuation, Hot Reload generations, full expression evaluation, real Runtime Inspector/GC integration, DAP transport and IDE-client acceptance are not claimed by OGM-33's M0 proof.
