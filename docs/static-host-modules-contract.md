# OGM-25 — Static Host Modules and feature profiles

**Status:** M0 architectural proof contract
**Implementation:** benchmarks/typed-host-dispatch/src/static_modules.rs
**Upstream contracts:** [OGM-19 versioned Rust ABI](rust-abi-contract.md), [OGM-21 typed host dispatch](typed-host-dispatch-contract.md), Notion architecture §8.9.6

## 1. Purpose and implementation boundary

This proof adds declarative module metadata on top of the existing OGM-19 ABI and OGM-21 function table. It deliberately does not create another ABI, runtime, resolver, JavaScript object model, or production module loader. Each registered export is backed by the existing HostFunctionDescriptor and a generation-checked HostFunctionSlot.

The proof establishes the metadata and linking contract needed by a future engine implementation. It is not evidence that Ogma currently executes ECMAScript modules: the repository still has no production parser, Runtime, interpreter, or module loader.

### 1.1. Native requirement semantics

The descriptor's `nativeDependencies` contains stable **logical Native requirement IDs**, not Cargo crate names or concrete provider IDs. For example, a future host module may require `native.crypto.digest` or `native.crypto.random`; the current `physics-kernel` test identifier is an opaque M0 proof value. Production Native requirement IDs use the reserved `native.` namespace so they remain distinct from Capability IDs such as `crypto.digest`; existing OGM-25 fixture IDs are not retroactively renamed. The v1 proof's permissive string schema is retained for those historical fixtures; production registration/provider validation must enforce the reserved prefix for production requirement IDs. The profile's `enabledNativeDependencies` is an allow-list/availability declaration for those logical requirements. OGM-25 validates their presence and closure, but it does not resolve provider implementations, inspect Cargo metadata or compile native dependencies.

The production Native Provider architecture extends this proof: a provider declares which logical requirements it implements, which requirements it consumes, its OGM ABI compatibility and supported targets. A versioned provider registry resolves each requirement to exactly one compatible provider. When multiple providers can satisfy a requirement, the host/build profile must select one explicitly; missing or ambiguous mappings fail closed. See [Native Provider Architecture](native-provider-architecture.md).

Do not put Cargo package names, crate versions or Cargo feature names into the Host Module descriptor. Cargo remains the source of truth for actual Rust dependency resolution. Provider selection is performed against the Rust providers already compiled/registered by the host; it does not download or resolve crates.

## 2. Descriptor v1

The normative machine-readable descriptor shape is [contracts/host-modules/v1/descriptor.schema.json](../contracts/host-modules/v1/descriptor.schema.json). The Rust representation is HostModuleDescriptor / HostModuleExport; an example is [ogma-math.module.json](../benchmarks/typed-host-dispatch/tests/fixtures/ogma-math.module.json).

A module descriptor records:

- descriptor schema version and required OGM ABI major/minor;
- stable module specifier and semantic version;
- module-level capability requirements and feature requirements;
- host-module dependencies and logical Native requirement IDs (`nativeDependencies`), which may be satisfied by a native adapter but are not Cargo package or Provider identifiers;
- exported names, typed/generic signatures, effect metadata, per-export capabilities/features, and export binding semantics.

Capability names map to fixed bit positions (0..63). One name cannot be assigned to multiple bits and one bit cannot identify different names. A module-level capability is merged into every registered export descriptor, so the existing OGM-21 dispatch path continues checking it on every invocation. Export-specific capabilities are enforced by profile resolution and the existing slot invocation guard.

Export function descriptor names are canonicalized as <module-specifier>/<export-name>. Descriptor v1 export names are ASCII TypeScript binding identifiers; strict-mode restricted names such as `arguments` and `eval` are rejected. Descriptor validation is performed before registration. If a function-table registration fails part-way through a module, already registered slots from that module are unregistered; the module is never published as partially registered.

The module descriptor is Rust-side metadata. It does not append fields to OGM-19 C structs or alter OGM ABI 1.x layouts.

## 3. TypeScript declaration generation

HostModuleRegistry::generate_typescript_declarations generates an ambient declaration from the same registered export records that own the function slots. The generator orders exports by name, uses stable formatting, and includes module version plus capability/feature annotations where relevant.

This M0 proof covers the **Rust exports → TypeScript declarations** direction only. It is separate from the planned M2 **portable `.ogmb` → Rust embedding module** generator (OGM-106), which emits Rust source that embeds bytecode for a Rust consumer; see [Rust Embedding Code Generation Contract](rust-embedding-codegen-contract.md).

Current v1 mappings are intentionally narrow:

