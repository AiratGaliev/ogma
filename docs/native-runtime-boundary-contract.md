# Native Runtime boundary, roots and safepoints — OGM-27 M0 proof

## Authority and scope

This document records the OGM-27 M0 prototype in `benchmarks/native-runtime-boundary/`. The authoritative requirements remain the Ogma architecture specification, especially sections 0.4, 7.3 and 9.5. The Core IR v1 contract and Rust ABI v1 proof remain independent upstream contracts; this document composes them and does not redefine either one.

The repository is still in M0. It has no production JavaScript Runtime, interpreter, AOT compiler, object model or runtime heap. The prototype is a Rust model of the boundary rules. It proves root-map validation, root-provider enumeration, frame lifecycle, explicit completion propagation and deterministic interrupt accounting; it does not execute JavaScript or machine code.

## Decisions for the M0 boundary

### 1. Common root protocol

Use one logical root protocol for interpreter and native execution, with backend-specific representations:

- **Interpreter frames** enumerate managed references in live VM locals/registers, operand storage and closure environments. Enumeration is explicit; the collector must not conservatively scan arbitrary Rust stack memory.
- **Native frames** publish a precise shadow/root frame at each GC-capable safepoint. The frame descriptor identifies its slot layout and maps each safepoint ID to the exact slots holding live GC-managed references at that point. The prototype validates map uniqueness, bounds, managed-value type and equality with an independent expected-live-slot set.
- **Persistent host handles** remain an independent external root provider. Releasing a handle removes the root and invalidates its generation-checked token.
- **Temporary root scopes** remain explicit and LIFO, as specified by the ABI/embedding proof.

The internal `GcRef`/object identity is not the same thing as the external `HostHandle`. Generated code must not route ordinary heap accesses through the host-handle registry. The M0 GC comparison recommends non-moving tracing mark-sweep as the next integration candidate, so this prototype does not implement relocation or pointer rewriting.

A native frame is pushed before the function body begins and is popped only when the activation returns. At entry and every later safepoint, the active map must be published before any operation that may collect or re-enter script code. Nested native frames form a strict LIFO chain. A safepoint without a valid map is a runtime contract violation, not permission to guess roots.

For a production AOT backend, either stack maps or explicit shadow/root frames may implement this contract. The choice is backend-specific; the invariant is not. A stack-map encoder must describe register spills and stack slots after register allocation, while a shadow-frame lowering must keep every live managed reference in a registered root slot. It is invalid to publish maps from pre-register-allocation liveness and assume they still describe final machine code.

### 2. Core IR effects and safepoint placement

The Core IR effect model is the semantic source for placement and validation:

- `may_allocate` / `may_gc`: every live managed reference crossing the operation must be in the active root map before the runtime helper can collect.
- `may_call_host` / `may_reenter`: publish the caller frame and preserve its source, lexical/closure and completion state before control leaves generated code. Do not retain an unsafe mutable borrow of Runtime/heap state across user or host callbacks.
- `may_throw`: preserve the pending completion and its managed value as a root until the caller consumes it or transfers it to another activation.
- `may_suspend`: use an explicit continuation/root representation in the future async contract; a native stack frame alone cannot be assumed to remain live across suspension.

At minimum, instrument function entry, every loop back-edge, allocation boundaries and call/host-callback boundaries. A safepoint is a published state at which GC and re-entry are permitted; a budget poll is an interrupt check. They may share a location, but they are separate obligations: a budget poll does not automatically prove the root map is correct.

The prototype represents native root maps by `SafepointId → slot indexes` and rejects unknown safepoints, duplicate slots/IDs, out-of-range slots, non-managed mapped values and a mismatch between the declared map and the expected live references. This is a contract fixture, not a real compiler stack-map encoder.

### 3. Native ↔ interpreter and host callback transitions

All execution backends use one activation/completion model:

- Activation records retain execution kind, function identity, source location and closure context.
- Native → interpreter calls push an interpreter activation without discarding the native caller.
- Interpreter → host callback calls push a callback activation; callback re-entry into the interpreter pushes another interpreter activation with its own closure/source context.
- Returns unwind activations in strict LIFO order. The caller's activation remains available until the callee's completion is consumed.
- Script completions are explicit values: `Normal`, `Return`, `Break`, `Continue` and `Throw`. A `Throw` is not an ABI transport failure. Transport failures remain ABI status values.
- The completion payload is kept rooted while crossing a boundary. Exception stack/source metadata must be captured before frames are removed.

The existing Rust ABI v1 proof defines opaque handles, callback lifetime, reentrancy and completion-vs-status separation. This prototype composes those rules; it does not add fields to or change the v1 public ABI. Rust panic/unwind must never cross the external ABI. The ABI boundary remains responsible for panic containment; script exceptions use explicit completions.

### 4. Budget and cancellation contract

A single logical execution budget and cancellation token follow an execution across interpreter, AOT helpers, nested script calls and host callbacks. Backend-local instruction counters must not reset the budget when execution changes backend.

The proof uses deterministic **work units**, not wall-clock time. Each uninterrupted region records work and must reach a poll before exceeding its configured maximum poll window. The next poll checks cancellation first, then compares accumulated work with the budget. If a poll interval is bounded by `B` units, budget overshoot is at most `B` units. The default values are policy inputs, not universal timing guarantees.

Required poll locations:

