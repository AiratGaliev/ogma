# M1 register-bytecode interpreter contract

- **Implementation issue:** [OGM-46](https://linear.app/ogma-engine/issue/OGM-46/implement-the-register-bytecode-interpreter-and-m1-execution-semantics)
- **Artifact:** OGMB 1.0 with ogma.register.m1.v1
- **Runtime boundary:** ogma-interpreter → ogma-bytecode + ogma-runtime
- **Status:** initial M1 synchronous execution profile. This is an implementation contract for the current supported subset, not a full ECMAScript conformance statement or a frozen production ISA.
- **Architecture authority:** the Ogma architecture specification and [Core IR Contract v1](core-ir-contract.md). This document describes the current consumer boundary and does not redefine Core IR semantics.

## 1. Responsibility and dependency boundary

ogma-interpreter is the portable execution consumer for verified OGMB function artifacts. Its normal production dependencies are limited to:

- ogma-bytecode: container loading, registered register decoder, structural verification, typed instructions and manifest-derived entry-register layout;
- ogma-runtime: JavaScript Value handles, heap/GC roots, lexical environments, bindings, closures, objects, property operations and call activations.

The interpreter must not depend on ogma-compiler, ogma-frontend or ogma-core-ir in its normal dependency graph. It does not accept AST or Core IR input and does not evaluate source text. A compiler or frontend may be used by a separate host/tooling layer to produce artifacts, but execution always starts from serialized .ogmb bytes.

Every artifact is untrusted input. ExecutableModule::load calls verify_register_artifact_from_manifest for every artifact before admitting it. That path validates the complete OGMB v1 container, manifest, resource declarations, code digest, registered encoding, decoded operation stream, CFG, register bounds/definite initialization and verifier summary. Typed executable instructions are produced only after the existing verifier accepts the artifact. Callers cannot supply or mutate an alternate decoded instruction stream.

A module consists of one or more independently verified function artifacts. Function IDs and lexical environment IDs must be unique. References to a closure target must resolve inside the module. A capture must resolve to a binding owned by its declared source environment, and that binding must be visible from the immediate lexical parent as either a local binding or a forwarded capture. Invalid or incomplete module metadata is rejected before dispatch.

The interpreter does not treat verifier acceptance as a general sandbox. The host remains responsible for selecting trusted inputs, setting resource limits and explicitly registering capabilities. Native AOT code is outside this interpreter boundary.

## 2. Execution and frame model

The M1 VM uses a register frame per active function and an explicit VM frame stack. Calls do not recursively invoke the Rust interpreter. Each decoded instruction carries its code-section byte offset, encoded size, typed operation and verified exceptional-effect flag. Branch destinations are byte offsets and are mapped to verifier-accepted instruction boundaries before a program counter is changed.

The entry-register layout is derived from manifest metadata and re-verified: parameters occupy the initial register range, followed by captures in manifest order. Before allocating the entry environment or closure, the interpreter roots all supplied arguments and `global_this` in a temporary setup scope; this protects newly allocated host Values if setup allocations trigger GC under quota pressure. Function arguments are then copied into those parameter registers. The emitted StoreBinding operations initialize lexical parameter bindings in the shared Runtime environment. Runtime activation roots preserve arguments and the closure environment for the call lifetime.

The VM register file distinguishes four internal states:

- JavaScript Value handles managed by the shared Runtime;
- compiler metadata tokens (for example function, binding and host-capability identities);
- opaque lexical cell identities used by closure lowering;
- uninitialized registers.

Tokens and cells are VM metadata, not fabricated JavaScript values or heap pointers. Reading an uninitialized register fails closed. Register bounds, constant references and normal control-flow edges have already been checked by the verifier and are defended again at dispatch boundaries.

## 3. M1 operation coverage

The registered register decoder currently exposes the following operation families to the VM:

| Family | Implemented behaviour |
|---|---|
| Constants | undefined, null, booleans, binary64 numbers, strings and compiler metadata tokens. BigInt constants are rejected while loading a module. |
| Unary | Copy, JavaScript truthiness, primitive number/string conversion, typeof and lexical binding load. |
| Binary | Addition, strict equality and relational less-than/greater-than for the currently represented primitive values. |
| Control flow | Direct jump, conditional branch, normal return and propagated throw. |
| Calls | Calls to bytecode functions in the loaded module, constructor calls within the supported ordinary-function profile, and explicit host capabilities. |
| Properties | Runtime-backed property get/set for property-key and element access operations. |
| Closures | Closure creation, mutable cell capture, immutable value capture, capture load/store and lexical binding initialization/store. |

The VM delegates JavaScript-visible heap objects, identity, property storage, prototype links, lexical environments and closure storage to ogma-runtime. It must not introduce a second object heap or expose raw GC addresses in register state. Core IR represents immutable `const` values as SSA definitions; the compiler emits an explicit copy into the corresponding binding register, and the VM initializes the Runtime's immutable lexical slot at that point so a later by-value closure capture observes the same value.

Compiler-produced M1 artifacts are the compatibility target. The independent compiler/Core IR fixtures remain the semantic reference. When an operation cannot be faithfully represented by the current Runtime, metadata or execution profile, the loader or VM must reject it instead of silently dropping its semantics.

## 4. Completions, exceptions and host calls

An invocation produces one of three externally observable completions:

- Return(Value): normal function completion;
- Throw(Value): a JavaScript thrown value propagated through VM frames;
- Interrupted(reason): cooperative fuel, cancellation or deadline interruption.

A child function's return value is written to its caller's destination register. A constructor call returns an explicit object/function result when supplied; otherwise the newly allocated instance is returned. A child throw propagates out of the current invocation after all VM frames and pending root scopes are unwound.

A top-level Return/Throw completion carries a transient Runtime Value after the invocation frames have been released. The caller must immediately promote any managed result with `Heap::retain_host_value` before performing another allocation or any other operation that may trigger collection. The interpreter does not silently create a persistent host root or extend result lifetime beyond the completion boundary.

OGMB v1 M1 compiler output does not include local catch/finally handlers. The bytecode verifier rejects exception-region records for this profile; Throw is therefore a propagated completion, not a locally caught exception. This does not claim complete JavaScript try/catch/finally, Error construction, stack formatting or host exception translation.

Host effects are explicit and named. Host::call receives the capability identifier and evaluated arguments on the Runtime owner thread. The default NoHost denies every capability. A host may return a value, deliberately propagate a thrown value or deny the call. The interpreter does not grant ambient filesystem, process, network or other OS authority. Host callbacks are synchronous; a callback that blocks cannot be pre-empted by the VM's cooperative interrupt checks.

Runtime errors that currently arise from unsupported operations or invalid environment state are reported as interpreter/runtime errors where the current Runtime API does not yet expose them as JavaScript Throw values. This is a known boundary, not a claim of complete abrupt-completion equivalence.

## 5. Resource accounting and cancellation

ExecutionOptions defines:

- one instruction-fuel budget shared by every frame in the invocation;
- a host call-depth ceiling, combined with the declared limit of each artifact;
- an optional thread-safe cancellation token;
- an optional absolute monotonic deadline or relative time budget;
- a positive polling interval for ordinary instruction dispatch.

Each dispatched instruction consumes exactly one unit of fuel. Exhaustion is reported before the next instruction is dispatched. The VM checks cancellation/deadline at function entry, before and after calls and potentially allocating/runtime operations, at normal/throw function exits and at loop back-edges, and polls ordinary operations at the configured interval. A request made while a synchronous host callback is running is observed immediately after the callback returns; the VM cannot pre-empt the callback itself. The VM does not use wall-clock timestamps for duration accounting.

Fuel, cancellation and deadline are distinct completions. Interruption unwinds child and parent activations and their pending root scopes before returning to the host. The Runtime remains owner-thread-affine: the cancellation token communicates only an atomic request; another thread must not inspect or mutate VM frames or heap state.

The polling interval bounds dispatches between ordinary cooperative polls, not wall-clock latency. A synchronous host callback may delay the next poll. Hosts requiring a hard real-time bound or isolation must enforce it outside the VM, for example by using a separately controlled worker/process and host-specific quotas.

## 6. Source and debug metadata

The module retains the original verified manifest and optional source map. source_location resolves a verified code offset to source provenance, code-generation identity/version, lexical scope ranges and variable-location ranges when those records exist. ExecutionReport includes the last dispatched location.

This is a metadata-preservation and lookup surface for future debugger integration. It does not implement the backend-neutral DebuggerPort, breakpoints, pause/step commands, DAP transport, Inspector or a production debugging session. See [Debugger API Contract](debugger-contract.md).

## 7. Explicit current limitations

The initial interpreter profile intentionally does not claim:

- BigInt values or operations (the shared Runtime Value model does not currently represent BigInt);
- complete object-to-primitive conversion for addition, relational comparison, ToNumber, ToString or property keys; object coercion that is not implemented fails closed;
- full ECMAScript Number-to-string formatting equivalence for every binary64 edge case;
- complete this semantics for strict/sloppy functions, bound functions, arrow functions or lexical this; OGMB v1 metadata does not yet preserve all required function-kind/strictness flags, so this implementation uses the current ordinary strict function definition;
- local catch/finally handlers or complete JavaScript Error objects/stack traces;
- async functions, Promises, generator suspension/resumption, host-pump scheduling or microtask semantics;
- accessors, Proxy traps, complete prototype/exotic object semantics or every ECMAScript built-in/intrinsic;
- top-level ES module loading/linking, Node.js module resolution/APIs, ambient I/O or the complete ogma CLI;
- full Test262, differential conformance, fuzzing, browser acceptance or production readiness.

These limitations are versioned execution-profile boundaries, not reasons to weaken the bytecode verifier. A future extension must be represented by the appropriate Runtime, Core IR, metadata/bytecode and host contracts before it is enabled.

## 8. Required verification for changes to this boundary

Changes to loading, decoding, execution, Runtime calls, closures or resource policy should include focused tests for the affected contract. The interpreter integration suite executes the canonical OGM-31 Core IR branch, loop-carried-value and host-call fixtures through the Rust compiler, serialized OGMB and verifier; it also exercises ordinary/nested calls, compiler-produced mutable and immutable closure captures, Runtime property access, propagated throws, observable host capability traces, malformed-container rejection, fuel exhaustion, pre-cancelled entry, cancellation after a nested host callback, deadline expiration, call-depth rejection and root cleanup after interruption/resource failure.

The repository Fast CI is the required feature-PR gate and runs Rust formatting, Clippy and workspace tests, as well as the existing bytecode verifier/reference corpora and dependency-boundary check. Do not run or claim full Test262/differential/browser acceptance on a feature PR. Runtime conformance promotion remains a separate follow-up after the runtime adapter exists.
