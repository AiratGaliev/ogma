# Native Provider Architecture

**Status:** Target architecture contract; not a production implementation claim.
**Implementation owner:** [OGM-107](https://linear.app/ogma-engine/issue/OGM-107/resolve-logical-native-requirements-through-registered-rust-providers), planned M3 capability.

**Architecture authority:** Ogma Notion specification, sections 7.4, 8.9.6, 11, 20.9 and 20.12.
**Related contracts:** [Static Host Modules](static-host-modules-contract.md), [Rust Embedding Code Generation](rust-embedding-codegen-contract.md), [Runtime Feature Profiles](runtime-feature-profiles.md), [Compatibility Profile](compatibility-profile.md), [Rust Workspace](rust-workspace.md).

## 1. Decision

Ogma will reuse trusted Rust-native implementations through a **Native Provider architecture** integrated with the existing Host Module System, Rust embedding/host boundary and application packaging pipeline.

Ogma will not create a second package manager, Cargo resolver, generic native-addon loader or independent native dependency manager.

The responsibility split is:

- **Host Module** declares the JavaScript-facing module/API, its exports, module dependencies, required capabilities and logical native requirements.
- **Native Service** is a stable host-facing operation or service contract that can be used by one or more Host Modules and directly by a Rust host.
- **Native Provider** is a trusted, compiled Rust component that implements one or more Native Services and declares its ABI, target and dependency requirements.
- **Host / Embedding API** registers the available services/providers and remains the authority for capabilities, resource policy and lifecycle.
- **Ogma Packaging** combines the resolved JavaScript, module, provider and Cargo build graphs into one deterministic application plan.
- **pnpm and Cargo** remain the authoritative tools for resolving their respective package ecosystems.

A provider is an implementation choice, not a new JavaScript module format or a new ABI. The M2 Rust embedding source generator (OGM-106) is a separate build-time adapter that wraps canonical portable `.ogmb` bytes for compile-time inclusion in a Rust host. Its generated module is not a Native Provider or Native Service: it does not satisfy a `native.*` requirement, register/select an implementation, add a Cargo dependency, or generate native machine code.

## 2. Terminology and stable identifiers

Keep these identifiers distinct:

| Identifier | Meaning | Example |
|---|---|---|
| Host Module specifier | JavaScript module identity | `node:crypto`, `ogma:crypto` |
| Native requirement ID | Logical requirement declared by a Host Module | `native.crypto.digest`, `native.crypto.random` |
| Provider ID | Identity of a particular implementation | `ogma.crypto.rust` |
| Cargo package ID | Build package resolved by Cargo | Cargo package name, version and source |
| Capability ID | Permission granted by the host | `crypto.digest` may require a separately declared capability |

**A Host Module's `nativeDependencies` are logical Native requirement IDs. They are not Cargo crate names, Cargo feature names or concrete Provider IDs.** The current OGM-25 fixture's `physics-kernel` is an opaque logical dependency identifier; it is not evidence of Cargo package resolution.

Production Native requirement IDs use the reserved `native.` namespace (for example `native.crypto.digest`) so they cannot be confused with Capability IDs such as `crypto.digest`. The existing OGM-25 M0 fixture IDs remain opaque historical proof values and are not retroactively renamed.

The existing v1 Host Module descriptor/profile proof remains the historical M0 contract. Its `enabledNativeDependencies` expresses which logical native requirements a profile allows/makes available; it does not identify or build a Cargo package. Production provider selection must add a versioned provider descriptor/selection contract rather than silently changing the meaning of the v1 schema.

## 3. Provider contract and resolution

A production Native Provider descriptor must identify, at minimum:

- a stable provider ID and semantic version;
- the supported OGM ABI major/minor range and host-service contract revisions;
- target triples/architectures on which the compiled provider is available;
- the logical Native requirement IDs it implements;
- any logical Native requirements it itself consumes;
- the linked Rust implementation identity and its build provenance as observed in the host Cargo graph;
- provider-specific configuration requirements that the host must supply before registration.

A provider may implement several requirements. Several providers may implement the same requirement, but the build must select one deterministically. If more than one compatible provider is available and no explicit selection/default policy is declared, resolution fails as ambiguous; Ogma must not choose based on registration order or hash-map iteration.

The provider resolver computes the transitive closure of required Native requirement IDs and maps every requirement to a compatible registered provider. It rejects missing providers, cycles, duplicate/conflicting implementations, unsupported targets, ABI mismatches and unavailable transitive requirements. Its output is an immutable, deterministic part of the application build plan.

Provider metadata is declarative and validated before registration. Provider code itself is compiled Rust code and is trusted; a descriptor is not a sandbox or a way to execute arbitrary Cargo build scripts.

## 4. Reuse the existing Host Module and ABI architecture

The provider architecture extends, rather than replaces, the existing subsystem boundaries:

1. The Node resolver or generic ModuleLoader identifies the imported Host Module.
2. The Host Module descriptor identifies the required Native requirement IDs and capabilities.
3. The Host Module Registry computes the module dependency closure and validates runtime imports.
4. The Native Provider Registry resolves the required Native requirement IDs to providers available for the selected host/target.
5. The existing versioned OGM ABI, typed host dispatch and Host Service interfaces carry calls to the Rust implementation.
6. The capability/resource policy is enforced at the Host API boundary on every applicable operation.

Do not introduce a provider-specific ABI, a second function table, a second JavaScript object model or a separate module loader. The public Rust API may provide ergonomic registration types, but they must compose over the existing versioned ABI and opaque-handle/ownership rules.

Node-compatible facades and Ogma-native JavaScript APIs may call the same Native Service. For example, `node:crypto` and a future `ogma:crypto` facade may share one registered cryptographic service. The provider owns the low-level Rust implementation; each JavaScript facade owns its own observable JavaScript API semantics, conversions and error mapping.

Pure/runtime-level modules such as path/string/Buffer utilities may have no Native Provider requirement. A Node API must not acquire a provider merely because it has a `node:` specifier.

## 5. Cargo is the native package resolver

A provider is compiled into the Rust host/application through an explicit Cargo dependency or is implemented directly by the host application. The host's `Cargo.toml` and `Cargo.lock` remain authoritative for Rust package resolution, version selection, sources, checksums and target-specific dependency resolution.

Ogma may inspect Cargo's resolved metadata and validate that the provider selected in its build plan is actually present in the locked host build graph for the requested target. Ogma must not independently resolve crate versions, reimplement Cargo feature unification, silently edit Cargo manifests, fetch arbitrary crates because an npm package imported a Node API, or create a parallel Rust lockfile format.

Cargo features remain Cargo build configuration, not a direct one-to-one mapping from Ogma logical profile flags. Provider feature requirements must be checked against the effective Cargo-resolved build graph; do not assume that declaring a feature on one dependency edge creates an isolated feature variant when Cargo unifies features for the same package.

The first implementation uses **statically linked, trusted Rust providers** registered by the host/build. Runtime loading of arbitrary `.dll`, `.so`, `.dylib`, `.node` or other native binaries is out of scope. If a provider is not in the host's compiled dependency graph, the build reports the missing provider; it does not download or compile it implicitly.

## 6. Keep the dependency graphs separate

Ogma has different graphs with different owners and sources of truth:

| Graph | Owner / source of truth | Responsibility |
|---|---|---|
| JavaScript package graph | pnpm lockfile and pnpm install/materialization | Package versions, importers, snapshots, integrity and JS dependency topology |
| Runtime module graph | Generic ModuleLoader and the selected resolver (Node rules in `ogma-node-compat`) | ESM/CJS module identity, linking, resolution, loading and evaluation |
| Host Module graph | Host Module Registry | Host module dependencies, imported exports, feature/capability requirements and logical Native requirements |
| Native Provider graph | Native Provider Registry / host registration metadata | Mapping logical Native requirements to compatible compiled providers and transitive provider requirements |
| Rust build graph | Cargo manifest, Cargo.lock and Cargo target metadata | Rust crates, versions, sources, features, target-specific compilation and native linking |

The application build plan composes these graphs; it does not replace their resolvers.

A package importing `node:crypto` creates a JavaScript/runtime reference to the registered Node builtin. The builtin's Host Module metadata then requires the appropriate Native Services. The provider registry resolves those requirements against providers compiled into the host. **The npm package does not install or select Cargo dependencies.** The same rule applies to `node:fs`, networking and other host-backed APIs.

## 7. Capabilities, security and lifecycle

- Importing a Host Module or registering a Provider never grants a capability.
- Provider registration describes available implementation, not authority. The host explicitly grants capabilities and supplies constrained services/configuration.
- Direct Rust calls and JavaScript facade calls must share the applicable permission checks, quotas, cancellation, audit and error policy.
- Filesystem access remains rooted in the host-provided VFS; networking remains endpoint/listen-policy controlled; cryptographic algorithms and key operations remain explicit.
- Providers execute as compiled native code in the host process. Capability checks, ABI validation, bytecode verification and resource budgets do not sandbox a malicious or memory-unsafe provider.
- Only trusted providers may be linked. Untrusted extensions require a separate process/WASM isolation boundary with independently enforced limits.
- Provider initialization, shutdown and resource cleanup follow the existing Runtime/host lifecycle. Providers must not create a second JS event loop or execute JavaScript on worker threads.

## 8. Unified Ogma CLI

The `ogma` executable is the single user-facing **toolchain orchestrator**, not a replacement for pnpm or Cargo.

| Command family | Responsibility | Owner / phase |
|---|---|---|
| `ogma compile`, `ogma run`, `ogma inspect` | Compile, verify/execute and inspect portable artifacts through production libraries | OGM-93, M1 |
| TypeScript inputs to `ogma compile` | Route through the approved TS frontend into the same compiler/runtime path | M2 frontend work |
| Rust embedding emission from `ogma compile` (for example, `--emit rust`) | Emit a deterministic per-artifact Rust wrapper around the unchanged canonical `.ogmb`; no transpilation, provider resolution or native code generation | OGM-106, M2 |
| `ogma check` | Validate project configuration, selected profiles, module imports, capability grants and dependency/provider availability without silently changing dependencies | Toolchain evolution; packaging contract |
| `ogma deps plan` / `ogma deps why` | Show the composed dependency/build plan and explain why a module, requirement, provider or Rust artifact is included | Native provider and packaging work |
| `ogma providers list` / `ogma providers inspect` | Report providers registered/compiled for the current host and the services they implement | Native provider work |
| `ogma build` | Orchestrate lockfile ingestion, graph validation, TS/JS compilation, AOT/portable artifacts, Cargo host build/link and deterministic packaging | OGM-94, M3 |
| `ogma test` | Invoke the pinned conformance runner; never substitute for production execution | OGM-72 |

Optional `ogma packages add/remove/install` convenience commands may delegate directly to the pinned external pnpm executable. Such commands must remain transparent pass-throughs: pnpm owns package resolution, installation, lockfile mutation and its own diagnostics. They are not required; users may invoke pnpm directly. The normal Ogma build path consumes the supported pnpm lockfile contract and must not need pnpm at application startup.

Cargo remains the authority for Rust dependencies. The CLI may invoke Cargo as part of `ogma build` and report its resolved graph, but it must not become a Cargo resolver or silently add arbitrary crates. Adding a custom Rust Provider to a Rust host remains an explicit Cargo/host-application change.

The CLI is thin orchestration/presentation over production libraries and external tools. It must not duplicate ECMAScript semantics, module resolution, package resolution, provider resolution, Cargo resolution, ABI dispatch or runtime execution.

The exact project-manifest syntax and any package-manager pass-through command options are owned by the packaging/toolchain implementation. Do not introduce an unversioned speculative `ogma.toml` or make an optional CLI convenience command a prerequisite for building an application.

## 9. Ownership and milestone boundaries

- **OGM-25** remains the completed M0 proof of Host Module descriptors, typed registration, dependency closure and fail-closed stripping. It does not claim a production Native Provider Registry.
- **OGM-47** owns the production Runtime/embedding API, shared ABI and Rust host-service registration foundation. It must not add a second ABI or prematurely implement the M3 provider resolver.
- **OGM-59** owns Node package resolution, CJS and builtin identity/metadata. It records logical Native requirements for host-backed builtins but does not select Cargo crates/providers.
- **Native Provider implementation task (M3)** owns provider descriptors/registry, compatibility/selection, provider dependency closure and its integration with the existing Host Module graph.
- **OGM-98** implements selected Node Host APIs through the shared Host Services and selected providers; it does not implement a separate provider or Cargo resolver.
- **OGM-94** owns pnpm lockfile ingestion, canonical JS dependency graph, composition with module/provider/Cargo metadata, `ogma build`, native build/link and final bundle. It must not reimplement any upstream resolver.
- **OGM-93** owns the initial M1 CLI and keeps it thin. Later CLI commands are extensions of the same executable, not a second tool.

Do not create a standalone Native Dependency Manager or a new Cargo crate merely to mirror this logical architecture. Add a crate only when the production dependency/API boundary is demonstrated by the implementation task.
