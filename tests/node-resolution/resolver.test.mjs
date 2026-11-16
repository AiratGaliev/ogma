import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createRequire } from "node:module";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

import { importPackage } from "./fixtures/consumer.mjs";

const require = createRequire(new URL("./fixtures/consumer.cjs", import.meta.url));
const baseline = JSON.parse(await readFile(new URL("./baseline.json", import.meta.url), "utf8"));
const pinnedNodeVersion = (await readFile(new URL("../../.node-version", import.meta.url), "utf8")).trim();
const expected = baseline.expected;
const errors = baseline.errors;
const probePath = fileURLToPath(new URL("./fixtures/condition-probe.mjs", import.meta.url));

function runWithCondition(condition) {
  const result = spawnSync(process.execPath, [`--conditions=${condition}`, probePath], { encoding: "utf8" });
  assert.equal(result.status, 0, `Node exited with ${result.status}: ${result.stderr}`);
  assert.equal(result.stderr, "", "condition probe must not write diagnostics");
  return result.stdout.trim();
}
function errorCode(expectedCode) {
  return (error) => error?.code === expectedCode;
}

test("reference version and test command match the pinned baseline", () => {
  assert.equal(baseline.reference.runtime, "Node.js");
  assert.equal(baseline.reference.version, pinnedNodeVersion);
  assert.equal(baseline.reference.sourceCommit, "18b32a3b1552e199a01f1a8d9fd64aa756d3b75b");
  assert.equal(baseline.command, "node tests/node-resolution/run.mjs");
  assert.deepEqual(baseline.testCommand, ["--test", "--test-reporter=spec", "tests/node-resolution/resolver.test.mjs"]);
  assert.equal(process.versions.node, pinnedNodeVersion);
});
test("package.json type selects ESM semantics for .js", async () => {
  const module = await importPackage("@ogma/fixture/typed");
  assert.equal(module.default, expected.packageTypeEsm);
});
test("package.json type selects CommonJS semantics for .js", () => {
  const module = require("ogma-type-commonjs");
  assert.equal(module.kind, expected.packageTypeCommonJs);
});
test("legacy main is used when exports is absent", () => {
  const module = require("ogma-main-only");
  assert.equal(module.entry, expected.packageMain);
});
test("exports selects separate ESM and CommonJS entry points", async () => {
  const esm = await importPackage("@ogma/fixture");
  const commonJs = require("@ogma/fixture");
  assert.equal(esm.default, expected.dualImport);
  assert.equal(commonJs.entry, expected.dualRequire);
});
test("conditional exports use the first enabled key in package order", async () => {
  const module = await importPackage("@ogma/fixture/node-order");
  assert.equal(module.default, expected.nodeCondition);
});
test("default is an ordinary ordered condition, not a global priority", async () => {
  const module = await importPackage("@ogma/fixture/default-first");
  assert.equal(module.default, expected.defaultFirst);
});
test("an unknown package condition is skipped before a later default", async () => {
  const module = await importPackage("@ogma/fixture/unknown-condition-fallback");
  assert.equal(module.default, expected.unknownConditionFallsThrough);
});
test("an explicitly selected ogma condition is enabled", () => {
  assert.equal(runWithCondition("ogma"), expected.ogmaCondition);
});
test("development and production conditions require explicit selection", () => {
  assert.equal(runWithCondition("development"), expected.developmentCondition);
  assert.equal(runWithCondition("production"), expected.productionCondition);
});
test("package imports resolve # specifiers in the owning package scope", async () => {
  const module = await importPackage("@ogma/fixture/imports");
  assert.equal(module.default, expected.importsMap);
});
test("undefined package imports fail with a stable Node diagnostic code", async () => {
  await assert.rejects(importPackage("@ogma/fixture/missing-import"), errorCode(errors.undefinedPackageImport));
});
test("package self-references resolve through the package exports map", async () => {
  const module = await importPackage("@ogma/fixture/self");
  assert.equal(module.default, expected.selfReference);
});
test("explicit package subpath exports resolve", async () => {
  const module = await importPackage("@ogma/fixture/feature");
  assert.equal(module.default, expected.subpathExport);
});
test("exports takes precedence over main and encapsulates private files", async () => {
  const root = await importPackage("@ogma/fixture");
  assert.equal(root.default, expected.exportsRoot);
  await assert.rejects(importPackage("@ogma/fixture/private.mjs"), errorCode(errors.exportsBoundary));
});
test("no matching conditional export fails without arbitrary file fallback", async () => {
  await assert.rejects(importPackage("ogma-no-condition-match"), errorCode(errors.noConditionalMatch));
});
test("node: built-ins are explicit specifiers", async () => {
  const pathModule = await import("node:path");
  const assertModule = require("node:assert/strict");
  assert.equal(pathModule.posix.join("a", "b"), expected.nodePathJoin);
  assert.equal(typeof assertModule.ok, "function");
});
test("unknown node: built-ins fail instead of resolving as packages", async () => {
  await assert.rejects(import("node:ogma-nonexistent-built-in"), errorCode(errors.unknownNodeBuiltin));
});
test("CommonJS cycles expose partial exports and preserve cache identity", () => {
  const first = require("ogma-cjs-cycle");
  const second = require("ogma-cjs-cycle");
  assert.equal(first.observedByB, expected.cjsCycleObservation);
  assert.equal(first.phase, "a-ready");
  assert.strictEqual(second, first);
  assert.strictEqual(require.cache[require.resolve("ogma-cjs-cycle")].exports, first);
});
test("ESM cycles preserve live bindings and module identity", async () => {
  const first = await importPackage("ogma-esm-cycle");
  const second = await importPackage("ogma-esm-cycle");
  assert.equal(first.phaseObservedDuringCycle, expected.esmCycleObservation);
  assert.equal(first.phase, expected.esmCycleFinalState);
  assert.strictEqual(second, first);
});
