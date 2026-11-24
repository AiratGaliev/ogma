# Runtime Value and Heap contract (OGM-43)

**Status:** M1 implementation foundation. The API is available to the interpreter and embedding crates, but full interpreter/object-model and callback integration remains owned by downstream tasks.

## 1. Scope and ownership

`ogma-runtime` owns the canonical Runtime `Value`, UTF-16 string storage, managed heap, collection, root-provider registry, persistent host handles and Runtime allocation accounting.

The crate has no dependency on the parser, frontend, compiler, Core IR, bytecode or interpreter. Those layers may depend on Runtime; Runtime must remain backend-neutral.

This implementation replaces the earlier M0 proof-only models in the production workspace. The standalone M0 benchmarks remain historical evidence and are not production dependencies.

## 2. Value representation

`Value` is an explicitly tagged private Rust enum behind an opaque public wrapper:

| Tag | Payload |
|---|---|
| Undefined / Null | No payload |
| Boolean | Rust `bool` |
| Number | IEEE-754 binary64 `f64` |
| String | Internal generation-checked `GcRef` |
| Object / Function | Internal generation-checked `GcRef` |

There is no `repr(C)`, NaN-boxing, pointer tagging, architecture-specific encoding or supported public layout. The custom `Debug` output exposes only the value category, not the internal object identity.

The Rust embedding ABI must continue to use its opaque `HostHandle` / ABI token contract. It must not copy `Value` bytes, expose `GcRef` or pass managed-object pointers to hosts.

### Equality and primitive conversions

- `strict_equal` implements Number `==` semantics (`NaN` unequal, `+0` equal to `-0`), UTF-16 string content equality and managed object/function identity.
- `same_value` models `Object.is`: all NaNs compare equal and signed zeros remain distinct.
- `same_value_zero` treats NaNs and both zero signs as equal.
- `to_boolean` and primitive `to_number` cover the currently represented M1 primitive categories. Object-to-primitive conversion is rejected rather than guessed; that behavior belongs to the later object semantics layer.

ECMAScript strings are represented as UTF-16 code units. The heap preserves unpaired surrogates instead of forcing every value through Rust UTF-8 `String` validity.

## 3. Heap and identities

Each Runtime owns a thread-affine `Heap` and receives a unique heap identity. Internal `GcRef` identifies a non-moving cell by heap identity, slot index and generation. The slot directory stores a separately allocated, pointer-stable cell block; growing the directory moves slot metadata/pointers but never moves an existing managed cell. Every internal access validates all three identity fields; slot reuse increments the generation.

`HostHandle` is a separate opaque persistent token with its own generation-checked registry. It may retain a primitive or a managed value; only managed values contribute a GC root. Releasing a handle invalidates the token before a slot can be reused. Ordinary object graph operations use `GcRef` directly and never resolve a host handle.

The first production collector is non-moving tracing mark-sweep, consistent with the preferred M0 comparison candidate. The mark worklist is linked through metadata in the separately allocated cell blocks. This avoids allocating an unbounded traversal vector during collection and prevents collection scratch allocation from bypassing the heap quota.

## 4. Explicit roots

The collector reads only registered root providers:

| Provider | API | Lifetime |
|---|---|---|
| Interpreter activation roots | `enter_frame`, `frame_root`, `leave_frame` | Strict LIFO frames |
| Temporary host/runtime roots | `open_scope`, `scope_root`, `close_scope` | Strict LIFO scopes |
| Persistent host ownership | `retain_host_value`, `resolve_host_handle`, `release_host_handle` | Until explicit release |
| Native frames | Future safepoint/shadow-frame integration | M3; not implemented by OGM-43 |

Root snapshots are inspectable. Root registration rejects stale or foreign-heap references before publishing them. No conservative Rust stack scanning is used.

Callers must publish a managed value into an appropriate frame or scope before any operation that may trigger collection. A native AOT caller must not assume its Rust/native stack is implicitly visible to this collector.

## 5. Quotas, allocation and recovery

`HeapConfig` provides an aggregate heap byte quota and a maximum single-allocation limit. Allocation accounting covers the actual capacities reserved by the Runtime for:

- object-slot, per-cell header, persistent-handle, external-charge, frame and scope registries;
- UTF-16 string payloads;
- object/function reference-edge arrays;
- each root frame/scope reference array;
- externally backed memory explicitly registered using `charge_external_memory`.

External memory charges have generation-checked tokens and must be released exactly once. They are not silently inferred from arbitrary host pointers.

When an object, function or string allocation encounters aggregate quota pressure, the heap attempts one collection and retries. A single request above the configured maximum is rejected before a cell is published. Registry/edge growth and external charges fail closed when the quota is exhausted; they never publish a partially registered root or edge.

The transient ASCII buffer used by primitive string-to-Number conversion is also reserved through the same quota-aware allocator and released when conversion finishes. Invalid non-ASCII numeric content returns NaN without constructing a temporary UTF-8 string. The byte counters include slot-directory capacity, one separately allocated cell header for each live managed cell, UTF-16 payload capacity, edge/root/handle/frame registries, transient conversion buffers and registered external bytes. They are not process RSS and do not claim to account for allocator metadata, OS page rounding, fragmentation or unrelated allocations made by host code. Production footprint and representative interpreter workloads remain part of later measurement and integration work.

## 6. Verification and boundaries

The Runtime unit corpus covers number edge cases, strict/SameValue/SameValueZero semantics, UTF-16 surrogate preservation, primitive coercion, mark-sweep cycle reclamation, physical cell-address stability across slot-directory growth, all implemented root providers, LIFO violations, cross-Runtime identity, stale generation reuse, allocation limits, quota-triggered collection/retry, failed cell initialization cleanup, external charges and recovery after memory release.

OGM-26 remains evidence for the initial collector direction, not a permanent production performance claim. Re-evaluate collector pause time, throughput, heap overhead and realistic graph shapes once the interpreter and consumer workloads exist.

OGM-43 does not implement the interpreter's final liveness maps, AOT native stack maps, WeakRef/finalization semantics, a complete ECMAScript object/property model, callback dispatch or async continuations. These remain owned by the appropriate downstream Runtime/interpreter/embedding tasks.
