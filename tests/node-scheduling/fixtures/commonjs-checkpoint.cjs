const events = [];
const record = (event) => events.push(event);

record("sync:start");
process.nextTick(() => {
  record("nextTick:outer");
  process.nextTick(() => record("nextTick:nested"));
  Promise.resolve().then(() => record("promise:from-nextTick"));
  queueMicrotask(() => record("microtask:from-nextTick"));
});
Promise.resolve().then(() => {
  record("promise:outer");
  process.nextTick(() => record("nextTick:from-promise"));
  Promise.resolve().then(() => record("promise:nested"));
});
queueMicrotask(() => record("microtask:outer"));
record("sync:end");

setImmediate(() => {
  record("immediate");
  process.stdout.write(JSON.stringify(events));
});
