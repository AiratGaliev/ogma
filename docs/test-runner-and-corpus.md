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

The Rust workspace and OGM-41 JavaScript parser frontend now exist. `ogma-frontend` exposes a real parse-only adapter for the supplementary parser corpus. The ECMAScript Runtime and runtime Test262 adapter remain unimplemented. The Python runner and its reports are development/test tooling; they are not part of the Ogma Runtime or production dependency graph.

Consequently:

- `conformance/profile.json` declares runtime execution as `not_implemented` and parser-adapter readiness as `ready`;
- parser-corpus cases can be sent to the real parser adapter and classified as parser `PASS` / `FAIL`;
- runtime Test262 requests remain `UNSUPPORTED` until an execution adapter exists; parser acceptance must never be counted as runtime execution;
- runner self-tests still use a deterministic fake adapter to test generic protocol and reporting logic; they do not execute JavaScript and do not constitute engine conformance;
- the parser-only promotion suite can run independently, while runtime Test262 promotion remains fail-closed.

Do not change runtime execution state to `ready` merely to make a workflow green. It may be changed only when an executable Runtime adapter exists, its focused tests pass and the feature profile truthfully describes the semantics it can execute. Parser readiness is separate: `ready` means a real parser adapter exists and its focused checks pass, not that the full parser corpus or ECMAScript runtime is conformant.

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
| `crates/ogma-frontend/src/bin/parser_adapter.rs` | Real OGM-41 JSON-lines parser adapter for the pinned supplementary parser corpus |
| `tools/parser_corpus_smoke.py` | Six-case bounded Fast-CI smoke through the real parser adapter |
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
- `test262-parser-tests`: discovers `pass/`, `pass-explicit/`, `early/` and `fail/` parser-only cases.

The source checkout must be at the exact SHA in `conformance/baseline.json` and have a clean Git working tree. Planning and execution fail closed if the commit differs or tracked/untracked changes could alter test or harness input. The runner never downloads or updates an upstream corpus implicitly.

### 4.1 Test262 metadata

The runner reads the optional YAML frontmatter in the Test262 test prologue. License/copyright or generated-source comments may precede the frontmatter; tests with no frontmatter use the default empty metadata map. The full parsed metadata object is forwarded to the adapter, including fields such as `locale` that influence host environment setup. Tests under both `test/intl402/` and `test/staging/intl402/` are treated as requiring the `Intl` feature even when the feature is not repeated in their metadata. It handles the metadata needed for the runner contract:

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
- `raw` tests receive no harness scripts, execute exactly once in non-strict script mode and must not declare `includes`. They cannot be combined with `onlyStrict`, `module` or `async`.
- Non-raw tests receive the pinned `assert.js`, then `sta.js`, then `doneprintHandle.js` for async tests, followed by metadata `includes` in source order. Duplicate include names are removed without changing first occurrence order.
- Test, parser and harness source files must resolve to regular files inside their respective pinned corpus directories. Symlink escapes, path traversal and external files are harness errors.
- The adapter receives the original source plus the complete YAML `metadata`, `sourceType`, mode, raw feature tags, inferred `featureRequirements`, flags and loaded include source. The adapter owns correct compilation/execution preparation; the generic runner does not rewrite source text and must not change JavaScript semantics.

A negative test passes only when the adapter reports a thrown completion with the exact expected phase and error name. Supported phases are `parse`, `resolution` and `runtime`. Completing normally, throwing in the wrong phase or producing a different error type is `FAIL`.

For an async test, the runner supplies `harness/doneprintHandle.js`. The adapter must implement the Test262 `print` host function and report the exact `Test262:AsyncTestComplete` signal as `asyncSignal`. A signal prefixed with `Test262:AsyncTestFailure:` is `FAIL`; missing completion before the async timeout is also `FAIL`. This is the pinned Test262 print-based protocol, not the separate `$DONE` convention used by other test harnesses. The runner applies a bounded per-test timeout; timeouts are reported as `FAIL` with `failureKind: TIMEOUT`, while adapter crashes or malformed responses are `HARNESS ERROR`.

### 4.3 Parser-only corpus

The parser suite does not execute test bodies:

