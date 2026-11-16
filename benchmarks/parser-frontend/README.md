# OGM-14 — JavaScript frontend benchmark

This is a task-scoped M0 experiment. It does not implement an Ogma frontend or add parser dependencies to a production/runtime package.

## Compared implementations

| Backend | Pinned crates | Measured stages |
| --- | --- | --- |
| Oxc | `oxc_parser`, `oxc_semantic`, `oxc_allocator` 0.158.0 | parser-only; parser + semantic syntax checks |
| SWC | `swc_ecma_parser` 47.0.1, `swc_common` 27.0.0, `swc_ecma_ast` 30.0.2 | parser-only |

Both parser implementations consume the same source files and valid-fixture selection from `fixtures/manifest.json`. Oxc uses an arena allocator and `SemanticBuilder::with_check_syntax_error(true)`. SWC uses its standalone ECMAScript parser.

The SWC parser package has no semantic-diagnostics stage equivalent to Oxc's semantic builder. SWC resolver passes provide binding/scope context, but are not counted as equivalent early-error/semantic diagnostics here. This experiment therefore reports that comparison as unavailable rather than making an apples-to-oranges claim.

## Pinned fixture corpus

The manifest covers:
- six curated JavaScript fixtures: modern expressions, class/private fields/static blocks, async iteration, ESM, Unicode/regular expressions and object iteration;
- five syntax-only consumer excerpts adapted from Codogma Engine at commit `982d25c26da94c1f73d61457bf55da682413eff2` (see `fixtures/consumer/NOTICE.md`);
- five invalid syntax inputs, including a multi-error source for parser-recovery observations;
- one duplicate lexical binding fixture for semantic early-error detection.

The benchmark is intentionally JavaScript-only. It contains no TS, TSX, JSX, declaration-file, type checker or lowering tests. JavaScript parser results do not establish TypeScript frontend compatibility.

The corpus is a bounded M0 decision fixture, not a Test262 run. Ogma Compatibility Profile 1.0.0 pins ECMA-262 17th edition (ECMAScript 2026), Test262 commit `2e58f87d8350ad51224617a041c6e041d52f8444`, and supplementary parser-tests commit `0e808c74fbec780646434cad17bb22dc52461003`. The older parser-only corpus is not treated as sufficient proof for the 2026 grammar.

## Measurement protocol

- Rust toolchain: 1.100.0; exact Oxc/SWC crate versions are in the manifests and resolved dependencies in `Cargo.lock`.
- Two warm-up rounds, then twelve measured rounds; each accepted fixture is parsed once per round.
- Throughput excludes fixture disk I/O and is reported as files/second and MiB/second.
- Oxc parse-only, Oxc parse+semantic and SWC parse-only run as separate processes.
- Peak RSS is Linux `getrusage(RUSAGE_SELF).ru_maxrss`, converted from KiB to bytes. It includes process/code and the preloaded fixture corpus, so it is a process high-water mark, not a parser-allocation-only metric.
- Oxc also records arena used bytes and capacity. SWC has no directly comparable arena counter; RSS is the shared memory metric.
- The collector records the direct and transitive normal dependency closures using `cargo metadata` and `cargo tree`, the lockfile SHA-256, target OS/CPU/toolchain and release-build duration. Dependency counts include the small harness/JSON dependencies; they are not binary sizes.

## Run

From the repository root on Linux with Rust/Cargo 1.100.0 and Python 3.12+:

```sh
bash benchmarks/parser-frontend/run.sh
```

The first build needs crates.io access. The script formats, tests, lints and builds both isolated benchmark executables, runs all measurement modes, collects JSON results and validates the pinned fixture expectations.

The task-specific GitHub Actions job runs this proof on the OGM-14 topic PR and uploads raw results, Cargo.lock and environment metadata. No full Test262, differential suite or E2E run is added to feature PR CI.

## Interpretation limits

- This suite is a reproducible comparison workload, not a full grammar or conformance proof. Test262 and the pinned TypeScript compiler corpus remain the relevant references for their separate language layers.
- Parser acceptance/error recovery is reported separately from Oxc semantic diagnostics.
- Oxc allocates syntax nodes in an arena; AST and semantic references are bounded by that allocator's lifetime. This supports bulk reclamation but means retained compiler data must be copied/lowered into an owned representation before the compile-unit arena is dropped.
- SWC's parser produces an owned AST with source positions backed by a `SourceMap`; this has a different allocation and source-management profile.
- Neither candidate is adopted as production dependency by this benchmark. The measured trade-offs inform a later frontend decision after the Rust workspace and representative TS/TSX lowering proof exist.