| OGM host signature | TypeScript declaration |
|---|---|
| Generic | (...args: unknown[]) => unknown |
| F64Binary | (left: number, right: number) => number |
| NumberHandleBinary | (left: number, right: number) => number |

Mutable exports are emitted as `export let`; immutable host-function and dynamically read-only exports are emitted as `export const`. This keeps the declaration binding category aligned with the descriptor rather than incorrectly presenting every export as immutable.

The generated ogma:math output is checked byte-for-byte against [ogma-math.d.ts](../benchmarks/typed-host-dispatch/tests/fixtures/ogma-math.d.ts). This is a binding proof for the current typed signature set, not a full ogma-bindgen or TypeScript type-system implementation.

## 4. Static import resolution and dynamic semantics

A function slot is returned only for a **static named ESM import** of an export explicitly declared ImmutableHostFunction, after the build plan has validated the module, feature, and capability requirements.

The resolver deliberately returns the generic path for:

- namespace imports;
- dynamic import();
- CommonJS require() interop;
- dynamic property access;
- mutable export bindings;
- dynamic/accessor-style exports.

These operations must not be converted into cached direct calls. Static binding does not authorize bypassing ordinary property access, live/mutable binding behavior, Proxy traps, or function identity changes. The OGM-21 call-site guard remains separate: it must validate the current function identity, and Proxy property reads continue through generic behavior.

The current ModuleImportResolution::GenericFallback is a routing decision only. The actual ECMAScript resolver/loader and generic call machinery remain future Runtime work.

## 5. Feature profiles, dependencies, and fail-closed stripping

The [profile schema](../contracts/host-modules/v1/profile.schema.json) and Rust HostFeatureProfile describe:

- root modules enabled in the application;
- enabled language/host features;
- named capability grants and their stable bit assignments;
- enabled native dependencies;
- literal runtime module imports;
- modules referenced directly by native adapters.

build_profile computes a deterministic transitive closure of declared module dependencies. It rejects unknown or missing dependencies, dependency cycles, missing required module features, denied module-level capabilities, and unavailable native requirement IDs. Export-level feature/capability requirements are checked when a concrete static import is resolved; unavailable exports return deterministic diagnostics rather than an unsafe slot. The M0 proof stops at logical native-requirement availability; production provider selection and Cargo-graph validation are separate M3 responsibilities defined in [Native Provider Architecture](native-provider-architecture.md) and owned by the Native Provider/packaging tasks.

Unused registered modules appear in ModuleBuildPlan::stripped_modules. A module referenced by a runtime import or native-module dependency must be present in the validated closure. Removing it from the profile yields RuntimeImportModuleStripped or NativeModuleDependencyStripped; it is never silently dropped. A module's declared `nativeDependencies` requirement IDs must also be present in `enabledNativeDependencies`. This checks logical availability only; it does not prove that a concrete Provider is selected or that a Cargo package is linked.

Diagnostics are stable values with machine-readable codes and are sorted by (code, module, subject, detail). They can be rendered without depending on hash-map iteration order.

A build plan is an immutable, registry-bound snapshot. Successful module registration or unregistration advances the registry revision; plans created by an earlier revision or another registry are rejected by both static resolution and declaration generation. This prevents an old dependency closure from being reused after modules are replaced or removed.

## 6. Verification map

| OGM-25 acceptance criterion | Implementation / evidence |
|---|---|
| Descriptor schema covers ABI, exports/types, capabilities and features | contracts/host-modules/v1/descriptor.schema.json; Rust descriptor validation |
| TypeScript declarations match Rust exports | generate_typescript_declarations; byte-for-byte .d.ts fixture test |
| Static resolution only where semantics permit | resolve_import; immutable ESM binding gets slot; dynamic/mutable/namespace/CommonJS/property paths use generic fallback |
| Capability and feature diagnostics are deterministic | stable ModuleDiagnostic; repeatability, feature-denial and capability-denial tests |
| Runtime/native references cannot be stripped | build_profile; runtime import, native module dependency and native-library closure tests |

Run from the repository root:

~~~
cargo fmt --manifest-path benchmarks/typed-host-dispatch/Cargo.toml --all -- --check
cargo clippy --manifest-path benchmarks/typed-host-dispatch/Cargo.toml --all-targets --all-features --locked -- -D warnings
cargo test --manifest-path benchmarks/typed-host-dispatch/Cargo.toml --all-features --locked
python3 .github/scripts/validate_static_host_modules.py
~~~

The branch-specific Fast CI job runs these checks and uploads a machine-readable schema/fixture validation report. Full Test262, browser/runtime acceptance, and promotion-only conformance are not part of this M0 feature PR.
