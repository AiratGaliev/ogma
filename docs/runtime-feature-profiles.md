# Runtime feature profiles and production stripping (OGM-28)

**Status:** M0 architectural build/profile contract
**Machine-readable profile set:** [profiles.json](../contracts/runtime-profiles/v1/profiles.json)
**Machine-readable schema:** [profile-set.schema.json](../contracts/runtime-profiles/v1/profile-set.schema.json)
**Validator and evidence report:** [validate_runtime_profiles.py](../.github/scripts/validate_runtime_profiles.py)
**Architecture authority:** Ogma Notion specification, sections 8.9.6, 19.8 and 20.9.

## 1. Decision and implementation boundary

Ogma has one ECMAScript execution model with selectable build composition, not separate language variants for Debug, Development, portable interpreter and Native AOT.

The initial profile contract is:

1. **Portable interpreter is the baseline execution path.** It includes the portable bytecode format, verifier, Runtime core and versioned host ABI.
2. **Hybrid Native AOT is additive.** It retains the portable interpreter as a semantics-preserving fallback for eval, new Function, runtime-compiled source and functions that cannot be lowered safely. Native frames/root maps and the AOT bridge are included only for this backend.
3. **Debug/Development are orthogonal tooling/build profiles.** They preserve source metadata and interpreter debug hooks. DAP, Inspector UI, REPL, profiler hooks and Hot Reload are optional tooling components, not language features.
4. **Release strips optional tooling and optional source/debug metadata by default.** It keeps metadata required by the bytecode verifier and Runtime contract. Native symbols, source maps and crash artifacts can be retained as separate build artifacts.
5. **Node compatibility is a separate, explicit host layer.** Selecting an ECMAScript Runtime never implicitly enables Node resolution or Node APIs and does not grant ambient filesystem, process or network access.

This remains an M0 architectural profile contract, not a production build implementation. Component identifiers in profiles.json are **logical build boundaries**, not a one-to-one list of Cargo crates. OGM-40 introduces only the minimal M1 package boundaries documented in [Rust workspace](rust-workspace.md); the profile graph remains the source of truth for inclusion/stripping and must not be mechanically mapped to one crate per component. OGM-19, OGM-25, OGM-27 and OGM-33 remain owners of the existing ABI, host-module, native-root and debugger subcontracts.

## 2. Profile matrix

profiles.json defines eight complete, deterministic configurations across three independent axes: `buildMode`, `executionBackend` and `hostCompatibility`. Debug and Development are represented for both execution backends; the Node compatibility overlay is represented for both interpreter and hybrid-native Release. This is deliberate: tooling/build mode must not be coupled to a single execution backend, and Node adapters must not be tied to only one backend.

`includeRoots` are explicit profile roots; the resolved inclusion set is the transitive closure of the `requires` graph. `excludeComponents` are hard exclusions: a profile is invalid if an included component requires one. All unreferenced components are listed in `strippedComponents`.

| Profile | Execution / host | Included runtime composition | Default exclusions | Debug metadata / compiler policy |
|---|---|---|---|---|
| `debug-interpreter` | Portable interpreter; no Node layer | Runtime core, host ABI/registry, portable bytecode + verifier + interpreter, debugger API/hooks, DAP, Inspector/UI and REPL | Native AOT/root maps, Node resolver/APIs, profiler and Hot Reload | Full source metadata/maps; native symbols not applicable; Rust opt-level 0, full debug info |
| `debug-hybrid-native` | Hybrid AOT + portable fallback; no Node layer | Same debugger/tooling composition plus native root maps and AOT bridge | Node resolver/APIs, profiler and Hot Reload | Full source metadata/maps and embedded native symbols; debugger controls apply to interpreter fallback; native AOT stepping remains M3; opt-level 0 |
| `development-test` | Portable interpreter; no Node layer | Interpreter execution core, debugger API/hooks, DAP, Inspector/UI, REPL, profiler hooks and Hot Reload | Native AOT/root maps and Node resolver/APIs | Embedded source/debug metadata and maps; opt-level 1, debug assertions and overflow checks enabled |
| `development-hybrid-native` | Hybrid AOT + portable fallback; no Node layer | Development tooling plus native root maps and AOT bridge | Node resolver/APIs | Embedded source/debug metadata/maps and native symbols; DAP/Hot Reload apply to interpreter fallback; opt-level 1 |
| `portable-interpreter-release` | Portable interpreter; no Node layer | Minimal Runtime, host ABI/registry, portable bytecode, verifier and interpreter | Native AOT/root maps, source/debug metadata, debugger/API hooks, DAP, Inspector/UI, REPL, profiler, Hot Reload and Node compatibility | Source/debug metadata omitted; symbols not applicable; compiler opt-level 3, thin LTO, symbol stripping |
| `hybrid-native-release` | Native AOT + portable fallback; no Node layer | Portable execution core, native root maps, AOT bridge and host ABI/registry | Debugger/API hooks, DAP, Inspector/UI, REPL, profiler, Hot Reload, Node compatibility and embedded source/debug metadata | Source/debug metadata stripped; native symbols external; compiler opt-level 3, thin LTO, debug-info stripped from final runtime |
| `node-compatibility-portable-release` | Portable interpreter + explicit Node layer | Portable execution core, host ABI/registry, Node module resolver and selected Node API adapters | Native AOT/root maps and all developer tooling/debug metadata | Same portable Release stripping policy; Node compatibility grants no ambient OS/process/network access |
| `node-compatibility-hybrid-native-release` | Hybrid AOT + portable fallback + explicit Node layer | Hybrid execution core, host ABI/registry, Node module resolver and selected Node API adapters | All developer tooling/debug metadata | Same hybrid Release symbol/stripping policy; Node facade uses the same host ABI for interpreter and AOT calls |

