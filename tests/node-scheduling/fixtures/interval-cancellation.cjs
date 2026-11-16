const events = [];
const record = (event) => events.push(event);
let ticks = 0;

const interval = setInterval(() => {
  ticks += 1;
  record("interval:" + ticks);
  if (ticks === 2) {
    clearInterval(interval);
    setTimeout(() => {
      record("after-clear");
      process.stdout.write(JSON.stringify(events));
    }, 5);
  }
}, 1);
