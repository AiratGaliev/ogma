# Ogma Compatibility Profile

- **Profile ID:** ogma-compatibility-profile
- **Profile version:** 1.1.0
- **Status:** target compatibility contract with implementation status for the delivered M1 subset
- **Baseline captured:** 2026-11-27
- **Normative owner:** this document. Architecture rationale remains in the [Ogma architecture specification](https://app.notion.com/p/4167ef18f18181bd9b34f9dda764c92d#b5ca462a10c841279d724b74c44b239a).

This profile defines the language, frontend, Node.js host APIs, module behaviours and consumer workloads Ogma intends to support. It separates the target contract from implementation status. The Rust workspace, Oxc frontend, synchronous interpreter, Runtime and embedding API now exist. OGM-48 adds a deliberately partial static ESM graph loader and a minimal Promise/PromiseJobs path. OGM-50 adds a production source-execution adapter, but its Test262 claim is restricted to the exact raw-test allowlist in `conformance/profile.json`; the Node.js host adapter and Node package resolver remain unimplemented. These M1 slices do not imply full ECMAScript or Node.js conformance.

## 1. Compatibility claims and status vocabulary

Ogma publishes independent compatibility results. A result in one matrix never implies a result in another.

| Dimension | What is measured | Primary evidence |
|---|---|---|
| ECMAScript | Language syntax and observable runtime semantics for a declared ECMA-262 edition/profile | Pinned Test262, parser fixtures, differential tests |
| TypeScript frontend | TS/TSX grammar, type erasure, runtime-affecting transforms and source metadata | Pinned TypeScript reference, transform fixtures and diagnostics tests |
| Type analysis | Static checker behaviour and declaration/type metadata | Separate checker corpus; not a runtime conformance claim |
| Node.js API | Selected module/API behaviour, tier, host capability and target platform | Pinned Node.js docs and selected Node reference tests |
| Module/package compatibility | ESM/CJS semantics, package resolution and conditional exports/imports | Resolver fixtures and pinned Node reference tests |
| npm package compatibility | Exact package/version/lockfile combinations that install, resolve, build and run | Versioned package corpus |
| Consumer compatibility | Real application workloads and dependency graphs | Pinned consumer repository revisions and integration results |

Use these states in every feature/API matrix:

- **supported** — implemented for the declared scope and passed its pinned reference/regression tests on the stated targets.
- **partial** — only the explicitly listed sub-surface is implemented and tested; exclusions and edge cases are named.
- **unsupported** — deliberately outside this profile; requests fail with a stable diagnostic.
- **deferred** — accepted as possible later capability or planned milestone work, but not implemented or verified in this profile revision.

A feature must never be labelled supported merely because a parser accepts its syntax, TypeScript accepts its types, an API name exists, or an AOT compiler can emit machine code. An UNSUPPORTED test result is not a pass.

The architecture's `implemented` state is published as **supported** in this profile: both mean that the declared behaviour is implemented and has passed its required tests. The architecture's `host-provided` label describes ownership/provisioning, not conformance; it is represented by the required host-capability and provider columns, and it does not by itself mean that an API is supported.

**Current implementation status:** the Rust workspace includes the OGM-41 Oxc JavaScript parser/source-diagnostics frontend, OGM-46 synchronous interpreter, OGM-48 M1 static ESM and PromiseJobs subset, and the OGM-50 production source-execution adapter. The parser-only readiness is **ready** and the Test262 execution adapter is **ready only for the exact three-test raw Script allowlist** in `conformance/profile.json`; every other Test262 ID is reported as **UNSUPPORTED** before invoking the engine. The bounded Fast-CI smoke separately checks the six parser-corpus cases, three allowlisted runtime cases and a small QuickJS-NG differential fixture set. None of these bounded results imply full parser-corpus, Test262, ECMAScript or Node.js conformance. ESM and Promise rows below are **partial** only for their explicitly enumerated M1 subsets. Node package resolution, Node scheduling APIs and broad ECMAScript semantics remain **deferred** or **unsupported** as listed.

## 2. ECMAScript baseline

### 2.1. Normative edition and test snapshot

| Item | Pinned baseline |
|---|---|
| Language standard | **ECMA-262, 17th edition — ECMAScript 2026**, June 2026 |
| Normative specification | [ECMA-262 2026](https://tc39.es/ecma262/2026/) |
| Publication record | [Ecma International — ECMA-262, 17th edition](https://ecma-international.org/publications-and-standards/standards/ecma-262/) |
| Test262 repository | [tc39/test262](https://github.com/tc39/test262) |
| Test262 commit | [conformance/baseline.json](../conformance/baseline.json) — `repositories.test262.revision` |
| Supplementary parser corpus | [tc39/test262-parser-tests](https://github.com/tc39/test262-parser-tests) — `repositories.test262-parser-tests.revision` in [conformance/baseline.json](../conformance/baseline.json) |

The immutable Test262 and parser-corpus revisions are maintained in [conformance/baseline.json](../conformance/baseline.json), which is the runner's machine-readable pin source. CI and local tooling must fetch those exact commits, never a moving branch. Updating a snapshot requires a reviewed profile/baseline change, upstream diff review and compatibility-result comparison. See [Test runner and corpus architecture](test-runner-and-corpus.md) for manifest generation, metadata handling and adapter requirements.

The parser-only corpus is an older supplementary fixture set (2021); it is not sufficient by itself for ECMAScript 2026 grammar acceptance. Test262 parser tests and Ogma-owned grammar/early-error fixtures remain authoritative for the selected 2026 profile.

ECMA-402 / Intl is a separate specification surface. The 2026 Ecma General Assembly approved ECMA-402 13th edition (ECMAScript 2025 Internationalization API) separately from ECMA-262. **Intl is deferred in profile 1.1.0**; no Intl conformance is implied by the ECMA-262 baseline. A later Intl profile must pin its own ECMA-402 edition, data/ICU provider and tests.

**Temporal is an intentional edition boundary exception.** The proposal reached Stage 4 during 2026 and Node.js 26 ships the Temporal API, but TC39's finished-proposals tracking assigns it to the 2027 publication year. It is therefore not part of the pinned June 2026 ECMA-262 17th-edition baseline. Profile 1.1.0 keeps the ECMAScript Temporal global and its associated built-ins **deferred**, even when running with the Node 26.13.0 reference line. A later profile must explicitly adopt the ECMAScript 2027 snapshot (or an individually pinned extension contract) and its Test262/reference fixtures; the Node version alone does not silently add the feature to this edition baseline.

### 2.2. Initial execution subset

The normative language reference is ECMAScript 2026, but the first executable runtime is intentionally a subset. M1's initial target is the smallest useful JavaScript runtime described by the architecture: values, objects, functions, lexical scopes, control flow, exceptions, module execution and the shared Promise/job model. It is not a claim of full ECMA-262 coverage.

| Feature group | Initial execution target | Current status | Reference evidence |
|---|---|---|---|
| Primitive values, operators, coercion and equality | Core ECMAScript value and operator semantics required by ordinary scripts | deferred | Test262 language/expression/built-ins tests |
| Declarations, functions, lexical environments and closures | var/let/const, function declarations/expressions, arrow functions, scope/hoisting/TDZ and closure capture | deferred | Test262 language/function/lexical-binding tests |
| Objects, arrays and property operations | Object/array creation, property access and mutation, descriptors, prototypes and observable property semantics needed by the selected subset | deferred | Test262 built-ins/Object/Array/property tests |
| Control flow and exceptions | Branches, loops, calls, throw, try/catch/finally, abrupt completion and completion propagation | deferred | Test262 language/statement/exception tests |
| Classes | Class declarations/expressions, constructors, methods, inheritance and private elements are staged after the minimal function/object proof | deferred | Test262 language/class tests |
| ESM | Static import/export subset, host-pluggable source resolver, module identity/cache, instantiation, live named/default imports, local/re-export/star exports, dependency evaluation and cycles | partial ? namespace imports/exports, dynamic import, top-level await, Node resolution and broader early-error coverage are excluded | OGM-48 embedding ESM graph fixtures; full Test262 module-code execution remains unavailable |
| Promise jobs | Promise.resolve/reject/then/catch subset, Promise reaction FIFO, returned-Promise adoption and host-driven bounded pump | partial ? direct Promise expressions and immutable const aliases only; mutable bindings statically assigned a recognized Promise fail closed; Promise-valued parameters/returns are not inferred; no general thenables, constructor/executor, finally, async/await, queueMicrotask, unhandled-rejection policy or Node event loop | OGM-48 Promise heap, scheduler and ESM integration fixtures; full Test262 Promise/async execution remains unavailable |
| Dynamic source | eval and new Function must preserve ECMAScript scope semantics; runtime-compiled code uses interpreter/bytecode | deferred | Test262 eval/Function tests and interpreter regressions |
| RegExp, BigInt, ArrayBuffer and TypedArray | Required runtime built-ins from the QuickJS-NG-derived initial implementation direction; each built-in has an independent feature/test state | deferred | Test262 built-ins/RegExp/BigInt/TypedArray tests |
| Generators, async functions and async generators | Full suspension, resumption, completion and abrupt-completion semantics | deferred | Test262 generator/async tests |
| Proxy, Reflect, Symbol, WeakRef and FinalizationRegistry | No blanket support; each feature requires its own semantics, GC and lifecycle proof | deferred | Corresponding Test262 feature groups |
| SharedArrayBuffer and Atomics | Requires an explicit memory/concurrency model and host/worker integration | deferred | Test262 shared-memory tests and stress tests |
| Intl / ECMA-402 | Separate profile only; not in this profile's initial execution contract | deferred | Pinned ECMA-402 suite when adopted |
| Temporal (Stage 4 in 2026; planned ECMA-262 publication year 2027) and later additions | Explicitly excluded from the ECMA-262 2026 edition baseline in profile 1.1.0, even though Node.js 26 ships Temporal; adoption requires a later profile revision | deferred | Pinned ECMA-262 2027 / extension reference and corresponding Test262 snapshot |

The table describes the target work sequence, not an assertion that all listed features land in M1. M1 must not be expanded into full language conformance by inference. A feature becomes supported only after implementation and tests demonstrate the required observable behaviour. A subset may become partial only when its supported sub-surface and exclusions are recorded explicitly.

### 2.3. Proposals, Annex B and host APIs

- Stage 4 proposals included in the pinned ECMA-262 2026 edition are part of the normative reference; they are not automatically implemented.
- Stage 0–3 proposals and proposal-specific syntax/semantics are **unsupported** in profile 1.1.0 unless a later profile explicitly adopts and tests them.
- Features added to the live TC39 draft after the 2026 edition are **deferred** until an edition/profile update.
- Annex B browser/web legacy semantics are **unsupported** in the embedded baseline unless a consumer requirement and explicit compatibility decision justify a narrowly scoped addition.
- DOM, window, document, Fetch, Web Storage and browser event APIs are not ECMAScript features and are not supplied by this language profile. A host may add separately capability-gated APIs.
- Node.js APIs are not part of ECMAScript Core. They belong only to the optional Node.js Compatibility Layer.

### 2.4. Test262 execution and result classification

Test262 is a reference suite, not a declaration that every test is applicable. The runner must read test metadata (features, flags, includes, negative phase/type, strict/sloppy mode, module/raw/async flags) and select tests against this profile.

Use the architecture's result categories:

- PASS — selected test passed.
- FAIL — observed behaviour differs from the expected result.
- UNSUPPORTED — test requires a feature outside the declared supported profile.
- EXCLUDED — test is excluded by a checked-in, reasoned exclusion.
- HARNESS ERROR — runner/harness setup or execution failed; this is never a pass.
- UNEXPECTED PASS — a previously unsupported/excluded test now passes and requires profile review.

Every exclusion must identify the test or deterministic selector, reason, affected feature and owner/revisit condition. Do not blanket-exclude a feature family to hide implementation failures. Preserve raw JSON/JUnit reports and the exact Test262 commit.

Profile 1.1.0 declares only these bounded runtime cases executable: `language/comments/hashbang/not-empty.js` (the exact partial `hashbang` allowlist entry), `language/directive-prologue/10.1.1-8gs.js` and `language/directive-prologue/14.1-4gs.js` (negative early-error cases). The latter two have no feature tags and are enabled solely by `test262Allowlist`; this prevents untagged tests from bypassing feature gates. The adapter creates a fresh Engine/Runtime/Context per request, parses the source, compiles through the production frontend/compiler, loads serialized verified OGMB and executes the Program entry. It supports Script/raw/synchronous requests without harness includes. Modules, async tests, non-raw tests and tests outside the exact allowlist remain `UNSUPPORTED`.

CI policy keeps full Test262 and full differential/stress/fuzz/E2E work in promotion or scheduled/manual lanes. Fast CI runs only the exact three Test262 IDs, six parser-corpus cases, and the deterministic differential fixtures; JSON and JUnit evidence are retained. The differential fixtures compare return and throw completions, primitive values, ordered side-effect traces and the host-call name/argument/completion/result trace using a registered Rust callback and its equivalent JavaScript reference function. QuickJS-NG remains a development/test oracle, never a production dependency. Do not claim runtime conformance from adapter or runner self-tests, and do not infer support for a feature family from these narrow probes.

## 3. TypeScript frontend and compile-time contract

### 3.1. Reference version

- TypeScript syntax/reference version: **7.0.2**.
- Pinned upstream reference: [microsoft/TypeScript v7.0.2](https://github.com/microsoft/TypeScript/releases/tag/v7.0.2), commit 1e4744d68260a7cb91b62b12edc3f6a2187faaf1.
- The pinned TypeScript compiler is a grammar/transform/type-checking oracle and test reference. It is not a production runtime dependency and does not replace Ogma's AST → semantic analysis → Core IR → bytecode/AOT pipeline.
- A newer TypeScript release or nightly must not silently change the accepted syntax or emitted runtime semantics. Update the reference and fixtures in a separate profile revision.

### 3.2. Layer boundary

Ogma has three distinct responsibilities:

1. **ECMAScript runtime** — executes JavaScript semantics only. It does not interpret TypeScript annotations, interfaces, generic parameters or declaration files.
2. **TypeScript frontend** — parses the declared TS/TSX subset, erases type-only syntax and lowers runtime-affecting TS constructs into semantics-preserving JavaScript/Core IR operations.
3. **Type analysis** — optional compile-time diagnostics and type metadata. Full TypeScript checker compatibility is a separate product claim and is not a prerequisite for executing a program whose frontend lowering succeeds.

Type assertions, any, external JavaScript, dynamic properties and reflection mean TypeScript types are not proof of runtime representation, object layout, pointer safety or absence of aliasing. Native AOT may use compiler facts only with a valid proof/guard and a generic runtime fallback.

### 3.3. Pinned consumer compiler and JSX configuration

The SolidJS Native consumer revision pins its actual TypeScript configuration in `tsconfig.json` and its JSX transform in `packages/solid-native/vite.config.ts`. These are the first concrete TSX compatibility fixtures; they are not generic defaults for every Ogma application.

| Setting | Pinned consumer value |
|---|---|
| TypeScript `target` | `ES2020` |
| `module` / `moduleResolution` | `ESNext` / `Bundler` |
| Type-check mode | `strict: true`, `noEmit: true` |
| JSX | `jsx: preserve`, `jsxImportSource: solid-native` |
| Type declarations | `vite/client` and `node` |
| Module path aliases | `solid-native` → `packages/solid-native/src/index.ts`; `solid-native/jsx-runtime` → `packages/solid-native/src/jsx-runtime.ts` |
| Vite build target | `es2020`, IIFE library output |
| Solid JSX transform | `vite-plugin-solid` with `moduleName: solid-native` and `generate: universal` |

TypeScript's `noEmit` means the consumer compiler performs diagnostics/type checking without emitting JavaScript or lowering JSX. The Vite/Solid plugin performs the JSX transform. Ogma's frontend must therefore validate TS/TSX syntax and the configured `solid-native/jsx-runtime` contract; it must not assume that TypeScript's JSX emitter is the consumer's runtime transform. The consumer's `moduleResolution: Bundler` is a build/type-check resolver setting and does not replace Ogma's separate Node package resolver contract. The consumer's ES2020 build target is a bundler output target and does not reduce the normative ECMAScript 2026 semantics baseline of Ogma.

TypeScript emit behaviour is compiler-option-sensitive. Every transform fixture must pin the relevant options (including `target`, `module`, `jsx`, `jsxImportSource`, `experimentalDecorators`, `emitDecoratorMetadata`, `useDefineForClassFields`, `preserveConstEnums` and `isolatedModules` when they affect the tested construct). Do not claim one universal enum/decorator/class-field lowering independent of its compiler options.

### 3.4. TypeScript syntax and transform matrix

All rows are deferred until Ogma implements the frontend and passes fixtures. “Erase” means the syntax has no direct runtime representation; it does not mean Ogma executes TypeScript types.

| TS/TSX construct | Frontend contract | Runtime effect / policy | Current status |
|---|---|---|---|
| Type annotations, interfaces, type aliases, generic declarations/parameters/arguments | Parse and erase | No runtime type object is created | deferred |
| as / angle-bracket assertions, non-null assertion, satisfies, type-only imports/exports | Parse and erase where allowed by the selected TS grammar; angle-bracket type assertions are rejected in TSX mode where they conflict with JSX syntax | No runtime check or cast is introduced | deferred |
| declare, ambient declarations, .d.ts, readonly and access modifiers | Parse for diagnostics/type analysis; erase where appropriate | Declaration/type metadata is compile-time only | deferred |
| Enums, including const enum | Parse and lower according to an explicitly pinned TS-compatible emit mode | Runtime enum objects or inlining must match selected emit options; cross-module const-enum behaviour needs fixtures | deferred |
| Non-ambient namespaces and namespace merging | Parse and lower only the declared supported subset | May create runtime objects and merge bindings; never treat as type-only by default | deferred |
| Parameter properties | Parse and lower to constructor assignments with TS-compatible ordering | Runtime code is emitted | deferred |
| Decorators | Keep TC39 standard decorators and legacy experimentalDecorators as distinct modes | No implicit mixing of decorator semantics; each mode requires independent fixtures | deferred |
| emitDecoratorMetadata / reflection metadata | Not enabled by default | Requires a separate metadata contract and explicit runtime library; TS type metadata is not implicitly injected | unsupported |
| JSX / TSX | Parse and lower using explicit jsx, jsxImportSource and runtime configuration | JSX runtime is a frontend/host package dependency, not an ECMAScript intrinsic | deferred |
| import = require, export = and other module-specific TS constructs | Lower only when the selected ESM/CJS mode defines the resulting module contract | Must not bypass Node resolver or CJS adapter rules | deferred |
| Type checker, inference, narrowing, overloads, mapped/conditional types, declaration merging | Separate compile-time subsystem or external checker integration | Never executed by Runtime; no claim of full TypeScript checker compatibility | deferred |

TypeScript source maps and source spans must remain associated with lowered code for diagnostics/debugging. Any TS transform that emits runtime operations must preserve evaluation order, this binding, lexical scope, exception behaviour and observable property access.

### 3.5. Consumer-relevant TypeScript/JSX workload

The first pinned consumer is [AiratGaliev/solidjs-native](https://github.com/AiratGaliev/solidjs-native), repository revision bcd0fa119c5a0b0fc762462bd5ddc4b2818c7668 (main at baseline capture):

| Package / component | Pinned observed version | Compatibility role |
|---|---:|---|
| solid-js | 1.9.18 | Primary JS runtime/reactivity workload |
| local solid-native package | 0.1.0 | Consumer adapter; exports package root and ./jsx-runtime |
| typescript | 7.0.2 | TS compiler/reference and build-time type checking |
| vite | 8.3.7 | Build-time tooling only |
| vite-plugin-solid | 2.11.15 | Build-time JSX transform only |
| @types/node | 26.6.6 | Compile-time declarations only; does not imply Node runtime API use |
| pnpm | 12.18.0 | External package manager; not shipped in Ogma Runtime |
| pnpm-lock.yaml | Git blob SHA 9c35ab8401a96ef04d9b1e88cab4a0e5a031b034 | Exact dependency graph snapshot at the pinned SolidJS Native revision |
| rquickjs (Rust workspace) | 0.14.0 | Existing consumer-side reference integration; not part of Ogma's target architecture |

The first SolidJS workload should cover TSX/JSX parsing/lowering under the pinned `jsx: preserve` / `jsxImportSource: solid-native` contract, the `solid-native/jsx-runtime` module, the Vite Solid plugin's `moduleName: solid-native` / `generate: universal` transform, reactive computations/effects and component functions. The current demo imports `createSignal`, `render` and the local `View`/`Text`/`Button` components; it is bundled as an IIFE before Rust runtime startup. A later workload may additionally test module imports and calls across the Rust host boundary. The pinned package manifest exposes `pnpm check` (typecheck, build and Rust workspace tests) and `pnpm smoke` (build plus native runtime launch) as candidate consumer commands; they must be rerun against the Ogma adapter when that integration exists. They were not run for this documentation task. Vite, its plugin, TypeScript CLI and pnpm are development/build tools; their use of Node.js does not make them runtime dependencies of a packaged native application.

[AiratGaliev/codogma-engine](https://github.com/AiratGaliev/codogma-engine) revision 982d25c26da94c1f73d61457bf55da682413eff2 is a secondary future integration workload identified by the architecture. Its current root package is a Node-based editor/tooling application; it is not evidence that an Ogma-native consumer or a particular Node API is already supported. Promote it into the executable consumer corpus only after a concrete Ogma integration path and exact package/lockfile revision are selected.

## 4. Node.js Compatibility Profile

### 4.1. Reference Node.js line

| Item | Baseline |
|---|---|
| Node.js release line | **26.x — Active LTS (Lithium)** |
| Exact reference release | **26.13.0**, released 2026-10-28 |
| Pinned source commit | 18b32a3b1552e199a01f1a8d9fd64aa756d3b75b |
| Versioned API documentation | [Node.js v26.13.0 API docs](https://nodejs.org/docs/v26.13.0/api/) |
| Release policy source | [Node.js Release Working Group schedule](https://github.com/nodejs/Release) |
| Reference test source | Node.js repository at the pinned release commit; selected test/parallel suites only |

Node.js 26 was the Active LTS line at baseline capture. Pinning 26.13.0 makes documentation and test results reproducible; it does not require shipping Node, V8, libuv or a Node executable. New Node 26 patch/minor releases are not adopted automatically. A profile update reviews API changes and re-runs the selected compatibility corpus.

### 4.2. Tier semantics

- **Node Base** — portable/runtime-compatible APIs with no implicit access to arbitrary OS resources. A small number of facades (console/process/timers) still require narrowly scoped host capabilities.
- **Node Host** — optional adapters backed by explicit Rust host services and capability grants. No module name grants permission by itself.
- **Node Workers** — optional isolated execution profile; not included in the default embedded profile.
- **Tooling-only / not Runtime** — developer/build/test/debug utilities. These may use Node on the developer machine but are not part of the packaged application runtime.
- **Not in baseline** — deliberately excluded Node/V8/process platform features.

The tables below define the candidate v1 API surface, not all exports from each Node module. Each API starts as deferred; only the named sub-surface may later become supported/partial. Unlisted exports remain unsupported until added explicitly.

Node stability is attached to individual public symbols, not uniformly to an entire module. Profile 1.1.0 admits only Node 26.13.0 symbols documented as **Stability 2 — Stable** into a future supported subset by default. Experimental, deprecated and legacy symbols stay unsupported/deferred unless a later profile explicitly adds them with rationale. Two narrowly scoped Legacy exceptions are retained because the accepted architecture explicitly requires Node scheduling compatibility: `process.nextTick()` (Node documents `queueMicrotask()` as the preferred alternative) and `clearImmediate()` (the cancellation counterpart for `setImmediate()`). They are retained only for Node-compatibility behaviour, remain deferred until their pinned fixtures pass, and are not recommended for new Ogma-only application code. Before any symbol is promoted to supported/partial, expand the relevant row to record the exact export, its versioned Node documentation stability label, observable behaviour/edge cases, required capability, supported target tuples and selected reference test files. A module-wide wildcard such as `test-buffer*.js` is a discovery family, not sufficient acceptance evidence by itself.

### 4.3. Node Base API matrix

| Module / global | Candidate v1 surface | Required host capability | Target platforms | Pinned Node reference test family | Current status |
|---|---|---|---|---|---|
| node:assert, node:assert/strict | Assertion functions and documented strict-mode behaviour used by consumer tests | none | Windows, Linux, macOS; x86_64 and ARM64 | test/parallel/test-assert*.js | deferred |
| node:buffer | Buffer construction, encoding/decoding, comparison, slicing/copying and ArrayBuffer/TypedArray interop | none; memory quotas remain Runtime policy | Windows, Linux, macOS; x86_64 and ARM64 | test/parallel/test-buffer*.js | deferred |
| node:events | EventEmitter core: on/once/off, emit, listener removal/order and error event behaviour | none | Windows, Linux, macOS; x86_64 and ARM64 | test/parallel/test-event-emitter*.js | deferred |
| node:util | M2: formatWithOptions(), inspect() and deprecate() only (debug@4.4.3; default warn-once path); M4: other explicitly selected exports, potentially promisify()/callbackify() | process.stderr.write for selected deprecation warnings; no arbitrary OS access | Windows, Linux, macOS; x86_64 and ARM64 | selected test/parallel/test-util*.js cases plus OGM-101 package fixture | deferred |
| node:path | posix and win32 string semantics: normalize, join, resolve, relative, parse/format and separators | none | Windows, Linux, macOS; both variants tested on every target | test/parallel/test-path*.js | deferred |
| node:url | URL/URLSearchParams and file URL conversion helpers in the declared subset | none; file conversion does not grant file access | Windows, Linux, macOS; x86_64 and ARM64 | test/parallel/test-url*.js | deferred |
| node:module | createRequire() bound to an explicit parent URL/path, isBuiltin() and read-only builtinModules in the declared resolver profile | none; resolution remains constrained to the selected package graph and builtin registry | Windows, Linux, macOS; x86_64 and ARM64 | test/parallel/test-module-create-require*.js, test/parallel/test-module-isBuiltin.js, test/parallel/test-module-builtin.js | deferred |
| node:tty | M2: isatty(fd) only, backed by host-provided terminal metadata; no TTY streams, raw mode or terminal-control APIs | terminal.isatty | Windows, Linux, macOS; x86_64 and ARM64 | test/parallel/test-tty-isatty.js and OGM-101 package fixture | deferred |
| node:querystring | parse/stringify and documented escaping semantics | none | Windows, Linux, macOS; x86_64 and ARM64 | test/parallel/test-querystring*.js | deferred |
| node:string_decoder | StringDecoder and encoding-boundary behaviour | none | Windows, Linux, macOS; x86_64 and ARM64 | test/parallel/test-string-decoder*.js | deferred |
| node:stream | Readable, Writable, Duplex, Transform, PassThrough and selected pipeline/backpressure contracts | none for in-memory streams; external I/O requires a Host adapter | Windows, Linux, macOS; x86_64 and ARM64 | test/parallel/test-stream*.js, test/parallel/test-readable*.js | deferred |
| node:timers and timer globals | setTimeout/setInterval/setImmediate with clearTimeout/clearInterval/clearImmediate cancellation and observable callback ordering | scheduler.timers | Windows, Linux, macOS; x86_64 and ARM64 | test/parallel/test-timers*.js, test/parallel/test-timers-clearImmediate.js, test/parallel/test-microtask-queue-run-immediate.js | deferred |
| console global | log/info/warn/error and selected formatting behaviour | console.write | Windows, Linux, macOS; x86_64 and ARM64 | test/parallel/test-console*.js | deferred |
| process facade | M2: Runtime/Realm-scoped allowlisted process.env string map (DEBUG/DEBUG_* only; local write/delete) and logical stderr fd=2/write; M4: other selected metadata/cwd/nextTick surface | process.env.read, process.stderr.write; process.metadata/process.cwd.read/scheduler.next_tick remain separately gated | Windows, Linux, macOS; x86_64 and ARM64 | selected test-process cases, test/parallel/test-process-next-tick.js and OGM-101 | deferred |

### 4.4. Node Host API matrix

All Host modules require an explicit capability grant and a Rust host adapter. The target set is Windows, Linux and macOS on x86_64 and ARM64; actual availability is the intersection of the target build and the host's registered capability manifest. A module is not advertised on a target merely because its JavaScript facade compiles.

| Module | Candidate v1 surface | Required host capability | Target platforms | Pinned Node reference test family | Current status |
|---|---|---|---|---|---|
| node:fs / node:fs/promises | Explicitly rooted VFS operations: read/write, stat, readdir, mkdir, rename and remove; sync APIs excluded unless a host opts into blocking calls | fs.read, fs.write, fs.metadata with root/path policy | Windows, Linux, macOS; x86_64 and ARM64, subject to VFS adapter | selected test/parallel/test-fs*.js and fs/promises cases | deferred |
| node:os | Read-only platform/arch/temp/home/CPU/memory metadata in the granted subset | os.metadata; sensitive paths may require narrower grants | Windows, Linux, macOS; x86_64 and ARM64 | selected test/parallel/test-os*.js cases | deferred |
| node:crypto | Selected random, digest, HMAC and key operations backed by a maintained provider; algorithm availability is explicit | crypto.random, crypto.digest, crypto.key_ops | Windows, Linux, macOS; x86_64 and ARM64, subject to provider | selected test/parallel/test-crypto*.js cases | deferred |
| node:perf_hooks | Monotonic performance clock and selected mark/measure APIs | clock.monotonic | Windows, Linux, macOS; x86_64 and ARM64 | selected test/parallel/test-perf-hooks*.js cases | deferred |
| node:http | Explicitly selected HTTP client and, if separately granted, server APIs | network.http.client; optional network.http.server | Windows, Linux, macOS; x86_64 and ARM64 | selected test/parallel/test-http*.js cases | deferred |
| node:https | HTTPS client/server subset with TLS verification and policy controlled by host | network.http.client + network.tls; optional server capability | Windows, Linux, macOS; x86_64 and ARM64, subject to TLS provider | selected test/parallel/test-https*.js cases | deferred |
| node:net | TCP client/server subset, sockets, errors and close lifecycle | network.tcp.client; optional network.tcp.listen | Windows, Linux, macOS; x86_64 and ARM64 | selected test/parallel/test-net*.js cases | deferred |
| node:tls | TLS client/server subset and certificate validation | network.tls.client; optional network.tls.listen | Windows, Linux, macOS; x86_64 and ARM64, subject to TLS provider | selected test/parallel/test-tls*.js cases | deferred |
| node:dns | Selected lookup/resolve operations with bounded results and explicit resolver policy | network.dns | Windows, Linux, macOS; x86_64 and ARM64 | selected test/parallel/test-dns*.js cases | deferred |
| node:dgram | UDP socket subset, bind/send/receive/close and lifecycle | network.udp | Windows, Linux, macOS; x86_64 and ARM64 | selected test/parallel/test-dgram*.js cases | deferred |

The Host layer must not implement cryptography itself; it uses vetted Rust/OS providers. Filesystem APIs must be mediated by a host-provided filesystem/VFS with explicit roots and path permissions. Synchronous filesystem APIs, wherever enabled, must declare blocking behaviour. Networking APIs require explicit endpoint/bind policy, quotas, timeouts and cancellation.

**Native Provider integration:** these Node Host APIs consume logical Native Services through the shared Host Module/Host API boundary. Host Module metadata uses logical Native requirement IDs such as `native.crypto.digest`; the `crypto.digest`/`crypto.random` identifiers in this profile's capability table remain permission grants and are a separate namespace. The provider implementation may be a trusted Rust crate already present in the host application's Cargo dependency graph or a service supplied directly by the Rust host. Node facades do not own or resolve Cargo dependencies, and npm packages cannot install/select Rust crates by importing `node:*`. Provider identity, ABI/target selection, dependency closure and Cargo provenance follow [Native Provider Architecture](native-provider-architecture.md); the actual provider registry is a planned M3 capability, not current Runtime functionality.

### 4.5. Node Workers profile

| Module / API | Candidate surface | Required capability | Target platforms | Reference tests | Current status |
|---|---|---|---|---|---|
| node:worker_threads | Independent Isolate lifecycle, worker startup/shutdown/errors, postMessage, MessagePort/MessageChannel, structured clone and transferables | workers.spawn, workers.message | Windows, Linux, macOS; x86_64 and ARM64, subject to thread/host adapter | selected test/parallel/test-worker*.js cases | deferred |
| Shared memory | SharedArrayBuffer/Atomics only after memory model and safety proof | workers.shared_memory | Same target set; target-specific atomics support must be verified | Test262 shared-memory tests plus selected Node worker tests | deferred |

Workers are optional and are not part of the default Node Base profile. Ogma's thread-affine Runtime does not imply that JavaScript objects can be shared across threads. Worker support requires independent Isolates, structured-clone/transfer semantics, bounded queues, GC/root lifecycle, cancellation and termination guarantees.

### 4.6. Tooling-only and excluded APIs

| Classification | APIs / facilities | Policy |
|---|---|---|
| Tooling-only / not Runtime | Node CLI flags, Node REPL, Node test runner, Node Inspector transport, npm/pnpm/yarn/Corepack commands, install scripts and build-tool process orchestration | Use Ogma CLI/toolchain, Debugger API/DAP and external package managers. These may run on the developer machine but are not bundled into a native app. |
| Not in baseline | node:v8, V8 APIs, NAN and V8-specific addons | Unsupported: they depend on V8 internals and conflict with the independent Rust Engine. |
| Not in baseline | node:child_process, node:cluster, unrestricted process spawn/termination, signals, credentials and privilege changes | Unsupported in the embedded application profile; process lifecycle and privileged OS actions remain with the Rust host. |
| Not in baseline | Arbitrary prebuilt .node binaries and Node-API addons | No automatic ABI compatibility. A specific Node-API adapter would require its own ABI implementation, version matrix, platform tests and separate decision. |
| Not in baseline | node:inspector, node:test, Node CLI internals | Tooling is replaced by Ogma-owned tooling; these are not Runtime APIs. |

This exclusion does not prohibit a Rust application from implementing a narrow host service with similar functionality. Such a service is a separate capability/adapter and must not be presented as general Node.js compatibility.

### 4.7. Capability and platform contract

The host owns permissions and resources. Importing a module never grants access.

| Capability identifier | Grants | Typical consumers |
|---|---|---|
| console.write | Write structured console output to a host sink | console |
| process.metadata | Read limited Runtime/process metadata | process facade |
| process.env.read | Seed a Runtime/Realm-scoped map from explicitly allowlisted environment values; later JS writes/deletes stay inside that map | process.env |
| process.stderr.write | Write text to an explicit host-provided stderr/logging sink; the exposed fd=2 is logical, not an OS handle | process.stderr |
| terminal.isatty | Read whether a declared logical stream is connected to a terminal | node:tty.isatty |
| process.cwd.read | Read the configured application working directory | process.cwd() |
| scheduler.next_tick | Enqueue Node-style nextTick callbacks | process.nextTick |
| scheduler.timers | Schedule/cancel host-driven timers | node:timers |
| fs.read, fs.write, fs.metadata | Access explicitly rooted filesystem/VFS paths | node:fs |
| os.metadata | Read approved OS metadata | node:os |
| crypto.random, crypto.digest, crypto.key_ops | Use the configured cryptographic provider | node:crypto |
| clock.monotonic | Read monotonic time | node:perf_hooks |
| network.http.client, network.http.server | Make/listen for HTTP traffic under host policy | node:http, node:https |
| network.tcp.client, network.tcp.listen | Create/connect/listen on TCP sockets | node:net, node:tls |
| network.tls.client, network.tls.listen | Establish TLS connections/listeners under certificate policy | node:https, node:tls |
| network.dns, network.udp | Resolve names and use UDP | node:dns, node:dgram |
| workers.spawn, workers.message, workers.shared_memory | Create Isolates and use explicitly supported worker communication | Node Workers profile |

The declared platform target is Windows, Linux and macOS, including x86_64 and ARM64 configurations. The baseline Rust target triples are:

| OS | x86_64 | ARM64 |
|---|---|---|
| Windows | `x86_64-pc-windows-msvc` | `aarch64-pc-windows-msvc` |
| Linux | `x86_64-unknown-linux-gnu` | `aarch64-unknown-linux-gnu` |
| macOS | `x86_64-apple-darwin` | `aarch64-apple-darwin` |

These are target-build tuples, not a claim that every API already runs on all six targets. Linux glibc minimum and supported distribution floor remain deferred until the Rust/toolchain deployment baseline is selected; musl, Android, iOS, WebAssembly and other targets are outside profile 1.1.0. A capability is available only if its adapter is compiled, registered and tested for the exact tuple.

Capability denial, unavailable host adapters, invalid arguments and unsupported APIs must produce deterministic errors. They must not silently return empty values, access ambient process state or fall back to unrestricted OS access. Quotas, cancellation and audit policy apply to Node facades and direct Rust host calls alike.

## 5. ESM, CommonJS and package resolution contract

### 5.1. Ownership boundary

ECMAScript module records, linking, instantiation and evaluation belong to the language/runtime layer. Node package rules and CommonJS compatibility belong to the optional Node.js Compatibility Layer. Node-specific resolution rules must not leak into ECMAScript Core.

### 5.2. ESM/CJS scope

The target Node 26.13.0 compatibility surface includes:

- ESM static import/export, module graph construction, live bindings, cycles, linking/instantiation/evaluation and module identity.
- Dynamic import() and top-level await only when the selected ECMAScript/module feature profile supports them.
- CommonJS wrapper scope with module, exports, require, __filename and __dirname; preserve module.exports identity, cache and cycle behaviour.
- ESM ↔ CJS interop for explicitly tested import/require forms. require(esm) and less common interop edge cases remain deferred until Node 26.13.0 behaviour is pinned by fixtures.
- Built-in modules are registered under explicit node: specifiers. Bare aliases are exposed only where the pinned Node profile defines them.
- Planned CJS/module-reference selectors are taken from the pinned Node source: `test/parallel/test-module-*.js`, `test/parallel/test-require-*.js`, and `test/es-module/test-require-*` for ESM/CommonJS interop and cycle coverage.
- Planned ESM reference fixtures are taken from `test/es-module` in the same pinned Node source revision, including test-esm-basic-imports.mjs, test-esm-live-binding.mjs, test-esm-imports.mjs, test-esm-exports.mjs, test-esm-cjs-exports.js, test-esm-cjs-builtins.js, test-esm-package-map-basic.mjs, test-esm-package-map-exports.mjs, test-esm-package-map-boundaries.mjs and test-esm-package-map-errors.mjs.

### 5.3. Package manifest and resolver rules

The initial resolver contract covers the following fields and rules, subject to tests:

- package.json: type, main, exports, imports.
- Package self-references, subpath exports/imports and package encapsulation.
- ESM and CommonJS entry points, extension rules and module cache/identity.
- Conditional exports conditions are enabled only when their semantics are valid for the selected profile: import/require reflect the active module mode; `default` matches unconditionally when reached in package key order; ogma, development and production require explicit profile/build configuration. Preserve the package's conditional object key order exactly; Node resolves the first matching condition in that order. Package authors should place `default` last, and the resolver must not reorder it to prefer a later key.
- The node condition is enabled only in a build that explicitly selects the Node Compatibility Profile and registers the Node API adapters in that build's capability/module manifest. It is not a blanket marker for any Ogma Runtime and does not claim universal Node API coverage; an API outside the selected profile still fails deterministically when imported or invoked. node-addons is never enabled in profile 1.1.0 because native Node addons are excluded.
- browser is not an implicit condition for a native application. A host may opt into it only in a separate explicit profile.

The resolver must distinguish an unrecognized condition in a package's conditional map (which is simply not an enabled match) from an invalid condition supplied in Ogma's own profile configuration (which is a deterministic configuration error). Conditional-object keys must be evaluated in their original package.json insertion order: `default` is an unconditional match when reached, not a resolver-priority rule that may be moved ahead of earlier keys. Package authors should place `default` last; Ogma must not reorder condition keys. If no enabled branch/default matches, or a package's `exports` boundary forbids the requested subpath, resolution fails with a stable package-resolution diagnostic. It must not silently fall back to `main`, an arbitrary file or an unexported path after an `exports` map has been applied, and must not bypass package encapsulation.

Unknown node:* built-ins, unsupported package entry points, denied host capabilities and unsupported module formats fail deterministically. Error identity/message mapping to Node error codes is part of the tested compatibility surface, not an excuse to silently resolve another path.

The initial package-manager baseline is **pnpm 12.18.0** with **pnpm-lock.yaml lockfile format 9.0**, matching the pinned SolidJS Native consumer (this is a project pin, not a claim that 12.18.0 is the latest pnpm). In M2, an isolated fixture pins packageManager to pnpm@12.18.0, uses .npmrc node-linker=isolated and stages the exact dependency tree with `pnpm install --frozen-lockfile --ignore-scripts`. M2 validates the materialized local node_modules tree, including the node_modules/.pnpm virtual store and symlink/nested-dependency topology; it does not parse pnpm-lock.yaml. M3 OGM-94 owns lockfile v9 ingestion, canonical dependency-graph construction and final application packaging. npm/yarn lockfile adapters are not in the initial baseline and require a separately reviewed profile change. pnpm, Node, Corepack, registry/network access and package lifecycle scripts remain build-time only; packaged Runtime startup must not require them or a source node_modules tree. Package lifecycle/install scripts are untrusted build-time code and are never run automatically at application startup. JavaScript package dependencies remain separate from Host Module/Native Provider requirements: a Node builtin import may add a logical native-service requirement to the host build plan, but it never adds a Cargo crate to the JavaScript package graph. Cargo resolves Rust dependencies from the host application's Cargo manifest/lockfile. The CLI ownership and delegation rules are defined in [Toolchain CLI Contract](toolchain-cli-contract.md).

### 5.4. Resolver precedence, diagnostics and module identity

The Node package resolver is a compatibility-policy adapter over the generic ModuleLoader contract. It is not part of ECMAScript Core and must not make the shared Runtime depend on Node.js concepts.

OGM-48 implements the lower-level generic boundary as ModuleResolver plus ResolvedModule/ModuleDescriptor in ogma-embedding/ogma-bytecode. The host maps the original static specifier and referrer ID to a canonical module ID, then supplies verified artifacts and static linking metadata. This M1 resolver does not implement the package.json rules in this section; the Node package adapter remains deferred. See [Module Graph Contract](module-graph-contract.md).

A resolution request is defined by the specifier, referrer/package scope, active mode (`import` or `require`), explicitly enabled compatibility conditions and the host-provided package/module manifest. The host supplies package graph and filesystem/VFS access through an explicit capability. The resolver must not acquire ambient filesystem or network access. Its result identifies a canonical module resource and format (ESM, CommonJS or registered host module), or returns a structured diagnostic. Generic ModuleLoader primitives own module records, graph linking, instantiation, evaluation and language-level module identity.

The initial resolution order and boundaries are:

1. A `node:` specifier is resolved only against the built-in module registry of the selected Node profile. An unregistered built-in fails; it is never retried as a package or file path.
2. A package `#imports` specifier is resolved only through the `imports` map of the owning package scope. A missing mapping fails with `ERR_PACKAGE_IMPORT_NOT_DEFINED`.
3. A package self-reference is resolved by matching the nearest owning package's declared name and then applying that package's `exports` map. Self-reference does not bypass package encapsulation.
4. A bare package specifier is located through the host-supplied package graph. Package discovery is a host responsibility; the resolver does not walk arbitrary process directories outside the granted package graph.
5. For a located package, `exports` governs the package root and requested subpaths. Only when `exports` is absent may the legacy `main` field select the package-root entry. An existing `exports` map is authoritative: failure to match it must not fall back to `main`, an unexported file or an arbitrary path.
6. `.mjs` and `.cjs` explicitly identify ESM and CommonJS. For `.js`, the nearest package's `type` selects the format. The proof corpus pins these cases; broader extension and directory probing is not implied by this initial subset.

Conditional maps are evaluated in their original JSON insertion order. The first enabled key wins; `default` is unconditional only when reached in that order and is not promoted ahead of earlier keys. The profile rules are:

- `import` and `require` are enabled by the active resolution mode.
- `node` is enabled only in a build that explicitly selects the Node Compatibility Profile and registers the corresponding Node adapters. A plain Ogma Runtime must not enable it implicitly.
- `ogma`, `development` and `production` are enabled only by explicit profile/build configuration.
- `browser` is not implicit for a native application. `node-addons` is disabled in profile 1.1.0.
- An unknown key in a package-authored conditional map is an unavailable branch and is skipped. An invalid condition in Ogma's own profile configuration is a configuration error (Ogma diagnostic `ERR_OGMA_INVALID_CONDITION`), not a reason to silently enable a branch.

When no branch matches, or an `exports` boundary denies a requested subpath, resolution fails with `ERR_PACKAGE_PATH_NOT_EXPORTED`. Missing `imports` mappings fail with `ERR_PACKAGE_IMPORT_NOT_DEFINED`; unknown `node:` built-ins fail with `ERR_UNKNOWN_BUILTIN_MODULE`. Other missing-module and unsupported-format diagnostics must be mapped to the pinned Node baseline for the active ESM/CJS mode before they are claimed as compatible. Diagnostics have a stable code and normalized specifier/referrer/reason fields. Human-readable messages may include platform paths and are not the cross-platform identity contract.

Module identity and cache ownership are separated by format:

- The generic ModuleLoader owns ESM module records and their identity within a loader/realm. Repeated imports of the same canonical module resource reuse the record; graph cycles link once and preserve live-binding observations.
- The CommonJS adapter owns a cache entry keyed by the canonical resolved filename within the loader/realm. It installs the in-progress module record before evaluation, so a cycle can observe partially initialized exports; repeated `require()` returns the same `module.exports` identity.
- This proof does not claim untested cross-format identity equivalence for one file imported through both ESM and CommonJS.

#### M0 reference proof

The black-box reference corpus lives in `tests/node-resolution/`. It does not implement an Ogma resolver and does not promote any deferred runtime feature to supported. It records observable Node.js behaviour for the minimum package-resolution cases in this issue.

The reference is pinned to Node.js **26.13.0**, source commit `18b32a3b1552e199a01f1a8d9fd64aa756d3b75b`. `.node-version` pins the executable used by CI; `tests/node-resolution/baseline.json` pins the source revision, expected resolution observations, diagnostic codes and reference test selectors. Run the corpus with:

```sh
node tests/node-resolution/run.mjs
```

The fixtures cover package `type` and legacy `main`, ESM/CJS package entry points, `node`/mode/default/selected Ogma conditions, insertion-order precedence, `imports`, self-reference, explicit subpaths, exports encapsulation, `node:` built-ins, unsupported branches, ESM/CJS cycles and cache identity. The selected upstream Node test selectors are recorded for later expansion; this focused M0 corpus does not claim to run the full upstream Node test suite or the complete package-resolution surface (for example wildcard/pattern exports, array targets, directory probing, custom loaders, native addons or `require(esm)`).

The profile's runtime compatibility states remain **deferred** until an Ogma Node resolver/adapter exists and passes its own differential and regression tests.

## 6. Scheduling and async semantics

The normative host-pump and Node scheduler contract is [Host Pump and Node-Compatible Scheduler Contract](host-pump-contract.md), contract version 1.0.0. It defines queue ownership, context-specific callback checkpoints, host pumping, fairness, pause/resume, shutdown, cancellation, error handling and the explicit limits of the observable compatibility claim.

The compatibility boundaries are:

- ECMAScript PromiseJobs and queueMicrotask belong to the shared Ogma Runtime core.
- Node process.nextTick uses a separate FIFO queue in the optional Node compatibility adapter. Checkpoint priority is context-sensitive: CommonJS entry execution and ESM module evaluation/top-level-await must not be collapsed into one global ordering rule.
- setTimeout, setInterval, setImmediate and their cancellation APIs belong to the Node scheduler adapter, using host-provided monotonic time and wake-ups.
- Async I/O and Rust executor completions are delivered as owned host events to the Runtime owner thread. Completion threads never execute JavaScript, and Rust Future/Waker internals are not exposed through the public ABI.
- The host owns pump/poll/run-until-idle, pause, resume, shutdown and the executor/event loop. Runtime work is non-blocking and thread-affine; no hidden second event loop is created.
- The ECMAScript core does not depend on Node-specific scheduling APIs. The Node adapter is layered above generic Runtime job and host-callback contracts.
- Tokio and libuv are not mandatory Runtime dependencies. A host may provide an executor or I/O adapter without changing the observable Promise and declared Node scheduling contract.

The pinned black-box reference corpus is [tests/node-scheduling](../tests/node-scheduling/README.md). It records Node.js 26.13.0 callback traces for CommonJS and ESM/top-level-await checkpoints, nested nextTick/Promise scheduling, I/O callback ordering with timers and immediates, timer/immediate cancellation, interval cancellation and error reporting.

These are M0 reference observations and an implementation contract, not an Ogma conformance result. Scheduling API states remain deferred until an Ogma Runtime and Node adapter execute equivalent fixtures and pass differential checks. The small reference corpus does not replace the full Node.js test suite or the promotion-only conformance work owned by OGM-35.

## 7. Interpreter, AOT and deterministic fallback

1. Interpreter and Native AOT implement the same ECMAScript semantics and use the same Runtime, host ABI, capability checks and Node compatibility adapters.
2. Build-time-known code may use Native AOT when the backend can preserve the required semantics.
3. Dynamic source (eval, new Function, runtime-loaded source) is compiled at runtime and defaults to portable bytecode/interpreter execution.
4. If a function cannot safely or profitably be lowered to AOT, the compiler may use interpreter/bytecode fallback only when the Runtime implements that feature.
5. AOT limitations must not silently disable or alter dynamic JavaScript semantics. Local eval/lexical-environment behaviour must be preserved; materialize required bindings or use interpreter fallback.
6. If a language feature or Node API is outside the declared profile, both interpreter and AOT paths reject it with deterministic diagnostics. AOT fallback is not a way to claim support for an unimplemented runtime feature.
7. TypeScript annotations are never proof of runtime representation, memory safety or absence of aliasing. Any specialized AOT path needs guards/proofs and a semantics-preserving generic fallback.
8. Node host calls in AOT use the versioned Ogma ABI/function-table contract, not Rust's default ABI, V8 symbols or libuv.

## 8. Initial consumer and package corpus

The corpus is versioned independently from the Node API matrix. Every executable consumer entry must record repository commit, lockfile hash, package name/version, target OS/architecture, build profile, required capabilities, exact test command and expected observable behaviour. Future reference repositories that are not yet executable Ogma consumers are listed separately and do not imply a completed package corpus entry.

| Consumer | Pinned repository revision | Initial workload | Scope classification |
|---|---|---|---|
| SolidJS Native | AiratGaliev/solidjs-native@bcd0fa119c5a0b0fc762462bd5ddc4b2818c7668 | solid-js@1.9.18, local solid-native@0.1.0, TSX/JSX runtime, reactivity/effects, custom renderer and future Rust-host bridge | Primary planned runtime consumer; current smoke is still backed by rquickjs, not Ogma |
| Codogma Engine | AiratGaliev/codogma-engine@982d25c26da94c1f73d61457bf55da682413eff2 | Future native application/editor/game-loop integration and host callbacks | Secondary reference repository only; not yet an executable Ogma consumer and therefore has no compatibility result or consumer-lockfile pin in profile 1.1.0 |
| cge-experiments — Brick Breaker | AiratGaliev/cge-experiments@91dfa48300f6ba3f0bc4f2a9db9b7ac298dc508c (`main`) | Existing TypeScript Brick Breaker game hosted by a native Rust game loop; QuickJS-NG reference backend | M2 interpreter-only replacement/performance pilot; no Ogma execution or compatibility result yet. M4 OGM-78 remains the full Rust embedding/AOT consumer acceptance |

### M2 interpreter pilot — cge-experiments Brick Breaker

This is a pinned experimental workload for evaluating whether Ogma's portable bytecode/interpreter can replace the QuickJS-NG backend in a small existing native game. It is **not** a supported-consumer claim, Test262 result, release acceptance result or evidence that an Ogma runtime is already implemented.

The baseline was captured from the current `main` after all changes previously developed on the repository's `dev` branch had been merged:

| Item | Frozen baseline |
|---|---|
| Repository revision | [AiratGaliev/cge-experiments@91dfa48300f6ba3f0bc4f2a9db9b7ac298dc508c](https://github.com/AiratGaliev/cge-experiments/tree/91dfa48300f6ba3f0bc4f2a9db9b7ac298dc508c) |
| Application | `rust-brick-breaker/` |
| TypeScript game source | `scripts/game.ts`, blob `404916d5416077528920d9dbbf5a4769f5c8a502` |
| Generated JavaScript | `assets/game.js`, blob `4fddc001d9ff946280309d72e38c0c72a0f57f31` |
| Rust dependency lock | `Cargo.lock`, blob `9b262808a6dd59137afc47649dea6ac121d8029e` |
| JavaScript build/test lock | `package-lock.json`, blob `5fb65ece1ba976cd649e711b8485baa1cec69f40` |
| Existing Criterion harness | `benches/runtime.rs`, blob `56230eb9be0f0a51173d19f0690cd1dc90d3c5a9` |
| Existing benchmark methodology | `BENCHMARKS.md`, blob `e1d840b56befe6b78c491b85dab2cb3ffa94c258` |
| Reference runtime | `rquickjs 0.14.0`, QuickJS-NG commit `0fdea21ff1090084e91dad812b343d92e79ba9d9` |
| Consumer build/test toolchain | Rust 1.97.0, Node.js 22.x, TypeScript 5.9.3; these are consumer prerequisites, not Ogma's normative TypeScript 7.0.2 / Node.js 26.13.0 references |

The package-lock is retained solely as part of the pre-existing cge-experiments toolchain baseline. It does not select npm as Ogma's package manager or imply npm lockfile support; the separate M2 package/API micro-corpus uses the pinned pnpm baseline in OGM-101.

The M2 experiment must preserve the current game design: gameplay state, collision reactions, score, lives, victory/game-over and restart stay in the TypeScript game. Rust continues to provide the native host services. QuickJS-NG remains a selectable reference backend until parity and performance evidence has been reviewed.

For a fair runtime comparison, both backends execute the **identical generated `assets/game.js` bytes** through the same host API and deterministic input/physics observations. Independently, the original pinned `scripts/game.ts` must pass Ogma's TypeScript frontend and `ogma compile` path. This separates runtime performance from differences in TypeScript emit.

The report must cover gameplay parity; runtime/context startup and script load/initialization; no-op lifecycle and real game update latency; host-boundary costs; a deterministic CPU-only game tick; and comparable process/runtime memory metrics. Record the same machine and build environment, at least three independent runs, raw machine-readable samples, p50/p95/p99 and Criterion confidence/variability where available. Keep GPU submission, present/vsync and historical pre-Jolt Box2D measurements out of interpreter CPU claims. CPU microbenchmarks are not whole-game FPS.

The expected outcome is an evidence-based Go / Conditional Go / No-Go decision for replacing QuickJS-NG **for this workload**. There is no fabricated pass requirement that Ogma must be faster; an honest No-Go is a valid experiment result. This M2 pilot is interpreter/portable-bytecode only. Native AOT/hybrid execution belongs to M3, and the broader public Rust embedding, async, GC and interpreter/AOT consumer acceptance remains M4 in [OGM-78](https://linear.app/ogma-engine/issue/OGM-78/validate-rust-embedding-contract-and-integrate-the-cge-experiments). The pilot itself is tracked by [OGM-100](https://linear.app/ogma-engine/issue/OGM-100/run-the-cge-experiments-brick-breaker-on-ogma-and-compare-quickjs-ng).

**Evidence state at this profile revision:** baseline and measurement method are pinned; only the QuickJS-NG consumer path and its existing benchmark harness are present. No Ogma execution, parity result or side-by-side Ogma performance result has been produced.

### M2 pnpm-backed package and Node Base API proof

This planned micro-corpus is owned by [OGM-101](https://linear.app/ogma-engine/issue/OGM-101/run-a-pnpm-backed-package-corpus-with-the-initial-node-base-api-slice). It is a narrow consumer-backed check, not a supported npm ecosystem claim. API rows above remain **deferred** until Ogma implements them and passes the pinned tests.

| Item | M2 baseline |
|---|---|
| External package manager | pnpm 12.18.0; pinned to match the SolidJS Native baseline |
| Lockfile | pnpm-lock.yaml, lockfileVersion 9.0; fixture must commit the exact lockfile and integrity values |
| Install layout | .npmrc: node-linker=isolated; local node_modules/.pnpm virtual store plus symlinked package/dependency entries |
| Staging command | pnpm install --frozen-lockfile --ignore-scripts |
| CJS / Node Base package | debug@4.4.3; runtime dependency ms@2.1.3 is resolved and pinned by the fixture lockfile; optional supports-color is intentionally absent in the first test |
| ESM / exports + dependency control | strip-ansi@7.1.0 → ansi-regex@6.2.2; both type=module with ESM default exports, strip-ansi has a string-form exports entry and imports ansi-regex at runtime |
| Node reference | Node.js 26.13.0 and selected reference cases from sections 4.1–4.3 |

Do not substitute p-limit@7.3.3/yocto-queue for this initial ESM control: yocto-queue uses private class fields and generator methods, which are not a reason to silently widen the M2 language contract. Add such a package only after those ECMAScript features are explicitly accepted and tested.

The primary debug@4.4.3 scenario must exercise:

- node:util.formatWithOptions(), inspect() and deprecate() only, with the exact formatting/inspection options and default warn-once behaviour exercised by the package and selected Node reference fixtures; other util exports remain deferred.
- node:tty.isatty(fd) only, returning the Node-compatible result from host-provided terminal metadata; no TTY streams, raw mode or terminal control.
- process.env as a Runtime/Realm-scoped string map seeded from the explicit DEBUG and DEBUG_* allowlist; support key enumeration, reads, string-coercing writes and deletes, without mutating the parent OS environment or exposing ambient secrets.
- process.stderr.fd === 2 as a logical stream id and write(string) to a host-provided sink gated by process.stderr.write; return the Node-style boolean acceptance/backpressure result. It is not an OS file descriptor or filesystem capability.
- Bare util/tty and node:util/node:tty aliases resolving to the same registered builtin identities/cache entries; missing optional supports-color must produce a normal missing-package error that the package catches.

Node process.nextTick(), timers/immediates, broader Base APIs and Node Host I/O are not part of this M2 proof; they remain with OGM-95/96/98 in M4. PromiseJobs are not being claimed by this package set.

**Package-manager boundary:** pnpm runs only in the developer/CI staging environment. OGM-59 resolves the materialized isolated layout, including validated symlinks and nested dependencies, within the host-authorized package graph. M2 does not read or implement pnpm lockfile resolution. M3 OGM-94 imports lockfile v9, validates resolved package metadata and builds the canonical application dependency graph. A packaged app must not need pnpm, Node, Corepack, registry access or the original node_modules tree at startup.
For SolidJS Native, keep these categories separate:

- Runtime packages: solid-js@1.9.18 and the local solid-native@0.1.0 adapter.
- Build/test tools: typescript@7.0.2, vite@8.3.7, vite-plugin-solid@2.11.15, pnpm 12.18.0.
- Type-only declarations: @types/node@26.6.6; declarations do not establish runtime Node API support.
- Existing reference integration: rquickjs@0.14.0 in the consumer's Rust workspace; it is not a dependency Ogma should retain.
- The current demo's `globalThis.__sn_emit` callback is optional and is not registered by its Rust launcher. The smoke currently validates SolidJS rendering/reactivity only; it does not yet prove a Rust host-callback bridge.
- The Vite config imports `node:url` only in build tooling. The bundled application source does not import a Node built-in at runtime; `@types/node` is type-only. Therefore the current SolidJS smoke requires no Node Host API capability from the packaged Runtime.

**Pinned consumer verification contract (future Ogma adapter):**

| Dimension | Baseline |
|---|---|
| Target OS/architecture | The six Rust target triples in section 4.7; each remains unverified until a platform run is recorded |
| Build profile | Locked workspace dependencies; Vite production bundle targeting ES2020; Rust debug/test profile for `cargo test` and `pnpm smoke` |
| Required Runtime host capabilities | None for the current counter/render/reactivity smoke; a future registered `__sn_emit` host sink would be a separate explicitly granted capability |
| Exact commands | `pnpm typecheck`; `pnpm build`; `cargo test --workspace --locked`; `pnpm smoke` |
| Expected observable results | TypeScript accepts the pinned TS/TSX source; Vite emits `dist/solid-native-demo.js`; Rust workspace tests pass; the bundle mounts the View/Text/Button tree, the button callback runs, and the displayed counter changes from 0 to 1 |
| Current evidence state | These commands currently exercise the pinned rquickjs prototype only; none is an Ogma compatibility pass. Re-run them against the Ogma adapter before promoting this consumer entry to supported |

Do not claim universal npm compatibility. A package becomes part of the supported corpus only after its exact version and lockfile are pinned and its install/build/runtime path is exercised. Transitive dependencies must be included in the captured lockfile graph. Native addons, install scripts and platform-specific binaries require separate review and do not become compatible merely because a pure-JS package imports successfully.

### JoltPhysics.js staged consumer probes

This is a separate, staged package consumer entry for **`jolt-physics@1.1.2`** from the official [JoltPhysics.js repository](https://github.com/jrouwe/JoltPhysics.js). It is distinct from the native Rust-host Jolt Physics integration used by cge-experiments and from OGM-23's FrameSnapshot/batch host benchmark. The npm package is ESM, declares MIT licensing, exposes explicit `asm`, single-thread WASM and multithreaded subpaths, and has no runtime npm dependency graph. Therefore it does not replace the `strip-ansi@7.1.0 → ansi-regex@6.2.2` transitive-dependency control in OGM-101.

**Pin/fixture state:** `1.1.2` is the selected package version, but it is **not yet installed in the shared pnpm fixture**. OGM-102 must add this exact version and its npm tarball integrity to the OGM-101 pnpm 12.18.0 / lockfile v9 fixture, retaining `node-linker=isolated`, frozen installation and disabled lifecycle scripts. Until that fixture change is committed, the Jolt package is planned but not staged or runtime-tested; compatibility status remains **deferred**.

| Stage | Tracked task | Package entrypoints | Required proof and boundary |
|---|---|---|---|
| M2 interpreter — planned | [OGM-102](https://linear.app/ogma-engine/issue/OGM-102/probe-jolt-physics-esmasmjs-compatibility-in-m2) | `jolt-physics/asm` only; `exports["./asm"].default` resolves to `./dist/jolt-physics.js` | Verify ESM `type`/`exports`, async initializer and a headless fixed-step one-body simulation plus deterministic teardown when initialization succeeds. Inventory all non-ECMAScript Emscripten globals/hooks and classify missing host capabilities. No WebAssembly, DOM/WebGL, Node/fs/network fallback, Workers or complete Jolt support. |
| M3 packaging — conditional on an accepted single-thread WebAssembly path | [OGM-104](https://linear.app/ogma-engine/issue/OGM-104/validate-jolt-physics-webassembly-loading-and-packaging-in-m3) | `jolt-physics/wasm-compat` embeds WASM bytes as base64 in JS glue (no sidecar); `jolt-physics/wasm` exports JS glue plus `jolt-physics.wasm.wasm` | Inventory the actual `WebAssembly.*` and host-global surface in the generated glue. For `wasm`, verify Emscripten `Module.locateFile` plus an explicit host-backed asset loader/capability: `locateFile` resolves a location but does not load bytes. Verify pnpm package graph, selected-variant asset closure, MIT provenance/SBOM and clean relocated application launch. No ambient browser `fetch`, Node `fs`, Vite `?url`, arbitrary filesystem path or network fallback. Unsupported execution paths are OUT_OF_PROFILE; this task does not mandate a general-purpose WebAssembly engine. |
| M4 multithreading — conditional platform assessment | [OGM-105](https://linear.app/ogma-engine/issue/OGM-105/evaluate-jolt-physics-multithreaded-variants-against-m4-host) | `jolt-physics/wasm-compat-multithread`, `jolt-physics/debug-wasm-compat-multithread`, `jolt-physics/wasm-multithread` | Inspect Emscripten pthread startup and Jolt's `multi-threaded.js` bootstrap, including `Module.configureWorkerScripts(scriptUrl, scriptParams)`, worker/pool readiness and shutdown. Record Worker, SharedArrayBuffer, Atomics and shared-memory requirements; browser COOP/COEP applies only to browser targets, while native hosts are assessed independently. Account for the documented small Emscripten thread-local residual separately from actual wrapper leaks. Classify the debug renderer's DOM/WebGL needs separately from core pthread support. Preserve the thread-affine Ogma Runtime. A justified EXCLUDED/OUT_OF_PROFILE result is valid. |

All variants must follow Jolt's Emscripten ownership rules. Emscripten does not automatically destroy wrappers created with `new Jolt.XXX`: destroy owned wrappers with `Jolt.destroy(...)`; use `AddRef()/Release()` only for independently retained/released references; do not release objects after ownership has transferred to a parent; remove bodies through `BodyInterface.RemoveBody(body.GetID())` before `BodyInterface.DestroyBody(body.GetID())`; treat `GetPhysicsSystem()` and `GetBodyInterface()` results as borrowed views owned by the `JoltInterface`, not as independently owned objects. After destroying the Jolt interface, never reuse those borrowed views. Repeated lifecycle tests must detect double destroy, stale handles, use-after-destroy and unbounded memory growth.

Record cold initialization, first step, steady-state step, memory and cleanup separately; do not infer game FPS from runtime/physics microbenchmarks. Every result must be tied to the exact package/tarball integrity, pnpm lockfile, Ogma commit, target tuple, selected entrypoint and test evidence. These staged probes are not blanket ECMAScript, WebAssembly, Worker, SharedArrayBuffer or Jolt compatibility claims.

**Upstream references:** [JoltPhysics.js README and entrypoint variants](https://github.com/jrouwe/JoltPhysics.js/blob/main/README.md); [package exports](https://github.com/jrouwe/JoltPhysics.js/blob/main/package.json); [proper object cleanup example](https://github.com/jrouwe/JoltPhysics.js/blob/main/Examples/proper_cleanup.html); [Jolt multithread worker bootstrap](https://github.com/jrouwe/JoltPhysics.js/blob/main/multi-threaded.js); [Emscripten `Module.locateFile`](https://emscripten.org/docs/api_reference/module.html#Module.locateFile); [Emscripten pthreads](https://emscripten.org/docs/porting/pthreads.html).


## 9. Versioning and change control

This file is the single repository source of truth for compatibility status and pinned baselines.

- Patch profile revision: editorial clarification that does not change observable compatibility claims or pinned references.
- Minor profile revision: additive feature/API/package support or a new explicitly partial surface without invalidating prior claims.
- Major profile revision: change of ECMAScript edition, TypeScript syntax baseline, Node major/reference line, resolver/scheduler semantics or a breaking change to previously supported claims.
- Every revision updates pinned sources, feature/API matrices, fixtures and the compatibility report together.
- No dependency or profile update may silently turn deferred/unsupported into supported.
- A compatibility report must identify profile ID/version, all upstream SHAs, consumer lockfile/repository revisions, platform tuple, selected tests and PASS/FAIL/UNSUPPORTED/EXCLUDED/HARNESS ERROR counts.
- Full Test262 and broad differential runs remain on the configured promotion lane; topic PRs run focused tests only. This profile does not alter the existing OGM-35 conformance-gate ownership.

## 10. Reference sources

- [ECMA-262 2026 — normative language specification](https://tc39.es/ecma262/2026/)
- [Ecma International — ECMA-262 publication record](https://ecma-international.org/publications-and-standards/standards/ecma-262/)
- [TC39 Test262 — official ECMAScript conformance suite](https://github.com/tc39/test262/tree/2e58f87d8350ad51224617a041c6e041d52f8444)
- [TC39 Test262 parser tests — pinned supplementary corpus](https://github.com/tc39/test262-parser-tests/tree/0e808c74fbec780646434cad17bb22dc52461003)
- [TypeScript 7.0.2 release](https://github.com/microsoft/TypeScript/releases/tag/v7.0.2)
- [Node.js v26.13.0 API documentation](https://nodejs.org/docs/v26.13.0/api/)
- [Node.js v26.13.0 source revision](https://github.com/nodejs/node/tree/18b32a3b1552e199a01f1a8d9fd64aa756d3b75b)
- [Node.js Release Working Group schedule](https://github.com/nodejs/Release)
- [TC39 finished proposals — expected publication editions](https://github.com/tc39/proposals/blob/main/finished-proposals.md)
- [Node.js 26.0.0 release notes — Temporal availability](https://nodejs.org/en/blog/release/v26.0.0#temporal-api)
- [SolidJS Native pinned consumer](https://github.com/AiratGaliev/solidjs-native/tree/bcd0fa119c5a0b0fc762462bd5ddc4b2818c7668)
- [Codogma Engine pinned secondary consumer](https://github.com/AiratGaliev/codogma-engine/tree/982d25c26da94c1f73d61457bf55da682413eff2)
- [cge-experiments pinned Brick Breaker M2 workload](https://github.com/AiratGaliev/cge-experiments/tree/91dfa48300f6ba3f0bc4f2a9db9b7ac298dc508c/rust-brick-breaker)
- [Ogma architecture specification — sections 3, 13 and 20](https://app.notion.com/p/4167ef18f18181bd9b34f9dda764c92d#b5ca462a10c841279d724b74c44b239a)
