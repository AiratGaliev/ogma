# OGM-14 — JavaScript frontend benchmark

This is a task-scoped M0 experiment. It does not implement an Ogma frontend or add parser dependencies to a production/runtime package.

## Compared implementations

| Backend | Pinned crates | Measured stages |
| --- | --- | --- |
| Oxc | `oxc_parser`, `oxc_semantic`, `oxc_allocator` 0.158.0 | parser-only; parser + semantic syntax checks |
| SWC | `swc_ecma_parser` 47.0.1, `swc_common` 27.0.0, `swc_ecma_ast` 30.0.2 | parser-only |

Both implementations consume the same source strings and valid-fixture selection from `fixtures/manifest.json`. The timed metric is **adapter-inclusive parse throughput**, not isolated parser-core throughput: Oxc includes source-type detection, arena allocator and parser construction; SWC includes `SourceMap`/`SourceFile` construction (including a source-string clone), lexer and parser construction. This deliberately captures each library's in-memory-source-to-AST integration path. The asymmetry in source ownership/setup is material and is not removed by the benchmark. Oxc uses `SemanticBuilder::with_check_syntax_error(true)` for the separate semantic pass.

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

- Rust toolchain: 1.100.0; exact Oxc/SWC crate versions are in the manifests. The complete resolved dependency graph is pinned in the committed `Cargo.lock.gz` snapshot. `run.sh` materializes `Cargo.lock` and verifies SHA-256 `73ffa651c29a26e0831f163b6c448ecb4a50e5af4d504173e1d177bafc0731c0` before any Cargo command; it never generates a new resolution.
- Two warm-up rounds, then twelve measured rounds with 2,000 parses per accepted fixture per round. This keeps aggregate timed work in the multi-second range instead of relying on sub-millisecond samples.
- Adapter-inclusive throughput excludes fixture disk I/O and source-file preloading, and is reported as files/second and MiB/second. It includes each backend's required per-parse setup; it must not be read as a pure parser algorithm comparison.
- Oxc parse-only, Oxc parse+semantic and SWC parse-only run as separate processes.
- Peak RSS is Linux `getrusage(RUSAGE_SELF).ru_maxrss`, converted from KiB to bytes. It includes process/code and the preloaded fixture corpus, so it is a process high-water mark, not a parser-allocation-only metric. SWC's SourceMap/source-file construction is included in its timed parser entrypoint because it is part of the adapter cost; fixture disk I/O is excluded for both.
- Oxc also records arena used bytes and capacity. SWC has no directly comparable arena counter; RSS is the shared memory metric.
- The collector records the direct and transitive normal dependency closures using `cargo metadata` and `cargo tree`, the lockfile SHA-256, target OS/CPU/toolchain and release-build duration. Dependency counts include the small harness/JSON dependencies; they are not binary sizes.

## Run

From the repository root on Linux with Rust/Cargo 1.100.0 and Python 3.12+:

```sh
bash benchmarks/parser-frontend/run.sh
```

The first build needs crates.io access to download the exact locked crate sources. The script materializes and verifies the committed lock snapshot, then formats, tests, lints and builds both isolated benchmark executables, runs all measurement modes, collects JSON results and validates the pinned fixture expectations.

The task-specific GitHub Actions job runs this proof on the OGM-14 topic PR and uploads the materialized `Cargo.lock`, raw results and environment metadata. The compressed lock snapshot is committed; the expanded lock is a reproducible build artifact ignored by Git. No full Test262, differential suite or E2E run is added to feature PR CI.

## Interpretation limits

- This suite is a reproducible comparison workload, not a full grammar or conformance proof. Test262 and the pinned TypeScript compiler corpus remain the relevant references for their separate language layers.
- Parser acceptance/error recovery is reported separately from Oxc semantic diagnostics. The recovery counter is a coarse fixture-level signal (partial AST after Oxc parser diagnostics, or successful SWC parse with recoverable errors); it is not treated as a directly equivalent parser-recovery score.
- Oxc includes allocator construction in the timed frontend operation and allocates syntax nodes in an arena; AST and semantic references are bounded by that allocator's lifetime. This supports bulk reclamation but means retained compiler data must be copied/lowered into an owned representation before the compile-unit arena is dropped.
- SWC's parser produces an owned AST with source positions backed by a `SourceMap`; this has a different allocation and source-management profile.
- Neither candidate is adopted as production dependency by this benchmark. The measured trade-offs inform a later frontend decision after the Rust workspace and representative TS/TSX lowering proof exist.


