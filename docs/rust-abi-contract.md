# Rust embedding ABI — OGM-19 M0 proof

## Authority and scope

This document records the concrete M0 proof in benchmarks/rust-abi/. It is not a new permanent architecture authority and does not freeze a production Ogma ABI. The authoritative requirements remain the Ogma Notion architecture specification, especially sections 0.5 and 8.1–8.8. If this prototype conflicts with that specification, the specification wins and the prototype must be corrected.

The repository is in M0 and has no production Rust Runtime, Context, JS heap, interpreter or AOT backend. This proof uses a mock value store and a callback-backed mock callable so ownership, token validation, callback, completion and host ABI behaviour can be tested independently. It makes no ECMAScript execution or production collector conformance claim.

## Versioning and compatibility

The proof advertises ABI version **1.0.0** and a feature bitset. Consumers negotiate a required major, minimum minor and required feature mask before creating a runtime.

- Major mismatch is incompatible.
- A consumer requiring a minor newer than the implementation is incompatible.
- Unknown required feature bits are rejected.
- A compatible minor revision may add optional fields/functions/features without changing existing meanings or layouts.
- A change to an existing field's meaning, token contract, completion convention, calling convention or ownership/lifetime semantics is incompatible and requires a major revision.
- Patch revisions are restricted to fixes that do not change the binary contract.
- Reserved fields are zero in v1.0.0.

The returned version descriptor contains struct_size, allowing future extensions to be detected. All exposed integer fields have explicit widths. size_t is used only for pointer-length pairs and therefore follows the target pointer width. This prototype does not yet implement a complete target-triple/endianness/native-entry manifest because it has no bytecode or AOT artifact contract.

Feature bits are: opaque handles, root scopes, persistent handles, callbacks, explicit completions and MOCK_PROOF. The last bit is deliberately present so a caller cannot mistake the proof harness for an executable JS Runtime.

## ABI surface and representation boundary

The C header is benchmarks/rust-abi/include/ogma_abi.h. OgmaRuntime is an incomplete opaque type. Context, value, root-scope, persistent and callback tokens are fixed-layout structs containing five 64-bit opaque words. The words are private in Rust and have no supported public encoding.

A token is an identity/lifecycle capability, not a pointer. It is validated against runtime identity, context identity/generation, slot index and slot generation. It must not be inspected, persisted across runtime destruction, modified or synthesized by a host. A token from another Runtime/Context or a released/recycled slot is rejected as stale.

The ABI does not expose:
- internal Rust Value/JsValue representations;
- JS object, string, function, heap-cell or GC layouts;
- compiler, parser, IR, VM frame or stack-map layouts;
- raw pointers to JS objects or runtime-owned heap cells.

The opaque runtime owner pointer is valid only as the handle returned from runtime creation. Host user_data is a host-owned pointer passed back unchanged to the callback; it is not an Ogma object reference.

## Threading and lifecycle

Each Runtime captures its creating thread. The safe Rust representation is !Send + !Sync; every C ABI entry verifies the owner thread before touching mutable runtime state. Different runtimes have independent identities and may be owned by different host threads. A single Runtime is not concurrently callable from multiple threads. Cross-thread work is intentionally not implemented here; the architecture requires owned messages/payloads rather than shared mutable JS values.

Destroying a Runtime from inside an active API/callback call returns Busy. Context destruction is also rejected while a callback is executing or a root scope is open. Runtime destruction consumes the unique pointer slot and writes null to it. The host must not duplicate the runtime owner pointer or call through a pointer after destruction.

## Values, conversions and roots

The proof currently models only Number(f64), Boolean and Function tokens. This is enough to exercise type checking and identity/lifecycle; it is not a full JS Value model.

- get_number accepts only Number.
- get_boolean accepts only Boolean, represented as uint8_t restricted to 0/1.
- get_i32 converts a Number only when finite, integral and within the i32 range; it never truncates or wraps.
- Primitive f64 arithmetic is a direct ABI call with no handle conversion.
- Bulk sum validates each handle and its type before returning a result.

A value handle is non-owning. It does not keep a value alive. The host uses explicit root scopes for temporary values and persistent roots for values retained beyond a scope.

Root scopes are strictly LIFO. A scope roots only the handles added to it and is closed exactly once. Closing an outer scope while an inner scope is active returns Busy. Persistent creation promotes a transient handle into a host-owned root; persistent_get resolves it, and persistent_release relinquishes that root. Once no scope, persistent root or active call roots the value, the mock collector invalidates the slot and advances its generation. Stale use returns StaleHandle.