- `pass/` and `pass-explicit/` expect parsing to succeed. Every `pass/<relative-path>` case has a corresponding `pass-explicit/<relative-path>` case with explicit grouping parentheses. The pair is assigned to the same deterministic shard; the adapter must return a SHA-256 fingerprint of its normalized AST for each, with insignificant grouping parentheses, whitespace, comments and semicolons normalized away. The runner fails the pair if the fingerprints differ, and the aggregator independently rechecks the fingerprints from shard reports.
- `fail/` expects grammar rejection;
- `early/` expects acceptance by the grammar parser when early-error checks are disabled, then rejection when early-error validation is enabled.

The adapter receives the expected parse outcome/phase and is responsible for running the `early/` cases with the correct early-error mode. A parser that does not expose a separate early-error mode may reject these cases in the early phase, but must not conflate them with grammar-invalid `fail/` cases.

The adapter must distinguish `parsed` and `rejected`, and report `parse` versus `early` phase. A parser corpus result is not a runtime result.

## 5. Feature profile and classifications

`conformance/profile.json` defines the active profile consumed by the runner. It is tied to `docs/compatibility-profile.md` and includes:

- profile ID and version;
- execution readiness;
- parser readiness;
- default feature status and explicit feature-status overrides;
- exact reviewed exclusions.

Feature statuses use the compatibility profile vocabulary: `supported`, `partial`, `unsupported` and `deferred`. The default status is `deferred`, so an unlisted Test262 feature is not optimistically assumed supported. A `partial` feature runs only the exact test IDs listed under `supportedTests.<feature>`; glob selectors are rejected. In addition, the optional `test262Allowlist` is a suite-wide exact allowlist for the current executable slice: when present, unlisted IDs are `UNSUPPORTED` even if they have no feature tags. An empty or absent allowlist means no tests for that partial feature are claimed as supported.

A test requiring a feature with status `unsupported` or `deferred` is reported as `UNSUPPORTED`. The runner also infers the `Intl` requirement for every Test262 test under `intl402/`, because that suite does not consistently declare `features: [Intl]`; the active profile explicitly keeps `Intl` deferred. If the execution/parser adapter is not implemented, all cases in that suite are `UNSUPPORTED`, including tests with no explicit `features` metadata. They are not passed through a fake empty implementation.

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

The engine adapter is an executable development/test tool, not a runtime dependency. The runner invokes one long-lived adapter worker per shard, without a shell. It sends newline-delimited JSON requests on stdin and expects exactly one newline-delimited JSON response per request on stdout; stdout is reserved for protocol messages and diagnostics belong on stderr. Every request carries a monotonically increasing `requestId`, which the response must echo, including after a worker restart. The worker stays alive across tests and both strict/sloppy variants; it must create a fresh ECMAScript Realm for each Test262 test/variant and must not leak globals, jobs or harness state from one request to another. The per-request timeout bounds both stdin delivery and response wait; requests larger than 16 MiB and responses larger than 8 MiB are rejected. On a request timeout the runner terminates that worker and starts a fresh process for the next request. After the last request, the worker must exit successfully when stdin is closed. A non-zero shutdown is recorded as `workerError` in the shard report and blocks aggregation. A worker startup/crash, malformed JSON, missing/mismatched `requestId`, oversized or unsolicited output, or wrong protocol version is `HARNESS ERROR`; a test execution or request-delivery timeout is `FAIL`.

Request envelope:

    {
      "protocol": "ogma-test-runner-adapter",
      "version": 1,
      "requestId": 1,
      "test": {
        "id": "language/example.js",
        "suite": "test262",
        "source": "...",
        "metadata": {"locale": ["en", "fr"]},
        "mode": "strict",
        "sourceType": "script",
        "raw": false,
        "async": false,
        "flags": [],
        "features": [],
        "featureRequirements": [],
        "equivalenceGroup": null,
        "equivalenceRole": null,
        "includes": [
          {"name": "assert.js", "source": "..."},
          {"name": "sta.js", "source": "..."}
        ],
        "negative": null,
        "corpusRoot": "/path/to/pinned/test262",
        "testPath": "test/language/example.js",
        "testDirectory": "/path/to/pinned/test262/test/language"
      },
      "timeoutMs": 10000
    }

