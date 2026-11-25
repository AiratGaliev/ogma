# Rust embedding ABI — M0 proof and M1 production boundary

## Authority and scope

This document keeps two distinct evidence sets together without conflating their guarantees. The historical M0 proof lives in `benchmarks/rust-abi/` and uses a mock value store/callback dispatcher. The production synchronous M1 embedding implementation lives in `crates/ogma-embedding/` and uses the verified OGMB interpreter and the real Runtime heap. The M0 statements below apply only to that mock proof; they do not describe the current repository state or replace the production M1 contract later in this document.

This document is not a new permanent architecture authority and does not freeze capabilities beyond the implemented versioned contract. The authoritative requirements remain the Ogma Notion architecture specification, especially sections 0.5 and 8.1–8.9.2. If either implementation or proof conflicts with that specification, the specification wins and the discrepancy must be resolved explicitly.

## M0 proof: Versioning and compatibility

The proof advertises ABI version **1.0.0** and a feature bitset. Consumers negotiate a required major, minimum minor and required feature mask before creating a runtime.

- Major mismatch is incompatible.
- A consumer requiring a minor newer than the implementation is incompatible.
- Unknown required feature bits are rejected.
- Within ABI major 1, a compatible minor revision may add optional exported functions or feature bits while preserving every existing struct layout, token representation, function signature/calling convention, status/completion value and ownership/lifetime rule. It must not append fields to an existing C struct.
- `OgmaAbiVersion` is a fixed 24-byte ABI-v1 record. Its `struct_size` field is a layout sanity marker, not an output-buffer capacity; it does not authorize a newer library to write a larger record into storage allocated by an older consumer. Additional manifest metadata requires a separate size-negotiated query/descriptor (or a new major-version API with distinct symbols).
- A change to an existing field's meaning, token contract, completion convention, calling convention or ownership/lifetime semantics is incompatible and requires a major revision.
- Patch revisions are restricted to fixes that do not change the binary contract.
- Reserved fields are zero in v1.0.0.

The returned v1 descriptor reports its exact fixed size so consumers can verify that the expected record was returned. Future metadata must use a separately size-negotiated descriptor/query rather than extending this record in place. All exposed integer fields have explicit widths. `size_t` is used only for pointer-length pairs and therefore follows the target pointer width. This prototype does not yet implement a complete target-triple/endianness/native-entry manifest because it has no bytecode or AOT artifact contract.

Feature bits are: opaque handles, root scopes, persistent handles, callbacks, explicit completions and MOCK_PROOF. The last bit is deliberately present so a caller cannot mistake the proof harness for an executable JS Runtime.

## M0 proof: ABI surface and representation boundary

The C header is benchmarks/rust-abi/include/ogma_abi.h. OgmaRuntime is an incomplete opaque type. Context, value, root-scope, persistent and callback tokens are fixed-layout structs containing five 64-bit opaque words. The words are private in Rust and have no supported public encoding.

A token is an identity/lifecycle capability, not a pointer. It is validated against runtime identity, context identity/generation, slot index and slot generation. It must not be inspected, persisted across runtime destruction, modified or synthesized by a host. A token from another Runtime/Context or a released/recycled slot is rejected as stale.

The ABI does not expose:
- internal Rust Value/JsValue representations;
- JS object, string, function, heap-cell or GC layouts;
- compiler, parser, IR, VM frame or stack-map layouts;
- raw pointers to JS objects or runtime-owned heap cells.

The opaque runtime owner pointer is valid only as the handle returned from runtime creation. Host user_data is a host-owned pointer passed back unchanged to the callback; it is not an Ogma object reference.

## M0 proof: Threading and lifecycle

Each Runtime captures its creating thread. The safe Rust representation is !Send + !Sync; every C ABI entry verifies the owner thread before touching mutable runtime state. Different runtimes have independent identities and may be owned by different host threads. A single Runtime is not concurrently callable from multiple threads. Cross-thread work is intentionally not implemented here; the architecture requires owned messages/payloads rather than shared mutable JS values.

Destroying a Runtime from inside an active API/callback call returns Busy. Context destruction is also rejected while a callback is executing or a root scope is open. Runtime destruction consumes the unique pointer slot and writes null to it. The host must not duplicate the runtime owner pointer or call through a pointer after destruction.

## M0 proof: Values, conversions and roots

The proof currently models only Number(f64), Boolean and Function tokens. This is enough to exercise type checking and identity/lifecycle; it is not a full JS Value model.

- get_number accepts only Number.
- get_boolean accepts only Boolean, represented as uint8_t restricted to 0/1.
- get_i32 converts a Number only when finite, integral and within the i32 range; it never truncates or wraps.
- Primitive f64 arithmetic is a direct ABI call with no handle conversion.
- Bulk sum validates each handle and its type before returning a result.

