# Ogma

**Ogma is a Rust-native JavaScript/TypeScript execution engine for native Rust applications.** It is intended to provide an embeddable ECMAScript runtime with portable bytecode execution and native ahead-of-time compilation, while keeping host integration and optional Node.js compatibility behind explicit contracts.

Ogma is an independent engine project. It is not a wrapper around QuickJS-NG, a replacement for the host application/game engine, or a requirement to ship the Node.js runtime with a packaged native application.

> **Current phase: M0 — Architectural Proofs & Decisions.** The repository is at its initial bootstrap stage. The target architecture is documented, but the production runtime and Rust workspace have not been implemented yet. Planned capabilities must not be described as available functionality.

## Project direction

The target architecture is built around these boundaries:

- **Rust-first implementation.** Ogma owns its parser/frontend, runtime, execution engines and Rust embedding API. QuickJS-NG v0.18.0 is a pinned engineering/reference baseline, not a production runtime dependency.
- **ECMAScript semantics.** JavaScript defines runtime behaviour. TypeScript is a frontend and compile-time layer; TypeScript type metadata is not mixed with runtime JavaScript values.
- **One backend-neutral Core IR.** Function-level control flow, value flow, JavaScript semantic operations, effects/exceptions, closure/environment data and source metadata are shared by execution backends.
- **Two required execution paths.** A versioned portable .ogmb bytecode format with a verifier and interpreter, plus target-specific native AOT artifacts. Both use the same Ogma Runtime and host ABI; native objects are not a substitute for portable bytecode.
- **First-class Rust embedding.** A versioned ABI, opaque handles, explicit roots and ownership rules, callbacks, controlled reentrancy and host-driven scheduling define the Rust ↔ Ogma boundary.
- **Separate Node.js compatibility layer.** Node-compatible module resolution, built-ins and scheduling are optional host-layer capabilities with explicit compatibility profiles. Ogma does not claim universal Node.js or npm compatibility.
- **Optional JIT.** JIT is a later research direction, not a prerequisite for the portable interpreter or native AOT.

