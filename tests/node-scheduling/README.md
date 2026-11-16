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
| io-phase-order.cjs | Async fs completion, callback checkpoint, and the documented setImmediate-before-timer ordering when both are scheduled from the I/O callback. |
| cancellation.cjs | clearTimeout and clearImmediate suppress callbacks; surviving nextTick, Promise, immediate and timer callbacks are all observed. Timer/immediate order is intentionally not asserted for top-level scheduling. |
| interval-cancellation.cjs | setInterval runs twice, is cleared from its second callback and does not fire again. |
| errors.cjs | On Node.js 26.13.0 the fixture observes the nextTick exception, then the immediate callback, then the unhandled Promise rejection event. |

The baseline stores complete event traces, not timing measurements. The tests assert only ordering that the pinned reference process actually demonstrates. In particular, the corpus does not assert a universal ordering between top-level setTimeout() and setImmediate(): the cancellation fixture compares their observed callback set without imposing an order.

## Run

~~~sh
node tests/node-scheduling/run.mjs
~~~

CI installs the exact version through actions/setup-node and runs the same command. The topic-PR lane executes this focused corpus; it does not run the full Node.js test suite, Test262, full differential testing or E2E.

## Interpretation

A passing run means only that the checked-in fixtures reproduce the pinned Node.js reference observations. It does not test an Ogma Runtime. Once the Runtime and optional Node adapter exist, run equivalent fixtures through Ogma and compare their observable traces before changing any compatibility state from deferred.
