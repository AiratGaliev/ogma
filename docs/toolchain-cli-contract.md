# Ogma Toolchain CLI Contract

**Status:** M1 command implementation is delivered in `ogma-cli` by OGM-93; the command ownership for later milestones remains as specified below.

**Architecture authority:** Notion Ogma architecture specification, sections 12–14 and 20.9.
**Related implementation issues:** OGM-93 (M1 CLI), OGM-106 (M2 Rust embedding emission), OGM-72 (`ogma test`), OGM-94 (M3 packaging), OGM-107 (M3 Native Provider registry).

## 1. Decision

Ogma has one user-facing executable: **`ogma`**.

It is the command-line front door and orchestration layer for compiling, running, inspecting, validating and building Ogma applications. It delegates ecosystem-specific package resolution and compilation to the tools that already own those contracts.

**A unified CLI is not a unified package manager.** It must not reimplement pnpm, Cargo, Node package resolution, ECMAScript module resolution, the Native Provider resolver or Runtime execution.

The CLI remains thin: it calls production Ogma libraries, produces deterministic plans/diagnostics and invokes external tools where their ownership requires it.

## 2. Command ownership by milestone

### M1 — Portable runtime CLI (OGM-93)

The implemented M1 command surface is deliberately limited to:

- `ogma compile <source.js|source.mjs> -o <artifact.ogmb> [--module|--script]` — parse using the production frontend, run semantic/Core IR lowering and emit canonical verified OGMB v1 artifacts. `.js` defaults to Script goal and `.mjs` to Module goal; an explicit goal overrides the extension.
- `ogma inspect <artifact.ogmb>` — re-verify the container and show its version, feature flags, code/function metadata, available source/debug metadata and any indexed compilation-unit functions/module graph without executing code.
- `ogma run <artifact.ogmb> [--heap-bytes n] [--stack-bytes n] [--call-depth n] [--fuel n] [--timeout-ms n] [--job-capacity n]` — re-verify the persisted artifact (and all sidecar artifacts when present), load through the public Runtime/embedding API and execute verified bytecode. The same finite `EngineConfig` and per-execution cancellation/deadline policy are used for execution and PromiseJobs pumping. Every numeric override must be positive. When a limit is omitted, the CLI uses the public `EngineConfig::default()` values: 64 MiB aggregate heap quota, 16 MiB maximum single allocation, 8 MiB execution stack, 1,000,000 dispatched instructions, 256 nested calls, 256 loaded modules, 1,024 pending PromiseJobs and 64 callbacks per pump turn. `--heap-bytes` also caps the maximum single allocation when the heap quota is lower. The optional deadline has no default.

M1 accepts JavaScript `.js` and `.mjs` only. TypeScript/TSX, REPL, `ogma test`, package installation, lockfile ingestion, provider selection, AOT linking and application bundling are excluded. The command remains usable without pnpm or Cargo package-manager operations when running an already-built `.ogmb` artifact.

#### Compilation-unit sidecar

OGMB v1 remains the canonical, independently verified per-function artifact; the CLI does not change its container format. A compile operation writes the Program entry function to the requested `.ogmb` path and writes `<artifact.ogmb>.d/unit.json` plus helper/dependency OGMB files into the adjacent sidecar directory. The versioned `ogma.cli-unit` 1.0 index records source-relative identities, SHA-256 digests, function IDs, module descriptors and the complete static `referrer + specifier → module ID` table. Every referenced OGMB is re-read and re-verified before inspect/run; sidecar paths are bounded, relative and symlink/traversal checked. A standalone verified OGMB without a sidecar can still run when its entry function is self-contained.

For Module-goal input, the compile-time filesystem resolver accepts explicit relative `.js`/`.mjs` specifiers that canonicalize below the entry source directory. It rejects bare specifiers, unsupported extensions and traversal outside that root; it does not implement package lookup, extension probing, CommonJS or Node.js resolution. Dependency modules are compiled once and persisted as OGMB artifacts with their descriptors and exact request mappings. Runtime module linking, cycles, live bindings, evaluation and PromiseJobs remain in the production `ModuleResolver`/module-graph API. `ogma run` consumes only the re-verified persisted compilation unit; it does not reopen or recompile source files, so a compiled ESM graph can run after its original sources are removed.