The Test262 adapter responds with `outcome` equal to `completed`, `threw`, `unsupported` or `harness_error`. A thrown completion includes `phase` and `errorName`. Async completion uses the `asyncSignal` field described below.

    {
      "protocol": "ogma-test-runner-adapter",
      "version": 1,
      "requestId": 1,
      "outcome": "completed",
      "durationMs": 1.25
    }

For async Test262 tests the runner adds `doneprintHandle.js` to the harness. The adapter's host `print` function must capture the official `Test262:AsyncTestComplete` or `Test262:AsyncTestFailure:…` signal and return it as `asyncSignal`. The worker must not return before that signal or the configured async timeout. This protocol deliberately follows the pinned Test262 harness, not `$DONE`.

Parser adapters respond with `outcome: parsed` or `outcome: rejected`; rejected cases include `phase: parse` or `phase: early`. Adapters may also return `unsupported` or `harness_error`. For `pass/` and `pass-explicit/` positive cases, responses must also contain `normalizedAstSha256`, a lowercase SHA-256 of the canonical AST after removing irrelevant grouping-parentheses, whitespace, comments and semicolon differences.

The OGM-41 parse-only implementation is `ogma-parser-adapter` in `crates/ogma-frontend`. It is a long-lived JSON-lines worker: one process handles successive requests, echoes each `requestId`, emits and flushes one response line per request, and exits successfully on clean stdin EOF. It limits each request line to 16 MiB. The adapter parses JavaScript Script/Module input, checks early errors, returns structured source diagnostics and does not execute JavaScript. Fast CI builds it and runs the bounded six-case smoke against the exact parser-corpus revision in `conformance/baseline.json`.

OGM-50 adds `ogma-test262-adapter` in `crates/ogma-cli` as a separate production-path source-execution worker. It shares the same JSONL protocol and request limits, creates a fresh Engine/Runtime/Context for every request, parses with the real frontend, compiles with `ogma-compiler`, loads serialized verified OGMB through the embedding API and invokes the Program entry. Its initial Test262 profile is deliberately fail-closed: only the three exact IDs in `conformance/profile.json:test262Allowlist` can reach the Runtime. It accepts raw, synchronous Script tests without includes; module, async, harness-dependent, and unlisted tests are `UNSUPPORTED`. A frontend syntax/early error is returned as the Test262 parse-phase `SyntaxError`. Full Test262 and full parser-corpus runs remain promotion-lane checks; the bounded Fast-CI cases are not full conformance claims.

The full Test262 adapter contract still requires module linking/evaluation, harness preparation, the `print`-based async protocol, `$262` host hooks, full negative-completion observation and cancellation. The OGM-50 adapter does not claim those capabilities yet: the bounded raw Script path intentionally rejects modules, async tests and harness includes as `UNSUPPORTED`, and tests outside the profile's exact allowlist never reach the worker. The parser adapter has its own readiness flag and does not have to wait for the ECMAScript execution adapter.

## 7. Deterministic manifests, reports and aggregation

### 7.1 Planning and sharding

The `plan` command discovers the checked-out suite and writes a deterministic shard manifest:

    python tools/test_runner.py plan \
      --suite test262 \
      --suite-root .artifacts/upstream/test262 \
      --shard-count 8 \
      --output .artifacts/test262-manifest.json

Each ordinary test ID is assigned using the first 64 bits of `SHA-256(test_id)` modulo the shard count. For `test262-parser-tests`, matching `pass/<relative-path>` and `pass-explicit/<relative-path>` entries use the shared key `parser-pass-equivalence:<relative-path>`, keeping the AST-equivalence pair in one shard. IDs are sorted before the manifest digest is calculated. The manifest records:

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
- `1`: at least one `FAIL`, `HARNESS ERROR`, `UNEXPECTED PASS` or adapter worker shutdown error;
- `2`: invalid input, source revision, manifest, profile or runner invocation.

### 7.3 Aggregation

    python tools/test_runner.py aggregate \
      --manifest .artifacts/test262-manifest.json \
      --suite-root .artifacts/upstream/test262 \
      --baseline conformance/baseline.json \
      --profile conformance/profile.json \
      --reports .artifacts/results/test262-shard-0.json \
                .artifacts/results/test262-shard-1.json \
      --report .artifacts/test262-aggregate.json \
      --junit .artifacts/test262-aggregate.xml

The aggregate command fails closed unless:

