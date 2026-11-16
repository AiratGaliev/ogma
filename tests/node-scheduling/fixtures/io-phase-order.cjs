const fs = require("node:fs");
const events = [];
const record = (event) => events.push(event);

fs.readFile(__filename, () => {
  record("io:complete");
  process.nextTick(() => record("nextTick:io"));
  Promise.resolve().then(() => record("promise:io"));
  setTimeout(() => {
    record("timer:io");
    process.stdout.write(JSON.stringify(events));
  }, 0);
  setImmediate(() => record("immediate:io"));
});
