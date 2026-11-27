# Node.js scheduling reference corpus

This directory captures observable scheduling behaviour from the repository's pinned Node.js reference runtime for OGM-13. It is a black-box reference proof, not an Ogma scheduler implementation and not evidence that any Node API is already supported by Ogma.

## Pinned reference

- Runtime: Node.js 26.13.0, pinned in the repository-root .node-version.
- Upstream source: 18b32a3b1552e199a01f1a8d9fd64aa756d3b75b.
- Compatibility profile: Ogma Node.js Compatibility Profile 1.0.0.
- Expected callback traces: baseline.json.
- Runner: node tests/node-scheduling/run.mjs.

The runner checks the actual Node version before invoking Node's built-in test runner. No npm install, third-party package or network access is required.

## Coverage

| Fixture | Reference behaviour |
|---|---|
| commonjs-checkpoint.cjs | Synchronous CommonJS entry, nextTick priority, recursively queued nextTick, Promise reactions, queueMicrotask and nested scheduling. |
| esm-checkpoint.mjs | ESM evaluation with top-level await; PromiseJobs/microtasks and nextTick are observed at the module evaluation boundary. |
| io-phase-order.cjs | Async fs completion, callback checkpoint, and `setImmediate()` before `setTimeout(0)` when both are scheduled from the I/O callback. |
| nested-immediate.cjs | An immediate queued from a running immediate is deferred until a subsequent event-loop check turn. |
| cancellation.cjs | clearTimeout and clearImmediate suppress callbacks; surviving nextTick, Promise, immediate and timer callbacks are all observed. Timer/immediate order is intentionally not asserted for top-level scheduling. |
| interval-cancellation.cjs | setInterval runs twice, is cleared from its second callback and does not fire again. |
| errors.cjs | On Node.js 26.13.0 the fixture observes the nextTick exception, then the immediate callback, then the unhandled Promise rejection event. |
| promise-jobs-reference.mjs | Imports the same `promise-jobs-module.mjs` source compiled by the Ogma embedding integration test; pins FIFO order when the first reaction schedules a nested Promise reaction and returns its child Promise. |

The baseline stores complete event traces, not timing measurements. The I/O fixture pins only the Node-defined `setImmediate()`-before-`setTimeout(0)` order when both are scheduled from the I/O callback; a separate fixture checks nested-immediate deferral. The corpus does not assert a universal ordering between top-level `setTimeout()` and `setImmediate()`: the cancellation fixture compares their observed callback set without imposing an order.

## Run

~~~sh
node tests/node-scheduling/run.mjs
~~~

CI installs the exact version through actions/setup-node and runs the same command. The topic-PR lane executes this focused corpus; it does not run the full Node.js test suite, Test262, full differential testing or E2E.

## Interpretation

The `promise-jobs-module.mjs` file is intentionally shared: Node executes it through the reference runner, while the Rust embedding test compiles and evaluates these exact source bytes. That differential fixture covers only the declared M1 PromiseJobs subset; the remaining Node scheduling fixtures are reference observations for a later Node compatibility adapter.
