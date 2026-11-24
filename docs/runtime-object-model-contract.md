# Runtime object, function and lexical-environment contract (OGM-44)

**Status:** M1 Runtime semantic substrate. This contract is implemented in
`ogma-runtime`; bytecode execution and end-to-end JavaScript observability are
owned by OGM-46 and must not be inferred from these unit-level APIs.

## 1. Ownership and backend boundary

`ogma-runtime` owns the canonical heap object model. It has no dependency on
frontend, compiler, Core IR, bytecode, interpreter, native backend or host ABI.
Backends call the same Runtime operations for property lookup/mutation, array
length, lexical binding access, closure creation and call activation. A backend
must not duplicate these semantics in opcode handlers or native stubs.

Managed references remain internal generation-checked `GcRef` values. Public
Runtime APIs expose opaque `Value`, `PropertyKey` and
`LexicalEnvironment` handles. A `HostHandle` is reserved for persistent
host ownership and is not used for object graph edges or property access.

## 2. Property keys and descriptors

M1 property keys are interned UTF-16 string atoms scoped to one heap. Repeated
interning of identical code units returns the same key; unpaired surrogates are
preserved. A key from a different heap is rejected. Symbols are not represented
in this M1 Value model.

The supported descriptor surface is data-only:
- `value`, `writable`, `enumerable` and `configurable` are independently
  optional on partial descriptor updates;
- absent fields on a new property default to `undefined/false/false/false`;
- assignment-created properties default to writable, enumerable and
  configurable;
- non-configurable properties enforce enumerable/configurable invariants, and
  non-writable properties reject non-SameValue changes;
- delete rejects non-configurable own properties and otherwise removes the
  property edge from the GC graph.

Accessor descriptors and accessor invocation are explicitly unsupported.
Operations return a rejected boolean or `HeapError`; the strict assignment
helper converts a rejected write into a Runtime error. The interpreter maps
that error to the ECMAScript completion/exception contract.

## 3. Ordinary objects and prototypes

Objects, arrays and functions carry a common own-property store and optional
prototype reference. Prototype references are traced GC edges. Lookup and
presence checks walk the prototype chain and detect cycles rather than looping
indefinitely. `OrdinarySet` accepts an explicit receiver: an inherited
writable data property creates/updates an own property on the receiver without
mutating the prototype's slot; an inherited or own non-writable property
rejects the write.

Prototype changes reject cycles and changes to non-extensible targets. New
object cells start with a null prototype; Runtime/bootstrap code attaches the
selected intrinsic prototype explicitly. This keeps allocation backend-neutral
and does not pretend that Object/Array built-in prototype objects or their
methods are already installed.

Own-key ordering places canonical array-index strings first in ascending
numeric order, then other strings in insertion order. Array `length` is a
non-enumerable, non-configurable own property and is listed after index keys.
The object model has no Proxy trap path or accessor fallback; generic means the
canonical descriptor/prototype algorithm for the supported data-property
surface, not full ECMA-262 object exotic behavior.

## 4. Arrays

Arrays use sparse indexed properties, not dense vectors. Missing indices remain
holes and read as `undefined`, but are distinguishable from present own
properties. The `length` property is always present, non-enumerable and
non-configurable. Defining an index extends length; deleting an index leaves a
hole and does not shrink length. Shrinking length removes configurable indices
in descending order and stops at a non-configurable index with the required
partial-shrink result. Invalid lengths fail with `InvalidArrayLength`.

The array index range is the ECMAScript array-index range
0..=4294967294; `4294967295` is an ordinary string key. Dense iteration,
Array.prototype methods, species, iterators and other built-ins are not claimed
by this Runtime substrate.

## 5. Functions, calls and lexical environments

A function object stores stable function-table identity, function kind,
strictness, lexical receiver (for arrows) and a traced closure environment.
Function IDs are metadata, not native code pointers. The backend function table
owns executable bodies; Runtime creates closures and prepares activations.

A call activation:
- roots callee, receiver, global receiver and arguments in an explicit GC
  frame before allocating the activation environment;
- binds parameters in source order, using `undefined` for missing arguments;
- creates fresh local binding storage with the captured environment as parent;
- supplies the function ID and effective `this` value to the backend;
- remains rooted until `leave_function_call` closes the frame on normal or
  abrupt completion.

Ordinary strict functions preserve the supplied `this`. Sloppy ordinary
functions replace nullish `this` with the supplied global receiver. Sloppy
primitive boxing is not yet implemented and fails closed with
`UnsupportedCoercion`. Arrow functions ignore the call-site receiver and use
the receiver captured at closure creation.

Mutable lexical bindings use independently managed shared cells, so closures
share later stores. Immutable bindings are copied by value. Uninitialized
bindings remain in TDZ; `initialize_binding` performs the one-time
initialization, `load_binding` checks TDZ, and `store_binding` only updates
already-initialized mutable bindings. Missing identities fail closed. Parent
environment and capture edges are traced by the common collector.

## 6. Heap accounting and roots

Each object, array, function, lexical environment and binding cell has a
separately allocated fixed payload behind the compact managed-cell header.
The fixed payload size is charged before the cell is published and released
when that cell is swept. Property arrays, lexical binding arrays,
property-key atom storage and object graph edges are reserved through the
existing Runtime quota budget. The collector releases each managed cell's
property/binding/edge capacity when sweeping it. Property-key atoms are
interned for the lifetime of the heap and their capacity remains charged until
the heap is dropped.

New environments and returned closures are not implicit host roots. Callers
must publish an environment using `scope_root_environment` or
`frame_root_environment`, and publish returned object/function values through
the existing scope/frame root APIs before any allocation that may collect.
`enter_function_call` manages its activation frame and must be paired with
`leave_function_call` for both normal and abrupt execution.

## 7. Deliberate exclusions

This contract does not claim accessor properties, Symbols, Proxy/Reflect,
classes/private elements, primitive wrapper objects, Object/Array built-in
methods, WeakRef/finalization, async/generator continuations, or full
ECMAScript 2026 conformance. The compatibility profile remains the authority
for feature support. OGM-46 must add interpreter-level regressions before any
surface is promoted beyond this Runtime substrate.