### Orthogonal execution, tooling and compatibility axes

- The Debug and Development build modes exist for both Portable Interpreter and Hybrid Native execution. Hybrid profiles retain the interpreter for dynamic source and non-lowerable code.
- OGM-33's backend-neutral Debugger API, DAP adapter and interpreter hooks are included in hybrid Debug/Development profiles. In this M0 contract, breakpoints/stepping/inspection are proven for the portable-interpreter path; native AOT stepping/debug integration remains a later milestone and is not claimed here.
- Node compatibility is an independent host overlay, not an execution backend. Both Node Release variants include the same resolver/API adapters over the same versioned host ABI; Node APIs do not change ECMAScript semantics and do not grant capabilities merely by being imported.
- The matrix is a set of supported baseline configurations, not a requirement that every project ship all variants. A host selects one backend/build mode/compatibility combination and may opt into extra tooling through explicit roots.

### Compiler and native build settings

Every profile pins two separate sets of settings: `rustBuild` for compiling the Rust-native Engine/Runtime crates and `aotBuild` for generated JavaScript functions compiled by the Native AOT backend. They must not be conflated: Cargo optimisation of the Engine is not the same decision as optimisation/debug information for generated application code. The validator checks both sets against the selected build mode/backend and includes them in the deterministic plan/evidence report. `rustBuild` maps to eventual Cargo `[profile.*]` settings; `aotBuild` is the Ogma Toolchain contract for generated native code. Both are versioned M0 settings, not a claim that a production Runtime/AOT compiler already consumes them.

| Build mode / backend | `optLevel` | `debug` | Assertions / overflow checks | LTO | Codegen units | `panic` | Final strip policy |
|---|---:|---|---|---|---:|---|---|
| Debug / either backend | 0 | true | enabled | off | 256 | unwind | none |
| Development / either backend | 1 | true | enabled | off | 16 | unwind | none |
| Release / Portable Interpreter | 3 | false | disabled | thin | 1 | unwind | symbols |
| Release / Hybrid Native | 3 | true while extracting external symbols | disabled | thin | 1 | unwind | debuginfo |

For Hybrid Native Release, debug information is generated so the build can emit a separate external symbol artifact before debug sections are removed from the shipped executable. The external artifact is not linked into the Runtime package. For Debug/Development Hybrid Native, native symbols remain embedded in the internal build. The interpreter-only Release strips symbol data because native AOT symbols are not applicable.

The separate `aotBuild` policy for generated JavaScript code is:

| Execution backend / mode | Enabled | AOT optimization | AOT debug information | AOT symbols |
|---|---|---|---|---|
| Portable Interpreter, any mode | No | Not applicable | Not applicable | Not applicable |
| Hybrid Native, Debug | Yes | Minimal | Full | Embedded |
| Hybrid Native, Development | Yes | Balanced | Full | Embedded |
| Hybrid Native, Release | Yes | Aggressive | Full in the symbol-generation stage | External artifact |

This records that Debug uses minimally optimized native code, Development uses moderate optimization, and Release uses production optimization while retaining portable bytecode/interpreter fallback for dynamic and non-lowerable code. OGM-33's currently proven debugger commands operate on that interpreter fallback; `aotBuild.debugInfo` and symbol policy do not claim that native AOT stepping already exists.

### Feature-flag mapping

Feature flags in profiles.json are **logical contract identifiers** for the eventual build system. They are not Cargo features today. The validator derives the expected flag set from the resolved component closure and rejects missing and stale flags.

