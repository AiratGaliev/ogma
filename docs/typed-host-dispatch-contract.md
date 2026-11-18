# OGM-21 — typed host function table and pre-resolved dispatch

**Status:** M0 architectural proof contract
**Implementation:** benchmarks/typed-host-dispatch
**Upstream contracts:** [versioned Rust ABI](rust-abi-contract.md), OGM-20 HostExecutionScope proof

## 1. Purpose and boundary

The proof compares four costs: dynamic host-name lookup, a pre-resolved function-table
slot, a fixed-signature typed thunk, and the generic OgmaHostCallback fallback.
It is deliberately a standalone M0 prototype. It consumes the mock OGM-19 ABI and
OGM-20 owner-thread scope; it is not a production Runtime, interpreter, or AOT backend.

The table is not permission to replace JavaScript property access or [[Call]]
semantics with a direct native call. A fast path is valid only when the caller has
already established the function identity it is about to call. A statically resolved
module import can retain its registered slot; an ordinary-object call-site cache must
guard the receiver layout and the current function-object identity. Proxy property
access always executes the Proxy get trap.

## 2. Registration descriptor v1

The C-compatible descriptor is OgmaHostFunctionDescriptor in
benchmarks/typed-host-dispatch/include/ogma_host_dispatch.h. Its Rust counterpart is
HostFunctionDescriptor. Both layouts are asserted by the Rust unit tests and a C11
compile-time layout test.

| Field | Contract |
|---|---|
| struct_size | Size of the complete descriptor; smaller/unknown layouts are rejected |
| descriptor_version | Descriptor schema version, currently 1 |
| abi_major, abi_minor | Negotiated OGM ABI version; major must match and requested minor must be supported |
| function_id | Host-assigned non-zero HostFunctionId, stable within one ABI major; duplicates are rejected |
| name, name_len | UTF-8 module specifier/export name, copied by registration; at most 128 bytes |
| signature, signature_id | Raw 32-bit signature tag decoded only after validation, plus stable signature identifier; unknown or inconsistent pairs are rejected |
| effects | Bitset: may allocate, GC, throw, suspend, or re-enter |
| required_capabilities | Capability bitset checked on every call, not only at resolution time |
| typed thunk pointers | Optional fixed C-compatible primitive or opaque-handle signature |
| generic_callback | Required versioned OGM callback used for dynamic/generic fallback |
| user_data | Host-owned pointer; must remain valid until unregistration completes |

### v1 type mappings

| Signature ID | Mapping | Typed entry point |
|---|---|---|
| 0 | Generic opaque values / dynamic conversion | OgmaHostCallback |
| 0x1001 | (f64, f64) -> f64 | OgmaPrimitiveF64Thunk |
| 0x1002 | (OgmaHandle, OgmaHandle) -> f64, each handle must contain a Number | OgmaNumberHandleF64Thunk |

Generic callback is required even when a typed thunk is present: the same registration
must remain callable through the versioned generic OGM ABI. Typed pointers use fixed
extern "C-unwind" signatures; arbitrary Rust-ABI function pointers are forbidden.
Descriptor v1 negotiates OGM ABI major 1, minimum minor 0. A future incompatible thunk
or descriptor layout requires a versioned contract change, not an unannounced cast.

user_data lifetime is owned by the host. Unregister removes the callback and
persistent function root before the host may free that data. Unregister returns
Busy while any host dispatch is active, preventing a re-entrant callback from
freeing its own user_data before it unwinds; retry after the active call returns.
The descriptor name is copied; its source storage need not outlive registration.

## 3. Function slot and invalidation

HostFunctionSlot is a 24-byte opaque token containing registry identity, slot index,
slot generation and ABI major/minor. It contains no pointer to an Ogma value, Runtime,
Rust object, or function implementation.

- HostFunctionId is stable for the host registration within the ABI major.
- The table has a monotonic structural table_version, incremented on register and
  unregister.
- A slot generation increments on unregister. Reusing the same index/FunctionId
  returns a new generation; the old token always fails as stale.
- Adding or removing an unrelated slot does not invalidate still-live tokens.
- Resolution by name uses a HashMap once; execution through a retained slot does not
  perform another name lookup.
- A slot is registry-local and thread-affine. A token from another registry is rejected.
- Unregister is rejected with Busy while a dispatch is active; the host may retry
  after the synchronous call stack has unwound.
- The generic mock function is persistently rooted while registered. Unregister removes
  the callback and root and collects the now-unrooted mock function.
