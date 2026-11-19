# Runtime, GC and memory evidence — OGM-8 M0 synthesis

**Status:** M0 evidence consolidation and architecture handoff  
**Scope:** GC/root strategy, native-frame integration contract, runtime feature profiles and interpreter/hybrid footprint  
**Architecture authority:** [Ogma Notion architecture specification](https://app.notion.com/p/4167ef18f18181bd9b34f9dda764c92d#b5ca462a10c841279d724b74c44b239a), especially sections 0.3–0.4, 7.3 and 9.5  
**Baseline revision:** \`dev\` at \`4efdb3727fbd41e9452df2b3e34b1e16c1109981\` (OGM-29 merge)

## 1. M0 outcome

The completed OGM-26, OGM-27, OGM-28 and OGM-29 proofs provide the evidence requested by the OGM-8 workstream. Their results support the following M0 direction:

1. **Prefer non-moving tracing mark-sweep for the next Runtime integration prototype.** It outperformed RC with cycle removal in all three measured synthetic workloads while preserving identical graph identity and liveness. This is a model-level recommendation, not an irreversible production collector selection.
2. **Keep internal object references and external host handles distinct.** Internal GcRef/ObjectRef values address heap slots; opaque generation-checked HostHandle values represent persistent Rust-side ownership. Ordinary heap operations must not go through the host-handle registry.
3. **Use one explicit root inventory across execution tiers.** Interpreter frames, precise native-frame roots at GC-capable safepoints, persistent host handles and temporary LIFO scopes are all visible to the collector. The proof uses shadow/root frames with exact per-safepoint slot maps. A production AOT backend may use shadow frames or post-register-allocation stack maps, provided it preserves the same invariant.
4. **Keep interruption and GC visibility as separate obligations.** Core IR effects determine where live references must be published; budget/cancellation polls determine where cooperative execution can be interrupted. Sharing a code location does not make one proof a substitute for the other.
5. **Treat runtime profiles as semantic-preserving build contracts.** The hybrid profile retains portable interpreter fallback, and general profiles do not silently remove eval, new Function or other dynamic-code semantics. Node compatibility and developer tooling remain explicit overlays.
6. **Use the OGM-29 results as a proof-tool baseline only.** They quantify the existing controlled harnesses and their process/toolchain costs. They are not production Runtime startup, binary size, AST memory, steady-state RSS or interpreter-versus-AOT throughput.

These conclusions complete the M0 evidence and decision scope. They do not claim that the production JavaScript Runtime, collector, interpreter, AOT compiler, generated stack maps or profile-linked Runtime binaries exist.

## 2. Evidence map and integration status

All four child issues are Done and their PRs are merged into dev. The current dev baseline is the OGM-29 merge commit listed above.

| Workstream | Delivered proof | PR / merge | Verified CI evidence |
|---|---|---|---|
| [OGM-26 — GC comparison](https://linear.app/ogma-engine/issue/OGM-26/compare-rc-with-cycle-removal-against-non-moving-tracing-mark-sweep-gc) | Equivalent Rust-native heap model, RC + trial deletion versus non-moving tracing mark-sweep, common roots and paired measurements | [PR #23](https://github.com/AiratGaliev/ogma/pull/23), merge \`13f77e6012ba741632ba37d0b283dcfd129cc292\` | [Measurement run #340](https://github.com/AiratGaliev/ogma/actions/runs/44326700282); [PR-head checks #341](https://github.com/AiratGaliev/ogma/actions/runs/44327205677): Fast CI and OGM-26 proof passed |
| [OGM-27 — native roots and transitions](https://linear.app/ogma-engine/issue/OGM-27/prove-native-frame-roots-safepoints-and-interpreteraot-runtime) | Precise native shadow-root maps, root-provider enumeration, frame lifecycle, completions, callbacks and bounded cooperative polling | [PR #24](https://github.com/AiratGaliev/ogma/pull/24), merge \`ae4ebf74e20c5507797f9eadda2bcdfdbba67702\` | [Run #44333039884](https://github.com/AiratGaliev/ogma/actions/runs/44333039884): Fast CI and OGM-27 proof passed |
| [OGM-28 — runtime feature profiles](https://linear.app/ogma-engine/issue/OGM-28/define-minimal-runtime-feature-profiles-and-production-stripping-rules) | Versioned profile schema, dependency closure, semantic invariants and metadata/stripping policy | [PR #25](https://github.com/AiratGaliev/ogma/pull/25), merge \`60ef1d06898b6c3476ee651c73b72cb050ba61d4\` | [PR CI #383](https://github.com/AiratGaliev/ogma/actions/runs/44339677514) and [post-merge CI #384](https://github.com/AiratGaliev/ogma/actions/runs/44339988364): Fast CI and OGM-28 profile proof passed |
| [OGM-29 — startup, memory and compile footprint](https://linear.app/ogma-engine/issue/OGM-29/measure-interpreter-only-and-hybrid-startup-memory-and-compilation) | Unified same-fixture portable/native proof measurements, machine-readable summary and raw CI report | [PR #29](https://github.com/AiratGaliev/ogma/pull/29), merge \`4efdb3727fbd41e9452df2b3e34b1e16c1109981\` | [PR CI #483](https://github.com/AiratGaliev/ogma/actions/runs/44539965495): Fast CI and OGM-29 footprint proof passed; [measurement/report run #478](https://github.com/AiratGaliev/ogma/actions/runs/44538873406) |

The task-scoped CI runs execute focused proof checks, not the complete Test262 or browser/E2E acceptance suites. Full conformance remains a promotion gate under the repository workflow; it is not a missing OGM-8 child proof.

## 3. GC direction and object/handle lifecycle

### 3.1 Comparable GC result

OGM-26 compares two policies over the same non-moving, generation-checked object-slot heap:

- **RC + cycle removal:** reference counts on graph/root mutations, immediate reclamation of acyclic objects and trial deletion for unreachable cycles.
- **Tracing mark-sweep:** trace from the shared root inventory and reclaim unmarked slots without relocating objects.

The paired benchmark uses 2,048 objects, three warmups and 101 samples per collector for each of three workloads: balanced roots/cycles, persistent host retention and allocation churn. Candidate order alternates across samples. The validator recomputes aggregate metrics from raw observations and checks graph identity, root inventory and liveness consistency.

| Workload | Mark-sweep collection p50 improvement | Mark-sweep build + collection p50 improvement |
|---|---:|---:|
| Balanced roots and cycles | 44.8% | 20.4% |
| Persistent host retention | 46.7% | 19.8% |
| Allocation churn | 32.3% | 18.5% |

Mark-sweep also had lower p95 and p99 collection pauses in all three workloads. The paired samples had matching graph fingerprints and identical final live/reclaimed object counts. Root-inventory scan cost was similar between the candidates.

**M0 direction:** carry non-moving tracing mark-sweep forward as the preferred Runtime integration candidate. Do not describe this as a final production GC lock. The benchmark's logical heap-byte model deliberately excludes collector-specific side tables/scratch storage, vector spare capacity, allocator metadata, fragmentation and process RSS; it cannot establish production memory overhead.

Raw evidence is committed in [the OGM-26 compressed report](../benchmarks/gc-comparison/results/ogm-26-gc-comparison-report.json.gz). The reproducible method, detailed measurements, limitations and SHA-256 values are documented in [the GC comparison README](../benchmarks/gc-comparison/README.md). The CI report is from source revision \`4a572f992bf522278b053335be38768254fb3109\`, tested revision \`044beb35569eac2a9a3ed6738dc76b67b5f8bda8\`.

### 3.2 Internal references, external handles and roots

The common lifecycle contract is:

| Concept | Responsibility | Lifetime / rule |
|---|---|---|
| GcRef / ObjectRef | Internal heap slot + generation identity | Used for normal Runtime object/property/element operations; stale generations must not resolve after slot reuse |
| HostHandle | Opaque external Rust/ABI token with its own slot + generation | Persistent host ownership roots the target; release removes that root and invalidates the token |
| Interpreter frame roots | Live references in VM locals/registers, operand storage and closure environments | Explicitly enumerated while the activation is live |
| Native frame roots | Live managed references in a published native frame map | Exact map must be active before any operation that may GC or re-enter script code |
| Temporary root scopes | Short-lived host/runtime roots | Explicit strict-LIFO lifetime |

The root-provider inventory is shared conceptually across the collector and execution tiers. It must never depend on conservative scanning of arbitrary Rust stack memory. Internal object accesses must not pay an external HostHandle registry lookup on the hot path.

### 3.3 Native frames, effects and completion propagation

OGM-27 exercises precise shadow/root frames whose descriptors map each GC-capable safepoint to the exact live managed-reference slots. The model rejects duplicate or unknown safepoints, duplicate/out-of-range slots, non-managed values in root slots and mismatches with an independent expected-live-slot set.

For production AOT, a stack-map implementation must describe the final register spills and stack slots after register allocation. A shadow-frame implementation must keep every live managed reference in a registered slot. Pre-register-allocation liveness alone is not a valid native root map.

Core IR effects establish the obligations:

- **may_allocate / may_gc:** publish all live managed references before a collecting helper.
- **may_call_host / may_reenter:** preserve the caller frame, closure/source context and completion state; do not hold an unsafe mutable Runtime/heap borrow across callbacks.
- **may_throw:** keep the pending completion and its managed payload rooted until consumed or transferred.
- **may_suspend:** use an explicit continuation/root representation; a native stack frame cannot be assumed to survive suspension.

The proof carries explicit Normal, Return, Break, Continue and Throw completions across native/interpreter/host transitions. A script Throw is not an ABI transport failure, and Rust panic/unwind must not cross the external ABI. A returned managed value must be promoted to a matching persistent host handle before releasing its temporary execution root.

A **safepoint** is a published GC/re-entry state; a **budget poll** is a cooperative interruption check. They may coincide, but are independently required. The shared execution meter polls at function entry, loop back-edges, allocation and call boundaries, and bounded chunks of long-running operations. The proof defines initial abstract work-unit policies for RegExp, sort, JSON and string operations. Those are prototype limits, not production latency guarantees. Cooperative polling cannot preempt arbitrary native instructions or a blocking/untrusted native host callback.

The complete contract, evidence map, reproduction commands and limitations are in [Native Runtime boundary, roots and safepoints](native-runtime-boundary-contract.md) and [the standalone OGM-27 proof](../benchmarks/native-runtime-boundary/README.md).

## 4. Runtime feature and stripping contract

OGM-28 provides a versioned logical build graph with **20 components and eight configurations** across three independent dimensions: build mode, execution backend and host compatibility.

| Configuration family | Contract |
|---|---|
| Debug / Portable Interpreter | Runtime + interpreter + verifier and debugger/inspection tools |
| Debug / Hybrid Native | Same tooling with native root/AOT boundary plus portable fallback |
| Development / Portable Interpreter | Interpreter with DAP, Inspector/REPL, profiler and Hot Reload tooling |
| Development / Hybrid Native | Development tooling plus native path and portable fallback |
| Release / Portable Interpreter | Minimal portable Runtime without developer tooling by default |
| Release / Hybrid Native | Native AOT plus portable fallback, without developer tooling by default |
| Release / Node + Portable Interpreter | Explicit Node compatibility overlay on portable Runtime |
| Release / Node + Hybrid Native | The same separate Node overlay on hybrid Runtime |

The versioned profile manifest and JSON Schema are [here](../contracts/runtime-profiles/v1/profiles.json) and [here](../contracts/runtime-profiles/v1/profile-set.schema.json); the owning explanation and metadata lifecycle are in [Runtime feature profiles and production stripping](runtime-feature-profiles.md).

The invariants relevant to OGM-8 are:

- Hybrid AOT is additive and retains the portable interpreter for dynamic source and code that cannot be lowered safely.
- ECMAScript semantics are not selectable tooling features. General profiles preserve dynamic-code semantics; eval/new Function must not be silently disabled to reduce footprint.
- Debug/Development tooling and metadata have explicit inclusion and stripping rules. DAP, UI/REPL, profiler and Hot Reload are excluded from Release by default, not conflated with language capabilities.
- Node compatibility is a separate host overlay; enabling it does not grant ambient filesystem, process or network capabilities.
- Include roots resolve through the complete dependency graph. Unknown components, cycles or exclusion of a required dependency fail closed.

These are **logical build-profile contracts**, not actual Cargo features or proof that distinct production Runtime binaries have been built. The current repository has no root Cargo workspace or production Runtime crate graph.

## 5. OGM-29 resource and compile footprint baseline

### 5.1 Measurement protocol

OGM-29 runs the same five canonical Core IR v1 fixtures — branch, loop, dynamic exception, call boundary and host call — on Ubuntu 24.04 / x86_64 Linux. The recorded environment is Python 3.12.16, Rust/Cargo 1.100.0 and LLVM/Clang 18.1.3 on an AMD EPYC 9V45 runner. Startup measurements use three warmups and 101 process launches per tool; native compile/link measurements use 101 samples. Portable bytecode compile/verification distributions use 15 rounds.

The two columns below are deliberately different measurement boundaries. They must not be read as a production interpreter-versus-AOT speed comparison.

### 5.2 Measured proof results

| Measurement | Portable path | Hybrid/native path |
|---|---:|---:|
| Startup p50 / p95 / p99 | Python proof worker: 61.73 / 75.58 / 85.32 ms | Native benchmark tool: 10.18 / 13.79 / 15.88 ms |
| Compile p50 / p95 | Core IR fixture → register .ogmb in Python proof: 1.371 / 1.881 ms | Cranelift fixture compile: 0.288 / 0.470 ms; LLVM: 3.739 / 4.395 ms |
| Link p50 / p95 | Not applicable | Cranelift: 53.36 / 62.53 ms; LLVM: 53.86 / 65.36 ms |
| Process peak RSS | Python proof worker: 24,535,040 B | Native benchmark process: 80,384,000 B, including both compiler backends, linking, mock Runtime setup and workloads |
| Other memory observations | Source read peak: 8,930 B; Core IR JSON parse peak: 95,635 B; bytecode compile tracemalloc peak: 162,008 B | Actual production Runtime heap/RSS not measured |
| Output size | Five proof .ogmb containers: 1,482–2,426 B | Benchmark executable: 10,022,656 B; Cranelift .o/.so: 2,944 / 26,488 B; LLVM .o/.so: 3,112 / 26,576 B |
| Host-call overhead proxy | Python proof path only; not comparable to native timings | Mock native ABI helper: 163.3 ns (Cranelift), 184.3 ns (LLVM), above a direct native helper |

The portable path is a Python bytecode proof interpreter, not a Rust VM. The native path uses exact-fixture adapters, a mock C ABI and modeled Runtime boundaries, not a complete hybrid Runtime application. The native process RSS includes both compilers and is not an isolated AOT compiler peak or steady-state Runtime RSS. The 10,022,656-byte executable is benchmark tooling, not a shipped Ogma binary.

### 5.3 Explicitly unavailable production measurements

The report records these as not implemented rather than estimating them:

- production interpreter-only or hybrid Runtime startup;
- shipped Runtime executable/library size and profile-specific binary deltas;
- production steady-state and peak Runtime RSS / managed-heap overhead;
- JavaScript AST peak memory and production source-to-Core-IR frontend allocation;
- the final shipped Runtime Cargo dependency graph;
- production GC pause/throughput, real interpreter dispatch and real host-call overhead.

AST memory is unavailable because there is no production JavaScript parser/AST in the M0 repository. Core IR JSON fixture materialization is a separate input-memory proxy, not AST memory. Likewise, the 70-package Cargo graph collected for the native benchmark executable is a build-time tooling graph, not the shipped Runtime dependency graph.

Reproduction commands, schemas, the compact report and full raw CI report are recorded in [the runtime-footprint README](../benchmarks/runtime-footprint/README.md) and [the committed OGM-29 summary](../benchmarks/runtime-footprint/results/ogm29-footprint-summary.json). The full report artifact is [CI run #478](https://github.com/AiratGaliev/ogma/actions/runs/44538873406); its raw JSON SHA-256 is \`a1e2521af8b3ecffd15c68aef09152015c5aca4e91a26c528cf7df102864c760\`.

## 6. OGM-8 acceptance and production boundary

| OGM-8 requirement | M0 result | Qualification |
|---|---|---|
| Compare RC + cycle removal with non-moving tracing mark-sweep using equivalent graphs/roots | **Satisfied as a model proof** | Mark-sweep is preferred for next integration; final production choice remains open |
| Separate internal GcRef/ObjectRef from external HostHandle lifecycle | **Satisfied as a model contract** | Real Runtime object layout and host API integration remain future implementation work |
| Native roots, safepoints, transitions, completions and cancellation polls | **Satisfied as a boundary prototype** | No executable Ogma interpreter/AOT compiler or compiler-generated native maps |
| Minimal feature profiles and semantic-preserving stripping | **Satisfied as a logical profile contract** | No production Cargo feature graph or profile-linked Runtime binaries |
| Interpreter/hybrid startup, binary, RSS, host-call and compile footprint | **Partially measured within M0 proof boundaries** | The production Runtime/AST/frontend measurements remain explicitly unavailable |

Do not infer any of the following from this workstream:

- that Ogma already has a production ECMAScript heap, tracing collector, interpreter or Native AOT compiler;
- that synthetic GC pause results predict production latency or collector memory overhead;
- that OGM-29 tool startup, RSS or executable sizes describe a packaged Runtime;
- that root-map validation proves physical register/spill correctness of generated machine code;
- that cooperative quotas, capabilities, safepoints or bytecode verification sandbox arbitrary native AOT code.

**AOT is not a process or memory sandbox.** Native code executes inside the host process. Strong isolation for untrusted code requires a separately enforced OS-process or WASM boundary. Similarly, cooperative cancellation cannot forcibly interrupt a blocking native host callback.

## 7. Handoff to actual Runtime/AOT integration

When the production Runtime and AOT path exist, revalidate these M0 contracts against real implementation artifacts rather than expanding the proof harnesses into speculative production abstractions:

1. Integrate the non-moving collector candidate with the real heap/object layout and measure collector-owned side tables, allocation overhead, pause distributions, peak RSS and fragmentation separately.
2. Enumerate real interpreter values and closure environments; verify native roots after final register allocation/spilling or through registered shadow frames at every GC-capable safepoint.
3. Exercise actual JS object cycles, host-handle retain/release, stale-generation rejection, finalization/weak-reference semantics, nested callbacks, exceptions, cancellation and collection at re-entry boundaries.
4. Add explicit continuation roots if execution can suspend; do not keep native stack frames alive by assumption.
5. Build actual profile-linked interpreter and hybrid artifacts. Re-measure startup, linked size, compile cost, memory and host-call overhead on the same target and representative workloads.
6. Preserve interpreter fallback and ECMAScript dynamic-code behaviour in hybrid builds; any unsupported language capability requires an explicit compatibility-profile diagnostic, not silent stripping.

These are revalidation requirements for later implementation milestones. This document does not create M1/M3 implementation scope or claim that those requirements are already implemented.

## 8. Reproduction entry points

Run the focused proof commands from the repository root with the pinned Rust toolchain. Each proof owns its own standalone manifest; there is no root Cargo workspace yet.

~~~sh
cargo fmt --manifest-path benchmarks/gc-comparison/Cargo.toml --all -- --check
cargo clippy --manifest-path benchmarks/gc-comparison/Cargo.toml --all-targets --all-features --locked -- -D warnings
cargo test --manifest-path benchmarks/gc-comparison/Cargo.toml --all-features --locked

cargo fmt --manifest-path benchmarks/native-runtime-boundary/Cargo.toml --all -- --check
cargo clippy --manifest-path benchmarks/native-runtime-boundary/Cargo.toml --all-targets --all-features --locked -- -D warnings
cargo test --manifest-path benchmarks/native-runtime-boundary/Cargo.toml --all-features --locked

python -m unittest discover -s .github/scripts -p "test_runtime_profiles.py" -v
python -m unittest discover -s benchmarks/runtime-footprint -p "test_*.py" -v
~~~

The CI runs linked above are the recorded execution evidence for the merged child proofs. The commands in this section are reproduction entry points; they are not a claim that they were run locally for this documentation-only OGM-8 synthesis.
