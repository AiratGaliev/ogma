import assert from "node:assert/strict";

const fixture = await import(new URL("./promise-jobs-module.mjs", import.meta.url));
await new Promise((resolve) => setImmediate(resolve));
assert.equal(fixture.trace, "first,second,nested,");
console.log(JSON.stringify(fixture.trace.split(",").filter(Boolean)));
