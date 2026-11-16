# OGM-15 implementation package

## Scope

Adds a versioned `ogma.core-ir` contract (`1.0.0`), JSON Schema Draft 2020-12, dependency-free Python verifier, positive/negative fixture corpus, and unit tests. The contract describes backend-neutral function-level CFG, SSA block arguments and instruction-level dominance, JavaScript semantic operations, representation facts, effect-token SSA, exception edges, lexical environments/captures, and source/debug/suspension provenance.

The verifier rejects backend-specific schema fields/opcodes and validates stable identifiers, declaration mutability, canonical constant payloads, capture and suspension uniqueness, malformed attributes, CFG, SSA dominance across exceptional instruction edges, effect chains, exception flow and representation contracts.

No production Rust runtime, interpreter, AOT codegen, native ABI, VM bytecode format, root Cargo workspace or new third-party dependency is introduced.

## Verification

- GitHub Fast CI is the authoritative gate for the final PR head; the passing run must match the exact final head SHA.
- The combined `python -m unittest discover -s .github/scripts -p 'test_*.py' -v` suite contains 52 tests.
- `tests/core-ir/manifest.json` contains 25/25 expected outcomes (4 accepted, 21 rejected).
- The Fast job validates Markdown/repository rules, runs the test suite, invokes the Core IR verifier and uploads its machine-readable JSON report, compiles Python CI helpers and runs the pinned Node.js package-resolution/scheduling reference corpora.
- Rust formatting, Clippy and workspace tests are explicitly not applicable yet because the repository has no root `Cargo.toml`/Cargo workspace.
- Test262, end-to-end runtime execution and browser/promotion acceptance are not claimed by this topic PR.

## Acceptance status

1. Versioned and documented backend-neutral contract/invariants: implemented.
2. Dynamic JavaScript operations remain semantic operations rather than backend-specific lowering instructions: implemented.
3. Exception/effect edges, closure/environment, representation and source metadata have explicit semantics: implemented.
4. The verifier rejects malformed CFG, invalid SSA uses/dominance (including instruction-level exceptional edges), incorrect edge/effect metadata, malformed attributes and unsupported representations: implemented.
5. Positive/negative fixtures and deterministic machine-readable verifier results: implemented (25 cases; 4 accepted, 21 rejected).
6. The schema, document and fixture corpus define one shared backend-neutral IR input format; no semantic translation between two backend-specific IR dialects is introduced. This M0 package does not claim actual interpreter/AOT execution because those prototypes and their runtime prerequisites do not yet exist. OGM-31 owns real consumption of the same fixtures by both backends and differential comparison of values, exceptions, side effects and host calls.

## Repository delivery

The implementation is reviewed in [PR #7](https://github.com/AiratGaliev/ogma/pull/7), targeting `dev`. The current GitHub PR head SHA and exact-commit Fast CI result are authoritative; no stale local report should override them.