## Measured M0 result — 2026-11-16

The committed baseline was measured by GitHub Actions run [43873014136](https://github.com/AiratGaliev/ogma/actions/runs/43873014136), on PR head `c911108d09a034e2f274f61ea02608d9299b5023` merged with the then-current `dev` base for the CI checkout. The runner used Rust/Cargo 1.100.0 on Linux x86_64 with an AMD EPYC 9V74 80-Core Processor. The run processed 264,000 parse operations and 376,920,000 source bytes per backend measurement.

| Metric | Oxc | SWC |
| --- | ---: | ---: |
| Adapter-inclusive parse throughput | 173,606 files/s | 54,933 files/s |
| Adapter-inclusive parse throughput | 236.38 MiB/s | 74.80 MiB/s |
| Adapter process peak RSS | 3,452,928 bytes | 4,882,432 bytes |
| Normal transitive dependency closure | 84 packages | 90 packages |
| Per-AST arena usage | mean 12,349 bytes; max 34,792 bytes | not applicable |

Oxc parse + semantic run:
- parse phase: 1,504,762,899 ns;
- semantic phase: 1,109,088,229 ns;
- combined throughput: 101,000 files/s (137.52 MiB/s);
- process peak RSS: 3,764,224 bytes;
- maximum measured arena capacity: 49,024 bytes per fixture parse.

Both backends matched all 16 comparable parse expectations: 11 valid JavaScript fixtures and five invalid-syntax fixtures. Each emitted five parser diagnostics over the invalid corpus. The Oxc semantic pass ran on 12 parseable fixtures, reported the one expected duplicate lexical binding error, and had no semantic expectation mismatches. SWC semantic diagnostics were not measured. The recovery counters were one Oxc fixture and three SWC fixtures; this is a coarse signal and the parsers' recovery behavior is not directly equivalent.

The complete per-mode JSON, dependency closure, Cargo.lock SHA-256 and runner metadata are committed in `results/`; the exact compressed lock snapshot is committed as `Cargo.lock.gz`. The expanded Cargo lock contains 141 package records; the combined release workspace build took 45 seconds on this runner.

## M0 recommendation

**Keep Oxc as the leading candidate for Ogma's next JavaScript frontend proof, but do not adopt it as a production dependency yet.** In this bounded workload Oxc's measured adapter-inclusive parse path was about 3.16× faster and its process peak RSS was about 29% lower than the SWC adapter path. This is an integration-path result, not evidence that Oxc's parser core alone is 3.16× faster: SWC's timed path includes `SourceMap`/`SourceFile` allocation and source-string cloning, while Oxc parses a borrowed source string. The extra SWC setup is itself a relevant ownership/API integration cost, but it contributes to the throughput and RSS difference. Oxc also provides the parser and semantic-analysis stages in one integrated API, and the measured semantic pass detected the expected duplicate lexical binding. Its normal dependency closure was six packages smaller in this experiment.

The principal integration cost is Oxc's arena lifetime: syntax and semantic references are tied to the compile-unit allocator. Ogma should keep the allocator alive for the required compile phase and lower/copy only the durable information into an owned internal representation before releasing it. SWC's owned AST and SourceMap-based source positions may be easier to retain across phases, but the parser-only dependency does not provide the equivalent semantic-diagnostics stage measured here.

A handwritten minimal parser was not used as a competing baseline: implementing only a small grammar subset would make the result incomparable and would not be a meaningful alternative for Ogma's JavaScript compatibility target. SWC is the more useful established parser reference for this M0 decision.

These measurements come from one CI runner and one measured run over a small, repeatedly parsed fixture corpus. They are evidence for prioritizing the next proof, not a stable cross-machine performance ranking or an isolated parser-core speed comparison. Before production selection, Ogma still needs the separate TypeScript 7.0.2 parser/lowering proof, TSX/JSX coverage where applicable, broader Test262 and differential/fuzz coverage, and a concrete ownership/lowering integration prototype. **No TypeScript frontend completion or full ECMAScript conformance is inferred from this JavaScript-only result.**
