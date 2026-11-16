# OGM-15 implementation package

## Scope

Adds a versioned `ogma.core-ir` contract (`1.0.0`), JSON Schema Draft 2020-12, dependency-free Python verifier, positive/negative fixture corpus, and unit tests. The contract describes backend-neutral function-level CFG, SSA block arguments/dominance, JavaScript semantic operations, value representation facts, effect-token SSA, exception edges, lexical environments/captures and source/debug/suspension provenance.

No runtime, interpreter, AOT codegen, native ABI, VM bytecode format, root Cargo workspace, or new third-party dependency is introduced. Backend-specific fields/opcodes are rejected.

## Local validation performed in the isolated task workspace

- `python -m unittest discover -s .github/scripts -p 'test_*.py' -v`: 27 tests passed.
- `python .github/scripts/verify_core_ir.py --manifest tests/core-ir/manifest.json`: 11/11 expectations passed (4 accepted, 7 rejected).
- `python -m compileall -q .github/scripts`: passed.
- JSON Schema Draft 2020-12 self-check and independent `jsonschema` 4.26.0 validation: schema valid; all 11 fixtures structurally valid.
- Verifier report: `evidence/local-verifier-report.json`.

These validations were executed against the files in this package, not against a pushed topic branch or GitHub Actions run.

## Acceptance status

1. Versioned and documented contract/invariants: implemented.
2. Dynamic JavaScript operations without backend-specific lowering: implemented; generic dynamic results are required to remain boxed.
3. Effect/exception, closure/environment, representation and source metadata: implemented.
4. Verifier checks malformed CFG, reachability, SSA definitions and instruction-level dominance across normal and exceptional edges, edge arity/types, effect chains, exception metadata and unsupported representations: implemented.
5. Positive/negative fixture corpus and deterministic machine-readable report: implemented.
6. The same versioned IR documents are the common input contract for both backends. Actual execution by an interpreter and native AOT prototype could not be tested because neither prototype exists in the current M0 repository; actual dual-consumer execution remains the downstream OGM-31 proof, not a claim of this package.

## Repository delivery state

The implementation was committed to `feat/ogm-15-core-ir-contract-verifier` in `2dffac20ea6e98ecbc4e5bfebecd9ec35cbd31dd` and opened as [PR #7](https://github.com/AiratGaliev/ogma/pull/7), targeting `dev`. The PR contains the contract/verifier package and the existing Fast CI integration. The PR remains open; no merge was performed. Fast CI evidence is still required before delivery can be considered complete.
