# OGM-41 — JavaScript frontend contract

## Scope

OGM-41 implements the JavaScript-only frontend entry point for the M1 subset. It owns lexical/syntactic parsing, the borrowed Oxc AST boundary, source identity/positions and parse/early-error diagnostics.

The crate does not implement TypeScript/TSX/JSX, type checking, TypeScript lowering, module resolution/linking, semantic binding storage for Core IR, Core IR lowering, bytecode generation or runtime execution. Those responsibilities remain with their owning M1/M2 issues.

## Parser decision

Use Oxc **0.158.0**, pinned exactly in `crates/ogma-frontend/Cargo.toml`:

- `oxc_parser` supplies the ECMAScript lexer/parser and syntax diagnostics;
- `oxc_semantic` supplies ECMAScript syntax/early-error checks, including redeclaration and strict-mode validation;
- `oxc_allocator` owns the compile-unit AST arena;
- `oxc_ast` and `oxc_span` expose the AST and source ranges.

This follows OGM-14's recorded M0 evidence. On the pinned Linux x86_64/Rust 1.100.0 benchmark runner, the adapter-inclusive parse path measured 173,606 files/s for Oxc and 54,933 files/s for SWC; the Oxc parse-plus-semantic path measured 101,000 files/s. The benchmark's Oxc normal transitive closure was 84 packages versus 90 for the SWC adapter. Those measurements are from a bounded JavaScript corpus and one runner, not a parser-core speed guarantee or TypeScript compatibility proof. The SWC timing includes SourceMap/SourceFile construction and a source clone while Oxc parses a borrowed source string.

A handwritten parallel parser is not introduced. The selected Oxc crates are isolated to `ogma-frontend`; the Runtime, bytecode and interpreter dependency paths do not include frontend/compiler dependencies.

## Source identity and coordinates

A `SourceFile` is immutable for the lifetime of its AST and carries:

- caller-provided stable SourceId;
- URI/path and optional caller-supplied revision;
- exact UTF-8 source bytes and their SHA-256 digest;
- a line-start index supporting LF, CRLF, CR, U+2028 and U+2029.

Every public source span retains inclusive/exclusive UTF-8 byte offsets and editor coordinates: one-based line, zero-based UTF-16 code-unit column. Oxc byte ranges are converted at the boundary; offsets are clamped to valid UTF-8 character boundaries.

This is the canonical source identity/position information for later AST → semantic model → Core IR propagation. The adapter uses the corpus-relative test path as source identity; it does not create persistent source-map artifacts.

## AST arena and lifetime

The caller creates a compile-unit `oxc_allocator::Allocator` and a `SourceFile`, then calls `ogma_frontend::parse(&allocator, &source, options)`.

The returned `ParsedProgram` borrows both inputs and carries the original `ParseGoal` plus effective `is_strict` metadata. Modules are strict, an explicit strict parse mode is strict, and a top-level `use strict` directive activates strictness even when Script mode was requested as sloppy. Function-local directive prologues remain in the AST for OGM-42 to inspect. Drop the parsed value and all data derived from the AST before resetting/dropping the allocator or source. AST nodes, Oxc semantic structures and parser-owned slices must not escape into Runtime values or longer-lived compiled artifacts. OGM-42 must lower/copy durable facts into its owned semantic/Core IR representation before the compile-unit arena is released.

Oxc may produce a partial AST after recoverable syntax errors. Ogma never exposes it as a successful parse: any error-severity parser or early-error diagnostic returns `ParseFailure` without an AST. Oxc's recovery is used to collect diagnostics only; there is no best-effort compile mode.

## Parse goal and strictness

- Script input uses the Script grammar goal.
- Module input uses the Module grammar goal and is inherently strict.
- Script sloppy mode respects an existing `"use strict"` directive.
- Script strict mode runs the Script parser against a temporary source with a leading strict directive. If a hashbang exists, the directive is inserted after its line. Diagnostic offsets are translated back to the original source. The public AST is always the parse of the original, unmodified source.

The temporary strict parse is deliberately a Script parse, not a Module parse: strict Scripts must not accidentally inherit Module-only grammar such as static import/export or top-level await.

## Diagnostics

Diagnostics are independent of Oxc's terminal renderer and preserve:

- parse versus early-error phase;
- optional Oxc diagnostic code, severity and message;
- all labeled source spans, primary-label state and label text;
- optional help and note text.

Parse/early-error failures are deterministic structured results. An invalid source cannot silently yield a partial AST.

## Parser conformance adapter

`ogma-parser-adapter` is a line-delimited JSON worker implementing the existing `ogma-test-runner-adapter` v1 envelope for `test262-parser-tests`. It returns:

- `parsed` for syntactically and early-error-valid input;
- `rejected` with phase `parse` or `early` for invalid input;
- `normalizedAstSha256` for positive `pass/` and `pass-explicit/` equivalence pairs.

It returns `unsupported` for runtime Test262 execution requests. Parsing is not execution; this adapter must never produce a runtime Test262 PASS.

The Fast-CI smoke checks six fixed cases from the immutable `test262-parser-tests` revision in `conformance/baseline.json`: script and module pass/pass-explicit pairs, one early error and one syntax failure. It verifies the adapter protocol, expected classifications and matching normalized-AST fingerprints. The full supplementary parser corpus remains a promotion-lane responsibility and has not been claimed as passed by this bounded smoke.

The compatibility profile therefore marks parser-adapter readiness as `ready` and Runtime execution as `not_implemented`. Parser readiness is not a claim of full ECMA-262 grammar/conformance or runtime compatibility. Full parser-corpus promotion invokes the real binary only when `OGMA_PARSER_ADAPTER` is configured to the verified command in repository Actions variables.
