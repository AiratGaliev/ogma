# Ogma test runner and regression corpus

This document defines the repository-level test infrastructure for the M0/M1 transition. It complements, but does not replace:

- the normative [Ogma architecture specification](https://app.notion.com/p/Ogma-JS-TypeScript-Execution-Engine-4167ef18f18181bd9b34f9dda764c92d#b5ca462a10c841279d724b74c44b239a);
- the versioned [compatibility profile](compatibility-profile.md);
- the backend-neutral [Core IR contract](core-ir-contract.md) and verifier corpus;
- the [portable bytecode contract](portable-bytecode-contract.md) and decoder/verifier corpus;
- the [development workflow](development-workflow.md).

The architecture specification is authoritative for test categories, engine semantics and CI lane policy. This document specifies the repository runner protocol and how those tests are organized.

## 1. Current implementation boundary

The repository contains an adapter-driven Python runner, a pinned source baseline, a machine-readable active profile, deterministic shard manifests, a result aggregator and protocol self-tests. The runner is development/test tooling only. It is not part of the Ogma Runtime or production dependency graph.

The current M0 repository still has no root Cargo workspace, executable Ogma parser, ECMAScript Runtime or engine adapter. Consequently:

- `conformance/profile.json` declares execution and parser states as `not_implemented`;
- a real Test262 or parser-corpus invocation under that profile classifies tests as `UNSUPPORTED`, never as `PASS`;
- self-tests use a deterministic fake adapter to prove the runner/adapter contract; they do not execute JavaScript and do not constitute engine conformance;
- CI promotion remains blocked until OGM-35 connects the actual adapter and promotion suites.

Do not change the active execution state to `ready` merely to make a workflow green. It may be changed only when an executable adapter exists, its focused tests pass and the feature profile truthfully describes the semantics it can run.

## 2. Pinned conformance baseline

`conformance/baseline.json` is the machine-readable source of truth for the upstream source revisions used by the runner.

| Corpus / oracle | Pinned source | Role |
|---|---|---|
| Test262 | `tc39/test262`, immutable commit in `conformance/baseline.json` | ECMA-262 conformance tests |
| Parser tests | `tc39/test262-parser-tests`, immutable commit in `conformance/baseline.json` | Supplementary grammar and early-error tests |
| QuickJS-NG | v0.18.0 and immutable source commit in `conformance/baseline.json` | Development/test-only differential reference oracle |

The ECMA-262 edition is ECMA-262 17th edition (2026). The Test262 commit is the snapshot selected by the accepted compatibility profile. The parser-tests repository is an older supplementary corpus and is not a complete grammar oracle for the 2026 edition. Ogma-owned parser fixtures and the pinned Test262 snapshot remain necessary.

QuickJS-NG is an oracle, not an Ogma runtime dependency. It must not be linked into a production Ogma artifact or used as a hidden C runtime behind a Rust wrapper.

### Baseline update policy

Changing any repository revision requires a separate reviewed change that:

1. records the old and new full commit SHAs and the reason for the update;
2. reviews the upstream diff and any changed harness/metadata conventions;
3. updates the matching compatibility-profile reference without introducing a second independent pin;
4. re-runs affected runner self-tests and compares old/new compatibility classifications and reports;
5. records any newly failing, unsupported, excluded or unexpected-pass cases.

Never use a moving branch, tag-only reference or unqualified latest checkout as the input to a conformance run. The runner checks the checked-out Git `HEAD` against the manifest's pinned commit before executing or planning a suite.

## 3. Repository layout

The current repository keeps independent contract corpora separate. The runner and fixtures are located as follows:

| Path | Responsibility |
|---|---|
| `conformance/baseline.json` | Immutable upstream Test262, parser-corpus and QuickJS-NG revisions |
| `conformance/profile.json` | Active runner profile, execution readiness, feature status and exact reviewed exclusions |
| `tools/test_runner.py` | Suite discovery, metadata preparation, adapter invocation, JSON/JUnit reporting, deterministic sharding and aggregation |
| `tests/test-runner/fixtures/` | Small fake corpora and harness files used only to test the runner protocol |
| `.github/scripts/test_test_runner.py` | Fast deterministic runner self-tests |
| `tests/core-ir/` | Canonical Core IR contract/verifier fixtures |
| `tests/ogmb/` | Portable bytecode container/verifier fixtures |

The broader engine corpus grows alongside actual subsystems. The following are logical areas, not a requirement to create empty crates or placeholder directories in M0:

| Corpus area | Test subject and boundary |
|---|---|
| Unit / internal | Value representations, strings, atoms, objects/prototypes, properties, arrays, scopes, closures, exceptions, built-ins and VM operations |
| Parser | Lexer/grammar, syntactically valid programs, early errors and syntax rejection; Test262 parser categories are `pass/`, `early/` and `fail/` |
| Runtime regressions | Small JavaScript programs for fixed, previously reproduced semantic defects |
| Test262 | Normative ECMAScript observable behaviour within the declared feature profile |
| Differential | Identical programs and structured observable results from Ogma and pinned reference engines |
| Property / generative | Invariants such as Value conversion, serialization roundtrips, verifier constraints and handle lifecycle |
| Bytecode / verifier | Valid and invalid `.ogmb` containers, decoder bounds, feature flags and compatibility fixtures |
| Interpreter / AOT equivalence | Same Core IR inputs compared across required execution backends |
| GC / lifecycle | Roots, cycles, handles, callbacks, runtime teardown and bounded collection stress |
| Rust interop | Public embedding API, ownership, borrowing, conversions, reentrancy, callbacks and async completion |
| Security / resource limits | Host capabilities, malformed inputs, quotas, cancellation, timeouts and adversarial programs |
| Consumer integration | Exact pinned dependency graphs and workloads from supported consumer repositories |
| Benchmarks | Reproducible compiler/runtime microbenchmarks and representative macro workloads; performance data is not a conformance pass/fail |

Do not create all of these areas in advance. Add focused fixtures with the subsystem that introduces the corresponding contract. Promote reusable test harnesses into separate tools/crates only when actual reuse justifies the boundary.

### Test ownership

- A unit test checks one component's invariant.
- An integration test checks collaboration across real components or a real boundary.
- A contract test checks a versioned API, file format, adapter or host agreement.
- A reference/conformance test checks observable behaviour against a normative or pinned external source.
- A differential test compares independently executed implementations for the semantics they share.
- A consumer test verifies a pinned real dependency graph and application workload.

Do not count a Core IR verifier fixture, bytecode-container fixture, parser-only fixture or fake adapter fixture as an executed JavaScript runtime test.

## 4. Test262 runner contract

The runner is invoked as `python tools/test_runner.py`. It supports two suites:

- `test262`: discovers `*.js` under the pinned checkout's `test/` directory and loads harness files from `harness/`;
- `test262-parser-tests`: discovers `pass/`, `early/` and `fail/` parser-only cases.

The source checkout must be at the exact SHA in `conformance/baseline.json`. Planning and execution fail closed if the checkout revision differs. The runner never downloads or updates an upstream corpus implicitly.

### 4.1 Test262 metadata

The runner reads the YAML frontmatter at the beginning of every Test262 test. It handles the metadata needed for the runner contract:

- `features`;
- `flags`, including `onlyStrict`, `noStrict`, `module`, `raw` and `async`;
- `includes`;
- `negative.phase` and `negative.type`;
- additional scalar/list/mapping metadata without treating it as executable configuration.

The parser intentionally supports the constrained YAML structures used by Test262 frontmatter and rejects duplicate keys, contradictory mode flags, unsafe paths, malformed negative metadata and unsupported YAML features. A malformed case is a `HARNESS ERROR`; it is not silently ignored.

### 4.2 Execution modes and harness

- `onlyStrict` runs exactly one strict script variant.
- `noStrict` runs exactly one sloppy script variant.
- A module test runs exactly once as a module. Module code has strict semantics; it is not duplicated into script variants.
- A regular non-module test without either flag runs in both sloppy and strict modes.
- `raw` tests receive no harness scripts and must not declare `includes`.
- Non-raw tests receive `sta.js`, `assert.js` and the metadata `includes` in deterministic order, with duplicate include names removed.
- Harness include paths must remain inside the pinned harness directory. Path traversal is a harness error.
- The adapter receives the original source plus `sourceType`, mode, flags, feature requirements and loaded include source. The adapter owns correct compilation/execution preparation; the generic runner does not rewrite source text and must not change JavaScript semantics.

A negative test passes only when the adapter reports a thrown completion with the exact expected phase and error name. Supported phases are `parse`, `resolution` and `runtime`. Completing normally, throwing in the wrong phase or producing a different error type is `FAIL`.

An async test passes only when the adapter reports successful completion through the Test262 `$DONE` protocol. A missing completion is `FAIL`. The runner applies a bounded per-test timeout, using the async timeout for async tests. Timeouts are reported as `FAIL` with `failureKind: TIMEOUT`; an adapter crash or malformed response is `HARNESS ERROR`.

### 4.3 Parser-only corpus

The parser suite does not execute test bodies:

- `pass/` expects parsing to succeed;
- `fail/` expects syntax rejection in the parse phase;
- `early/` expects rejection during early-error validation.

The adapter must distinguish `parsed` and `rejected`, and report `parse` versus `early` phase. A parser corpus result is not a runtime result.

## 5. Feature profile and classifications

`conformance/profile.json` defines the active profile consumed by the runner. It is tied to `docs/compatibility-profile.md` and includes:

- profile ID and version;
- execution readiness;
- parser readiness;
- default feature status and explicit feature-status overrides;
- exact reviewed exclusions.

Feature statuses use the compatibility profile vocabulary: `supported`, `partial`, `unsupported` and `deferred`. The default status is `deferred`, so an unlisted Test262 feature is not optimistically assumed supported.

A test requiring a feature with status `unsupported` or `deferred` is reported as `UNSUPPORTED`. If the execution/parser adapter is not implemented, all cases in that suite are `UNSUPPORTED`, including tests with no explicit `features` metadata. They are not passed through a fake empty implementation.

Exclusions are exact test IDs, scoped by suite, and require all of:

- a concrete reason;
- the affected feature;
- an owner;
- a condition for revisiting the exclusion.

Blanket exclusions by directory, regular expression or feature family are forbidden. An exclusion is reported as `EXCLUDED`, not `PASS`.

Diagnostic probe flags `--probe-unsupported` and `--probe-excluded` allow an otherwise gated test to be executed deliberately. If it passes, the result is `UNEXPECTED PASS`, which requires a compatibility-profile review. A probe must never silently mutate the profile or baseline.

Every result has exactly one primary classification:

| Classification | Meaning | Gate treatment |
|---|---|---|
| `PASS` | Expected observable behaviour was confirmed | Success |
| `FAIL` | Behaviour mismatch, timeout or missing async completion | Blocks |
| `UNSUPPORTED` | Required capability is not in the active execution profile | Visible, never counted as pass |
| `EXCLUDED` | Exact reviewed exclusion with reason and owner | Visible, never counted as pass |
| `HARNESS ERROR` | Metadata, harness, adapter, protocol or setup failure | Blocks |
| `UNEXPECTED PASS` | A probed unsupported/excluded case now passes | Blocks until reviewed |

`UNSUPPORTED` and `EXCLUDED` are not conformance passes. They may be present in an accurately declared partial execution profile, but reports and dashboards must display them separately. `FAIL`, `HARNESS ERROR`, timeout and `UNEXPECTED PASS` block promotion.

## 6. Engine adapter protocol

The engine adapter is an executable development/test tool, not a runtime dependency. The runner invokes it without a shell, sends one JSON request on stdin and expects exactly one JSON object on stdout. Adapter diagnostics belong on stderr. A non-zero exit, timeout, invalid JSON or wrong protocol version is a harness error, except a test execution timeout, which is reported as `FAIL`.

Request envelope:

    {
      "protocol": "ogma-test-runner-adapter",
      "version": 1,
      "test": {
        "id": "language/example.js",
        "suite": "test262",
        "source": "...",
        "mode": "strict",
        "sourceType": "script",
        "raw": false,
        "async": false,
        "flags": [],
        "features": [],
        "includes": [{"name": "sta.js", "source": "..."}],
        "negative": null
      },
      "timeoutMs": 10000
    }

The Test262 adapter responds with `outcome` equal to `completed`, `threw`, `unsupported` or `harness_error`. A thrown completion includes `phase` and `errorName`. Async completion includes `asyncDone: true`.

    {
      "protocol": "ogma-test-runner-adapter",
      "version": 1,
      "outcome": "completed",
      "asyncDone": true,
      "durationMs": 1.25
    }

Parser adapters respond with `outcome: parsed` or `outcome: rejected`; rejected cases include `phase: parse` or `phase: early`. Adapters may also return `unsupported` or `harness_error`.

The adapter is responsible for implementing the actual engine-facing details: parse-only mode, module linking/evaluation, correct strict/sloppy semantics, Test262 harness preparation, `$DONE`, `$262` host hooks required by the active profile, negative completion observation and resource cancellation. The generic runner validates and reports this contract; it does not pretend to provide these engine capabilities. The parser adapter has its own readiness flag and does not have to wait for the ECMAScript execution adapter.

## 7. Deterministic manifests, reports and aggregation

### 7.1 Planning and sharding

The `plan` command discovers the checked-out suite and writes a deterministic shard manifest:

    python tools/test_runner.py plan \
      --suite test262 \
      --suite-root .artifacts/upstream/test262 \
      --shard-count 8 \
      --output .artifacts/test262-manifest.json

Each test ID is assigned using the first 64 bits of `SHA-256(test_id)` modulo the shard count. IDs are sorted before the manifest digest is calculated. The manifest records:

- suite and exact source repository/revision;
- compatibility profile ID, version and content SHA-256, plus separate parser/runtime readiness states;
- sorted test IDs and their shard indices;
- every shard index and its exact test-ID list;
- a digest over the manifest contents.

For an unchanged source revision, profile and shard count, repeated planning must produce the same manifest digest and assignments. Changing the shard count intentionally changes assignment.

The runner re-discovers the test list before execution and rejects a mismatch with the planned manifest. This prevents executing a stale or incomplete plan.

### 7.2 Running a shard

    python tools/test_runner.py run \
      --manifest .artifacts/test262-manifest.json \
      --suite-root .artifacts/upstream/test262 \
      --shard-index 0 \
      --adapter "target/debug/ogma-test-adapter" \
      --timeout-ms 10000 \
      --async-timeout-ms 10000 \
      --report .artifacts/results/test262-shard-0.json \
      --junit .artifacts/results/test262-shard-0.xml

The command emits one result per test ID assigned to that shard. The result includes per-variant mode outcomes, feature/flag metadata, profile and source references, manifest digest and a summary. JSON is the canonical machine-readable report. JUnit XML maps `FAIL` and `UNEXPECTED PASS` to failures, `HARNESS ERROR` to errors, and `UNSUPPORTED` / `EXCLUDED` to skipped cases with explicit messages.

Exit codes:

- `0`: no blocking result; unsupported/excluded cases remain explicit in the report;
- `1`: at least one `FAIL`, `HARNESS ERROR` or `UNEXPECTED PASS`;
- `2`: invalid input, source revision, manifest, profile or runner invocation.

### 7.3 Aggregation

    python tools/test_runner.py aggregate \
      --manifest .artifacts/test262-manifest.json \
      --reports .artifacts/results/test262-shard-0.json \
                .artifacts/results/test262-shard-1.json \
      --report .artifacts/test262-aggregate.json \
      --junit .artifacts/test262-aggregate.xml

The aggregate command fails closed unless:

- every expected shard index has exactly one result file;
- all shard reports reference the same manifest digest, suite and shard count;
- each shard reports exactly its assigned test IDs;
- there are no missing, duplicated, unexpected or cross-shard test IDs;
- all result classifications are valid.

A successful individual shard never implies an aggregate success. Missing shard artifacts, incomplete coverage or duplicate test execution are aggregate errors. The aggregate JSON records `missingShards`, `duplicateShards`, `missingTests`, `duplicateTests` and detailed errors. The aggregate command exits `1` for an incomplete aggregate even when every received test passed. It also fails closed when the Test262 execution adapter is not declared ready in the pinned profile; a complete set of `UNSUPPORTED` reports cannot make an unimplemented runtime eligible for promotion. Parser-only aggregation checks `parserStatus` independently and can pass before runtime execution exists.

## 8. Differential and engine-level comparison contracts

Test262 is normative for ECMAScript semantics; QuickJS-NG is a compatibility oracle and is not the specification. A QuickJS mismatch must be resolved against ECMA-262 and Test262 rather than automatically treating QuickJS as correct.

Differential runners execute equivalent scripts and compare structured observable results. The comparison protocol must preserve distinctions that ordinary JSON serialization loses, including:

- `undefined`, `null`, `NaN`, positive/negative infinity and negative zero;
- BigInt values and error/completion categories;
- sparse array holes versus explicit `undefined`;
- thrown completions, error names and phase;
- ordered side-effect/host-call traces and Promise/job observations where the contract makes them observable.

Do not compare implementation-specific error strings, memory addresses, internal object layouts or nondeterministic timing. A mismatch must retain the test source, seed, Ogma build/profile, reference revision and relevant adapter configuration. Minimize it and promote it to the normal regression corpus.

Other engine-level suites remain independent of Test262:

- Core IR and `.ogmb` verifier positive/negative corpora;
- parser grammar/early-error tests;
- Value/object/handle property tests and serialization roundtrips;
- interpreter/AOT equivalence for the same accepted Core IR fixtures;
- GC roots, cycles, lifecycle and callback/reentrancy tests;
- embedding API, ABI conversion and async/shutdown tests;
- security, hostile input, cancellation and resource-budget tests;
- exact pinned consumer package/workload tests.

Benchmarks report reproducible measurements, not conformance classifications. Keep compiler, runtime, host interop, consumer orchestration and GC costs separated. Fuzzing and long stress runs save seed/crash/timeout artifacts; every minimized correctness failure becomes a deterministic regression case.

## 9. CI lane policy and OGM-35 boundary

Runner self-tests are bounded and belong in Fast CI. The complete Test262 suite, full differential corpus and extended regression suites do not belong on feature PRs.

| Lane | Required test scope |
|---|---|
| Feature PR → `dev` / ordinary `dev` CI | Focused unit/integration/contract tests for changed code; parser/verifier regressions where applicable; runner self-tests; bounded stable smoke only after the adapter exists |
| Promotion PR `dev → test` | Full pinned Test262 within the declared profile, complete deterministic corpus, extended differential tests, all applicable embedding/backend equivalence and aggregate verification |
| Nightly / manual | Long fuzzing, extended GC/lifecycle/resource-limit stress, broad platform/target matrices and performance tracking |
| Release qualification | Reproducible reports for the exact release candidate, ABI/bytecode compatibility and applicable consumer integration evidence |

OGM-34 delivers the runner contract, source pins, profile filtering, deterministic shard manifest/aggregation implementation and self-test coverage. OGM-35 owns the workflow integration: Fast lane wiring, promotion branch checks, shard matrix/artifact handling, aggregate gate, and simulated successful, failed and missing-shard CI cases. Do not pull that separate workflow task into this change.

Until OGM-35 wires the actual runtime adapter and promotion pipeline, `.github/workflows/conformance-promotion.yml` must remain fail-closed. A runner self-test or a fake adapter is not grounds to remove that gate.