- Host-owned values which must survive explicit collection use
  `HostFunctionRegistry::root_handle`. Its `PersistentHandleRoot` guard is tied to
  the registry lifetime and is thread-affine; dropping it releases the persistent root.

The table snapshots the registered thunk, callback and metadata, then releases its
RefCell borrow before invoking external code. No mutable table borrow is held while
user code or a nested callback runs.

## 4. Dispatch and ECMAScript-observable behavior

1. **Static import / valid inline-cache hit.** A call-site guard matches receiver
   identity and object layout, then loads the current function value from the cached
   property cell without a name-based property lookup. It verifies the current
   function-object identity before using the slot. The table checks slot generation,
   ABI version, and capabilities on every invocation before calling a typed thunk.
2. **Function/property replacement.** Replacing a function-valued property changes
   the function identity even when the object's property layout is unchanged. The
   previous call-site guard misses; the current property value is used through the
   generic call path. The old slot is never called on the basis of stale cache state.
3. **Proxy.** Every property access runs the Proxy get trap. The proof always uses
   generic call dispatch for a Proxy receiver; a cached ordinary-object guard cannot
   skip a trap.
4. **Capability denial.** Required capabilities are tested on every invocation,
   including an otherwise valid cache hit. Denial is represented as a JavaScript
   Throw completion, not as an ABI transport error.
5. **Signature mismatch.** Wrong argument count/type becomes a JavaScript
   TypeError completion. It is not confused with OgmaStatus::TYPE_MISMATCH
   leaking as a transport result.
6. **Errors.** OgmaStatus reports ABI/transport failures. OgmaCompletion carries
   Normal or Throw for the generic callback. A callback can therefore return a
   JavaScript throw completion while the ABI call itself returns OK.
7. **Effects.** Primitive typed calls are restricted to descriptors without complex
   effects. May-allocate, may-GC, may-throw and may-re-enter functions use the generic
   path, where the OGM callback and root-scope contract remains in force. May-suspend
   descriptors are rejected: ABI v1 has no suspension/resumption contract.

This proof's ordinary-object/Proxy model covers the guard decision only. It does not
implement the complete ECMAScript property model, function objects, realms, symbols,
accessors, exotic objects, or the full specification for [[Call]].

## 5. Execution scope and ABI reuse

Each dispatch runs inside the caller's existing OGM-20 HostExecutionScope. The scope
is not re-created for every call. The table owns one OGM-19 mock Runtime/Context, uses
opaque handles, roots generic arguments for the duration of the callback, and keeps a
persistent root for each registered generic function. The typed handle thunk reads
values through OGM ABI accessors; it never dereferences a handle as a native pointer.

There is no second runtime, no alternate source of truth for ABI versions, and no
production host-state integration in this proof.

## 6. Benchmark methodology

The benchmark compares:

- raw ogma_primitive_add_f64 ABI baseline;
- dynamic ordinary-object property lookup with generic fallback;
- name lookup only;
- dynamic name lookup plus typed primitive thunk;
- pre-resolved primitive slot;
- pre-resolved typed opaque-handle thunk;
- dynamic name lookup plus generic callback;
- pre-resolved generic fallback;
- guarded ordinary-object call-site cache hit.

Each path reports single-call p50/p95/p99, batch median latency and throughput,
allocation/reallocation counts and bytes, name lookups, dynamic property lookups,
guarded property-cell loads, typed/generic calls, generic fallback rate and guard
counters. Inputs are prepared before timing; warmup
and measured iteration counts are recorded with Rust toolchain, OS, architecture,
CPU label, source revision and tested revision. The CI job emits a JSON artifact.

Generic benchmark paths create temporary mock values. Because the OGM-19 proof heap
uses explicit reclamation, the harness collects it every 128 generic operations
between timed chunks; collection time is excluded from latency/throughput samples,
and the report records collection cycles and interval. Allocation counters include
the collector's own allocations.

The measurements are an isolated M0 comparison on the mock ABI. They are not a
production engine performance claim and must not be extrapolated to an actual
interpreter or native AOT backend.

## 7. Explicit non-goals and follow-up boundary

- No production Runtime, module linker, interpreter, AOT code generator or JS engine
  integration.
- No full ogma-bindgen generator; the benchmark uses representative
  generated-style typed thunks behind the descriptor contract.
- No asynchronous/suspending host functions until the ABI defines resumable calls.
- No changes to the OGM-19 ABI major or the OGM-20 owner-thread execution-scope model.
- Runtime/browser acceptance remains a separate promotion-candidate activity; this
  engineering proof is validated by Rust/C contract tests, benchmark and CI Fast.
