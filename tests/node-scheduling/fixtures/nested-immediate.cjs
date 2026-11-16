const events = [];

setImmediate(() => {
  events.push("immediate:outer");
  setImmediate(() => {
    events.push("immediate:nested");
    process.stdout.write(JSON.stringify(events));
  });
});

setImmediate(() => {
  events.push("immediate:peer");
});
