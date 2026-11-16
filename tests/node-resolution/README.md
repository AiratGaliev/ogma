# Node.js package-resolution reference corpus

This directory is a deterministic, black-box reference proof for OGM-12. It records Node.js behaviour; it does not implement an Ogma resolver or claim that any Node compatibility feature is already supported by Ogma.

## Pinned reference

- Runtime: Node.js 26.13.0, pinned in the repository-root `.node-version`.
- Upstream source: `18b32a3b1552e199a01f1a8d9fd64aa756d3b75b`.
- Compatibility profile: Ogma Node.js Compatibility Profile 1.0.0.
- Expected observations, diagnostic codes and upstream test selectors: [baseline.json](baseline.json).

Run the corpus:

```sh
node tests/node-resolution/run.mjs
```

The runner materializes its first-party package fixtures in an isolated temporary directory, invokes Node's built-in test runner there, and removes the directory afterwards. No npm install, package lifecycle script or third-party dependency is needed. CI installs the exact `.node-version` through `actions/setup-node` and verifies `node --version` before running the same command. Tests also assert that the baseline version and test command match the checked-in pin.

## Coverage

| Area | Observable contract |
|---|---|
| Package format | `type: module/commonjs` controls `.js`; `.mjs` and `.cjs` fixtures establish the explicit entry formats. |
| Entry-point precedence | Legacy `main` is used when `exports` is absent; `exports` wins over `main`. |
| ESM and CommonJS | Root conditional exports select distinct ESM and CJS entry points. |
| Conditional exports | `node`, `import`, `default`, and explicitly enabled `ogma`, `development`, `production` conditions; original key order is observable. |
| Package maps | `imports` internal mapping, undefined `#imports`, self-reference and explicit subpath export. |
| Encapsulation | Existing private files remain inaccessible when omitted from `exports`. |
| Built-ins | Registered `node:` specifiers resolve; an unknown `node:` builtin fails with a stable error code. |
| Errors | No enabled condition and denied exports do not fall back to an arbitrary file. |
| Identity and cycles | CJS partial exports/cache identity and ESM cycle/live-binding/module identity. |

Fixture package contents are versioned in [fixtures.json](fixtures.json), then materialized under a temporary `fixtures/node_modules` tree so Node's own resolver uses normal package lookup. These are small, first-party fixture packages, not installed third-party packages or production dependencies.

## Scope limits

The corpus is a focused M0 proof, not the full Node module-loader conformance suite. Wildcard/pattern exports, arrays/null targets, custom loaders, broader extension/directory probing, `require(esm)`, native addons and untested cross-format cache identity remain outside this fixture set. The upstream Node test selectors recorded in [baseline.json](baseline.json) are reference provenance and future expansion points; this task does not claim to run those complete upstream suites.

The normative resolver layering, precedence, condition rules, diagnostics and cache-ownership contract is in [the compatibility profile](../../docs/compatibility-profile.md#54-resolver-precedence-diagnostics-and-module-identity). All Ogma runtime capability states remain deferred until an Ogma implementation passes its own tests.