Process exit codes are stable: `0` success; `2` invalid command/option/resource limit; `3` filesystem or host setup failure; `4` source parse/semantic/lowering/compile failure; `5` malformed, unsupported or unverified artifact/unit; `6` JavaScript exception or Runtime failure; `7` cooperative cancellation, deadline, fuel, stack/call-depth or job-budget interruption. Diagnostics are written to stderr; successful command results are JSON on stdout. Ctrl+C/SIGTERM are converted into the public Runtime cancellation token and signal handlers are restored when execution ends.

### M2 — TypeScript and package compatibility

The existing compile command accepts `.ts` and `.tsx` through the pinned `ogma.typescript.v1` profile and routes the type-erased Program through the same semantic/Core IR/bytecode pipeline as JavaScript. TypeScript inputs default to Module goal; `--script` overrides it. Type-only imports are erased before static module graph construction. TSX files without JSX are accepted; JSX elements/fragments are rejected with an actionable diagnostic because the consumer build owns framework lowering (the Solid Native consumer uses its pinned vite-plugin-solid transform and must pass emitted JavaScript to Ogma). Runtime enums/namespaces, decorators, parameter properties and import-equals/export-assignment are also rejected. The full supported/unsupported matrix, parser/transformer pins and source-span policy are in `docs/typescript-frontend-profile.md` and `contracts/typescript/v1/profile.json`. Node compatibility and package resolution remain in `ogma-node-compat` and OGM-59; M2 package fixtures use the externally installed pnpm tree.

OGM-106 also extends this same `ogma compile` pipeline with a Rust embedding-source emission mode (for example, `--emit rust`) for supported JavaScript and M2 TypeScript inputs. For each canonical `.ogmb` compilation-unit artifact, it emits one deterministic Rust source wrapper and its companion `.ogmb`; the wrapper embeds those exact canonical bytes at Rust compile time, normally with `include_bytes!`. This is a build-time artifact adapter, not JS/TS-to-Rust transpilation, a new execution backend or format, a multi-artifact bundler, a Native Provider, or a separate CLI command. The Runtime still verifies the embedded bytes through the ordinary loader. Exact emission flags and output details belong to OGM-106. A Cargo `build.rs` may invoke the same command; Cargo remains responsible for compiling and linking the Rust host.

The user may continue to call pnpm directly. Ogma does not need an install command to load a materialized package tree.

### M3 — Application toolchain and native build (OGM-94)

The CLI grows the application-level commands:

- `ogma check` — validate the project manifest, target, compatibility/build profiles, imports, capabilities, logical Native requirements, selected providers and lockfile/build inputs without installing or mutating dependencies;
- `ogma deps plan` — emit the deterministic composed build plan across the JS package, runtime module, Host Module, Native Provider and Cargo graphs;
- `ogma deps why <specifier-or-id>` — explain why a package, module, Native requirement, provider or native artifact is present;
- `ogma providers list` / `ogma providers inspect <provider-id>` — report providers compiled/registered for the selected host/target, the logical services they implement and their compatibility/provenance;
- `ogma build` — orchestrate supported lockfile ingestion, validation, compilation, Native AOT/portable artifact generation, Cargo host build/link and deterministic application packaging.

The exact flags for M3 packaging commands and the project-manifest schema are owned by OGM-94 and must be versioned there. The M2 Rust-emission flags/output contract remains owned by OGM-106. This document defines command responsibilities, not an unversioned `ogma.toml` schema.

OGM-94 consumes canonical `.ogmb` outputs from the existing compiler pipeline and may consume an OGM-106-generated Rust wrapper where a Rust host integration requires compile-time embedding. OGM-94 owns multi-artifact graph composition, linking and application packaging; it must not implement a second `.ogmb`-to-Rust emitter or treat the wrapper as a Native Provider.

### Optional package-manager convenience commands

A later CLI may expose transparent convenience wrappers such as:

- `ogma packages add <spec>`;
- `ogma packages remove <spec>`;
- `ogma packages install --frozen`.

