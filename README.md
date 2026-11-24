# Ogma

**Ogma is a Rust-native JavaScript/TypeScript execution engine for native Rust applications.** It is intended to provide an embeddable ECMAScript runtime with portable bytecode execution and native ahead-of-time compilation, while keeping host integration and optional Node.js compatibility behind explicit contracts.

Ogma is an independent engine project. It is not a wrapper around QuickJS-NG, a replacement for the host application/game engine, or a requirement to ship the Node.js runtime with a packaged native application.

> **Current phase: M1 — Minimal Executable Runtime.** The root Rust workspace and minimal M1 crate boundaries are in place. OGM-41 has implemented the JavaScript parser/source-diagnostics frontend and a parse-only conformance adapter. The production compiler/Core IR lowering, ECMAScript Runtime execution, interpreter and embedding behavior are still under implementation; parser availability must not be described as runtime or full language conformance.

## Project direction

The target architecture is built around these boundaries:

- **Rust-first implementation.** Ogma owns its parser/frontend, runtime, execution engines and Rust embedding API. QuickJS-NG v0.18.0 is a pinned engineering/reference baseline, not a production runtime dependency.
- **ECMAScript semantics.** JavaScript defines runtime behaviour. TypeScript is a frontend and compile-time layer; TypeScript type metadata is not mixed with runtime JavaScript values.
- **One backend-neutral Core IR.** Function-level control flow, value flow, JavaScript semantic operations, effects/exceptions, closure/environment data and source metadata are shared by execution backends. The M0 v1 contract and verifier are documented in [Core IR Contract](docs/core-ir-contract.md) and [JSON Schema](contracts/core-ir/v1/schema.json).
- **Two required execution paths.** A versioned portable .ogmb bytecode format with a verifier and interpreter, plus target-specific native AOT artifacts. Both use the same Ogma Runtime and host ABI; native objects are not a substitute for portable bytecode. The M0 container/verifier proof is documented in [Portable Bytecode Contract](docs/portable-bytecode-contract.md), with separate [manifest metadata](contracts/ogmb/v1/manifest-metadata.schema.json), [decoder verification-input](contracts/ogmb/v1/verification-input.schema.json) and [source-map](contracts/ogmb/v1/source-map.schema.json) schemas.
- **Backend-neutral debugging.** Source provenance, stable debugger DTOs, an interpreter-first M0 proof and the DAP adapter boundary are specified in [Debugger API Contract](docs/debugger-contract.md) and its [versioned schema](contracts/debugger/v1/api.schema.json). The current implementation is a proof harness, not a production debugger/runtime.
- **First-class Rust embedding.** A versioned ABI, opaque handles, explicit roots and ownership rules, callbacks, controlled reentrancy and host-driven scheduling define the Rust ↔ Ogma boundary. The current M0 mock ABI proof and its limits are documented in [Rust ABI Contract](docs/rust-abi-contract.md); this is not a production Runtime. The typed host-function registration and guarded-dispatch prototype is documented in [Typed Host Dispatch Contract](docs/typed-host-dispatch-contract.md); it also remains an M0 proof, not production Runtime integration.
- **Rust embedding code generation (planned M2).** The existing JS/TS compiler will be able to emit a thin Rust module plus the same canonical `.ogmb` artifact. The module embeds the bytes at Rust compile time; execution still goes through the public Ogma Runtime and verifier. This is not JS/TS-to-Rust transpilation. See [Rust Embedding Code Generation Contract](docs/rust-embedding-codegen-contract.md) and [OGM-106](https://linear.app/ogma-engine/issue/OGM-106/generate-rust-embedding-modules-from-portable-ogmb-artifacts).
- **Native Runtime boundary.** A separate M0 prototype exercises precise per-safepoint native shadow-root maps, interpreter/native/persistent-root enumeration, explicit completion transitions, nested callbacks and deterministic cancellation/budget policies. The [Native Runtime Boundary Contract](docs/native-runtime-boundary-contract.md) documents the shared invariants and the proof's limits; compiler-generated AOT stack maps and production Runtime integration are not implemented yet.
- **Buffer interoperability.** Owned transfer, external ownership, scoped borrow/revocation, alignment, detach/resize and copy-versus-zero-copy evidence are documented in [Buffer Interoperability Contract](docs/buffer-interop-contract.md). The standalone M0 proof is in benchmarks/buffer-interop/; it is not a production Runtime integration.
- **Separate Node.js compatibility layer.** Node-compatible module resolution, built-ins and scheduling are optional host-layer capabilities with explicit compatibility profiles. Ogma does not claim universal Node.js or npm compatibility.
- **Shared Native Providers.** Host Modules may require logical native services implemented by trusted Rust providers registered through the existing Host API/ABI. The same service can serve Node-compatible and Ogma-native APIs; Cargo remains the Rust dependency resolver.
- **One Ogma CLI.** The `ogma` executable is the user-facing compile/run/inspect/check/build orchestrator. It delegates JavaScript package operations to pnpm and Rust dependency resolution/builds to Cargo rather than reimplementing either ecosystem.
- **Optional JIT.** JIT is a later research direction, not a prerequisite for the portable interpreter or native AOT.

The [Notion architecture specification](https://app.notion.com/p/Ogma-JS-TypeScript-Execution-Engine-4167ef18f18181bd9b34f9dda764c92d#b5ca462a10c841279d724b74c44b239a) is the authoritative source for technical decisions. The repository implementation contracts for [Native Providers](docs/native-provider-architecture.md), the [unified CLI](docs/toolchain-cli-contract.md), [Static Host Modules](docs/static-host-modules-contract.md) and [Runtime Feature Profiles](docs/runtime-feature-profiles.md) translate those decisions into implementation boundaries.

## Current milestone and roadmap

M0 architectural proofs and contracts provide the baseline for the authorized M1 implementation. M1 engineering work is tracked through OGM-39 and its scoped child issues; the existence of a workspace does not imply that production engine subsystems are implemented.

| Milestone | Scope | Expected result |
|---|---|---|
| **M0 — Architectural proofs and decisions** | Compatibility baseline, Core IR and bytecode contracts, Rust ABI/embedding, host-call and buffer spikes, GC/root strategy, AOT backend comparison, dual-backend proof, debugger/test architecture and CI proof. | Reproducible evidence and accepted architectural decisions for the minimal runtime. |
| **M1 — Minimal executable runtime** | JavaScript subset frontend, Core IR, portable bytecode/verifier/interpreter, runtime values/objects/functions/exceptions, selected GC, module execution, basic Rust embedding and the initial `ogma compile/run/inspect` CLI. | A real embeddable runtime using portable bytecode and a usable initial CLI. |
| **M2 — TypeScript frontend and bindings** | TypeScript lowering, Rust binding generation, Rust embedding-module codegen from canonical `.ogmb`, declarations/metadata, scoped handles, async bridge and initial Node compatibility adapters. | Rust APIs can be consumed from TypeScript, and compiled JS/TS can be embedded in Rust applications through generated modules. |
| **M3 — Native AOT & application toolchain** | Selected native backend, Core IR lowering, native roots/safepoints, mixed interpreter/AOT execution, Native Provider resolution, `ogma check`, `ogma deps plan/why`, provider inspection, `ogma build`, pnpm lockfile ingestion, linking and deterministic application packaging. | Native AOT integrated with the shared Runtime and a reproducible Rust-hosted application build. |
| **M4 — Compatibility and hardening** | Test262 and differential conformance, GC/security/fuzz/consumer tests, platform coverage and performance work. | Measured compatibility, reliability and production readiness. |
| **M5 — Optional JIT research** | Only if profiling demonstrates a material need for dynamic-code acceleration. | Evidence-based decision to implement or reject JIT. |

Track implementation work in the [Ogma Linear project](https://linear.app/ogma-engine/project/ogma-rust-native-jstypescript-execution-engine-1f94795b107b) and use the existing authorized issues for each milestone. Do not create duplicate implementation tasks or add work outside the active issue scope.

## Development workflow

The mandatory repository workflow is documented in [Development Workflow](docs/development-workflow.md). Every coding agent and contributor must also follow [AGENTS.md](AGENTS.md).

The permanent branch flow is:

**topic branch → dev → test → main**

- Start normal work from the latest dev in a focused topic branch.
- Use a PR for every change, including documentation-only changes. Never commit directly to dev, test or main.
- Merge ordinary implementation PRs into dev; promote a complete candidate from dev to test; release only from an accepted test candidate to main.
- Keep one coherent Linear issue per topic branch/PR. Record acceptance criteria, dependencies and actual verification evidence.
- Do not weaken or bypass failing checks. Do not auto-merge or merge a PR without the required review and explicit authorization.
- Keep tests and documentation in the same change when behaviour, contracts, formats or workflows change.

### CI and conformance policy

- **Fast CI** is the routine gate for topic PRs into dev and ordinary dev pushes. It validates repository documentation and routing, runs the Python repository/verifier/runner self-tests, verifies Core IR and .ogmb fixture corpora, compiles helpers, and runs the pinned Node.js package-resolution and scheduling reference fixtures.
- **Parser and runtime conformance are separate.** Fast CI builds the real OGM-41 parser adapter and runs a bounded six-case parser smoke. Runtime Test262 and QuickJS-NG differential execution still require their own executable adapters; fake adapters remain limited to protocol self-tests.
- **Full conformance and integration suites do not run on topic PRs.** The complete pinned Test262 corpus, extended differential suite and promotion-level integration checks belong to dev → test and later release-promotion gates. The bounded same-Core-IR portable/native proof is a separate M0 task-specific check, not a general conformance claim.
- Test coverage is developed with each implementation. Add or update focused regression, parser, ABI and integration fixtures in the same topic PR when the changed contract needs them; delaying the full-suite run must not delay creating tests.
- Long-running fuzzing, stress, GC/lifecycle, resource-limit and broad platform matrices belong in scheduled/manual lanes unless a specific change requires a focused check.
- M0 benchmark spikes are task-scoped experiments, not an unconditional benchmark job for every PR. Every result must identify its commit, toolchain, workload, configuration, repetitions, metrics and limitations.
- The CI Fast workflow runs Rust formatting, workspace Clippy and tests because the root Cargo workspace is now present. These checks establish workspace build health, not parser/runtime conformance.

### Current repository bootstrap status

The root Cargo workspace and minimal M1 crate boundaries are present. OGM-41 delivered a real JavaScript parser/frontend and parse-only adapter; a production ECMAScript Runtime and runtime Test262 adapter are not yet available. OGM-33, OGM-34, OGM-35 and OGM-37 delivered the debugger contracts, test-runner infrastructure, CI/promotion wiring and repository workflow. Parser availability is real, but runtime conformance is not yet available.

- **CI Fast** runs for PRs to dev/test/main and pushes to those permanent branches. The current dev head has a successful fast-checks run with 203 Python tests; the exact evidence and other child-task runs are indexed in [OGM-10 Developer Tooling & Verification evidence](docs/developer-tooling-verification-evidence.md).
- **Conformance Promotion** runs for PRs targeting test or main. OGM-35 delivered exact route validation, eight deterministic Test262 shards, four parser-corpus shards, JSON/JUnit artifacts and a required aggregate. OGM-41 marks parser readiness as `ready`; full parser-corpus promotion requires `OGMA_PARSER_ADAPTER` to be configured. Runtime execution remains `not_implemented`, so runtime Test262 promotion remains fail-closed. No full parser or runtime conformance result is claimed.
- The pinned Test262, test262-parser-tests and QuickJS-NG revisions are recorded in conformance/baseline.json. The active readiness and feature declarations are in conformance/profile.json. Parser readiness is `ready` after OGM-41; runtime execution must remain `not_implemented` until an executable Runtime adapter and focused tests exist.
- The OGM-31 same-Core-IR proof demonstrates bounded portable/interpreter and native-AOT fixture equivalence. It does not implement source parsing or establish general runtime/differential conformance.
- Active GitHub rulesets protect dev, test and main. All three require PRs, block deletion and non-fast-forward updates, and have no bypass actors. dev requires fast-checks; test and main require fast-checks plus conformance-promotion.
- The rulesets are [protect-dev](https://github.com/AiratGaliev/ogma/rules/27364036), [protect-test](https://github.com/AiratGaliev/ogma/rules/27364037) and [protect-main](https://github.com/AiratGaliev/ogma/rules/27364038).

The promotion infrastructure is implemented, but a configured gate is not a passed conformance suite. The parser-only path now has a real adapter, but full parser-corpus promotion still requires the verified command to be configured and the full pinned corpus to pass. Keep runtime Test262 fail-closed until a real Ogma Runtime adapter produces supported results. Root Cargo checks and the bounded parser smoke do not establish runtime or full parser conformance.

## Source of truth and project links

- [Architecture and implementation specification (Notion)](https://app.notion.com/p/Ogma-JS-TypeScript-Execution-Engine-4167ef18f18181bd9b34f9dda764c92d#b5ca462a10c841279d724b74c44b239a) — authoritative architecture and milestone scope.
- [Ogma project in Linear](https://linear.app/ogma-engine/project/ogma-rust-native-jstypescript-execution-engine-1f94795b107b) — issues, M1 milestone, dependencies and delivery evidence.
- [Rust embedding & host-performance evidence (OGM-7)](docs/rust-embedding-host-performance-evidence.md) — consolidated index of the M0 ABI, dispatch, buffer, frame, allocator and static host-module proofs, measurements and limitations.
- [Rust Embedding Code Generation Contract](docs/rust-embedding-codegen-contract.md) — planned M2 generation of Rust modules that embed canonical portable `.ogmb` artifacts; not JS/TS-to-Rust transpilation.
- [Runtime, GC & memory evidence (OGM-8)](docs/runtime-gc-memory-evidence.md) — consolidated M0 collector/root recommendation, native-frame contract, runtime profiles, measured proof-tool footprint and production limitations.
- [Developer Tooling & Verification evidence (OGM-10)](docs/developer-tooling-verification-evidence.md) — integrated debugger/source metadata, conformance runner, CI/promotion gates, delivery traceability and explicit M0 limits.
- [Development Workflow](docs/development-workflow.md) — branches, PR rules, CI/testing, Linear statuses and definitions of done.
- [Compatibility Profile](docs/compatibility-profile.md) — versioned ECMAScript, TypeScript, Node.js API, module-resolution and consumer compatibility contract.
- [Runtime feature profiles and production stripping (OGM-28)](docs/runtime-feature-profiles.md) — dependency-aware build profiles, language-semantic invariants, tooling/metadata lifecycle and M0 footprint evidence.
- [Test runner and corpus architecture](docs/test-runner-and-corpus.md) — pinned conformance sources, adapter protocol, feature profile, result classes and deterministic shard aggregation.
- [AGENTS.md](AGENTS.md) — repository-wide instructions for coding agents.
- [LICENSE](LICENSE) — project license.

Update this README when project direction, milestone boundaries or repository workflow changes. Describe implemented behaviour only when supported by source code and tests; keep planned capabilities clearly labelled as planned.
