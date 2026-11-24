# Rust workspace and M1 crate boundaries

**Status:** M1 bootstrap. `ogma-frontend` now contains the implemented JavaScript parser/source-diagnostics slice delivered by OGM-41. The remaining packages establish ownership and dependency boundaries; their presence does not claim that compiler/Core IR lowering, Runtime, verifier execution, interpreter or embedding behavior is complete.

## Workspace baseline

The repository root is the only production Cargo workspace. It pins Rust **1.100.0** in `rust-toolchain.toml`, including the `rustfmt` and `clippy` components. All production crates inherit version, edition, MSRV, license, repository metadata and lint policy from the root manifest.

The root `Cargo.lock` covers the seven production workspace members. The complete `benchmarks/` tree is excluded: each M0 proof/benchmark keeps its own manifest, lockfile, dependency resolution and task-specific CI. In particular, the nested `benchmarks/parser-frontend` workspace remains independent. Do not move M0 projects into the root workspace or replace their lockfiles with the root lockfile.

## Package ownership

| Crate | Responsibility | Internal dependencies |
|---|---|---|
| `ogma-frontend` | JavaScript parsing boundary, AST ownership, source spans and diagnostics (OGM-41) | None |
| `ogma-core-ir` | Canonical backend-neutral Core IR representation and validation (OGM-42) | None |
| `ogma-runtime` | Runtime Values, heap, objects/functions, explicit roots and shared language operations (OGM-43/44) | None |
| `ogma-bytecode` | Portable OGMB container, decoding/encoding and verifier boundary (OGM-45) | None |
| `ogma-compiler` | Compile-time orchestration from frontend/Core IR to portable bytecode (OGM-42/45) | `ogma-frontend`, `ogma-core-ir`, `ogma-bytecode` |
| `ogma-interpreter` | Execution of verified portable bytecode using the shared Runtime (OGM-46) | `ogma-bytecode`, `ogma-runtime` |
| `ogma-embedding` | Consumer-facing Rust Runtime/Context and host-call API (OGM-47) | `ogma-interpreter`, `ogma-runtime` |

The dependency graph is intentionally acyclic. Arrows point from a dependency to the crate that consumes it:

```text
ogma-frontend ─────┐
ogma-core-ir ──────┼──> ogma-compiler
ogma-bytecode ─────┘

ogma-bytecode ─────┐
ogma-runtime ──────┴──> ogma-interpreter

ogma-interpreter ──┐
ogma-runtime ──────┴──> ogma-embedding
```

The `ogma-runtime` and `ogma-bytecode` packages do not depend on compile-time crates. The interpreter path is `ogma-interpreter → ogma-bytecode + ogma-runtime`; the embedding path adds only the embedding facade. Neither path depends on `ogma-compiler`, `ogma-frontend` or `ogma-core-ir`. A CI dependency-tree assertion protects this boundary.

This is the complete initial production package set justified by M1. The JavaScript frontend is implemented in `ogma-frontend` using the pinned Oxc parser/semantic crates; it does not change the interpreter/Runtime dependency boundary. Do not scaffold separate crates for Node compatibility, TypeScript bindings, AOT backends, DAP, CLI, package resolution, fuzzing or release packaging in this task; their owning issues introduce those boundaries when implementation requires them.

## Dependency and feature policy

- The M1 JavaScript frontend pins Oxc 0.158.0 for parsing and early-error analysis. Its transitive dependency graph is locked at the root; Oxc stays within `ogma-frontend` and must not leak into interpreter-only Runtime packages. OGM-14 contains the bounded Oxc/SWC dependency-footprint evidence; OGM-41 records the production integration boundary.
- Internal dependencies are declared once in `[workspace.dependencies]` and inherited by member manifests.
- There are no Cargo feature flags at bootstrap. Do not create empty or speculative features merely to mirror logical identifiers in `contracts/runtime-profiles/v1/profiles.json`; those identifiers remain profile-contract data until a concrete implementation consumes them.
- Native Providers are a future Host Integration/build capability, not a reason to add an M1 crate. Their logical requirement IDs are not Cargo crate names or feature flags; provider crates must be explicitly present in the Rust host's Cargo graph, and Cargo remains authoritative for their versions, feature unification and target dependencies. See [Native Provider Architecture](native-provider-architecture.md).
- Keep the interpreter/embedding dependency path compile-time independent. Compiler, parser or semantic-analysis dependencies belong to the compiler/frontend side and must never be added to `ogma-runtime`, `ogma-bytecode` or `ogma-interpreter`.
- Add optional dependencies only with the crate that owns the capability. An interpreter-only build must not enable a compiler feature through default-feature unification.

## Build, formatting and lint policy

Run from the repository root with the pinned toolchain:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
```

- `rustfmt.toml` pins the Rust 2024 formatting edition.
- Root `Cargo.lock` is committed and all workspace verification uses `--locked`.
- Rust `missing_docs` is warned and Clippy `all` is enabled at workspace scope; Fast CI promotes warnings to errors.
- `unsafe_op_in_unsafe_fn` is denied. Do not blanket-forbid unsafe code: the future embedding/FFI boundary may require narrowly scoped unsafe blocks with documented invariants and review.
- Cargo profiles are centralized at the root. `dev` represents Debug, `development` represents optimized Development with debug metadata, and `release` is the portable-interpreter baseline. A hybrid-native symbol-preserving profile is introduced with the AOT backend, not in this bootstrap.

The OGM-40 PR also runs a task-scoped matrix of the existing lockfile-bearing M0 standalone proof crates. That regression matrix is not part of the required `fast-checks` context and does not run Test262, browser acceptance or E2E. The parser frontend benchmark has no checked-in lockfile and remains under its existing OGM-14 workflow.

## Smoke-test boundary

The initial OGM-40 bootstrap tests checked only crate compilation and contract identifiers. OGM-41 adds real JavaScript parser/source-diagnostic unit tests and a bounded pinned parser-corpus Fast-CI smoke through the Rust adapter. These checks do not execute JavaScript and do not test bytecode execution, Runtime values, GC, callbacks or embedding behavior; those remain with their owning issues.