The [Notion architecture specification](https://app.notion.com/p/Ogma-JS-TypeScript-Execution-Engine-4167ef18f18181bd9b34f9dda764c92d#b5ca462a10c841279d724b74c44b239a) is the authoritative source for technical decisions.

## Current milestone and roadmap

Only M0 is currently scheduled in Linear. M0 is a proof-and-decision phase, not a claim that production engine subsystems already exist.

| Milestone | Scope | Expected result |
|---|---|---|
| **M0 — Architectural proofs and decisions** | Compatibility baseline, Core IR and bytecode contracts, Rust ABI/embedding, host-call and buffer spikes, GC/root strategy, AOT backend comparison, dual-backend proof, debugger/test architecture and CI proof. | Reproducible evidence and accepted architectural decisions for the minimal runtime. |
| **M1 — Minimal executable runtime** | JavaScript subset frontend, Core IR, portable bytecode/verifier/interpreter, runtime values/objects/functions/exceptions, selected GC, module execution and basic Rust embedding. | A real embeddable runtime using portable bytecode. |
| **M2 — TypeScript frontend and bindings** | TypeScript lowering, Rust binding generation, declarations/metadata, scoped handles, async bridge and initial Node compatibility adapters. | Rust APIs can be consumed from TypeScript with generated bindings. |
| **M3 — Native AOT** | Selected native backend, Core IR lowering, native roots/safepoints, mixed interpreter/AOT execution, object emission and native debugging. | Native AOT integrated with the shared Runtime. |
| **M4 — Compatibility and hardening** | Test262 and differential conformance, GC/security/fuzz/consumer tests, platform coverage and performance work. | Measured compatibility, reliability and production readiness. |
| **M5 — Optional JIT research** | Only if profiling demonstrates a material need for dynamic-code acceleration. | Evidence-based decision to implement or reject JIT. |

Track implementation work in the [Ogma Linear project](https://linear.app/ogma-engine/project/ogma-rust-native-jstypescript-execution-engine-1f94795b107b). Do not create M1–M5 implementation issues in the active Kanban before those milestones are authorized for execution.

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

- **Fast CI** is the routine gate for topic PRs into dev: formatting, compile/static checks, focused unit and regression tests, security/dependency checks and a small bounded Test262/differential smoke where the runner supports it.
- **Full conformance and integration suites do not run on topic PRs.** The complete pinned Test262 corpus, full differential suite, interpreter/AOT equivalence and promotion-level integration checks belong to the dev → test promotion gate and later release-promotion gates. Do not add path-based or manual-dispatch exceptions that move the full suite onto feature PRs.
- Test coverage is still developed with the implementation. Add or update focused regression, parser, ABI and integration fixtures in the same topic PR when the changed contract needs them; delaying the full-suite run must not delay creating tests.
- Long-running fuzzing, stress, GC/lifecycle, resource-limit and broad platform matrices belong in scheduled/manual lanes unless a specific change requires a focused check.
- M0 benchmark spikes are task-scoped experiments, not an unconditional benchmark job for every PR. Every result must identify its commit, toolchain, workload, configuration, repetitions, metrics and limitations.
- The implemented bootstrap `CI Fast` workflow validates PR routing, Markdown encoding/structure/local links, its own validator tests, Python helper syntax and the pinned Node.js 26.13.0 package-resolution reference corpus. Rust formatting, Clippy and workspace tests are added conditionally when a root `Cargo.toml` exists; the current repository has no Rust workspace, so these steps are not reported as passing.
- `Conformance Promotion` runs only for PRs targeting test or main. It validates the promotion route and deliberately fails closed until OGM-35 supplies the pinned Test262/differential runner and deterministic shard aggregator. Promotion is therefore intentionally blocked rather than silently accepted without conformance evidence.
- Full Test262, full differential, interpreter/AOT equivalence and integrated runtime checks do not run on topic PRs. Keep those checks on the dev -> test and test -> main promotion path after the runner is implemented.
- Do not invent Cargo commands or claim runtime/conformance checks passed when the corresponding workspace or runner does not exist. M0 task [OGM-35](https://linear.app/ogma-engine/issue/OGM-35/create-minimal-fast-ci-and-test262-promotion-gate-proof) owns completion of the conformance gate.

### Current repository bootstrap status

The repository remains in M0 bootstrap: there is no root Cargo.toml, Rust workspace, pinned Test262/differential runner or deterministic shard aggregator. GitHub Actions and branch protections are now configured:

- CI Fast runs on PRs to dev/test/main and pushes to those permanent branches. Its fast-checks job passed on PR #1; Rust fmt/Clippy/tests remain conditional on a future Cargo workspace.
- Conformance Promotion runs automatically on PRs to test/main; manual dispatch is diagnostic only. Both paths intentionally fail closed until OGM-35 implements the real conformance runner and deterministic shard aggregation. No promotion PR has exercised this gate yet, so it is configured but not reported as a passed check.
- Active GitHub rulesets protect dev, test and main. All three require PRs, block deletion and non-fast-forward updates, and have no bypass actors. dev requires fast-checks; test and main require fast-checks plus conformance-promotion.
- The active rulesets are [protect-dev](https://github.com/AiratGaliev/ogma/rules/27364036), [protect-test](https://github.com/AiratGaliev/ogma/rules/27364037) and [protect-main](https://github.com/AiratGaliev/ogma/rules/27364038).

These protections enforce repository workflow, but they do not imply that Ogma runtime code or the full conformance suite already exists. OGM-35 remains open until the actual runner and shard success/failure/missing-result proof are delivered.

## Source of truth and project links

- [Architecture and implementation specification (Notion)](https://app.notion.com/p/Ogma-JS-TypeScript-Execution-Engine-4167ef18f18181bd9b34f9dda764c92d#b5ca462a10c841279d724b74c44b239a) — authoritative architecture and milestone scope.
- [Ogma project in Linear](https://linear.app/ogma-engine/project/ogma-rust-native-jstypescript-execution-engine-1f94795b107b) — issues, M0 milestone, dependencies and delivery evidence.
- [Development Workflow](docs/development-workflow.md) — branches, PR rules, CI/testing, Linear statuses and definitions of done.
- [Compatibility Profile](docs/compatibility-profile.md) — versioned ECMAScript, TypeScript, Node.js API, module-resolution and consumer compatibility contract.
- [AGENTS.md](AGENTS.md) — repository-wide instructions for coding agents.
- [LICENSE](LICENSE) — project license.

Update this README when project direction, milestone boundaries or repository workflow changes. Describe implemented behaviour only when supported by source code and tests; keep planned capabilities clearly labelled as planned.