A value handle is non-owning. It does not keep a value alive. The host uses explicit root scopes for temporary values and persistent roots for values retained beyond a scope.

Root scopes are strictly LIFO. A scope roots only the handles added to it and is closed exactly once. Closing an outer scope while an inner scope is active returns Busy. Persistent creation promotes a transient handle into a host-owned root; persistent_get resolves it, and persistent_release relinquishes that root. Once no scope, persistent root or active call roots the value, the mock collector invalidates the slot and advances its generation. Stale use returns StaleHandle.

This deterministic collector is only a lifecycle fixture. It does not select or model the production GC algorithm.

## M0 proof: Scoped borrowing, ownership and callbacks

ogma_context_call receives a pointer/length pair for the argument token array. The array is copied into a call frame; argument values and the callable are rooted for the duration of the callback. The callback args pointer is borrowed only until it returns. It must not retain that array pointer or a transient handle after return unless it creates a persistent root during the callback.

user_data remains host-owned. The host must keep it valid until callback unregistration and must arrange its own synchronization/lifetime. Unregistering invalidates the callback token; functions referring to it fail with StaleHandle.

The call path deliberately drops internal collection borrows before invoking user callback code. Nested ABI calls are allowed and have an explicit depth limit of 32 in this proof. Crossing the limit returns ReentrancyLimit; it is not silently converted to a JS exception. No Rust mutable borrow or heap registry lock is kept across callback execution.

The callback-backed mock function is a bridge fixture, not a JavaScript function implementation. It demonstrates the shape of a host → callable → registered Rust callback → host completion roundtrip. A real interpreter must later connect JS call evaluation to the same versioned host contract.

## M0 proof: Completion, errors and panic containment

Transport/ABI failures are represented by OgmaStatus, a transparent uint32_t status code. Script completion is represented separately by OgmaCompletion:

| Kind | Meaning |
|---|---|
| Normal | Successful completion with an optional value handle |
| Throw | JS-style thrown completion with a required value handle |
| Return | Return completion |
| Break | Break completion |
| Continue | Continue completion |

A callback can return Ok with a Throw completion; the ABI call itself succeeds and the caller inspects the completion kind. A host callback/transport failure returns a non-OK status. Invalid completion tags, reserved bits or stale completion handles are rejected.

The Rust callback function pointer uses the C-unwind ABI so a Rust panic can be caught by catch_unwind at the enclosing ABI boundary. A caught panic becomes OgmaStatus::PANIC; it never becomes an unwind across the exported C ABI. C callbacks must return normally and must not throw across this boundary. The C header describes the same pointer representation; a C implementation must return a status normally and must never attempt language-level unwinding through Ogma. A compiled C11 consumer smoke test exercises the header, fixed struct layouts, negotiation, lifecycle, root promotion, callback invocation and stale-handle failure against the shared library. The proof uses the unwind panic strategy; panic-abort builds cannot provide this containment guarantee and are not a supported configuration for this proof.

## M0 proof: Reproducible cost measurements

The standalone runner compares four paths:

1. primitive f64 addition without a Runtime handle;
2. checked handle → f64 conversion;
3. bulk read and conversion of 64 handles in one call;
4. host call through a mock callable and registered callback roundtrip.

For each path it performs configured warmups and repeated samples, then reports min/median/p95/p99/max nanoseconds per call and calls/second at the median. Percentiles use the nearest-rank method over per-sample mean latency; the runner requires at least 101 measurement samples so p95 and p99 are not both collapsed to the maximum in the default proof. The report also records source revision, exact tested revision, rustc version, OS, architecture, build profile, sample count and workload size. The CI job writes and uploads the JSON report.

These paths are not interchangeable workloads: scalar and bulk results must be interpreted with items_per_call, and the roundtrip is a callback bridge proof rather than JS execution. There is no machine-independent performance threshold. Measurements do not establish production interpreter throughput, Rust allocator/GC cost, physical register pressure, or AOT/native-call performance.

## M0 proof: Acceptance evidence map

| OGM-19 criterion | Proof location |
|---|---|
| Versioning and compatibility policy | This document; ogma_abi_negotiate; version-negotiation tests |
| No internal layouts or raw JS object pointers | OgmaHandle/opaque token definitions; C header; boundary documentation |
| Generational handle and roots lifecycle | Arena/value generation implementation; scope/persistent/collection tests |
| Panic containment and completion mapping | guarded; C-unwind callback; panic, Throw and invalid-completion tests |
| Thread affinity and independent runtimes | OgmaRuntime owner thread and !Send + !Sync; cross-runtime and wrong-thread tests |
| Bidirectional callback and ownership/error paths | callback registration, mock function dispatch, persistent promotion, unregister and reentrancy tests |
| Reproducible primitive/handle/bulk/roundtrip measurements | src/bin/benchmark.rs; CI JSON artifact |

