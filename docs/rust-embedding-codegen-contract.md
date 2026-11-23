# Rust embedding code generation for portable OGMB

- **Status:** planned M2 capability; not implemented.
- **Owner:** [OGM-106 — Generate Rust embedding modules from portable OGMB artifacts](https://linear.app/ogma-engine/issue/OGM-106/generate-rust-embedding-modules-from-portable-ogmb-artifacts).
- **Architecture authority:** Ogma Notion specification, sections 4, 8, 12 and 13.
- **Related contracts:** [Portable bytecode container](portable-bytecode-contract.md), [Rust embedding ABI proof](rust-abi-contract.md).

## 1. Purpose

Ogma will be able to emit a Rust source module for a JavaScript/TypeScript program that has been compiled through the normal Ogma pipeline. The generated module will embed the canonical portable `.ogmb` bytes into a Rust application so that Rust compilation places those bytes in the resulting executable.

This is the Rust embedding-code-generation capability discussed as a qjsc-like workflow. It is **not** a JavaScript/TypeScript-to-Rust transpiler: JavaScript control flow, values, objects and runtime semantics are not rewritten as Rust implementations.

The generated Rust source is an embedding wrapper around a bytecode artifact. It is not an execution backend, a native AOT output or a new artifact format.

## 2. Artifact model

The M2 generator emits a deterministic pair:

1. A Rust source module (for example, `game.rs`).
2. The companion canonical `game.ogmb` artifact referenced by that module.

The Rust module uses compile-time inclusion, normally `include_bytes!`, to expose the exact artifact bytes and a small amount of stable module/format metadata. Generate one Rust module/artifact pair per canonical `.ogmb` compilation-unit artifact. This contract does not define a multi-artifact dependency graph, module registry bundle or application package; those remain separate build/packaging concerns. The companion `.ogmb` must be available while Rust compiles the generated module; after linking, the bytes are part of the Rust executable and the application does not need a separate `.ogmb` file at runtime.

Illustrative generated surface (not a frozen API or an existing implementation):

~~~rust
pub const MODULE_ID: &str = "game";
pub const OGMB_FORMAT_VERSION: (u16, u16) = (1, 0);
pub static MODULE_BYTES: &[u8] = include_bytes!("game.ogmb");
~~~

The exact naming, module shape and path expression are implementation details for OGM-106. The generator must ensure the inclusion path resolves reliably for the selected output location, including Cargo's build output directory.

## 3. Required compilation and execution path

The feature reuses the existing compiler path:

~~~text
JavaScript / TypeScript
  -> existing frontend and semantic analysis
  -> canonical backend-neutral Core IR
  -> existing portable bytecode lowering and OGMB v1 emission
  -> canonical .ogmb + generated Rust embedding module
  -> Rust compiler embeds the .ogmb bytes
  -> application calls the public Ogma Runtime / embedding API
  -> Runtime decodes and verifies the artifact
  -> portable bytecode interpreter executes it
~~~

The `.ogmb` bytes produced alongside the Rust module must be byte-for-byte identical to ordinary portable compilation when source and compiler options are identical. Rust emission must not add a second frontend, Core IR, bytecode encoder, optimization pipeline or semantic execution path.

The generated module only supplies bytes and stable metadata. Loading, module registration, Runtime ownership, resource budgets, capability enforcement, exceptions, host calls and execution remain governed by the existing public Rust embedding API and Runtime contracts. The generator must not invent an API that bypasses those contracts or expose internal VM/heap structures.

## 4. Cargo integration

A Rust application can include the generated module as ordinary Rust source. A Cargo `build.rs` may invoke the same `ogma compile --emit rust` command and write both generated files into `OUT_DIR`, with `cargo:rerun-if-changed` directives for every source/configuration input that affects the result.

This is an invocation pattern for the same compiler capability, not a separate compiler or code-generation implementation. OGM-106 must document and test one minimal consumer workflow. A dedicated `ogma-build` crate is not a prerequisite; extract a reusable helper only if an actual stable API/dependency boundary is demonstrated.

Compile-time dependencies (Ogma CLI/compiler, TypeScript frontend tooling and any build helper) must not leak into the final application's runtime dependency graph. The linked consumer needs only the generated embedding module and the public Ogma Runtime/embedding crates. The source JS/TS and companion `.ogmb` are build inputs, not files that must be distributed beside the finished executable.

## 5. Verification, safety and determinism

- The generated module must embed the exact canonical `.ogmb` bytes; it must not patch, normalize or silently trust the artifact.
- Embedded bytes remain untrusted input. The normal Runtime loader must perform the same version, feature, bounds, integrity and bytecode-verifier checks as for a file-loaded artifact.
- Corrupt, truncated, unsupported-version or unsupported-feature artifacts must continue to fail closed. Rust emission is not a verifier shortcut or a signature of trust.
- Generated Rust must be deterministic, safe and narrowly scoped. It must not expose raw Runtime pointers, `JsValue` layouts, GC cells, VM frames, compiler data structures or an unstable Rust ABI.
- Source diagnostics and source/module identity must be the same as ordinary `ogma compile`.
- Identical source, toolchain and compile options must produce identical `.ogmb` and Rust source outputs.
- The feature must preserve portable bytecode semantics and must not require native executable-memory support or target-specific native objects.

## 6. Ownership boundaries

| Area | Owner | Boundary |
|---|---|---|
| Rust module wrapping canonical portable bytecode | OGM-106 (M2) | Emits the Rust wrapper and companion `.ogmb`; does not translate JS/TS semantics into Rust. |
| Rust exports to TypeScript declarations and host registration | OGM-55 / OGM-56 (M2) | Opposite generation direction: Rust APIs become visible to TypeScript. |
| Public Runtime/embedding API and ABI | OGM-47 (M1) | Owns lifecycle and execution surface called by the generated-module consumer. |
| JS/TS frontends and canonical bytecode compilation | OGM-41–46, OGM-54 and OGM-93 | Own the existing compilation pipeline and portable artifact. |
| Native AOT, dependency graph ingestion, `ogma build`, linker and application packaging | OGM-62–68 and OGM-94 (M3) | Consume canonical portable/native artifacts; must not implement a duplicate `.ogmb`-to-Rust emitter. |

This feature does not change the `.ogmb` v1 layout, Core IR semantics, Runtime ABI or native AOT artifact contract. It does not generate native machine code and does not replace M3 application packaging. M3 packaging can consume canonical `.ogmb` artifacts directly; a Rust embedding module may be consumed where the chosen host integration calls for it.

## 7. Acceptance evidence

OGM-106 must provide focused automated evidence for:

- deterministic generation of the Rust source and companion `.ogmb`;
- byte-for-byte identity between the embedded bytes and ordinary portable compilation output;
- compilation of the generated module by a real Cargo consumer on the supported Rust toolchain/MSRV;
- execution through the public Runtime API with observable parity against direct `.ogmb` loading;
- successful launch after the source JS/TS and separate `.ogmb` build input are absent from the runtime deployment directory;
- verifier rejection of malformed/unsupported embedded artifacts;
- absence of compiler/CLI dependencies from the final runtime dependency graph.

These are planned acceptance requirements. The current M1 workspace, parser and M0 contracts do not establish that the production compiler, Runtime or Rust embedding generator already implements this capability.