- Execution: execution.portable-bytecode, execution.bytecode-verifier, execution.interpreter, execution.native-root-maps, execution.native-aot.
- Runtime/embedding: runtime.core, runtime.host-abi, runtime.host-modules.
- Debugger: debug.source-metadata, debug.api, debug.interpreter-hooks.
- Tools: tools.dap-transport, tools.inspector, tools.inspector-ui, tools.repl, tools.profiler-hooks, tools.hot-reload.
- Optional Node layer: compat.node-resolver, compat.node-api.

The profile data carries the complete flag set per configuration. The matrix deliberately does not expose flags such as language.eval, language.function-constructor or language.dynamic-code: runtime profiles cannot feature-gate ECMAScript dynamic-code semantics.

## 3. Language/runtime semantic invariants

Every profile pins the same ECMAScript-2026 language baseline and dynamicCode: preserved policy. eval and new Function are ECMAScript semantics, not optional debugger, Node or package-manager capabilities. Build-mode/compiler settings and execution backend selection are independent axes; switching Debug/Development to Hybrid Native does not switch off those semantics.

- Runtime-compiled source uses the portable bytecode/interpreter path in both interpreter and hybrid-native deployments.
- AOT may be selected only where it preserves the required semantics. If a function or construct cannot be lowered safely, use interpreter fallback when the selected Runtime implements it.
- An unsupported language feature or unavailable host API fails with a deterministic diagnostic; backend fallback must not be used to claim a feature that the Runtime does not implement.
- Both backends use the same Runtime core, host ABI, capability checks and selected host adapters.
- The Node node condition and Node APIs are enabled only by the explicit Node compatibility profile. Node compatibility remains a compatibility layer over ECMAScript module/runtime contracts, not part of ECMAScript Core.

This matches the compatibility contract: dynamic source is runtime-compiled and uses portable bytecode/interpreter execution; AOT limitations must not silently alter dynamic JavaScript semantics or local eval scope behaviour. See [Compatibility Profile](compatibility-profile.md), sections 2.3, 4, 5 and 7.

## 4. Dependency-aware stripping contract

The machine-readable graph is a build contract, not a file-name filter or a list of Cargo features to blindly subtract.

For each profile the validator:

1. Validates unique component IDs, unique feature flags and all declared dependency targets.
2. Resolves every includeRoots entry through the complete transitive requires closure.
3. Rejects unknown roots/dependencies and dependency cycles.
4. Rejects any hard-excluded component that is required by the resolved closure.
5. Checks that backend, Node compatibility selection, debugger/tooling switches, metadata policy and feature flags agree with the resolved closure.
6. Produces sorted included and stripped component sets and deterministic counts for the evidence report.

A referenced component cannot be removed merely because its implementation is optional in another profile. A missing module or adapter dependency is a build error, not a runtime surprise.

### Existing host-module graph remains authoritative

OGM-25 already owns concrete host-module selection. HostModuleRegistry::build_profile computes transitive host-module dependencies and validates declared native dependencies, runtime imports and native module references. It fails with deterministic diagnostics when a runtime import or native-module dependency points to a stripped module, and when a required native dependency is unavailable.

OGM-28 does not reimplement or replace that registry. A future build planner must validate both graphs and combine their resolved closures before linking:

- runtime-component graph from this contract;
- concrete host-module/native-dependency graph from [Static Host Modules](static-host-modules-contract.md).

The OGM-25 ModuleBuildPlan remains a registry-bound snapshot; a stale plan must not be reused after a registry mutation.

## 5. Debug metadata, hooks and symbol lifecycle

The following runtime metadata is mandatory in **every** profile and must not be stripped:

- function identity;
- code-generation/cache identity;
- frame layout;
- environment and capture descriptors;
- suspension state;
- bytecode-verifier summary.

These fields are required by portable bytecode verification, Runtime execution, cache identity and future backend transitions. They are not optional source-debug metadata.

Optional debugger metadata has a different lifecycle:

| Item | Debug | Development/Test | Release |
|---|---|---|---|
| Source file identities/digests and source maps | Embedded | Embedded | Stripped from runtime by default; source-map artifact may be external |
| Source spans, lexical scopes and variable/capture locations | Embedded | Embedded where available | Stripped from runtime by default |
| Code-generation identity | Retained | Retained | Retained |
| Interpreter debug hooks | Included | Included | Omitted unless host explicitly requests diagnostic support |
| Debugger API / DAP / interactive Inspector | Included | Included | Excluded by default |
| REPL / Inspector UI | Included as tools | Included as tools | Excluded by default |
| Profiler hooks / Hot Reload | Optional or excluded | Included | Excluded by default |
| Native debug symbols | Not applicable to interpreter profile | Not applicable to interpreter profile | Separate external artifact for hybrid native builds; not in runtime binary by default |
| Crash/source artifacts | External when retained | External when retained | External and independently retainable |

