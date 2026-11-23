# Ogma — development workflow and Linear working agreement

This document is the canonical procedure for engineering work in the Ogma repository. It follows the authorized M1 implementation phase and actual Linear status workflow. README.md is the public overview; AGENTS.md contains concise repository-wide instructions for coding agents.

## 1. Authority and scope

Use the following sources in this order:

1. User request and explicit task scope.
2. Accepted Ogma architecture decisions and the [Notion architecture/implementation specification](https://app.notion.com/p/Ogma-JS-TypeScript-Execution-Engine-4167ef18f18181bd9b34f9dda764c92d#b5ca462a10c841279d724b74c44b239a).
3. The active Linear issue, its acceptance criteria, dependencies and authorized milestone.
4. README, AGENTS, this workflow document and accepted repository ADRs.
5. Source code, tests and CI as evidence of current implementation behaviour.

If the specification and implementation disagree, do not silently treat the implementation as the approved architecture. Record the discrepancy, impact and decision needed. External projects and benchmarks are references, not authority for Ogma decisions.

## 2. Linear work ownership and issue quality

The Ogma workspace currently uses one team: **Ogma (OGM)**. Engineering, M0 research/proofs, implementation, test infrastructure and repository tooling are tracked in this team. Do not create another Linear team or a duplicate project solely to imitate Codogma's two-team setup.

Create a task only when its work has an independently verifiable result. Each issue should include:

- a precise objective and bounded scope;
- explicit acceptance criteria, including negative/error cases where relevant;
- dependencies and blocking relationships;
- relevant Notion, repository and consumer references;
- the milestone and workstream label that match the task;
- evidence requirements for benchmarks, prototypes and architectural decisions.

Use parent issues for coherent workstreams and sub-issues for independently deliverable tasks. Use blocks/blockedBy only for real prerequisites. Execute the authorized M1 issues within their stated scope; do not create duplicate tasks or start later-milestone work without authorization.

### Linear status semantics

The OGM team currently exposes Backlog, Todo, In Progress, Done, Canceled and Duplicate. There is no In Review state. Do not invent or document a status that the team cannot select.

| Status | Meaning | Transition |
|---|---|---|
| **Backlog** | Candidate work not yet ready for execution: scope, priority, dependencies or acceptance criteria still need triage. | Move to Todo only when the task is actionable and its acceptance criteria are understood. |
| **Todo** | Ready and accepted into the working backlog; implementation has not started. | Move to In Progress when work actually starts. |
| **In Progress** | Research, implementation, test development, documentation or CI work is active. Keep the issue here while a PR is waiting for review/checks because OGM has no In Review state. | Move to Done only after the task's own criteria, scoped checks and required PR integration are complete. |
| **Done** | The task's own acceptance criteria are met, required scoped automated checks pass, evidence is recorded and the reviewed PR is merged into dev. For a documentation-only task, the equivalent documentation checks and PR merge must be complete. | Reopen only when the same task's acceptance criteria were not met or a regression in its scope is confirmed. |
| **Canceled** | Work is intentionally abandoned. Record why. | Do not use to hide blocked or unfinished work. |
| **Duplicate** | The issue duplicates another issue. Link the canonical issue and explain the duplicate. | Do not bulk-close issues merely to clean the board. |

PR state and Linear issue state are related but not interchangeable. An open PR does not mean an issue is Done; a merged PR does not automatically prove the issue's criteria were satisfied. Do not mark work Done because a patch was drafted, a branch was pushed or a PR was opened.

### Independent acceptance work

There is no separate Ogma QA team at present. When a meaningful user/consumer acceptance workflow exists, represent it as a separate top-level acceptance issue in OGM, not as a child of every implementation task. Link implementation and acceptance using the neutral relatedTo relation. Do not use blocks/blockedBy to make Engineering Done depend on a later manual acceptance step. Record the exact candidate SHA, environment, steps, expected/actual result and evidence in the acceptance issue.

For the current M0 architecture work, benchmark correctness and proof criteria belong in the Engineering issue itself; do not create artificial manual QA tickets for individual research spikes.

## 3. Definition of Ready and Definition of Done

### Engineering Ready

An issue is ready to move from Backlog to Todo when:

- its objective and excluded scope are clear;
- acceptance criteria are independently testable;
- prerequisites and relevant architecture references are known;
- a suitable workstream/milestone is selected;
- the verification/evidence strategy is proportionate to risk.

### Engineering Done

An engineering issue may move to Done when all applicable conditions hold:

1. Its acceptance criteria are met; no material requirement has been silently deferred.
2. Focused automated checks for the changed scope pass. Negative and boundary cases are covered where they are material.
3. Fast CI passes on the PR. A full promotion-only gate is not required to close an issue that has been properly delivered to dev.
4. The PR has been reviewed, the changes are integrated into dev, and the issue contains links to the PR and verification evidence.
5. Documentation, benchmark outputs, decision records and known limitations are updated as required.

Do not hold Engineering issues open waiting for the next test candidate or unrelated manual acceptance. If a later acceptance run finds a defect, create a new Engineering issue with a reproducible description, affected SHA and evidence. Reopen the original issue only if its own acceptance criteria were not actually met or the regression belongs to its scope.

## 4. Git branches and promotion route

The repository uses three permanent branches:

| Branch | Purpose | Allowed incoming PR |
|---|---|---|
| dev | Daily integration of completed engineering work. | Focused topic branches; justified hotfix integration only when necessary. |
| test | Integrated candidate for full conformance and acceptance. | dev only. |
| main | Stable/release branch. | Accepted test only; exceptional release-blocking hotfix as described below. |

The mandatory normal flow is:

**topic branch → dev → test → main**

Rules:

1. Start normal work from the latest dev. Check the working tree and update the branch with fast-forward-only semantics before branching.
2. Never commit directly to dev, test or main. Documentation-only changes use a topic branch and PR too.
3. Never send routine feature/fix branches directly to test or main; do not open dev → main or test → dev PRs.
4. Keep a topic branch focused on one coherent Linear issue. Split independently reviewable changes.
5. Do not promote partial work merely to make it available for acceptance. A dev → test PR must describe a complete, internally consistent candidate.
6. Do not force-push, rewrite shared history, bypass branch rules or change a PR target to evade a failing check.
7. Delete obsolete topic branches only after they are no longer needed and repository permissions allow it.

Suggested topic branch prefixes:

| Prefix | Use |
|---|---|
| feat/* | New runtime/compiler/tooling capability |
| fix/* | Defect correction |
| refactor/* | Internal restructuring with no intended semantic change |
| test/* | Tests, conformance fixtures and test infrastructure |
| docs/* | Documentation and architecture records |
| chore/* | Repository maintenance and build/tooling setup |
| hotfix/* | Emergency release-blocking fix only |

Typical setup:

    git status --short
    git switch dev
    git pull --ff-only origin dev
    git switch -c docs/short-name

### Allowed PR routes

| Source → target | Type | Rule |
|---|---|---|
| feat/*, fix/*, refactor/*, test/*, docs/*, chore/* → dev | Normal development | One coherent issue, scoped checks and review. |
| dev → test | Promotion | Complete candidate; full promotion CI and conformance gate required. |
| test → main | Release | Candidate accepted, required checks green and release notes/evidence accurate. |
| hotfix/* → main | Emergency exception | Only for a release-blocking defect that cannot wait for normal promotion. Explain urgency, test impact and backport plan. |

Any hotfix merged into main must be integrated back into dev through a PR unless the fix is already present. The hotfix route is not a shortcut for ordinary feature work.

## 5. Pull request rules

Every PR must:

- target the branch allowed by the route table above;
- reference the relevant Linear issue identifier and explain its acceptance criteria;
- contain one logically coherent change, with unrelated work excluded;
- describe the goal, implementation, excluded scope and compatibility/architecture impact;
- list exact checks run and their results;
- explicitly list checks not run, known limitations and remaining risks;
- update tests and docs when semantics, APIs, formats, dependencies, build behaviour or developer workflow changes;
- contain no secrets, generated noise, unexplained vendored changes or unlicensed upstream code.

Do not auto-merge. A PR must receive the appropriate code review and required CI checks before merge. Agents must not merge a PR without explicit authorization. An open PR does not make an issue Done.

Use the repository [Pull Request template](../.github/pull_request_template.md).

## 6. Testing and CI policy

Separate routine engineering feedback from full-system conformance and release acceptance.

### Fast CI — topic PR → dev

The current implementation lives in [CI Fast](../.github/workflows/ci.yml) and exposes the stable required status context **fast-checks**. It validates PR routing, Markdown UTF-8/line endings/whitespace/fenced blocks/local links, tests the repository validator and Core IR/OGMB verifiers, validates both verifier corpora and uploads a separate machine-readable report for each. Before tests it deterministically generates binary/JSON .ogmb fixtures under the ignored `tests/ogmb/.generated/` directory. Fast CI also compiles the Python helpers and fixture generator, runs the pinned Node.js package-resolution and scheduling reference corpora from `.node-version`, and executes the root Rust workspace formatting, Clippy and locked tests using `rust-toolchain.toml`. The OGM-40 topic PR additionally checks existing M0 standalone proof lockfiles in a separate task-scoped matrix; it does not add these projects to the root workspace.

The routine gate should remain fast and scoped. The current Rust Fast-CI lane runs workspace formatting, Clippy and locked tests, then builds the real OGM-41 JavaScript parser adapter and runs a bounded smoke against the exact pinned TC39 parser-tests revision. It does not run the full parser corpus or Test262 on feature PRs. As further subsystems become available, the lane should include checks applicable to changed files, such as:

- formatting and static checks for Rust and other repository languages;
- compilation/type checks and Clippy for affected Rust workspace members;
- focused unit, parser, IR verifier, bytecode, runtime, ABI/interop and regression tests;
- security/dependency/license checks for the affected dependency scope;
- documentation validation for Markdown-only changes;
- a small bounded Test262/differential smoke shard only when the runner and baseline are ready.

Do not run the complete Test262 corpus, full differential suite, full interpreter/AOT conformance matrix, broad GC stress/fuzz suite or full integration/E2E suites on topic PRs. Do not add path-based exceptions, a manual dispatch or a renamed job that effectively moves the full promotion suite into the feature-PR lane.

A documentation-only PR should run documentation checks and should not be forced to build the Rust engine when no code/build files changed.

### Promotion CI — dev → test

The [Conformance Promotion](../.github/workflows/conformance-promotion.yml) workflow preserves the required status context **conformance-promotion** and runs only for PRs targeting `test` or `main` (or an explicit manual dispatch). It validates exact source/target direction before planning any suite: `dev → test`, `test → main`, or the documented emergency `hotfix/* → main` route.

The workflow reads immutable repository URLs and full commit SHAs from `conformance/baseline.json`, checks out those exact upstream revisions, and creates deterministic manifests before execution. Test262 uses eight shards; the supplementary parser corpus uses four. Each shard produces JSON and JUnit artifacts. The final **conformance-promotion** job waits for the complete matrix, downloads all available results and invokes the runner's aggregate mode for both suites. Missing manifests, missing shard artifacts, duplicate/misassigned tests, worker failures, timeouts, invalid classifications and other incomplete aggregates fail the required check. A matrix job failure also keeps the aggregate check red.

Adapter command strings are supplied through repository Actions variables `OGMA_TEST262_ADAPTER` and `OGMA_PARSER_ADAPTER`. They are optional only while the corresponding readiness flag in `conformance/profile.json` remains `not_implemented`; the runner then reports `UNSUPPORTED`, and aggregate mode fails closed. A profile must not be marked `ready` until the actual parser/runtime adapter exists and is independently verified. If a profile is marked ready without its adapter command, the shard reports a harness error and promotion still fails.

The current repository has no executable Ogma parser or ECMAScript Runtime adapter. Therefore this workflow proves planning, shard execution/report transport and aggregate enforcement, but it is not a successful Ogma conformance run today. Fake-adapter self-tests in Fast CI prove the runner protocol and aggregate algorithm only; they do not satisfy the promotion gate. The pinned QuickJS-NG revision is present in the baseline, but the repository does not yet have an executable differential suite, so differential conformance must not be claimed or silently counted as passed.

The promotion PR is the first mandatory full integration gate. As the relevant engine paths become available, it must cover:

- the complete pinned Test262 corpus within the declared compatibility profile;
- the complete pinned parser grammar/early-error corpus;
- the full differential/regression suite against pinned reference revisions once its executable runner is delivered;
- interpreter/AOT equivalence and integrated runtime/Rust ABI/host/consumer checks when those implementations exist;
- deterministic shard completion and aggregate verification for every configured suite.

A failed promotion gate stops promotion. Fix the root cause in a topic branch, merge the fix into dev, then create a fresh promotion candidate. Never weaken or delete a test merely to make the gate green.

### Release CI — test → main

Repeat the required promotion checks for the release candidate and verify that the accepted test SHA is the one being released. Keep release packaging and publishing separate; a successful build/archive job does not mean a public release was created unless that action actually occurred.

### Long-running checks and benchmark evidence

Fuzzing, broad GC/lifecycle stress, adversarial resource-limit tests, sanitizers, Miri where applicable and additional platform matrices belong in nightly/manual lanes unless a focused test is necessary for a changed contract.

M0 benchmark spikes are executed as task-scoped experiments. Do not run every expensive benchmark on every PR. A reproducible report records:

- commit/revision, OS/CPU/toolchain and dependency versions;
- build profile, flags, allocator and runtime configuration;
- workload corpus and observable correctness checks;
- warmup, repetitions and measurement method;
- relevant throughput, latency distribution, allocations, memory, code size and compile/startup metrics;
- raw machine-readable data, reproduction commands and limitations.

Keep Ogma execution/dispatch costs separate from host physics, ECS, synchronization and consumer orchestration costs.

### Current enforcement state

GitHub Actions and branch rulesets are active. The stable required check contexts are unchanged:

- `.github/workflows/ci.yml` runs on PRs targeting `dev`, `test`, `main` and pushes to permanent branches. Its required job context is **fast-checks**. The Fast lane runs focused repository/verifier tests and the conformance-runner protocol/aggregate self-tests; it does not check out or execute the full upstream Test262 corpus.
- `.github/workflows/conformance-promotion.yml` runs for PRs targeting `test` or `main`. Its required aggregate job context remains **conformance-promotion**. It validates the route, plans the complete pinned Test262 and parser corpora, executes deterministic shard matrices, uploads per-shard reports and requires the aggregate job to validate exact coverage and successful outcomes.
- The currently committed `conformance/profile.json` still marks both parser and runtime execution as `not_implemented`, and no real Ogma adapter exists. Consequently aggregate promotion intentionally fails closed; a run containing only `UNSUPPORTED` results cannot pass. This is expected until actual adapters are implemented and the profile truthfully changes to `ready`.
- The adapter command variables are `OGMA_TEST262_ADAPTER` and `OGMA_PARSER_ADAPTER`. Do not set them to fake adapters for a real promotion candidate.
- The workflow declares manual diagnostic dispatch with an explicit target branch; GitHub exposes that dispatch only when this workflow file is present on the default branch (`main`). The checked-out source branch must match the same allowed route.
- The workflow files and their workflow logic are not evidence of an executed promotion. A promotion check is claimed as verified only after a real PR to `test` or `main` runs it.
- `.gitattributes` pins LF for Markdown, Rust, TOML, YAML, Python and shell files; the local validator tolerates Windows CRLF checkouts while GitHub's Linux runner enforces LF.
- Fast CI is a bootstrap/engineering gate, not evidence that a production Ogma runtime exists or that Rust/Test262/differential conformance has passed.
- Active rulesets: [protect-dev](https://github.com/AiratGaliev/ogma/rules/27364036), [protect-test](https://github.com/AiratGaliev/ogma/rules/27364037) and [protect-main](https://github.com/AiratGaliev/ogma/rules/27364038). Each requires PRs, blocks deletion/non-fast-forward updates and has no bypass actors. `dev` requires **fast-checks**; `test` and `main` require **fast-checks** plus **conformance-promotion**. These existing rulesets were read back from GitHub and are not modified by OGM-35.

Long fuzzing, extended GC/lifecycle/resource-limit stress, sanitizers, Miri where applicable, broad platform/target matrices and performance tracking remain documented nightly/manual work. No such expensive job is added to topic PRs or claimed as an active scheduled workflow.

## 7. Manual and consumer acceptance

Formal acceptance is performed on an integrated test candidate, never on every topic PR. When a user-visible or consumer workflow becomes available:

- create a separate top-level acceptance issue in OGM for a meaningful workflow or release candidate;
- record the exact test commit SHA, fixture/consumer revision, OS/target, steps, expected/actual outcome and evidence;
- do not make each small Engineering issue carry an artificial manual QA subtask;
- do not use later acceptance as a blocker for Engineering Done;
- convert a discovered defect into a separate Engineering issue with its reproduction/evidence.

M0 architecture proofs are accepted against their own automated/prototype/benchmark criteria. Browser/gameplay acceptance is not applicable to the current engine-bootstrap phase unless a specific consumer workflow is introduced.

## 8. Documentation and decision records

Keep one clear source of truth per concern:

| Artifact | Responsibility |
|---|---|
| README.md | Public project overview, milestone status, links and mandatory workflow summary. |
| AGENTS.md | Durable repository-wide agent instructions. |
| docs/development-workflow.md | Detailed Git, PR, testing, Linear and Definition-of-Done rules. |
| Notion architecture specification | Normative engine architecture and milestone requirements. |
| docs/compatibility-profile.md | Single repository source of truth for versioned ECMAScript, TypeScript, Node.js API, package and consumer compatibility claims. |
| docs/core-ir-contract.md and contracts/core-ir/v1/schema.json | Versioned backend-neutral function-level Core IR v1 semantics, structural schema, verifier invariants and machine-readable proof corpus. |
| Linear project and issues | Work planning, ownership, dependencies, status and execution evidence. |
| ADRs | Accepted, durable architecture decisions once a decision is mature enough to freeze. |
| Benchmark reports/proof artifacts | Reproducible evidence used to make M0 decisions. |

Update the appropriate docs in the same PR as changes to public behaviour, ABI/IR/bytecode contracts, runtime semantics, build profiles, test/CI rules or contributor workflow. Avoid duplicating large architectural specifications into README or Linear issue descriptions; link the canonical source and keep summaries synchronized.

Never report a planned feature as implemented. Distinguish source-level proof, unit/contract checks, full conformance, consumer integration and manual acceptance.

## 9. Definition of a task report

Before moving an issue to Done, leave enough evidence to reconstruct what happened:

1. Linear issue and resulting status.
2. Branch and PR, including target and merge state.
3. Files/components changed and any contract/architecture decision.
4. Exact commands and CI checks run, with factual results.
5. Checks not run, skipped or unavailable.
6. Benchmark/proof result links and raw artifacts when applicable.
7. Known limitations and follow-up issues.

Do not promise background work. Do not claim that an unexecuted check passed.