## Production M1 embedding boundary (OGM-47)

The synchronous production embedding path is provided by the `ogma-embedding`
crate and uses the actual verifier, register-bytecode interpreter and Runtime
heap. The M0 benchmark/mock projects remain independent contract evidence and
are not used as the execution backend.

- `Engine` validates the resource policy and creates independent owner-thread
  `Runtime` instances. Each Runtime owns one production Heap/GC and is neither
  `Send` nor `Sync`. A `Context` exclusively borrows its Runtime, loads
  verifier-accepted OGMB artifacts, lists their stable function IDs/names with
  `Context::functions()`, executes a selected function by its canonical OGMB
  ID and is dropped before the Runtime can be reused or destroyed.
- The public Rust API exposes opaque `Value` values, explicit
  generation-checked `PersistentHandle` promotion/release, a strict-LIFO RAII
  `RootScope`, explicit collection and heap accounting. Heap cells, GC
  references, object layouts and interpreter frames are not exposed.
- ABI negotiation uses the fixed 24-byte ABI-v1 `OgmaAbiVersion` record.
  Consumers negotiate required major/minor and feature bits. The production
  C callback contract reuses the canonical opaque records:
  `OgmaContextHandle`, `OgmaHandle`, `OgmaRootScopeHandle`,
  `OgmaPersistentHandle` and `OgmaCallbackHandle` are 40 bytes;
  `OgmaCompletion` is 48 bytes; the host descriptor is 216 bytes on the
  supported 64-bit target; and `OgmaHostFunctionSlot` is 24 bytes.
- One typed host registry backs Rust and C callbacks. It resolves stable ID
  and name to generation-checked slots, validates ABI version, exact descriptor
  size, inline UTF-8 name, signature/signature ID, reserved fields, effects
  and capability requirements, then snapshots the callback before invoking
  user code. The registry also verifies the active HostCallContext belongs to
  the same Runtime heap; a detached/cloned registry cannot dispatch into a
  different Runtime. Unregistration returns `Busy` while dispatch is active.
- C callbacks receive callback-scoped opaque handles rooted in the actual
  Runtime. Handles are released after callback completion and become stale.
  Exported helpers provide checked Number/Boolean reads and writes, checked
  `i32` conversion and bounded bulk Number conversion. Fixed-signature
  `(f64, f64) -> f64` and `(OgmaHandle, OgmaHandle) -> f64` thunks are eligible
  only when descriptor signature, signature ID, argument types and declared
  effects allow the fast path; a generic callback is always required as the
  fallback.
- Rust callbacks receive `HostCallContext`, not a `Heap` reference. The
  callback surface is limited to string creation, explicit collection,
  generation-checked opaque root retention/resolution/release and nested
  execution. Heap cells, GC registries and Runtime internals remain private.
  Nested execution reuses the same Heap, root state, cancellation/deadline,
  instruction fuel, stack quota and call-depth accounting. It does not create
  another Runtime. Registry borrows are released before user callbacks run.
  Rust and C-unwind callback panics are caught and converted to interpreter
  boundary errors rather than being unwound through an exported C ABI function.
  C callback transport failures and malformed completion records remain distinct
  from capability denial and are returned as explicit interpreter boundary errors.
- `EngineConfig` validates heap quota, maximum single allocation, estimated
  active VM stack bytes, instruction fuel, call depth, interrupt polling
  interval and pending-job capacity before Runtime creation. The interpreter
  enforces the estimated frame/register-stack quota, call depth and shared
  instruction/cancellation/deadline policy. `max_stack_bytes` is an explicit
  estimate of active frame structures, register windows and local-binding
  tracking; it is not a claim about process RSS or allocator capacity.

The synchronous M1 interpreter does not yet enqueue or pump PromiseJobs.
`max_pending_jobs` is validated and retained in Engine configuration so
OGM-48 can consume the same resource policy, but queue admission, scheduling,
fairness, pumping and shutdown are implemented in OGM-48. OGM-47 does not
advertise asynchronous scheduling as available.

The public C declarations are kept in
`crates/ogma-embedding/include/ogma_embedding_abi.h`. The Rust consumer
integration tests compile JavaScript to OGMB, discover stable entry IDs from
the loaded module metadata, create and drop real Runtime/Context values,
execute the production interpreter, invoke Rust/C host callbacks, preserve
Return/Throw completion kind, force GC and verify handle invalidation.

## OGM-47 explicit exclusions

OGM-47 does not add native AOT entry points, the HostExecutionScope-vs-mutex
performance experiment, buffer leases/zero-copy, FrameSnapshot/batch CGE APIs,
the async Future/Waker bridge, bindgen or TypeScript declarations. The
PromiseJobs queue and host pump are OGM-48. M2/M3 provider selection, generated
Rust modules and native AOT remain separate architecture milestones.