The debugger contract already defines the safe stripping boundary: source/debug additions can be removed, but executable bytecode, Core IR semantics, exceptions, effects, module identity and runtime behaviour must remain unchanged. In particular, do not remove verifier/runtime metadata listed above. benchmarks/debugger/metadata.py::strip_debug_metadata and test_release_stripping_removes_only_source_debug_metadata provide the M0 executable-payload preservation proof; OGM-28 keeps that proof as the metadata implementation authority.

DAP is a transport adapter over the backend-neutral DebuggerPort. Inspector and UI tooling are separate from ordinary execution. Excluding DAP or Inspector UI does not remove interpreter semantics or the execution hooks required by the Runtime itself; it only removes the optional diagnostic adapter/service. An actual build must not make debugger transport a hidden Runtime dependency.

The checked-in Release profiles set all tooling switches to false and strip optional source/debug metadata by default. A host may explicitly opt a tooling component back in by setting its tooling request and selecting the component in includeRoots; the validator then resolves its full dependency closure, checks feature flags and requires the matching metadata policy. This makes 'not shipped unless requested' an enforceable configuration rule rather than an unconditional prohibition on diagnostic Release builds.

## 6. Initial footprint evidence and limits

The Fast-PR-specific OGM-28 job emits ogm28-runtime-profile-evidence. It records:

- profile-manifest and schema byte sizes;
- number of logical components and resolved/stripped components per profile;
- linked-runtime, runtime-module, tooling-only and metadata component counts;
- resolved Rust compiler and generated-code AOT settings (optimization, debug information, assertions, LTO, codegen units, panic strategy and symbol stripping) for all eight build/backend/host combinations;
- Rust/Python toolchain and runner metadata;
- exact size and SHA-256 of the existing OGM-25 ogma-typed-dispatch-bench Release proof executable.

The OGM-25 executable measurement is an **M0 tooling baseline only**. It is not the size of an Ogma Runtime and does not estimate the size delta between production profiles. The current repository has no production Runtime, parser or AOT engine, so real per-profile Runtime binary sizes cannot yet be measured without fabricating numbers. The M1 Cargo workspace exists, but its initial crate boundaries are scaffolding rather than executable engine components. The report explicitly marks productionRuntimeExecutable as not-implemented with no byte count.

At this stage, binary impact is therefore specified through the exact linked/tooling/metadata component sets, while the measured proof artifact and profile-manifest footprint provide the available initial baseline. Real interpreter-only, hybrid AOT and Node-overlay binary deltas belong with the production Runtime/profile implementation and OGM-29's footprint comparison; they must be captured from reproducible release builds on a pinned toolchain.

## 7. Validation and evidence

Run from the repository root:

~~~bash
python .github/scripts/validate_runtime_profiles.py --report /tmp/ogm28-runtime-profile-report.json
python -m unittest discover -s .github/scripts -p 'test_runtime_profiles.py' -v

cargo fmt --manifest-path benchmarks/typed-host-dispatch/Cargo.toml --all -- --check
cargo clippy --manifest-path benchmarks/typed-host-dispatch/Cargo.toml --all-targets --all-features --locked -- -D warnings
cargo test --manifest-path benchmarks/typed-host-dispatch/Cargo.toml --all-features --locked
~~~

The OGM-28 workflow builds the existing OGM-25 proof executable in Release mode, passes that exact artifact to the validator and uploads the machine-readable evidence report. CI Fast also runs repository-wide Python self-tests, including OGM-33's debug-metadata stripping and execution-equivalence tests.

No full Test262, differential conformance or browser/runtime acceptance is claimed by this M0 contract proof. Promotion-only conformance remains on the dev to test route.

## 8. Acceptance map

| OGM-28 criterion | Contract / verification |
|---|---|
| Profile matrix lists included/excluded components, flags, compiler options and binary impact | profiles.json, all eight backend/build/host combinations, rustBuild/aotBuild settings and validator's resolved component/packaging report |
| Production stripping is dependency-aware and rejects required removed components | Dependency-closure validator; OGM-25 registry proof validates concrete host imports/native references |
| Debugger transport/UI removal does not alter semantics | Developer-tooling graph nodes are checked semantics-neutral; shared ECMAScript semantic contract is immutable |
| Dynamic-code semantics remain enabled in general compatibility profiles | Schema and validator pin dynamicCode: preserved; no dynamic-code language feature flag exists |
| Debug metadata/hooks and external symbols have explicit lifecycle | metadata profile policy plus section 5, backed by OGM-33 stripping proof |
| Initial size/footprint evidence is attached | CI evidence includes exact M0 proof executable bytes/hash and per-profile component/manifest measurements; production Runtime size is explicitly unavailable until Runtime crates exist |
