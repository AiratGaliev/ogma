const events = [];
const record = (event) => events.push(event);

process.on("uncaughtException", (error) => record("uncaught:" + error.message));
process.on("unhandledRejection", (reason) => {
  record("unhandled:" + reason.message);
  process.stdout.write(JSON.stringify(events));
});
process.nextTick(() => {
  throw new Error("tick-failure");
});
Promise.reject(new Error("promise-failure"));

setImmediate(() => record("immediate"));