These are optional UX shortcuts, not a required dependency-management subsystem. They delegate to the project's pinned external pnpm executable and preserve pnpm's exit code, diagnostics and lockfile semantics. They must not parse or mutate lockfiles themselves, silently change the pinned package-manager version, or run third-party lifecycle scripts implicitly. The default automated install path remains equivalent to a frozen install with lifecycle scripts disabled; any script execution is an explicit build-environment decision. Calling pnpm directly remains supported and authoritative.

Cargo operations remain explicit Rust-host operations. `ogma build` may invoke Cargo and inspect Cargo's resolved metadata, but the CLI must not implement Cargo dependency resolution or silently add arbitrary Rust crates. Adding a custom provider requires the Rust host project to declare and lock the provider crate, then register it through the host integration API.

### M4 — Conformance and expanded compatibility

`ogma test` is owned by OGM-72 and invokes the pinned conformance runner. It must not be implemented as a second test harness in the CLI. Future compatibility/report commands may summarize the versioned API/consumer matrices but cannot claim support beyond their test evidence.

## 3. CLI design invariants

- Every command is a thin wrapper over the production library/contract that owns the behavior.
- The CLI does not contain an independent ECMAScript interpreter, ECMAScript linker/evaluator, package resolver, provider resolver, Cargo resolver or linker implementation. Its compile-time file resolver only creates the bounded M1 static module graph; at runtime its host adapter serves the versioned sidecar mapping through the public `ModuleResolver` contract. Linking and evaluation stay in the production Runtime.
- Read-only commands (`check`, `deps plan`, `deps why`, provider inspection) must not install packages, rewrite lockfiles, mutate Cargo manifests or silently enable capabilities.
- Build commands must be deterministic from pinned inputs and report the exact target triple, toolchain, selected profiles, dependency graph identities, provider IDs/versions, ABI/bytecode requirements and artifact hashes.
- Unsupported APIs, unavailable providers, missing capabilities, ambiguous provider selection, target mismatches, lockfile drift and ABI mismatches fail closed with actionable diagnostics.
- External tool invocation must be explicit, logged and reproducible. Ogma must distinguish an external-tool failure from an Ogma validation/build failure.
- Normal packaged application startup must not invoke `ogma`, pnpm, npm, Corepack, Cargo or package lifecycle scripts.
- The same `ogma` executable may grow command groups across milestones; M1 scope must not be expanded to implement M3 packaging or M4 compatibility.

## 4. Relationship to Native Providers and dependency graphs

The CLI reports and orchestrates the architecture defined in [Native Provider Architecture](native-provider-architecture.md). It does not merge the five dependency graphs into one resolver:

1. pnpm resolves JavaScript packages and lockfile provenance;
2. Ogma ModuleLoader/Node resolver resolves Runtime modules;
3. Host Module Registry resolves Host Module and logical Native requirements;
4. Native Provider Registry maps requirements to compatible registered Rust providers;
5. Cargo resolves the actual Rust package/build graph.

The application build planner composes their results. A Node builtin import may introduce a Native requirement such as `native.crypto.digest`; it cannot trigger an automatic Cargo dependency installation. Provider implementations must already be explicitly included in the host Cargo graph or supplied by the Rust host.

## 5. References

- [Native Provider Architecture](native-provider-architecture.md)
- [OGM-93 — Initial CLI](https://linear.app/ogma-engine/issue/OGM-93/add-the-minimal-ogma-cli-for-compiling-inspecting-and-running-portable)
- [OGM-106 — Rust embedding source generation](https://linear.app/ogma-engine/issue/OGM-106/generate-rust-embedding-modules-from-portable-ogmb-artifacts)
- [OGM-72 — Conformance test command](https://linear.app/ogma-engine/issue/OGM-72/integrate-production-ogma-adapters-into-the-full-test262-promotion)
- [OGM-94 — Application Packaging](https://linear.app/ogma-engine/issue/OGM-94/implement-ogma-application-packaging-and-the-native-build-pipeline)
- [OGM-107 — Native Provider registry](https://linear.app/ogma-engine/issue/OGM-107/resolve-logical-native-requirements-through-registered-rust-providers)
- [OGM-59 — Node resolver and CommonJS host](https://linear.app/ogma-engine/issue/OGM-59/implement-the-initial-ogma-node-compat-resolver-and-commonjs-host)
