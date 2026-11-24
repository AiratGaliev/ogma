# Ogma Toolchain CLI Contract

**Status:** Target command ownership contract; the M1 CLI remains intentionally minimal.

**Architecture authority:** Notion Ogma architecture specification, sections 12–14 and 20.9.
**Related implementation issues:** OGM-93 (M1 CLI), OGM-72 (`ogma test`), OGM-94 (M3 packaging), Native Provider integration task.

## 1. Decision

Ogma has one user-facing executable: **`ogma`**.

It is the command-line front door and orchestration layer for compiling, running, inspecting, validating and building Ogma applications. It delegates ecosystem-specific package resolution and compilation to the tools that already own those contracts.

**A unified CLI is not a unified package manager.** It must not reimplement pnpm, Cargo, Node package resolution, ECMAScript module resolution, the Native Provider resolver or Runtime execution.

The CLI remains thin: it calls production Ogma libraries, produces deterministic plans/diagnostics and invokes external tools where their ownership requires it.

## 2. Command ownership by milestone

### M1 — Portable runtime CLI (OGM-93)

The initial command surface is deliberately limited to:

- `ogma compile <source.js> -o <artifact.ogmb>` — production frontend/compiler to portable bytecode;
- `ogma run <artifact.ogmb>` — decode, verify and execute through the production Runtime/interpreter with explicit bounded Runtime configuration;
- `ogma inspect <artifact.ogmb>` — inspect validated artifact metadata without executing it.

Do not add package installation, lockfile ingestion, provider selection, AOT linking or application bundling to OGM-93. The command must remain usable without pnpm or Cargo package-manager operations when running an already-built `.ogmb` artifact.

### M2 — TypeScript and package compatibility

The existing compile command may accept supported TypeScript/TSX inputs after the approved frontend lands, routing them through the same compiler pipeline. Node compatibility and package resolution remain in `ogma-node-compat` and OGM-59; M2 package fixtures use the externally installed pnpm tree.

The user may continue to call pnpm directly. Ogma does not need an install command to load a materialized package tree.

### M3 — Application toolchain and native build (OGM-94)

The CLI grows the application-level commands:

- `ogma check` — validate the project manifest, target, compatibility/build profiles, imports, capabilities, logical Native requirements, selected providers and lockfile/build inputs without installing or mutating dependencies;
- `ogma deps plan` — emit the deterministic composed build plan across the JS package, runtime module, Host Module, Native Provider and Cargo graphs;
- `ogma deps why <specifier-or-id>` — explain why a package, module, Native requirement, provider or native artifact is present;
- `ogma providers list` / `ogma providers inspect <provider-id>` — report providers compiled/registered for the selected host/target, the logical services they implement and their compatibility/provenance;
- `ogma build` — orchestrate supported lockfile ingestion, validation, compilation, Native AOT/portable artifact generation, Cargo host build/link and deterministic application packaging.

The exact flags and project-manifest schema are owned by OGM-94 and must be versioned there. This document defines command responsibilities, not an unversioned `ogma.toml` schema.

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
- The CLI does not contain an independent ECMAScript interpreter, module resolver, package resolver, provider resolver, Cargo resolver or linker implementation.
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
- [OGM-94 — Application Packaging](https://linear.app/ogma-engine/issue/OGM-94/implement-ogma-application-packaging-and-the-native-build-pipeline)
- [OGM-59 — Node resolver and CommonJS host](https://linear.app/ogma-engine/issue/OGM-59/implement-the-initial-ogma-node-compat-resolver-and-commonjs-host)
