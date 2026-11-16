#!/usr/bin/env python3
"""Collect reproducibility metadata and raw OGM-14 benchmark measurements."""

from __future__ import annotations

import hashlib
import json
import os
import platform
import re
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parent
RESULTS = ROOT / "results"


def run(command: list[str]) -> str:
    return subprocess.check_output(command, cwd=ROOT, text=True).strip()


def package_graph(package: str) -> dict[str, object]:
    tree = run([
        "cargo", "tree", "--manifest-path", str(ROOT / "Cargo.toml"),
        "--package", package, "--edges", "normal", "--prefix", "none", "--no-dedupe",
    ])
    packages = set(re.findall(r"^\s*([A-Za-z0-9_.+-]+) v([0-9][^\s]*)", tree, re.MULTILINE))
    packages.discard((package, "0.1.0"))
    return {
        "transitivePackageCount": len(packages),
        "transitivePackages": [f"{name} {version}" for name, version in sorted(packages)],
    }


def direct_dependencies(package: str) -> list[dict[str, str]]:
    metadata = json.loads(run([
        "cargo", "metadata", "--manifest-path", str(ROOT / "Cargo.toml"),
        "--format-version", "1", "--no-deps",
    ]))
    item = next(item for item in metadata["packages"] if item["name"] == package)
    return [
        {"name": dep["name"], "requirement": dep["req"]}
        for dep in item["dependencies"]
        if dep["kind"] in (None, "normal")
    ]


def cpu_model() -> str | None:
    cpuinfo = Path("/proc/cpuinfo")
    if not cpuinfo.exists():
        return None
    for line in cpuinfo.read_text(errors="replace").splitlines():
        if line.lower().startswith("model name"):
            return line.split(":", 1)[1].strip()
    return None


def read_json(name: str) -> dict[str, object]:
    return json.loads((RESULTS / name).read_text())


lockfile = ROOT / "Cargo.lock"
lock_bytes = lockfile.read_bytes()
rustc = run(["rustc", "-Vv"])
cargo = run(["cargo", "--version"])
run_id = os.environ.get("GITHUB_RUN_ID")
repository = os.environ.get("GITHUB_REPOSITORY", "AiratGaliev/ogma")
metadata = {
    "schemaVersion": 1,
    "capturedAtUtc": run(["date", "-u", "+%Y-%m-%dT%H:%M:%SZ"]),
    "repository": repository,
    "commit": os.environ.get("GITHUB_SHA"),
    "workflowRunId": run_id,
    "workflowRunUrl": f"https://github.com/{repository}/actions/runs/{run_id}" if run_id else None,
    "runner": {
        "os": platform.platform(),
        "architecture": platform.machine(),
        "cpuModel": cpu_model(),
        "rustcVerbose": rustc,
        "cargo": cargo,
    },
    "cargoLock": {
        "sha256": hashlib.sha256(lock_bytes).hexdigest(),
        "byteLength": len(lock_bytes),
        "packageCount": lock_bytes.count(b"[[package]]"),
    },
    "releaseBuildSeconds": int(os.environ.get("OGM14_BUILD_SECONDS", "0")),
    "dependencyFootprint": {
        "oxc": {
            **package_graph("ogma-parser-bench-oxc"),
            "directDependencies": direct_dependencies("ogma-parser-bench-oxc"),
        },
        "swc": {
            **package_graph("ogma-parser-bench-swc"),
            "directDependencies": direct_dependencies("ogma-parser-bench-swc"),
        },
    },
}
output = {
    "schemaVersion": 1,
    "metadata": metadata,
    "measurements": {
        "oxcParseOnly": read_json("oxc-parse.json"),
        "oxcParsePlusSemantic": read_json("oxc-full.json"),
        "oxcDiagnostics": read_json("oxc-diagnostics.json"),
        "swcParseOnly": read_json("swc-parse.json"),
        "swcDiagnostics": read_json("swc-diagnostics.json"),
    },
}
(RESULTS / "benchmark-run.json").write_text(json.dumps(output, indent=2) + "\n")
print(json.dumps({
    "workflowRunUrl": metadata["workflowRunUrl"],
    "cargoLockSha256": metadata["cargoLock"]["sha256"],
    "dependencyCounts": {
        "oxc": metadata["dependencyFootprint"]["oxc"]["transitivePackageCount"],
        "swc": metadata["dependencyFootprint"]["swc"]["transitivePackageCount"],
    },
}, indent=2))
