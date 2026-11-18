# Buffer interoperability and controlled zero-copy — OGM-22 M0 proof

## Authority and scope
The authoritative requirement is Notion section 8.9.3. This document records the proof in benchmarks/buffer-interop; it does not freeze a production Runtime API. Ogma has no production ECMAScript Runtime, JS heap, ArrayBuffer intrinsics or tracing GC yet. The harness validates host-side ownership and guards, not real interpreter integration. No allocation pointer, JS object address or internal Runtime layout is exposed through ABI.

## Ownership modes and transitions

| Mode | Owner | JS visibility | Lifetime / release | Invalidation |
|---|---|---|---|---|
| Owned transfer | ArrayBuffer backing store after move | ArrayBuffer/TypedArray may escape | Allocation survives while any view retains the store; final drop releases once | Detach invalidates aliases; resize stays within reserved capacity |
| External-owned buffer | Host owner plus escaped JS views | Views may escape host scope | Store releases after host owner and all views are dropped | Host cannot free/reallocate or mutate behind live views |
| Rust scoped borrow of JS memory | JS ArrayBuffer | Synchronous Rust slice callback only | Slice ends before returning to JS | No JS call, re-entry, GC, resize, detach, await or yield while borrowed |
| Scoped borrowed JS view of host memory | Host-owned stable allocation | Temporary lease-checked view | Lease ends on return, error or panic unwind | Escaped tokens return LeaseExpired; storage changes/GC are blocked in lease |
| Shared memory | Separate shared-agent protocol | Not implemented | Requires independent SharedArrayBuffer/Atomics contract | Unsynchronized concurrent access is forbidden |

Owned transfer consumes the source allocation and preserves its address. The proof reserves capacity before exposing storage; resize changes logical length within capacity and does not reallocate. Growth beyond capacity fails. Detach remains observable through every alias.

For external-owned storage, host and JS retain independent references to the same backing store. Dropping the host handle cannot invalidate escaped views. A production adapter must pair allocator/deallocator functions and prevent host reallocation/free while a view remains live.

## Borrow and lease invariants

Runtime.with_scoped_read / with_scoped_mut expose a Rust slice only during a synchronous callback. While the slice is live, JS execution, callback re-entry, nested borrows, GC safe points, resize, detach, await, yield and suspension are prohibited. RAII releases the guard on return, callback error and panic unwind. Every production engine entry point and invalidation path must enforce the same guard; no lower-level bypass is allowed.

Runtime.with_scoped_view creates a revocable lease. Cloned Rust tokens can outlive the callback, but every later operation fails with LeaseExpired. Resize, detach and collection fail while a lease is live. A production Runtime must make equivalent revocation observable to JS; otherwise it must use copy or owned transfer.

TypedArray remains mutable. A normal shared TypedArray is not a read-only snapshot. Production POD adapters must validate concrete representation, element size, alignment, endianness and aliasing; repr(C) alone is not proof. SharedArrayBuffer/Atomics is excluded.

## Unsafe boundary

The only unsafe code is the private OwnedMemory allocation/slice wrapper:
1. Allocation uses a validated non-zero Layout with power-of-two alignment.
2. The exact pointer/Layout pair is retained and deallocated once.
3. Pointer never moves; resize only adjusts logical length within capacity.
4. Slice ranges are checked before construction.
5. Mutable access is mediated by an exclusive RefCell borrow.
6. Backing-store references retain allocation until final Drop.

No unsafe slice or raw pointer is exposed through public APIs. Production code requires a fresh review against its actual allocator and GC implementation.

## Error and reentrancy behavior

| Condition | Behavior |
|---|---|
| Invalid alignment, overflow, out of bounds | Typed constructor/access error |
| Detached store | Detached |
| Resize beyond capacity | ResizeExceedsCapacity; address never changes |
| JS execution while Rust slice lives | JsExecutionWhileBorrowed |
| Nested scoped slice | BorrowAlreadyActive |
| Runtime alias conflict | BorrowConflict |
| Resize/detach/GC during lease | LeaseActive |
| Use of escaped lease | LeaseExpired |
| Rust panic in callback | Harness unwinds; RAII releases guard |
| Callback error | Error returned; guard released |
| Excessive nested JS-like execution | ReentrancyLimit |

This harness does not define C ABI panic behavior. OGM-19 remains authoritative: production ABI entry points catch Rust panics before crossing C and map them to stable ABI error status.

## Acceptance evidence map

| OGM-22 criterion | Proof location |
|---|---|
| Ownership/lifetime/state transitions | This contract; take_ownership, external_owned, with_scoped_mut and with_scoped_view |
| Alignment and pointer stability | alignment_and_pointer_stability |
| ArrayBuffer/TypedArray escape | external_owner_drop_preserves_escaped_view and typed_array_escape_retains_owned_storage |
| Detach/resize | resize_and_detach_invalidate_views |
| GC/finalizer | FinalizerProbe and collect_garbage guard tests |
| Panic/error/reentrancy | panic_and_callback_error_release_borrow_guard, JS/GC guard and bounded reentrancy tests |
| No JS with live Rust slice | Runtime.execute_js checks active borrow guard |
| Unsafe invariants | allocation section plus alignment/pointer/lifetime tests |
| Copy versus zero-copy | benchmark covers 64 B, 4 KiB and 1 MiB with equivalent checksums |

Benchmark evidence records source/tested revisions, Rust toolchain, OS/architecture, CPU label, warmups, sample count, mean latency per sample, nearest-rank p95/p99 and throughput. No universal zero-copy threshold is asserted: small/short-lived payloads may be faster with copying; adoption depends on measured benefit.

## Limitations and production requirements
- FinalizerProbe is deterministic Rc/drop behavior, not a tracing JavaScript GC. Production must test real roots, finalizer scheduling, collection during callbacks and finalization order.
- Detach/resize are modeled by the proof Runtime, not actual JS ArrayBuffer methods. Production tests must exercise real JS operations and aliases.
- Revocable view is a Rust token, not an ECMAScript object/Proxy. If reliable JS-observable revocation cannot be guaranteed, do not expose it.
- No SharedArrayBuffer/Atomics or concurrent cross-thread sharing is implemented.
- Benchmark excludes JS dispatch, production allocator/GC and application workload.
- Zero-copy remains opt-in until real Runtime and GC tests pass and end-to-end benefit is demonstrated.
