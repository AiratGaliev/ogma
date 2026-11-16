# Ogma Compatibility Profile

- **Profile ID:** ogma-compatibility-profile
- **Profile version:** 1.0.0
- **Status:** M0 decision baseline; target contract, not an implementation claim
- **Baseline captured:** 2026-11-16
- **Normative owner:** this document. Architecture rationale remains in the [Ogma architecture specification](https://app.notion.com/p/4167ef18f18181bd9b34f9dda764c92d#b5ca462a10c841279d724b74c44b239a).

This profile defines the language, frontend, Node.js host APIs, module behaviours and consumer workloads Ogma intends to support. It separates the target contract from implementation status. At this repository revision there is no Ogma Rust workspace or runtime implementation, so no runtime/API feature is reported as implemented or conformant.

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

**Current implementation status:** no Ogma runtime, frontend, Node adapter, module resolver or conformance runner exists in this repository. Therefore every planned runtime capability below remains **deferred**. Unsupported rows are explicit exclusions from the target profile, not missing implementations that are silently tolerated.

## 2. ECMAScript baseline

### 2.1. Normative edition and test snapshot

| Item | Pinned baseline |
|---|---|
| Language standard | **ECMA-262, 17th edition — ECMAScript 2026**, June 2026 |
| Normative specification | [ECMA-262 2026](https://tc39.es/ecma262/2026/) |
| Publication record | [Ecma International — ECMA-262, 17th edition](https://ecma-international.org/publications-and-standards/standards/ecma-262/) |
| Test262 repository | [tc39/test262](https://github.com/tc39/test262) |
| Test262 commit | 2e58f87d8350ad51224617a041c6e041d52f8444 |
| Supplementary parser corpus | [tc39/test262-parser-tests](https://github.com/tc39/test262-parser-tests), commit 0e808c74fbec780646434cad17bb22dc52461003 |

The Test262 commit is a fixed snapshot selected from the repository's main ref when this profile was captured. CI and local tooling must fetch that exact commit, never a moving branch. Updating the snapshot requires a profile revision, upstream diff review and compatibility-result comparison.

The parser-only corpus is an older supplementary fixture set (2021); it is not sufficient by itself for ECMAScript 2026 grammar acceptance. Test262 parser tests and Ogma-owned grammar/early-error fixtures remain authoritative for the selected 2026 profile.

ECMA-402 / Intl is a separate specification surface. The 2026 Ecma General Assembly approved ECMA-402 13th edition (ECMAScript 2025 Internationalization API) separately from ECMA-262. **Intl is deferred in profile 1.0.0**; no Intl conformance is implied by the ECMA-262 baseline. A later Intl profile must pin its own ECMA-402 edition, data/ICU provider and tests.

### 2.2. Initial execution subset

The normative language reference is ECMAScript 2026, but the first executable runtime is intentionally a subset. M1's initial target is the smallest useful JavaScript runtime described by the architecture: values, objects, functions, lexical scopes, control flow, exceptions, module execution and the shared Promise/job model. It is not a claim of full ECMA-262 coverage.

| Feature group | Initial execution target | Current status | Reference evidence |
|---|---|---|---|
| Primitive values, operators, coercion and equality | Core ECMAScript value and operator semantics required by ordinary scripts | deferred | Test262 language/expression/built-ins tests |
| Declarations, functions, lexical environments and closures | var/let/const, function declarations/expressions, arrow functions, scope/hoisting/TDZ and closure capture | deferred | Test262 language/function/lexical-binding tests |
| Objects, arrays and property operations | Object/array creation, property access and mutation, descriptors, prototypes and observable property semantics needed by the selected subset | deferred | Test262 built-ins/Object/Array/property tests |
| Control flow and exceptions | Branches, loops, calls, throw, try/catch/finally, abrupt completion and completion propagation | deferred | Test262 language/statement/exception tests |
| Classes | Class declarations/expressions, constructors, methods, inheritance and private elements are staged after the minimal function/object proof | deferred | Test262 language/class tests |
| ESM | Static import/export, module records, linking/instantiation/evaluation, live bindings and cyclic graphs | deferred | Test262 module-code tests plus resolver fixtures |
| Promise jobs | Promise semantics and a host-pumped ECMAScript job queue with observable microtask checkpoints | deferred | Test262 Promise/async tests and scheduler fixtures |
| Dynamic source | eval and new Function must preserve ECMAScript scope semantics; runtime-compiled code uses interpreter/bytecode | deferred | Test262 eval/Function tests and interpreter regressions |
| RegExp, BigInt, ArrayBuffer and TypedArray | Required runtime built-ins from the QuickJS-NG-derived initial implementation direction; each built-in has an independent feature/test state | deferred | Test262 built-ins/RegExp/BigInt/TypedArray tests |
| Generators, async functions and async generators | Full suspension, resumption, completion and abrupt-completion semantics | deferred | Test262 generator/async tests |
| Proxy, Reflect, Symbol, WeakRef and FinalizationRegistry | No blanket support; each feature requires its own semantics, GC and lifecycle proof | deferred | Corresponding Test262 feature groups |
| SharedArrayBuffer and Atomics | Requires an explicit memory/concurrency model and host/worker integration | deferred | Test262 shared-memory tests and stress tests |
| Intl / ECMA-402 | Separate profile only; not in this profile's initial execution contract | deferred | Pinned ECMA-402 suite when adopted |
| Temporal and post-2026 additions | Not implicitly included by tracking the latest TC39 draft | deferred | New profile revision and corresponding Test262 snapshot |

The table describes the target work sequence, not an assertion that all listed features land in M1. M1 must not be expanded into full language conformance by inference. A feature becomes supported only after implementation and tests demonstrate the required observable behaviour. A subset may become partial only when its supported sub-surface and exclusions are recorded explicitly.

### 2.3. Proposals, Annex B and host APIs

- Stage 4 proposals included in the pinned ECMA-262 2026 edition are part of the normative reference; they are not automatically implemented.
- Stage 0–3 proposals and proposal-specific syntax/semantics are **unsupported** in profile 1.0.0 unless a later profile explicitly adopts and tests them.
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

CI lane policy remains unchanged: focused tests and small bounded regressions belong on topic PRs; the full pinned Test262/differential corpus belongs to the dev → test promotion gate after OGM-35 implements its runner and deterministic shard aggregator. Do not run or claim full Test262 on this documentation PR.

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

### 3.3. TypeScript syntax and transform matrix

All rows are deferred until Ogma implements the frontend and passes fixtures. “Erase” means the syntax has no direct runtime representation; it does not mean Ogma executes TypeScript types.

| TS/TSX construct | Frontend contract | Runtime effect / policy | Current status |
|---|---|---|---|
| Type annotations, interfaces, type aliases, generic declarations/parameters/arguments | Parse and erase | No runtime type object is created | deferred |
| as / angle-bracket assertions, non-null assertion, satisfies, type-only imports/exports | Parse and erase | No runtime check or cast is introduced | deferred |
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

### 3.4. Consumer-relevant TypeScript/JSX workload

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

The first SolidJS workload should cover TSX/JSX lowering, solid-js/jsx-runtime integration, reactive computations/effects, component functions, module imports and calls across the Rust host boundary. The pinned package manifest exposes pnpm check (typecheck, build and Rust workspace tests) and pnpm smoke (build plus native runtime launch) as candidate consumer commands; they must be rerun against the Ogma adapter when that integration exists. They were not run for this documentation task. Vite, its plugin, TypeScript CLI and pnpm are development/build tools; their use of Node.js does not make them runtime dependencies of a packaged native application.

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

### 4.3. Node Base API matrix

| Module / global | Candidate v1 surface | Required host capability | Target platforms | Pinned Node reference test family | Current status |
|---|---|---|---|---|---|
| node:assert, node:assert/strict | Assertion functions and documented strict-mode behaviour used by consumer tests | none | Windows, Linux, macOS; x86_64 and ARM64 | test/parallel/test-assert*.js | deferred |
| node:buffer | Buffer construction, encoding/decoding, comparison, slicing/copying and ArrayBuffer/TypedArray interop | none; memory quotas remain Runtime policy | Windows, Linux, macOS; x86_64 and ARM64 | test/parallel/test-buffer*.js | deferred |
| node:events | EventEmitter core: on/once/off, emit, listener removal/order and error event behaviour | none | Windows, Linux, macOS; x86_64 and ARM64 | test/parallel/test-event-emitter*.js | deferred |
| node:util | Selected format/inspect helpers, promisify and callbackify; no undocumented internals | none | Windows, Linux, macOS; x86_64 and ARM64 | test/parallel/test-util*.js | deferred |
| node:path | posix and win32 string semantics: normalize, join, resolve, relative, parse/format and separators | none | Windows, Linux, macOS; both variants tested on every target | test/parallel/test-path*.js | deferred |
| node:url | URL/URLSearchParams and file URL conversion helpers in the declared subset | none; file conversion does not grant file access | Windows, Linux, macOS; x86_64 and ARM64 | test/parallel/test-url*.js | deferred |
| node:querystring | parse/stringify and documented escaping semantics | none | Windows, Linux, macOS; x86_64 and ARM64 | test/parallel/test-querystring*.js | deferred |
| node:string_decoder | StringDecoder and encoding-boundary behaviour | none | Windows, Linux, macOS; x86_64 and ARM64 | test/parallel/test-string-decoder*.js | deferred |
| node:stream | Readable, Writable, Duplex, Transform, PassThrough and selected pipeline/backpressure contracts | none for in-memory streams; external I/O requires a Host adapter | Windows, Linux, macOS; x86_64 and ARM64 | test/parallel/test-stream*.js, test/parallel/test-readable*.js | deferred |
| node:timers and timer globals | setTimeout/setInterval/setImmediate, cancellation and observable callback ordering | scheduler.timers | Windows, Linux, macOS; x86_64 and ARM64 | test/parallel/test-timers*.js, test/parallel/test-async-hooks-top-level-clearimmediate.js | deferred |
| console global | log/info/warn/error and selected formatting behaviour | console.write | Windows, Linux, macOS; x86_64 and ARM64 | test/parallel/test-console*.js | deferred |
| process facade | Read-only version/platform/arch metadata; capability-gated env/cwd; nextTick; no process control | process.metadata, optionally process.env.read, process.cwd.read, scheduler.next_tick | Windows, Linux, macOS; x86_64 and ARM64 | selected test/parallel/test-process*.js cases only | deferred |

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
| process.env.read | Read only explicitly allowed environment variables | process.env |
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

The declared platform target is Windows, Linux and macOS, including x86_64 and ARM64 configurations. A capability is available only if its adapter is compiled, registered and tested for the exact OS/architecture tuple. Mobile, WebAssembly and other targets require a separate platform profile; this document does not imply their support.

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
- Planned Node reference selectors are taken from the pinned Node source: test/parallel/test-module-*.js, test/parallel/test-require-*.js and test/parallel/test-esm-*.js. Only cases applicable to the declared module/profile surface are selected.

### 5.3. Package manifest and resolver rules

The initial resolver contract covers the following fields and rules, subject to tests:

- package.json: type, main, exports, imports.
- Package self-references, subpath exports/imports and package encapsulation.
- ESM and CommonJS entry points, extension rules and module cache/identity.
- Conditional exports conditions are enabled only when their semantics are valid for the selected profile: import/require reflect the active module mode; default is the final fallback; ogma, development and production require explicit profile/build configuration.
- The node condition is enabled only when the selected Node API profile and the package's required Node APIs are actually implemented and available. It is not a blanket marker for any Ogma runtime. node-addons is never enabled in profile 1.0.0 because native Node addons are excluded.
- node-addons is never enabled in profile 1.0.0 because native Node addons are excluded.
- browser is not an implicit condition for a native application. A host may opt into it only in a separate explicit profile.

The resolver must distinguish an unrecognized condition in a package's conditional map (which is simply not an enabled match) from an invalid condition supplied in Ogma's own profile configuration (which is a deterministic configuration error). If no enabled branch matches, or a package's exports boundary forbids the requested subpath, resolution fails with a stable package-resolution diagnostic. It must not silently fall back to an arbitrary file or bypass package encapsulation.

Unknown node:* built-ins, unsupported package entry points, denied host capabilities and unsupported module formats fail deterministically. Error identity/message mapping to Node error codes is part of the tested compatibility surface, not an excuse to silently resolve another path.

Package managers (npm/pnpm/yarn) remain external build-time tools. They provide a lockfile and resolved dependency graph; Ogma Toolchain consumes that graph and builds the application bundle. Runtime startup must not require Node, npm, Corepack, network registry access or a source node_modules tree. Package lifecycle/install scripts are untrusted build-time code and are never run automatically when an embedded application starts.

## 6. Scheduling and async semantics

- ECMAScript PromiseJobs/microtasks belong to the shared Ogma Runtime.
- Node process.nextTick() uses a separate queue from PromiseJobs. Queue checkpoints, re-entrant enqueueing, fairness and observable ordering must match pinned Node reference fixtures; do not reduce both queues to one FIFO.
- setTimeout, setInterval, setImmediate and cancellation are provided by a Node scheduler adapter over host-driven scheduling capabilities.
- Async I/O completion is delivered through a host scheduler/executor adapter. Rust Future/Waker internals are not exposed through the public ABI.
- The host controls pump/poll/run-until-idle, pause, shutdown and cancellation. The same observable Promise/Node scheduling contract must hold for game-loop, native UI, desktop and headless hosts.
- Ogma does not require Tokio or libuv as a mandatory runtime dependency and does not create a hidden second event loop.
- Tests compare observable callback order and completion/error behaviour with Node 26.13.0. Ogma does not need to reproduce libuv internal phases, thread pool or implementation details where the declared observable API contract is preserved.

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

The corpus is versioned independently from the Node API matrix. Every entry must record repository commit, lockfile hash, package name/version, target OS/architecture, build profile, required capabilities, exact test command and expected observable behaviour.

| Consumer | Pinned repository revision | Initial workload | Scope classification |
|---|---|---|---|
| SolidJS Native | AiratGaliev/solidjs-native@bcd0fa119c5a0b0fc762462bd5ddc4b2818c7668 | solid-js@1.9.18, local solid-native@0.1.0, TSX/JSX runtime, reactivity/effects, module graph, Rust host bindings | Primary runtime consumer |
| Codogma Engine | AiratGaliev/codogma-engine@982d25c26da94c1f73d61457bf55da682413eff2 | Future native application/editor/game-loop integration and host callbacks | Secondary future consumer; current repository is Node-based tooling, not an Ogma runtime acceptance result |

For SolidJS Native, keep these categories separate:

- Runtime packages: solid-js@1.9.18 and the local solid-native@0.1.0 adapter.
- Build/test tools: typescript@7.0.2, vite@8.3.7, vite-plugin-solid@2.11.15, pnpm 12.18.0.
- Type-only declarations: @types/node@26.6.6; declarations do not establish runtime Node API support.
- Existing reference integration: rquickjs@0.14.0 in the consumer's Rust workspace; it is not a dependency Ogma should retain.

Do not claim universal npm compatibility. A package becomes part of the supported corpus only after its exact version and lockfile are pinned and its install/build/runtime path is exercised. Transitive dependencies must be included in the captured lockfile graph. Native addons, install scripts and platform-specific binaries require separate review and do not become compatible merely because a pure-JS package imports successfully.

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
- [SolidJS Native pinned consumer](https://github.com/AiratGaliev/solidjs-native/tree/bcd0fa119c5a0b0fc762462bd5ddc4b2818c7668)
- [Codogma Engine pinned secondary consumer](https://github.com/AiratGaliev/codogma-engine/tree/982d25c26da94c1f73d61457bf55da682413eff2)
- [Ogma architecture specification — sections 3, 13 and 20](https://app.notion.com/p/4167ef18f18181bd9b34f9dda764c92d#b5ca462a10c841279d724b74c44b239a)