| Location | Required behaviour |
|---|---|
| Function entry | Check cancellation/budget before executing the body |
| Loop back-edge | Check on every taken back-edge, including optimized loops |
| Allocation boundary | Check before an allocation/helper that can allocate or trigger GC |
| Call boundary | Check before and after native, interpreter or host calls; preserve the active frame map |
| Long-operation chunk | Check after each bounded unit of RegExp/JSON/string work |
| Comparator callback boundary | Check before and after every comparator call; managed comparator code polls internally |

Cancellation is cooperative. No poll can interrupt arbitrary native instructions between polls or forcibly preempt a blocking native host callback. Wall-clock deadlines may be layered on top of these deterministic checks, but they do not replace a documented work bound or a safe interruption point.

### 5. Long-running built-ins

The prototype exposes an explicit per-built-in strategy:

| Operation | Interrupt strategy | Required bound / caveat |
|---|---|---|
| RegExp | Cooperative engine-step chunks | Poll at most every 64 abstract matching/backtracking steps in the proof policy. A production regexp VM must instrument all potentially unbounded loops, including backtracking; if an engine path cannot poll, apply an explicit input/step cap or do not expose it in a bounded profile. |
| `Array.prototype.sort` with comparator | Poll before and after every comparator call | Managed JS comparators share the execution budget and poll in their own interpreter/AOT loops. A native host comparator is trusted cooperative code and cannot be forcibly preempted by this in-process contract. |
| JSON parse/stringify | Cooperative parser/writer chunks | Poll at most every 256 abstract input/output units and at bounded nesting/element boundaries; enforce separate depth/size limits. |
| String processing | Cooperative scan/copy/search chunks | Poll at most every 256 abstract code-unit/work units; chunk boundaries must preserve UTF-16 semantics and not expose partially committed mutations. |

These numbers are initial deterministic prototype limits, not measured production latency or permanent ECMAScript semantics. Every concrete implementation must select and test its own safe chunk size. Operations that cannot be interrupted safely must either impose a documented maximum input/work bound or be unavailable in a constrained profile; they must never silently be described as bounded.

### 6. Security boundary

**AOT quotas and capability checks are not a process or memory sandbox.** Native AOT code runs in the address space of the Rust host process. Budget polls constrain only generated code and runtime/helper paths that cooperate with the contract. Capability checks constrain access through controlled Ogma host APIs; neither mechanism prevents arbitrary native instructions, memory corruption, a compiler defect or a blocking/untrusted native callback.

Untrusted native code requires a separate OS process or WASM isolation boundary with independently enforced memory, CPU and capability limits. Do not claim that verifier checks, AOT quotas, root maps or safepoints make arbitrary machine code safe to execute in-process.

## Prototype layout and reproduction

`benchmarks/native-runtime-boundary/` is a dependency-free standalone Rust crate. It contains:

- a small non-moving mark/sweep heap to exercise the shared root inventory;
- interpreter frame/closure roots, precise native shadow-frame maps and persistent host handles;
- generation/lifecycle checks and LIFO frame rules;
- activation-stack transitions and explicit completion propagation;
- a shared deterministic budget meter, cancellation token and long-operation policies.

With Rust 1.100.0:

```sh
cargo fmt --manifest-path benchmarks/native-runtime-boundary/Cargo.toml --all -- --check
cargo clippy --manifest-path benchmarks/native-runtime-boundary/Cargo.toml --all-targets --all-features --locked -- -D warnings
cargo test --manifest-path benchmarks/native-runtime-boundary/Cargo.toml --all-features --locked
```

The topic-PR CI job runs those focused commands. It does not run full Test262, full differential execution, browser/E2E acceptance or a production runtime test.

## OGM-27 acceptance evidence map

| Acceptance criterion | Prototype / document evidence |
|---|---|
| Native root representation and lifecycle | `FrameDescriptor`, per-safepoint maps, `NativeFrameStack`, validation and LIFO tests |
| GC sees interpreter/native/persistent roots | `RootProviders::collect`; combined-root and safepoint collection tests |
| Native ↔ interpreter transitions preserve completion/context/callbacks | `ActivationStack`, explicit `Completion`, nested host callback and closure/source-context tests |
| Poll locations and bounded overshoot | `PollSite`, shared `BudgetMeter`, max-window enforcement and overshoot test |
| Long built-ins have explicit interrupt policy | RegExp, comparator sort, JSON and string policy table; chunk/callback boundary tests |
| Adversarial cancellation/callback/exception/GC cases | Nested-callback cancellation, all completion variants including `Throw`, GC during a published native safepoint, malformed map and lifecycle tests |
| AOT is not a sandbox | Security boundary section above |

## Explicit limitations and next integration requirements

This proof does **not** establish:

- compiler-generated stack maps, physical register/spill correctness or a selected production AOT backend;
- an executable Ogma interpreter, JavaScript semantics, real Core IR ingestion or real native/interpreter calls;
- production GC integration, allocator behaviour, finalizer/weak-reference semantics or production pause/throughput;
- actual RegExp, sort, JSON or string implementations and their measured chunk costs;
- hard real-time deadlines, preemption of native host callbacks, OS process isolation or a sandbox;
- async continuation/suspension roots.

Those remain implementation/integration work for the appropriate later runtime/backend scope. This M0 result freezes the shared boundary invariants and a testable prototype only; it must not be reported as production Runtime/AOT completion.
