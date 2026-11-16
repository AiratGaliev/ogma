const events = [];
const record = (event) => events.push(event);

const cancelledTimeout = setTimeout(() => record("timeout:cancelled"), 0);
clearTimeout(cancelledTimeout);
const cancelledImmediate = setImmediate(() => record("immediate:cancelled"));
clearImmediate(cancelledImmediate);

process.nextTick(() => record("nextTick"));
Promise.resolve().then(() => record("promise"));
setImmediate(() => record("immediate:survived"));
setTimeout(() => {
  record("timer:survived");
  process.stdout.write(JSON.stringify(events));
}, 10);