- the supplied source checkout is at the exact pinned revision and has a clean Git working tree;
- the manifest contains exactly the complete test-ID set discovered from that checkout (not merely a complete subset of its own declarations);
- every expected shard index has exactly one result file;
- the manifest's repository URL/revision matches the active pinned baseline and its profile ID/version/content digest/readiness matches the active compatibility profile;
- all shard reports reference the same manifest digest, suite and shard count;
- each shard reports exactly its assigned test IDs and has no adapter `workerError`;
- there are no missing, duplicated, unexpected or cross-shard test IDs;
- all result classifications are valid;
- every parser `pass/` and `pass-explicit/` pair has both results and matching normalized AST fingerprints.

A successful individual shard never implies an aggregate success. Missing shard artifacts, incomplete coverage or duplicate test execution are aggregate errors. The aggregate JSON records `missingShards`, `duplicateShards`, `missingTests`, `duplicateTests` and detailed errors. The aggregate command exits `1` for an incomplete aggregate even when every received test passed. Any `FAIL`, `HARNESS ERROR` or `UNEXPECTED PASS` result also makes `aggregation.complete` false; the aggregate JSON cannot represent a blocking suite as complete. It also fails closed when the Test262 execution adapter is not declared ready in the pinned profile, or when no test in the complete aggregate has a `PASS` result; a complete set of `UNSUPPORTED`/`EXCLUDED` reports cannot make an unimplemented or unproven runtime eligible for promotion. Parser-only aggregation checks `parserStatus` independently and can pass before runtime execution exists, provided at least one parser fixture passed.

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

## 9. CI lane policy and OGM-35 integration

Runner protocol self-tests and aggregate proof cases are part of Fast CI's bounded Python unit suite. Feature PRs check out the immutable pinned source snapshots only to execute the explicit six-case parser smoke, three-test Runtime allowlist and bounded differential fixtures; they do not plan or execute the complete upstream Test262 or parser corpora.

The promotion workflow is the only lane that plans and executes the pinned full conformance corpora:

| Lane | Behaviour |
|---|---|
| Feature PR → `dev` / ordinary `dev` CI | Focused changed-scope tests, Core IR/OGMB verifier checks, runner/aggregate self-tests, the bounded parser/runtime allowlists and a small pinned QuickJS-NG differential fixture set. No full Test262 or parser-corpus execution. |
| Promotion PR `dev → test` | Eight deterministic Test262 shards plus four parser-corpus shards; each writes JSON/JUnit artifacts; a required aggregate job validates every report and the complete pinned test-ID set. |
| Release PR `test → main` | Repeats the same pinned promotion checks against the release candidate. |
| Nightly / manual | Long fuzzing, extended GC/lifecycle/resource-limit stress, sanitizer/Miri coverage where supported, broad platform/target matrices and performance tracking. |

The workflow reads immutable upstream URLs and SHA pins from `conformance/baseline.json`; the pin is not duplicated in YAML. It creates the manifests before running shards, stores each manifest as an artifact, executes the assigned shard with the existing runner, and downloads every available shard report before aggregation. The aggregate blocks missing/duplicate/misassigned tests, worker errors, failed classifications, invalid reports, incomplete parser-equivalence pairs, missing shard artifacts and incomplete execution profiles. A failed matrix is never converted to success merely because some shard artifacts exist.

`OGMA_TEST262_ADAPTER` and `OGMA_PARSER_ADAPTER` repository Actions variables supply command strings for the promotion worker processes. The Fast CI lane builds `ogma-test262-adapter` and `ogma-parser-adapter` directly from the checked-out commit. The Test262 adapter's `ready` state means only that its exact allowlist is executable; unlisted tests remain `UNSUPPORTED`. A promotion workflow without a configured real adapter command produces harness errors. The workflow must not use `tests/test-runner/fake_adapter.py` for real conformance execution.

The current profile's parser and Runtime adapters are implemented, but Runtime readiness is restricted to the exact raw Test262 allowlist. The fake adapter remains strictly a deterministic test double used by runner self-tests; those tests prove the gate's bookkeeping contract, not JavaScript execution.

The pinned QuickJS-NG revision remains a development/test reference only. Fast CI executes a small deterministic differential fixture set that compares primitive return values and an observable side-effect trace; this is not a full differential corpus. Broader differential coverage and long-running checks remain promotion or scheduled/manual work.
