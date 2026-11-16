import { spawnSync } from "node:child_process";
import { mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const directory = dirname(fileURLToPath(import.meta.url));
const repositoryRoot = resolve(directory, "../..");
const baselineText = await readFile(join(directory, "baseline.json"), "utf8");
const testText = await readFile(join(directory, "resolver.test.mjs"), "utf8");
const fixtureFiles = JSON.parse(await readFile(join(directory, "fixtures.json"), "utf8"));
const nodeVersion = (await readFile(join(repositoryRoot, ".node-version"), "utf8")).trim();
const baseline = JSON.parse(baselineText);

if (baseline.reference.version !== nodeVersion) {
  throw new Error(`Baseline Node.js version ${baseline.reference.version} does not match .node-version ${nodeVersion}`);
}
if (process.versions.node !== nodeVersion) {
  throw new Error(`Expected Node.js v${nodeVersion} from .node-version, got v${process.versions.node}`);
}
if (!Array.isArray(baseline.testCommand) || baseline.testCommand.length === 0 ||
    !baseline.testCommand.every((argument) => typeof argument === "string")) {
  throw new Error("baseline.testCommand must be a non-empty array of string arguments");
}

const temporaryRoot = await mkdtemp(join(tmpdir(), "ogma-node-resolution-"));
const testRoot = join(temporaryRoot, "tests", "node-resolution");
const fixturesRoot = join(testRoot, "fixtures");

try {
  await mkdir(fixturesRoot, { recursive: true });
  await writeFile(join(temporaryRoot, ".node-version"), nodeVersion + "\n");
  await writeFile(join(testRoot, "baseline.json"), baselineText);
  await writeFile(join(testRoot, "resolver.test.mjs"), testText);

  for (const [relativePath, content] of Object.entries(fixtureFiles)) {
    if (relativePath.startsWith("/") || relativePath.split(/[\\/]/).includes("..")) {
      throw new Error(`Unsafe fixture path: ${relativePath}`);
    }
    const destination = join(fixturesRoot, relativePath);
    await mkdir(dirname(destination), { recursive: true });
    await writeFile(destination, content, "utf8");
  }

  process.stdout.write(`Node.js runtime v${process.versions.node}; pinned source ${baseline.reference.sourceCommit}\n`);
  const result = spawnSync(
    process.execPath,
    baseline.testCommand,
    { cwd: temporaryRoot, encoding: "utf8" },
  );
  if (result.stdout) process.stdout.write(result.stdout);
  if (result.stderr) process.stderr.write(result.stderr);
  if (result.error) throw result.error;
  process.exitCode = result.status ?? 1;
} finally {
  await rm(temporaryRoot, { recursive: true, force: true });
}
