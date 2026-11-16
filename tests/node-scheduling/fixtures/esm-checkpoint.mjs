const events = [];
const record = (event) => events.push(event);

record("sync:start");
process.nextTick(() => record("nextTick:entry"));
Promise.resolve().then(() => record("promise:entry"));
queueMicrotask(() => record("microtask:entry"));
record("sync:end");

await Promise.resolve();
record("after:top-level-await");

await new Promise((resolve) => setImmediate(() => {
  record("immediate");
  resolve();
}));
process.stdout.write(JSON.stringify(events));
