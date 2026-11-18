# OGM-7 — Rust embedding and host-performance evidence

**Status:** M0 evidence synthesis, 2026-11-18  
**Authority:** The [Ogma architecture and implementation specification](https://app.notion.com/p/4167ef18f18181bd9b34f9dda764c92d#b5ca462a10c841279d724b74c44b239a), especially sections 0.5 and 8.1–8.9.8, remains authoritative. This page is an index and synthesis of the completed OGM-19…OGM-25 proofs; it does not introduce a second ABI, a production API, or a new architecture authority.

## 1. Purpose and scope

OGM-7 groups the M0 work needed to prove the Rust host ↔ Ogma boundary and measure the cost of the main host-call strategies. Its seven independently deliverable child issues are OGM-19 through OGM-25. All seven are marked Done in Linear, their PRs are merged into dev, and their PR-triggered Fast CI runs completed successfully.

The work is deliberately a set of isolated M0 proof crates and contracts. At this repository revision, Ogma does not yet contain a production JavaScript Runtime, parser/interpreter execution path, production host adapter, or native AOT host-call path. Therefore the evidence below validates contracts and bounded prototypes; it must not be read as an end-to-end production Ogma performance claim.

## 2. Integrated contract and decision summary

The following decisions are the combined outcome of the child proofs. Detailed structures, schemas, test maps, and proof limitations remain in their individual documents and crates.

| Boundary | M0 result | Source of implementation evidence |
|---|---|---|
| Versioned Rust ABI | ABI 1.0.0 proof with fixed-width C-compatible descriptors, major/minor/feature negotiation, opaque generation-checked tokens, explicit roots, checked conversions, callback/completion handling, and panic containment. Internal JS values, GC cells, compiler layouts, and raw JS object pointers are not exposed. | [Rust ABI contract](rust-abi-contract.md), [OGM-19 proof](../benchmarks/rust-abi/README.md), C header and C11 smoke test in benchmarks/rust-abi/ |
| Isolate ownership and host execution | Mutable Runtime/Context state is owner-thread-affine. A logical host execution enters one HostExecutionScope; callbacks take short checked borrows, and nested execution requires an explicit nested scope. An application-level Arc<Mutex<HostState>> is not required on every callback when the state is owner-thread-local. | [OGM-20 proof](../benchmarks/host-execution-scope/README.md), reused by the [typed dispatch contract](typed-host-dispatch-contract.md) |
| Typed host dispatch | A pre-resolved slot and fixed-signature typed thunk are available for guarded calls. Registration version, slot generation, signature/effects, and required capabilities are checked. Ordinary property replacement invalidates a guard; Proxy reads remain observable. Unsupported/dynamic cases use the generic callback route. | [Typed host dispatch contract](typed-host-dispatch-contract.md), [OGM-21 proof](../benchmarks/typed-host-dispatch/README.md) |
| Static host modules | Module descriptors, capabilities/features, TypeScript declaration generation, import-to-slot planning, dependency closure, and fail-closed stripping are layered over the existing ABI and function table. Only immutable named ESM host-function imports resolve statically; namespace, dynamic import, CommonJS, mutable/dynamic exports, and property access retain generic routing. | [Static host modules contract](static-host-modules-contract.md), [descriptor schema](../contracts/host-modules/v1/descriptor.schema.json), [profile schema](../contracts/host-modules/v1/profile.schema.json) |
| Buffer interoperability | Owned transfer, external ownership, scoped Rust borrowing of JS memory, and revocable scoped views of host memory are distinct modes. Leases are tracked by the shared backing store. A live Rust slice forbids JS execution/re-entry, GC-safe points, resize/detach, await, or suspension. SharedArrayBuffer/Atomics is a separate, unimplemented contract. | [Buffer interoperability contract](buffer-interop-contract.md), [OGM-22 proof](../benchmarks/buffer-interop/README.md) |
| Frame snapshots and batch APIs | Publish a consistent frame snapshot once per synchronous execution. Numeric IDs/bitsets and batch transform/collision reads reduce per-entity callback overhead; validate typed-array type, length, attachment, alignment, and overlap once before contiguous access. Host physics/ECS work remains outside the generic Ogma Runtime contract. | [OGM-23 proof](../benchmarks/frame-snapshot/README.md), pinned [CGE consumer source](https://github.com/AiratGaliev/cge-experiments/tree/c47ad08c3da9a3857f8429cebbed6ce88969715d) |
| Allocation | Keep System as the baseline Runtime allocator. The tested per-Isolate reuse/pool candidates did not justify becoming the default. A compile-unit bump arena is beneficial for bounded, trivially reclaimable compiler scratch with strict lifetime; it is not a JS heap or GC allocator decision. Quotas cover committed backing blocks/chunks, not total process memory. | [OGM-24 proof](../benchmarks/isolate-allocator/README.md) |

### Cross-cutting invariants

1. The versioned ABI is the single interop boundary. Typed dispatch and static modules reuse its versioning and opaque-handle rules; they do not define a second ABI or expose Rust's default internal ABI.
2. Host-call fast paths must preserve the generic semantic route. Static binding alone does not authorize skipping observable property access, Proxy traps, mutable exports, capability checks, exception/completion handling, or invalidation.
3. An owner-thread scope is not a lock held across arbitrary script execution. Borrows are short-lived; reentrancy is explicit. Work arriving from another host thread must be transferred as owned commands/tasks and applied by the Runtime owner. A bounded dispatcher with backpressure, cancellation, and shutdown is a future integration requirement, not a completed cross-thread Runtime implementation.
4. Zero-copy is opt-in, not the default. Use copy or ownership transfer unless the backing-store owner, pointer stability, aliasing, escape lifetime, invalidation, and finalization rules are enforceable by the real Runtime.
5. Snapshots and bulk APIs are appropriate for high-volume consumer paths, but game-specific entity storage, physics scheduling, and ECS sparse/dense thresholds belong to the host adapter.
6. Allocator and dispatch choices are evidence-based. The current measurements are machine/workload-specific and do not establish universal thresholds.

## 3. Performance evidence

All values are from the cited, isolated M0 benchmark runs. They are not production Ogma Runtime numbers. Percentiles and throughput must be compared only for equivalent workloads, toolchains, and environments.

| Proof | Recorded result | Interpretation |
|---|---|---|
| OGM-19 — ABI calls | Linux x86_64, Rust 1.100.0 release proof, 10,000 iterations, 3 warmups, 101 samples. Median mock call cost: primitive f64 add 1.750 ns; checked handle conversion 11.514 ns; 64-handle bulk read 145.324 ns; mock callable/callback roundtrip 73.114 ns. | The proof establishes a measurable primitive/handle/bulk/roundtrip baseline for the mock ABI. It does not measure JS execution, production GC, or host-object transfer. |
| OGM-20 — execution scope | Batch p50 per operation: no-op Arc/Mutex 34.723 ns vs owner scope 10.989 ns (3.16×); typed 35.600 vs 11.208 ns (3.18×); nested 105.600 vs 23.962 ns (4.41×). The owner-scope mock recorded zero application mutex acquisitions and zero allocations. Individual-call latency percentiles were reported separately. | Supports owner-thread scoped access for owner-thread-local mutable state. It does not remove synchronization internal to a selected binding/runtime library; the rquickjs profile was source-audited separately, not timed by this proof. |
| OGM-21 — typed dispatch | CI ran the dynamic lookup, pre-resolved slot, typed primitive/handle, generic fallback, and guarded-call benchmark with 10,000 iterations, 3 warmups, and 101 samples. The machine-readable JSON report is preserved as the [OGM-21 evidence artifact](https://github.com/AiratGaliev/ogma/actions/runs/44264703570/artifacts/14409338552). | Use the recorded report for exact latency, allocation, lookup, and fallback metrics. This proof exercises a mock ABI and a narrow ordinary-object/Proxy guard model, not interpreter-level ECMAScript semantics. |
| OGM-22 — buffer transfer | Copy vs scoped zero-copy median: 64 B — 21.530 vs 12.002 ns/op; 4 KiB — 701.645 vs 637.968 ns/op; 1 MiB — 208,191 vs 161,844 ns/op. | The harness shows a benefit for its tested checksum workloads. It does not justify enabling zero-copy before real Runtime/GC and consumer-level lifetime tests. |
| OGM-23 — frame/batch consumer | Pinned CGE reference-consumer p50, scalar X/Y callbacks vs typed-ID bulk read: 16 entities — 8.162 vs 5.926 µs (27.4% lower); 64 — 26.765 vs 17.195 µs (35.8% lower); 256 — 101.219 vs 62.313 µs (38.4% lower). | These are rquickjs 0.14.0 reference-consumer measurements. The report explicitly marks production Ogma Runtime dispatch as not measured. Synthetic physics/ECS model timings are kept in separate phases; actual Jolt/Flecs measurements remain in the pinned CGE benchmark notes. |
| OGM-24 — allocation | On the final proof run, System baseline throughput was about 11.94M ops/s; exact reuse and size-class candidates were about 11.38M and 11.47M ops/s. Size classes improved allocation p95/p99 from 80/111 ns to 61/90 ns but increased deallocation p95/p99 from 60/70 ns to 90/120 ns and retained 158,304 bytes. The compile-unit arena improved its tested POD workload from 30,187 to 59,877 units/s (+98.4%). | Keep System as the Runtime baseline; do not enable a custom pool by default. Restrict arenas to data whose lifetime ends with the compile unit and which does not require per-object destruction. |

OGM-25 is a functional module-registration/linking proof, not a standalone performance claim. Its value is semantic correctness and integration: host module declarations and slots are generated from the same records, static imports resolve only when safe, capability checks remain on the existing dispatch path, and profile stripping fails closed for referenced modules/native dependencies.

## 4. Delivery and verification traceability

Each row was checked against the GitHub PR metadata, merge commit, and PR-triggered workflow run. All listed PRs target dev, are merged, and their corresponding CI Fast run has conclusion success.

| Linear issue | PR / merged commit | CI Fast |
|---|---|---|
| [OGM-19](https://linear.app/ogma-engine/issue/OGM-19/prototype-versioned-rust-abi-and-interpreter-embedding-contract) | [#15](https://github.com/AiratGaliev/ogma/pull/15) · 9dd99bd0904a1bb6c3776304aa15ce2cbc2a8099 | [run #229](https://github.com/AiratGaliev/ogma/actions/runs/44165140557) |
| [OGM-20](https://linear.app/ogma-engine/issue/OGM-20/benchmark-owner-thread-hostexecutionscope-against-per-callback-shared) | [#16](https://github.com/AiratGaliev/ogma/pull/16) · 3a9aca1f2ea8990804967ae7dc291ae500cd2c94 | [run #246](https://github.com/AiratGaliev/ogma/actions/runs/44251826614) |
| [OGM-21](https://linear.app/ogma-engine/issue/OGM-21/prototype-typed-host-function-table-and-pre-resolved-call-dispatch) | [#17](https://github.com/AiratGaliev/ogma/pull/17) · b3f6af269b8089bb2c87825f2fa9db70e068f9c8 | [run #265](https://github.com/AiratGaliev/ogma/actions/runs/44264703570) |
| [OGM-22](https://linear.app/ogma-engine/issue/OGM-22/validate-buffer-ownership-scoped-borrowing-and-zero-copy) | [#18](https://github.com/AiratGaliev/ogma/pull/18) · 6be459aca7fba2bf452ff3e9ad72e5ffdac7efb9 | [run #275](https://github.com/AiratGaliev/ogma/actions/runs/44277986895) |
| [OGM-23](https://linear.app/ogma-engine/issue/OGM-23/benchmark-batch-framesnapshot-and-host-apis-against-scalar-callbacks) | [#19](https://github.com/AiratGaliev/ogma/pull/19) · b70afbe1d027488d830fca248bffcd31dc0e7bfd | [run #293](https://github.com/AiratGaliev/ogma/actions/runs/44293678442) |
| [OGM-24](https://linear.app/ogma-engine/issue/OGM-24/evaluate-per-isolate-allocator-and-compile-unit-arenas) | [#20](https://github.com/AiratGaliev/ogma/pull/20) · 116cb09213c6eba1480f5a09d09346867c674649 | [run #305](https://github.com/AiratGaliev/ogma/actions/runs/44302538348) |
| [OGM-25](https://linear.app/ogma-engine/issue/OGM-25/prove-static-host-modules-registration-declarations-and-feature-profiles) | [#21](https://github.com/AiratGaliev/ogma/pull/21) · bbb4ba51fc6f930ce35d5ccaae660d4eda7395e6 | [run #317](https://github.com/AiratGaliev/ogma/actions/runs/44310144250) |

For exact reproduction commands and report formats, use the README in each proof directory listed in the table in section 2. The CI jobs run the pinned Rust toolchain, formatting, Clippy with warnings denied, focused tests, C11 layout/consumer checks where applicable, benchmark/report validation, and evidence upload. The current repository workflow reserves full Test262 and browser/runtime acceptance for the dev → test promotion candidate; they are not feature-PR requirements.

## 5. Acceptance status for OGM-7

| OGM-7 workstream outcome | M0 status | Evidence |
|---|---|---|
| Versioned ABI, opaque handles, roots, conversions, callbacks, panic containment | Complete as an isolated ABI proof | OGM-19 |
| Owner-thread HostExecutionScope/dispatcher alternative and host-state lock comparison | Complete as a mock dispatch proof | OGM-20 |
| Typed host-function table, guarded fast path, generic fallback and invalidation | Complete as a narrow dispatch proof | OGM-21 |
| Buffer ownership, scoped borrowing, invalidation, copy/zero-copy comparison | Complete as a host-side safety proof | OGM-22 |
| FrameSnapshot and scalar-vs-batch consumer comparison | Complete as a reference-consumer proof; Ogma Runtime dispatch remains not measured | OGM-23 |
| Per-Isolate allocator and compile-unit arena comparison | Complete as a raw-memory proof; System remains the default recommendation | OGM-24 |
| Static host module descriptors, declarations, import planning and fail-closed profiles | Complete as a registration/linking proof | OGM-25 |
| Consolidated Rust host interoperability and performance evidence | Complete for M0 proof scope | This synthesis and the linked evidence |

## 6. Explicit limitations and follow-up validation

The evidence above does **not** claim any of the following:

- a production Ogma Runtime, interpreter, JS heap, tracing GC, production host API, or fully implemented TypeScript bindgen;
- production ECMAScript property/call/module semantics from the mock OGM-19/21 models;
- actual JS ArrayBuffer intrinsics, tracing-GC finalizer scheduling, or SharedArrayBuffer/Atomics behaviour from the OGM-22 harness;
- production Ogma dispatch or whole-frame performance from the CGE/rquickjs reference consumer;
- production JS heap quota enforcement, GC allocation throughput, or runtime memory behaviour from the OGM-24 raw-memory harness;
- a production ESM loader or complete feature stripping/linker integration from OGM-25;
- an AOT-native host-call path, process sandbox, full Test262 conformance, or browser/gameplay acceptance.

When the real Runtime is implemented, these contracts must be integrated and revalidated against actual execution: JS property and Proxy semantics, root/GC safepoints, callback and exception transitions, real ArrayBuffer/TypedArray operations, module loading, async/suspension, capability/resource enforcement, native AOT ABI calls, and end-to-end CGE consumer measurements. These are future Runtime/AOT integration gates; they are not evidence that the current M0 proof work failed.

The workstream therefore closes the M0 contract-and-measurement objective without claiming production readiness or selecting optimizations beyond what the isolated measurements support.
