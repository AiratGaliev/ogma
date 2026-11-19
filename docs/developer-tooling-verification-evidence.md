# OGM-10 — Developer Tooling & Verification evidence
**Status:** M0 evidence consolidation, 2026-11-19
**Scope:** backend-neutral debugging contracts, source metadata, test-runner/corpus architecture, Fast CI and promotion-gate proof
**Architecture authority:** the [Ogma architecture specification](https://app.notion.com/p/Ogma-JS-TypeScript-Execution-Engine-4167ef18f18181bd9b34f9dda764c92d#b5ca462a10c841279d724b74c44b239a), especially sections 0.6–0.6.5 and 19. This page is an evidence index, not a second architecture or API authority.
## 1. Purpose and M0 outcome
OGM-10 groups the tooling and verification work that must be in place before the production VM grows around debugger, metadata and test-runner assumptions. Its child deliveries are OGM-33, OGM-34, OGM-35 and OGM-37. The bounded interpreter/AOT equivalence fixtures referenced by this workstream are supplied by the related OGM-31 proof.
The workstream has delivered:
- a versioned, backend-neutral debugger API and source/debug metadata projection contract;
- a deterministic fixture-interpreter proof for breakpoints, pause/resume, stepping, stack/scopes, exceptions and safe object inspection;
- an in-process DAP boundary that depends on the debugger port rather than on ECMAScript Runtime internals;
- a pinned, adapter-driven Test262/parser runner, machine-readable classifications and reports, deterministic shards and fail-closed aggregation;
- Fast CI and a separate promotion workflow, with branch routing and enforcement documented;
- a bounded same-Core-IR portable/interpreter and native-AOT proof corpus.
These are M0 contracts, prototypes and verification infrastructure. They do **not** mean a production ECMAScript Runtime, parser, GC-backed Inspector, native debugger or complete DAP server exists.
## 2. Integrated tooling and contract map
| Area | M0 result | Source of truth / implementation |
|---|---|---|
| Debugger API | DebuggerPort, immutable DTO snapshots, capability reporting and backend-neutral commands. The public boundary does not expose mutable VM frames, registers, heap layout or Runtime objects. | [Debugger API contract](debugger-contract.md), [versioned schema](../contracts/debugger/v1/api.schema.json), [facade](../benchmarks/debugger/debugger_api.py) |
| Source identity and mapping | Source IDs/digests, 1-based lines, 0-based UTF-16 columns, half-open spans, generated/user provenance and code-generation identity are specified. Core IR source provenance is projected to the existing .ogmb v1 source-map shape using encoder-owned bytecode offsets and variable locations. | [Debugger contract](debugger-contract.md), [source-map schemas](../contracts/ogmb/v1/source-map.schema.json) and [manifest metadata schema](../contracts/ogmb/v1/manifest-metadata.schema.json), [projection helper](../benchmarks/debugger/metadata.py) |
| Pause, stepping and inspection | The fixture harness proves owner-thread safepoints, source breakpoints, pause/resume, step in/over/out, immutable stack/scope/variable snapshots and exception events. Object inspection is descriptor-only, bounded, avoids getters/Proxy traps/user stringification and releases temporary roots on success and failure. | [fixture proof](../benchmarks/debugger/prototype.py), [debugger regression tests](../.github/scripts/test_debugger_contract.py) |
| DAP boundary | A limited in-process command translator consumes DebuggerPort and advertises only capabilities supplied by the backend. Transport framing, process launch/attach, network/stdio server and IDE-client compatibility are explicitly outside this M0 proof. | [DAP adapter](../benchmarks/debugger/dap_adapter.py), [Debugger contract](debugger-contract.md) |
| Conformance runner | Test262 and test262-parser-tests source revisions, plus QuickJS-NG reference revision, are immutable pins. The runner supports metadata/flags/includes, strict/sloppy and module variants, negative phases, async completion, bounded worker execution, JSON/JUnit reports and deterministic shard planning/aggregation. | [runner and corpus contract](test-runner-and-corpus.md), [baseline](../conformance/baseline.json), [active profile](../conformance/profile.json), [runner](../tools/test_runner.py) |
| CI and repository workflow | Fast CI runs focused repository/verifier/runner checks. The separate promotion workflow checks direction, plans eight Test262 and four parser shards, uploads reports and requires a fail-closed aggregate. Topic → dev → test → main routing and branch rulesets are documented. | [development workflow](development-workflow.md), [Fast CI](../.github/workflows/ci.yml), [promotion workflow](../.github/workflows/conformance-promotion.yml), [agent instructions](../AGENTS.md) |
| Portable/native fixture comparison | The OGM-31 proof sends the same canonical Core IR fixture inputs through portable bytecode/interpreter and target-native lowering, then compares values, exceptions, side effects and host-call traces. | [OGM-31 issue](https://linear.app/ogma-engine/issue/OGM-31/build-an-end-to-end-dual-backend-execution-proof-from-one-core-ir), [proof sources](../benchmarks/dual-backend/README.md) |
## 3. Invariants retained across the tooling boundary
1. **One source of truth for provenance.** The frontend/Core IR owns source and binding identity. Lowering preserves or explicitly remaps it; generated operations remain synthetic rather than inheriting a misleading user location.
2. **No backend internals in debugger DTOs.** Debugger commands return immutable snapshots and opaque identities. A backend may expose fewer capabilities, but must not advertise unsupported operations.
3. **Runtime ownership remains explicit.** Runtime/Isolate state is owner-thread-affine. External control requests are applied at owner-thread safe points; transport threads do not inspect or mutate frames or heap state directly.
4. **Inspection is not script execution.** Safe inspection reads descriptors only. It must not invoke getters, Proxy traps, toString or user-defined formatting. Temporary inspection roots are released on every exit path.
5. **DAP remains an adapter.** Protocol transport and IDE dependencies do not become ECMAScript Runtime dependencies.
6. **Conformance classifications fail closed.** PASS, FAIL, UNSUPPORTED, EXCLUDED, HARNESS ERROR and UNEXPECTED PASS remain distinct. An unsupported feature or missing shard is never silently converted into a pass.
7. **Pinned inputs are reproducible.** Corpus and reference revisions are full immutable commits. Updating pins is a separate reviewed baseline change.
8. **Portable and native execution share semantic inputs.** The OGM-31 proof compares bounded fixture inputs from one Core IR source; it does not claim a complete JS frontend or production backend.
## 4. CI lane policy and current readiness
| Lane | Implemented behaviour | Current limitation |
|---|---|---|
| Feature PR → dev and ordinary dev CI | Repository validation; Python unit/self-tests; Core IR and .ogmb verifier corpora; helper compilation; pinned Node.js package-resolution and scheduling reference fixtures. Focused task-specific proof jobs run only for their matching topic PRs. | No root Cargo workspace exists, so root Rust fmt/Clippy/workspace-test steps are correctly skipped. Fast CI does not execute a live Ogma Test262 or QuickJS differential adapter. |
| Promotion PR dev → test | Validates exact direction, checks out the pinned Test262 and parser corpora, plans eight plus four deterministic shards, emits JSON/JUnit artifacts and requires aggregation. Failed, timed-out, malformed, missing or unsupported/unready results cannot qualify as a conformance pass. | conformance/profile.json still declares parser and execution as not_implemented. There is no executable Ogma parser/runtime adapter and no wired differential runner, so real conformance promotion remains fail-closed. |
| Release PR test → main | Repeats the required promotion checks for the release candidate. | It is not evidence of a passed release candidate until real adapters report supported results and the aggregate succeeds. |
| Nightly / manual | Reserved for long fuzzing, extended GC/lifecycle/resource-limit stress, sanitizer/Miri and broader platform/target coverage where supported. | These long-running lanes are not part of feature-PR Fast CI. |
### Test262 and differential smoke boundary
The architecture calls for a small stable Test262/differential subset in Fast CI when the relevant runner adapters exist. The repository must not fake this result:
- the active compatibility profile is explicitly not_implemented for both execution and parser;
- the fake adapter under tests/test-runner is used only by runner protocol self-tests; it does not execute JavaScript and is not a conformance adapter;
- Fast CI therefore proves the runner, shard and aggregate mechanics, but does not claim an Ogma Test262 pass;
- OGM-31 supplies a separate, bounded same-Core-IR portable/native differential proof, not a general Test262 or QuickJS-NG differential suite.
A real small Test262/parser smoke and a live QuickJS differential subset remain gated on executable Ogma adapters. Do not change the readiness profile or treat UNSUPPORTED as PASS to make CI green. This is a real M0-to-M1 execution dependency, not a hidden success.
## 5. Delivery and verification traceability
All four OGM-10 child PRs below are merged into dev. Their delivery evidence is recorded in the corresponding Linear child issues.
| Linear issue | Merged PR / commit | Fast CI evidence |
|---|---|---|
| [OGM-33 — backend-neutral debugger and source metadata](https://linear.app/ogma-engine/issue/OGM-33/specify-backend-neutral-debugger-api-and-source-metadata-contract) | [PR #13](https://github.com/AiratGaliev/ogma/pull/13) · merge commit f6b8c8e186fb6533b6cfeae81bcef56961780a6 | [run #44122390501](https://github.com/AiratGaliev/ogma/actions/runs/44122390501) · 177 Python tests passed on the reviewed PR head |
| [OGM-34 — runner and regression corpus](https://linear.app/ogma-engine/issue/OGM-34/define-test-runner-pinned-conformance-baseline-and-regression-corpus) | [PR #11](https://github.com/AiratGaliev/ogma/pull/11) · squash merge a206f8444a8fd947d161db3bcfc39914c6a3fc5f | [PR run #44099000855](https://github.com/AiratGaliev/ogma/actions/runs/44099000855) and [post-merge dev run #44099326616](https://github.com/AiratGaliev/ogma/actions/runs/44099326616) · 148 Python tests passed in the recorded post-merge evidence |
| [OGM-35 — Fast CI and Test262 promotion gate](https://linear.app/ogma-engine/issue/OGM-35/create-minimal-fast-ci-and-test262-promotion-gate-proof) | [PR #12](https://github.com/AiratGaliev/ogma/pull/12) · squash merge 0605691a6ce2421d5c59431b958e80ecc488f808 | [run #44105316212](https://github.com/AiratGaliev/ogma/actions/runs/44105316212) · 153 Python tests passed; promotion success/failure/missing-shard cases covered by deterministic self-tests |
| [OGM-37 — repository workflow and Linear agreement](https://linear.app/ogma-engine/issue/OGM-37/document-repository-development-workflow-and-linear-working-agreement) | [PR #1](https://github.com/AiratGaliev/ogma/pull/1) · merge commit 064b2b895552a37ade58aa3995f600dc7fd4949c | [PR run #43849557778](https://github.com/AiratGaliev/ogma/actions/runs/43849557778) and [post-merge dev run #43849709035](https://github.com/AiratGaliev/ogma/actions/runs/43849709035) |
The related bounded execution-equivalence proof is [OGM-31 / PR #27](https://github.com/AiratGaliev/ogma/pull/27), merged at 1c924f18a06090fd31813d82c8e6fd26b9c61dad. Its dedicated [Fast CI run #44496396418](https://github.com/AiratGaliev/ogma/actions/runs/44496396418) completed both fast-checks and the OGM-31 portable/native execution-proof job successfully.
### Fresh verification on the current dev head
The current dev branch was read at commit 52855777f296d7ac3bb44e3120e519378a6b7970. Its push-triggered [CI Fast run #44545129008](https://github.com/AiratGaliev/ogma/actions/runs/44545129008) completed successfully on that exact SHA:
- fast-checks: success;
- repository documentation validation: success;
- .ogmb fixture generation: success;
- Python repository/verifier/conformance-runner discovery: **203 tests passed**;
- Core IR v1 verifier and .ogmb v1 verifier: success;
- Python helper compilation: success;
- pinned Node.js package-resolution and scheduling reference fixtures: success;
- root Rust fmt, Clippy and workspace tests: skipped because no root Cargo.toml exists.
This current-dev run verifies the integrated M0 tooling state. It is not a Test262, differential-runtime, DAP transport or browser/IDE acceptance run.
## 6. Acceptance status
| OGM-10 deliverable | State | Evidence / qualification |
|---|---|---|
| Backend-neutral debugger API and source/debug metadata contract | Delivered | OGM-33, PR #13, versioned schema and focused regression suite |
| Interpreter pause, breakpoint, step, stack/scope and exception proof | Delivered as M0 fixture proof | OGM-33; deliberately not represented as a production ECMAScript interpreter |
| Safe inspection and temporary-root lifecycle proof | Delivered as M0 fixture proof | OGM-33; no production GC integration is claimed |
| DAP adapter boundary | Delivered as in-process adapter proof | OGM-33; no DAP server/transport or IDE-client acceptance |
| Test runner, pinned baselines, classifications, JSON/JUnit and deterministic sharding | Delivered | OGM-34, PR #11 |
| Fast CI and fail-closed promotion workflow | Delivered as CI infrastructure | OGM-35, PR #12; real promotion remains blocked until adapters exist |
| Repository workflow and Linear working agreement | Delivered | OGM-37, PR #1 |
| Bounded portable/interpreter and native-AOT equivalence fixtures | Delivered as separate M0 proof | OGM-31, PR #27 |
| Live Ogma Test262/parser smoke and QuickJS-NG differential execution | **Deferred / not implemented** | No executable Ogma parser/runtime adapter; active profile is not_implemented. Runner self-tests are not runtime tests. |
## 7. Reproduction commands
From the repository root, the focused debugger and runner suites are:
- python -m unittest discover -s .github/scripts -p "test_debugger_contract.py" -v
- python -m unittest discover -s .github/scripts -p "test_test_runner.py" -v
- python -m unittest discover -s .github/scripts -p "test_conformance_gate.py" -v
The Fast CI job runs the complete bounded Python discovery set and the following contract/reference checks:
- python .github/scripts/validate_repository.py
- python tests/ogmb/build_fixtures.py
- python .github/scripts/verify_core_ir.py --manifest tests/core-ir/manifest.json
- python .github/scripts/verify_ogmb.py --manifest tests/ogmb/.generated/manifest.json
- python -m compileall -q .github/scripts tools tests/ogmb/build_fixtures.py
- node tests/node-resolution/run.mjs
- node tests/node-scheduling/run.mjs
The commands above describe the repository's reproducible workflow. In this task, verification was read back from the cited GitHub Actions jobs; no local checkout or local command execution was available.
## 8. Explicit non-claims and remaining boundary
This M0 delivery does not claim:
- a root Rust workspace, production Ogma parser, ECMAScript Runtime or GC;
- live Test262, test262-parser-tests execution by an Ogma parser, or a working QuickJS-NG differential runner;
- a production interpreter debugger, native AOT debugger, mixed-stack/async continuation support or Hot Reload;
- production heap inspection, native stack-map generation, DAP wire transport or IDE-client compatibility;
- full promotion/browser/runtime acceptance.
The tooling contracts and fail-closed gates are in place so those capabilities can be added without changing the public seams. Until the actual adapters and runtime exist, conformance readiness must remain not_implemented and promotion must remain blocked.
