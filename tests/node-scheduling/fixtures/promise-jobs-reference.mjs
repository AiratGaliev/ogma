import assert from "node:assert/strict";

const fixture = await import(new URL("./promise-jobs-module.mjs", import.meta.url));
await Promise.all([fixture.first, fixture.second]);
assert.equal(fixture.trace, "first,second,nested,");
console.log(JSON.stringify(fixture.trace.split(",").filter(Boolean)));