This deterministic collector is only a lifecycle fixture. It does not select or model the production GC algorithm.

## Scoped borrowing, ownership and callbacks

ogma_context_call receives a pointer/length pair for the argument token array. The array is copied into a call frame; argument values and the callable are rooted for the duration of the callback. The callback args pointer is borrowed only until it returns. It must not retain that array pointer or a transient handle after return unless it creates a persistent root during the callback.

user_data remains host-owned. The host must keep it valid until callback unregistration and must arrange its own synchronization/lifetime. Unregistering invalidates the callback token; functions referring to it fail with StaleHandle.

The call path deliberately drops internal collection borrows before invoking user callback code. Nested ABI calls are allowed and have an explicit depth limit of 32 in this proof. Crossing the limit returns ReentrancyLimit; it is not silently converted to a JS exception. No Rust mutable borrow or heap registry lock is kept across callback execution.

The callback-backed mock function is a bridge fixture, not a JavaScript function implementation. It demonstrates the shape of a host → callable → registered Rust callback → host completion roundtrip. A real interpreter must later connect JS call evaluation to the same versioned host contract.

## Completion, errors and panic containment

Transport/ABI failures are represented by OgmaStatus, a transparent uint32_t status code. Script completion is represented separately by OgmaCompletion:

| Kind | Meaning |
|---|---|
| Normal | Successful completion with an optional value handle |
| Throw | JS-style thrown completion with a required value handle |
| Return | Return completion |
| Break | Break completion |
| Continue | Continue completion |

A callback can return Ok with a Throw completion; the ABI call itself succeeds and the caller inspects the completion kind. A host callback/transport failure returns a non-OK status. Invalid completion tags, reserved bits or stale completion handles are rejected.

The Rust callback function pointer uses the C-unwind ABI so a Rust panic can be caught by catch_unwind at the enclosing ABI boundary. A caught panic becomes OgmaStatus::PANIC; it never becomes an unwind across the exported C ABI. C callbacks must return normally and must not throw across this boundary. The C header describes the same pointer representation; a C implementation must return a status normally and must never attempt language-level unwinding through Ogma. The proof uses the unwind panic strategy; panic-abort builds cannot provide this containment guarantee and are not a supported configuration for this proof.

## Reproducible cost measurements

The standalone runner compares four paths:

1. primitive f64 addition without a Runtime handle;
2. checked handle → f64 conversion;
3. bulk read and conversion of 64 handles in one call;
4. host call through a mock callable and registered callback roundtrip.

For each path it performs configured warmups and repeated samples, then reports min/median/p95/p99/max nanoseconds per call and calls/second at the median. The report also records source revision, exact tested revision, rustc version, OS, architecture, build profile, sample count and workload size. The CI job writes and uploads the JSON report.

These paths are not interchangeable workloads: scalar and bulk results must be interpreted with items_per_call, and the roundtrip is a callback bridge proof rather than JS execution. There is no machine-independent performance threshold. Measurements do not establish production interpreter throughput, Rust allocator/GC cost, physical register pressure, or AOT/native-call performance.

## Acceptance evidence map

| OGM-19 criterion | Proof location |
|---|---|
| Versioning and compatibility policy | This document; ogma_abi_negotiate; version-negotiation tests |
| No internal layouts or raw JS object pointers | OgmaHandle/opaque token definitions; C header; boundary documentation |
| Generational handle and roots lifecycle | Arena/value generation implementation; scope/persistent/collection tests |
| Panic containment and completion mapping | guarded; C-unwind callback; panic, Throw and invalid-completion tests |
| Thread affinity and independent runtimes | OgmaRuntime owner thread and !Send + !Sync; cross-runtime and wrong-thread tests |
| Bidirectional callback and ownership/error paths | callback registration, mock function dispatch, persistent promotion, unregister and reentrancy tests |
| Reproducible primitive/handle/bulk/roundtrip measurements | src/bin/benchmark.rs; CI JSON artifact |

## Explicit exclusions

This task does not implement the production runtime, JS object/Value model, GC, interpreter, native AOT, typed host-function table, HostExecutionScope-vs-mutex experiment, buffer leases/zero-copy, FrameSnapshot/batch CGE APIs, async Future/Waker task bridge, bindgen or TypeScript declarations. Those are separate architecture areas and downstream M0/M1 tasks. The current code is evidence for designing the embedding seam, not a production-ready stable ABI.
