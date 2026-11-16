import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const baseline = JSON.parse(await readFile(new URL("./baseline.json", import.meta.url), "utf8"));
const pinnedNodeVersion = (await readFile(new URL("../../.node-version", import.meta.url), "utf8")).trim();
const fixtureNames = Object.keys(baseline.fixtures).sort();

function runFixture(fixture) {
  const fixturePath = fileURLToPath(new URL("./fixtures/" + fixture.file, import.meta.url));
  const result = spawnSync(process.execPath, [fixturePath], {
    cwd: fileURLToPath(new URL("../..", import.meta.url)),
    encoding: "utf8",
    timeout: 10000,
  });

  assert.equal(result.error, undefined, "fixture process failed to start");
  assert.equal(result.signal, null, "fixture terminated by signal " + result.signal);
  assert.equal(result.status, 0, "fixture exited with " + result.status + ": " + result.stderr);
  assert.equal(result.stderr, "", "reference fixture must not emit diagnostics");
  return JSON.parse(result.stdout.trim());
}

test("Node.js scheduler reference and runner pins match the repository", () => {
  assert.equal(baseline.schemaVersion, 1);
  assert.equal(baseline.reference.runtime, "Node.js");
  assert.equal(baseline.reference.version, pinnedNodeVersion);
  assert.equal(process.versions.node, pinnedNodeVersion);
  assert.equal(baseline.reference.sourceCommit, "18b32a3b1552e199a01f1a8d9fd64aa756d3b75b");
  assert.equal(baseline.command, "node tests/node-scheduling/run.mjs");
  assert.deepEqual(baseline.testCommand, [
    "--test",
    "--test-reporter=spec",
    "tests/node-scheduling/scheduler.test.mjs",
  ]);
});

test("the fixture manifest is complete and points only to known local fixtures", () => {
  assert.deepEqual(fixtureNames, [
    "cancellation",
    "commonjsCheckpoint",
    "errors",
    "esmCheckpoint",
    "intervalCancellation",
    "ioPhaseOrder",
  ]);
  for (const fixture of Object.values(baseline.fixtures)) {
    assert.equal(typeof fixture.file, "string");
    assert.match(fixture.file, /^[a-z0-9-]+\.(?:cjs|mjs)$/);
    assert.ok(Array.isArray(fixture.expected));
    assert.ok(fixture.expected.every((event) => typeof event === "string"));
    assert.ok(fixture.order === undefined || fixture.order === "unordered");
  }
});

for (const [name, fixture] of Object.entries(baseline.fixtures)) {
  test("pinned Node.js callback order: " + name, () => {
    const actual = runFixture(fixture);
    if (fixture.order === "unordered") {
      assert.deepEqual([...actual].sort(), [...fixture.expected].sort());
      return;
    }
    assert.deepEqual(actual, fixture.expected);
  });
}
