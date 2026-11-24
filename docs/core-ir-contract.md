# Ogma Core IR Contract v1

- **Contract ID:** `ogma.core-ir`
- **Contract version:** `1.0.0`
- **Schema:** [`contracts/core-ir/v1/schema.json`](../contracts/core-ir/v1/schema.json), JSON Schema Draft 2020-12
- **Verifier:** [`.github/scripts/verify_core_ir.py`](../.github/scripts/verify_core_ir.py)
- **Status:** canonical backend-neutral v1 contract. The JSON Schema and Python verifier remain the cross-language reference; `ogma-core-ir::verify_document` is the Rust implementation-level validation boundary used by `ogma-compiler`, and must remain consistent with those references. OGM-42 currently lowers its supported synchronous subset into verified function documents. This contract is not a bytecode format, native ABI or Runtime API.
- **Authority:** this document refines the Core IR requirements in the [Ogma architecture specification](https://app.notion.com/p/Ogma-JS-TypeScript-Execution-Engine-4167ef18f18181bd9b34f9dda764c92d#b5ca462a10c841279d724b74c44b239a), especially sections 2, 4, 5.2, 9.5, 13, 14 and 19.

## 1. Purpose and boundaries

Core IR is Ogma's backend-neutral, function-level semantic boundary between frontend analysis and execution backends. The same verified function representation is the input contract for:

1. **Portable path:** Core IR → bytecode lowering → verifier → portable `.ogmb` → interpreter.
2. **Native path:** Core IR → native lowering → selected code generator → target object → linker.

Both paths must preserve the same ECMAScript-visible values, exceptions, side effects, host calls and source-level provenance, and use the shared Ogma Runtime/ABI contracts. The IR producer and verifier must not depend on Cranelift, LLVM, a register VM, a stack VM, target pointer width, native calling conventions or a particular object layout.

Core IR is not a copy of the JavaScript/TypeScript AST, a bytecode format, a native instruction set, an LLVM-like optimizer, or a production Runtime API. It is deliberately compact: function-level CFG, SSA-style value flow, explicit JS semantic operations, effects/exceptions, closure/environment facts, representation facts and source metadata. TypeScript annotations are not runtime type guarantees. A physical representation is a compiler fact that must be proved/guarded before a backend relies on it.

The contract was established before the production Rust workspace and remains its sole canonical function-level IR boundary. `ogma-core-ir` owns the Rust model and implementation-level checks; `ogma-compiler` owns AST/semantic lowering into this contract. The Python verifier and fixture corpus remain independent reference checks. This does not claim that the portable interpreter or native AOT consumer is complete; those consumers remain owned by their downstream issues.

## 2. Document envelope and versioning

A function document has exactly this top-level shape:

```json
{
  "contract": "ogma.core-ir",
  "version": "1.0.0",
  "function": {}
}
```

The schema rejects unknown envelope fields. The contract version is independent of bytecode, ABI and native-artifact versions. A consumer must reject an unsupported major contract version and must not silently reinterpret unknown mandatory semantics. A compatible additive evolution may be released as a minor version only when older consumers can safely ignore the addition; otherwise use a new major version. The schema `$id` is stable for v1 and is not an implementation-specific backend URL.

All identifiers use the stable ASCII grammar `^[A-Za-z_][A-Za-z0-9_.:-]{0,127}$`. SSA value IDs and effect-token IDs are unique across a function; block IDs and instruction IDs are unique in their own function-wide namespaces. Lexical binding IDs are unique in the environment, capture IDs and imported binding IDs are individually unique, and capture aliases may not collide with local lexical binding IDs or one another. Nullable references (`parent`, `scope`, `suspension`, `effect_entry`, `effect_in` and `effect_out`) use the same grammar when non-null.

## 3. Function and control-flow model

A function declares its parameters, return signature, source-file table, lexical environment, imported captures, entry block, optional effect-entry token and basic blocks.

- Function parameters are SSA definitions available at function entry and dominate every block. Each parameter names its lexical `parameter` binding; type and representation facts must agree.
- Every block has a unique ID, ordered block parameters, an optional exception parameter, an ordered instruction list and exactly one terminator.
- Block parameters are SSA block arguments (the phi-equivalent mechanism). They are not backend registers or stack slots. Every incoming edge passes exactly one argument for each declared block parameter, in order and with exactly matching semantic type and representation.
- The entry block has no incoming edges. Every declared block must be reachable from the entry through normal or exceptional control flow. An entry edge or an orphan block is invalid in v1.
- A `branch` condition is a JavaScript boolean value. JavaScript truthiness must be represented explicitly by `js.to_boolean`; the branch instruction itself must not hide a JS conversion.
- A `return` value must match the function's declared return signature. A `null` return signature means the function returns no value.
- `jump`, `branch`, `return` and `unreachable` are the v1 terminators. `unreachable` has an explicit `reason`: `after_throw` is valid only after a final `js.throw`; `proven_unreachable` records a frontend/compiler proof obligation and must not be used as a substitute for an ordinary branch.

### 3.1 SSA use and dominance invariants

The verifier enforces:

1. Every operand, branch condition, return value and edge argument references an existing SSA value.
2. A value produced by instruction `i` is available only after that instruction completes on its normal continuation; it cannot be read by the same instruction or before it.
3. A value defined in another block may be used only when its definition point dominates the exact instruction/terminator use point.
4. Function parameters dominate all blocks. Block parameters and exception parameters are defined at block entry.
5. Exceptional control-flow leaves at the exact throwing instruction. Its ordinary result is unavailable on the exceptional edge, and values defined later in the source block cannot be read by the handler. Values defined before the throwing instruction may be forwarded as edge arguments.
6. The throwing instruction's `effect_out` token may be forwarded as the final effect argument on its exceptional edge; handlers use their own effect block parameter after entry.
7. Edge argument arity and representation/type facts exactly match destination block parameters. No implicit conversion is permitted on a CFG edge.
8. An effect token is never a JavaScript operand or return value.

The verifier computes dominators over an instruction-level CFG containing normal instruction continuations and exceptional edges from their throwing instruction sites. This prevents block-level dominance from making values defined after a throw site visible inside its handler. This is a structural SSA verifier, not a proof that arbitrary JavaScript optimization transformations preserve semantics; every future transformation must add its own invariants and regression/property tests.

## 4. Value semantics and physical representations

Each JavaScript value definition separates `semantic_type` from `representation`. The representation says how the compiler currently carries a value; it does not change ECMAScript semantics and is not an ABI promise.

| Semantic type | Permitted v1 representations |
|---|---|
| `dynamic`, `undefined`, `null` | `boxed` |
| `boolean` | `boxed`, `i1` |
| `number` | `boxed`, `i32`, `u32`, `f64` |
| `string` | `boxed`, `string_ref` |
| `symbol` | `boxed`, `symbol_ref` |
| `bigint` | `boxed`, `bigint_ref` |
| `object` | `boxed`, `object_ref` |
| `array` | `boxed`, `array_ref`, `object_ref` |
| `function` | `boxed`, `function_ref`, `object_ref` |
| `exception` | `boxed`, `exception_ref` |

These are abstract Ogma representations. `*_ref` means an opaque GC-managed reference, not a raw Rust pointer, C pointer, tagged address or target machine address. `i32`/`u32` are allowed only when semantic analysis proves the value range and its use remains valid; a TypeScript annotation alone is insufficient. `i1` carries an already-computed JS boolean, not a truthiness conversion.

Representation facts travel with parameters, instruction results, block arguments, returns, loop-carried values and environment bindings. A backend may keep an unboxed representation across locals and CFG edges while it remains valid. It must box or invoke the appropriate semantic helper at dynamic operations, runtime/host ABI boundaries or any point where the proof no longer holds. Dynamic JS results from v1 generic operations remain `dynamic/boxed`; a backend must not silently specialize them based on an assumed static type.

A representation conversion must be an explicit semantic operation or a separately specified, verifiable compiler transformation. A backend must not reinterpret a value's representation by changing metadata alone.

## 5. JavaScript semantic operations

The operation name identifies language/runtime semantics, never a backend opcode. Unknown operations are rejected until added to a versioned contract. The v1 operation set is intentionally small and focused on proving the contract boundary. `js.constant` supports `undefined`, `null`, `boolean`, `number`, `bigint` and `string`. `undefined` and `null` use a JSON null payload; booleans and strings use matching JSON types. BigInt uses a canonical base-10 integer string (`0` or an optional minus followed by a non-zero digit and digits). Number uses a finite JSON number exactly representable as binary64, or one of the explicit string sentinels `NaN`, `Infinity`, `-Infinity` and `-0`; these preserve ECMAScript Number values that JSON cannot represent directly, including overflow from a JS numeric literal such as `1e400`.

| Operation | Operand convention | Result | Minimum effect summary |
|---|---|---|---|
| `js.constant` | no operands; attributes `literal`, `value` | type follows literal kind | none |
| `js.add` | `[left, right]`; full ECMAScript `+` semantics | `dynamic/boxed` | allocate, GC, throw, host call, re-enter, mutate shape |
| `js.less_than` | `[left, right]`; ECMAScript Abstract Relational Comparison with left-to-right coercion | boolean | allocate, GC, throw, host call, re-enter, mutate shape |
| `js.greater_than` | `[left, right]`; ECMAScript greater-than comparison with left-to-right evaluation and relational coercion semantics | boolean | allocate, GC, throw, host call, re-enter, mutate shape |
| `js.get_property` | `[object]`; string `attributes.key` (including the empty string) | `dynamic/boxed` | allocate, GC, throw, host call, re-enter, mutate shape |
| `js.set_property` | `[object, value]`; string `attributes.key` (including the empty string) | assigned value's type/representation | allocate, GC, throw, host call, re-enter, mutate shape |
| `js.get_element` | `[object, key]` | `dynamic/boxed` | allocate, GC, throw, host call, re-enter, mutate shape |
| `js.set_element` | `[object, key, value]` | assigned value's type/representation | allocate, GC, throw, host call, re-enter, mutate shape |
| `js.call` | `[callee, thisArg, ...arguments]` | `dynamic/boxed` | allocate, GC, throw, host call, re-enter, mutate shape |
| `js.construct` | `[constructor, newTarget, ...arguments]` | `dynamic/boxed` | allocate, GC, throw, host call, re-enter, mutate shape |
| `js.to_boolean` | `[value]` | boolean | none |
| `js.to_number` | `[value]` | number | allocate, GC, throw, host call, re-enter, mutate shape |
| `js.to_string` | `[value]` | string | allocate, GC, throw, host call, re-enter, mutate shape |
| `js.strict_equal` | `[left, right]` | boolean | none |
| `js.typeof` | `[value]` | string | none |
| `env.load` | no operands; `attributes.binding` identifies a local/captured binding | binding's type/representation | throw (for TDZ and applicable lexical errors) |
| `env.store` | `[value]`; `attributes.binding` identifies a mutable local/cell capture | assigned value's type/representation | throw |
| `closure.create` | no operands; attributes `function`, `captures` | function | allocate, GC |
| `host.call` | host arguments; `attributes.symbol` is a stable host capability name | `dynamic/boxed` | allocate, GC, call host, re-enter, mutate shape, throw |
| `async.await` | `[awaitable]` | `dynamic/boxed` | allocate, GC, throw, suspend |
| `js.throw` | `[exceptionValue]` | no result | throw |

The minimum summaries are conservative lower bounds, not exact effect summaries. Producers may set additional flags, but may not omit an effect that the operation can exhibit. In particular, generic property access/calls can invoke accessors, proxies, user code and host-backed objects; generic `+` and `<`/`>` comparisons can invoke `ToPrimitive`, call user code and allocate strings. A backend may use a narrower operation only after a separate proof/guard with a correct generic fallback.

`js.call` explicitly carries the receiver (`thisArg`) and `js.construct` explicitly carries `newTarget`; backends must not reconstruct these from syntax or guess them from the callee. Computed property keys use `js.get_element`/`js.set_element`; named property operations carry an explicit string key. `host.call` names a capability-bound host symbol, not an arbitrary native address.

### 5.1 Closure and lexical environment facts

Every function carries an `environment` descriptor: a stable lexical environment ID, optional parent environment ID and declared bindings. Each binding specifies name, declaration kind, mutability, semantic type and representation. `var`, `let`, `parameter` and `catch` bindings must be mutable; `const` and `import` bindings must be immutable. The verifier enforces both directions rather than only rejecting mutable `const`/`import` bindings. A lexical environment cannot name itself as its parent. The binding kind `implicit` is not in v1; implicit function state must be represented by explicit frontend semantic operations until a compatible extension defines its capture rules. Function `captures` describe bindings imported from an outer lexical environment and identify their source environment, source binding and capture mode:

- `cell`: shared mutable lexical storage; mutations remain observable by all closures sharing the cell.
- `value`: immutable captured value copied into the closure environment. A mutable binding cannot use this mode.

`env.load` and `env.store` reference stable binding IDs, not backend frame slots or object offsets. `env.store` cannot target immutable bindings or by-value captures. Mutable `dynamic/boxed` bindings are polymorphic JavaScript slots: `env.store` may accept any valid SSA value representation and performs the required runtime boxing at the environment boundary. Statically represented mutable bindings still require an exact semantic-type/representation match. `env.store` always returns the assigned value's original type/representation. A `var` binding is initialized to `undefined` on entry to its owning function environment; a `let` binding remains in TDZ until its declaration's initializing store executes. Immutable `const` values are represented by their unique SSA definition; the initializer's source metadata retains the binding ID, and closure-by-value capture refers to that ID. A compiler must preserve the definition/use association and must reject any transformation that would permit a const read before initialization without retaining the TDZ check. `closure.create` names a stable target function ID and lists each captured binding with its capture mode; the capture list must be an array of exact `{binding, mode}` descriptors, where `mode` is `cell` or `value`. One imported lexical binding may be captured only once, including through an alias. Mutable bindings must be captured by cell; an imported by-value capture cannot later be promoted to a shared cell. Function and binding IDs are metadata identifiers, not object addresses.

## 6. Effect and exception contract

Every instruction declares exactly seven boolean effects:

`may_allocate`, `may_gc`, `may_throw`, `may_suspend`, `may_call_host`, `may_reenter`, `may_mutate_shape`.

These flags are independent semantic/compiler facts. For example, allocation may trigger GC but the flags are distinct; an explicit collection opportunity need not allocate. A producer must conservatively summarize behaviour, not merely the current fast path. Effects constrain code motion and specialization, root-map/safepoint placement, exception handling, callback reentrancy and shape-dependent fast-path invalidation.

### 6.1 Effect SSA chain

A function with at least one effectful instruction has an explicit effect-token chain:

1. Every block declares exactly one final block parameter with `kind: "effect"`.
2. `function.effect_entry` names the final effect parameter of the entry block.
3. Every instruction with any true effect flag consumes the current token through `effect_in` and defines the next token through `effect_out`.
4. A pure instruction has both effect fields null and leaves the current token unchanged.
5. Every normal outgoing edge forwards the current token as its final block argument.
6. A throwing instruction with a local handler forwards its newly defined `effect_out` as the final exception-edge argument. A `propagate` exception flow exits the function's current exceptional continuation and needs no local handler edge.
7. A pure function has no effect parameters, no `effect_entry` and no instruction effect tokens.

This creates an explicit ordering dependency for stateful/observable operations without pretending that all operations mutate one global memory cell. It gives later backends a shared place to reason about effect ordering, GC roots and reentrancy.

### 6.2 Exception flow

An instruction with `may_throw=true` must declare exactly one of:

- `{ "kind": "propagate" }` — the exception leaves the current function through its caller-visible exceptional completion; or
- `{ "kind": "edge", "target": "...", "arguments": [...] }` — control transfers to a local handler block.

A local handler block declares `exception_parameter` with type `exception/exception_ref`; only exception edges may enter that block. Its ordinary block parameters are still passed explicitly by the exception edge. If the function uses effects, the effect token is the last ordinary block parameter and the throwing instruction's `effect_out` must be passed there. An instruction with `may_throw=false` cannot declare exception flow.

`js.throw` is a semantic operation: it has no result, must declare `may_throw`, and must be the final instruction before an `unreachable` terminator with reason `after_throw`. A thrown JS value is not implicitly converted to a Rust panic or allowed to unwind through the host ABI.

## 7. Source/debug metadata

Every instruction and terminator carries source metadata. A source record has:

- `origin`: `user` or `generated`;
- `file`, `start`, `end` for user-authored spans; a file ID resolves through the function's `source_files` table;
- `scope`: lexical environment ID or null when no single lexical scope applies;
- `bindings`: stable lexical binding/capture identities relevant to the node;
- `suspension`: stable suspension-point ID or null;
- `inline_chain`: ordered inlining provenance frames (inlined function ID plus call-site file/span), nearest inlining call site first.

Source spans use one-based lines, zero-based UTF-16 code-unit columns and an exclusive end position. File paths are normalized repository-relative POSIX paths. Source file IDs are local to a function and must not be guessed from filenames.

Compiler-generated instructions use `origin: "generated"` and a non-empty `reason`; they must not be assigned a fabricated user span. They may still retain lexical scope, binding identities and suspension provenance. `async.await` requires a stable suspension-point ID that is unique within the function; other v1 operations and terminators must leave it null. `host.call` names a stable capability identifier, not an address or free-form native symbol.

Source identity/spans, scope, binding identity, inline provenance and suspension provenance are first-class metadata. A semantics-preserving transformation must preserve or correctly remap them. If it cannot map a synthesized instruction to a precise user span, it marks that instruction as generated and records why. Future inline provenance/source-map chains can be added by a compatible schema extension or a new contract version; they must never be reconstructed heuristically after lowering.

## 8. Backend neutrality and lowering obligations

The v1 schema intentionally has no backend, target, native opcode, register number, stack slot, pointer, object layout or machine address field. Unknown fields are rejected in the envelope, function, block, instruction and source structures. Operation attributes are validated against each operation's explicit v1 attribute set.

Bytecode stack-vs-register encoding is a separate VM-backend decision. Cranelift CLIF and LLVM IR are native-lowering targets, not Ogma Core IR. The same Core IR function must be lowerable by both execution paths without rewriting JS semantics between them. Runtime helpers/operations and shared Runtime invariants own observable JS behaviour; backend instruction handlers must not independently redefine it.

A backend lowering is responsible for:

- preserving values, exceptions, side effects, host interactions, receiver/new-target semantics and suspension behaviour;
- respecting effect-token order and exception edges;
- preserving representation proofs or inserting explicit conversions/guards;
- forwarding closure/environment identity and capture modes;
- keeping source/scope/binding/suspension provenance through bytecode PCs or native debug metadata;
- using a shared Runtime/GC/root/ABI contract rather than backend-private JS semantics.

This contract and verifier do not claim that interpreter/AOT prototypes already exist. The OGM-31 proof must consume the same positive fixtures through both paths and compare observable outputs, exceptions, side effects and host calls. It must not introduce a semantic translation layer between two different IR dialects.

## 9. Verifier and diagnostics

Run the dependency-free verifier from the repository root:

```bash
python .github/scripts/verify_core_ir.py --manifest tests/core-ir/manifest.json
python .github/scripts/verify_core_ir.py --input tests/core-ir/fixtures/positive-dynamic-exception.json
python .github/scripts/verify_core_ir.py --manifest tests/core-ir/manifest.json --report .artifacts/core-ir-report.json
```

The corpus manifest defines expected acceptance/rejection and, for negative cases, required diagnostic codes. The report is deterministic JSON containing contract/verifier versions, a per-fixture result, structured diagnostics (`code`, JSONPath-like `path`, `message`) and summary totals. Exit code `0` means all fixture expectations matched; `1` means a fixture was unexpectedly accepted/rejected or an expected diagnostic was absent; `2` means verifier invocation, manifest or JSON input failed to load.

The verifier checks JSON Schema structure plus semantic invariants: stable ID references, declaration-kind/mutability consistency, canonical literal payload encodings, unique capture/suspension identities, supported operations and operand counts, operation attribute contracts, effect lower bounds and token continuity, required exception flow, valid CFG/reachability/instruction-level dominance, block-argument arity/types, handler shape, closure/environment references, source identity/spans/binding/scope/suspension metadata and function returns. It rejects duplicate JSON object keys, non-finite JSON numbers and malformed attribute payloads without crashing.

The fixture corpus contains 29 cases (8 accepted and 21 rejected). It includes positive SSA/branch, dynamic JS exception/effect, closure/capture/source and async-suspension examples; negative malformed CFG, instruction-level exceptional dominance, edge-type, under-reported effects, unsupported representation, missing exception-flow, invalid source spans/IDs, mutability mismatches, literal payloads, duplicate captures/suspensions and malformed operation attributes. The corpus is a focused contract regression suite, not an ECMAScript conformance suite, optimizer proof, runtime fuzz campaign or backend equivalence test.

## 10. Compatibility and change policy

- Update the schema, this document, verifier and fixtures together for any v1 contract change.
- Keep existing v1 semantics stable. Do not reuse an existing operation name with different ECMAScript behaviour or weaken an effect/exception guarantee.
- Add a fixture for every newly supported semantic case and a negative fixture for every newly rejected malformed case.
- An additional operation/representation/effect semantic that old consumers may not safely ignore requires an explicit compatibility decision; do not silently extend the operation registry.
- A backend-specific encoding decision belongs in its own contract and must not leak into Core IR.
- Production Rust crate boundaries remain deferred until real compile/runtime API boundaries require them; this proof does not create empty `ogma-ir` crates or start M1.

## 11. Current implementation limits

OGM-42 now provides an Oxc-backed owned frontend semantic snapshot and syntax projection, a Rust-owned Core IR v1 model/verifier, and a compiler lowering subset that verifies each emitted function before returning it. Fast CI cross-checks the canonical Python fixture verifier, Rust workspace tests, the parser adapter and a bounded pinned parser-corpus smoke.

This is not full ECMAScript lowering. Unsupported syntax and function forms fail closed with source-positioned diagnostics. The initial synchronous subset does not lower async/generator suspension, destructuring, `try`/`catch` handler regions, `break`/`continue`, optional calls/members, update expressions, dynamic unary negation, unresolved globals or Node-specific module semantics. Per-block/per-iteration closure environments are not materialized; captures whose lifetime cannot be represented by the v1 function environment are rejected. The current IR uses function-environment `source.scope` plus stable `source.bindings`; it does not serialize Oxc block-scope IDs into Core IR v1.

TDZ coverage for immutable `const` bindings is intentionally narrower than for mutable lexical bindings. A `let` read is represented by `env.load` and retains the runtime TDZ check. A `const` value is represented by its initialized SSA definition; a read before that definition is available fails lowering with `lowering.const_tdz`. Likewise, a hoisted closure created before the initializer of a captured `const` fails with `lowering.const_capture_tdz`, because Core IR v1 has no operation for loading an uninitialized immutable SSA binding or capturing its temporal state. These are valid JavaScript source patterns, but this lowering subset rejects them rather than emitting IR with incorrect observable semantics. The compile-time diagnostic is not a claim that ECMAScript runtime `ReferenceError` behaviour has been implemented for these `const` cases. Supporting them requires a compatible Core IR/Runtime semantic contract and regression fixtures; this task does not silently extend v1.


This task does not implement Core IR consumers, portable bytecode execution, native AOT, optimization preservation, Test262 conformance or cross-backend equivalence. Those remain owned by downstream issues. Full Test262, differential, fuzz and promotion suites stay outside topic PR Fast CI as required by the repository workflow.
