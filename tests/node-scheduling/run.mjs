import { spawnSync } from "node:child_process";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

const directory = fileURLToPath(new URL(".", import.meta.url));
const repositoryRoot = resolve(directory, "../..");
const baseline = JSON.parse(await readFile(new URL("./baseline.json", import.meta.url), "utf8"));
const pinnedNodeVersion = (await readFile(new URL("../../.node-version", import.meta.url), "utf8")).trim();

if (baseline.reference.runtime !== "Node.js") {
  throw new Error("The scheduling baseline must identify Node.js as its reference runtime");
}
if (baseline.reference.version !== pinnedNodeVersion) {
  throw new Error("Baseline Node.js version does not match .node-version");
}
if (process.versions.node !== pinnedNodeVersion) {
  throw new Error("Expected Node.js v" + pinnedNodeVersion + " from .node-version, got v" + process.versions.node);
}
if (baseline.reference.sourceCommit !== "18b32a3b1552e199a01f1a8d9fd64aa756d3b75b") {
  throw new Error("The Node.js source reference changed; review and update the pinned scheduling baseline explicitly");
}
if (baseline.command !== "node tests/node-scheduling/run.mjs" ||
    !Array.isArray(baseline.testCommand) ||
    baseline.testCommand.length === 0 ||
    !baseline.testCommand.every((argument) => typeof argument === "string")) {
  throw new Error("The scheduling baseline contains an invalid runner command");
}

const result = spawnSync(process.execPath, baseline.testCommand, {
  cwd: repositoryRoot,
  stdio: "inherit",
});
if (result.error) throw result.error;
if (result.signal) throw new Error("Scheduling reference tests terminated by signal " + result.signal);
process.exitCode = result.status ?? 1;
