#!/usr/bin/env python3
"""Compare a bounded Ogma source subset with the pinned QuickJS-NG oracle."""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import tempfile
import xml.etree.ElementTree as ET
from pathlib import Path

import test_runner

QUICKJS_REVISION_KEY = "quickjs-ng"


def run_ogma(adapter: Path, fixture: dict, timeout_ms: int) -> dict:
    request = {
        "protocol": "ogma-test-runner-adapter",
        "version": 1,
        "requestId": 1,
        "timeoutMs": timeout_ms,
        "test": {
            "id": fixture["id"],
            "suite": "ogma-differential",
            "source": fixture["source"],
            "sourceType": "script",
            "mode": "sloppy",
            "entryFunction": fixture["entryFunction"],
            "hostCallTrace": fixture.get("hostCallTrace", False),
            "input": fixture.get("input"),
            "raw": True,
            "async": False,
            "includes": [],
        },
    }
    completed = subprocess.run(
        [str(adapter)],
        input=json.dumps(request) + "\n",
        text=True,
        capture_output=True,
        check=False,
        timeout=timeout_ms / 1000 + 2,
    )
    if completed.returncode != 0:
        raise RuntimeError(f"Ogma adapter exited {completed.returncode}: {completed.stderr[-2000:]}")
    lines = completed.stdout.splitlines()
    if len(lines) != 1:
        raise RuntimeError(f"Ogma adapter returned {len(lines)} response lines, expected one")
    response = json.loads(lines[0])
    if (
        response.get("protocol") != request["protocol"]
        or response.get("version") != request["version"]
        or response.get("requestId") != request["requestId"]
    ):
        raise RuntimeError("Ogma adapter returned an invalid response envelope")
    return response


def run_quickjs(reference: Path, fixture: dict, timeout_ms: int) -> dict:
    entry = fixture["entryFunction"]
    source = fixture["source"]
    host_trace = bool(fixture.get("hostCallTrace", False))
    input_value = fixture.get("input")
    arguments = [] if input_value is None else [input_value]
    prelude = """
var __ogmaHostCalls = [];
function benchmarkHostEcho(value) {
    var observable = {type: "number", value: value};
    __ogmaHostCalls.push({
        name: "benchmark.host.echo",
        arguments: [observable],
        completion: "return",
        value: observable
    });
    return value;
}
""" if host_trace else "var __ogmaHostCalls = [];\n"
    invocation = f"{entry}({', '.join(json.dumps(value) for value in arguments)})"
    script = (
        prelude
        + source
        + "\ntry {\n"
        + f"  var __ogmaResult = {invocation};\n"
        + '  print(JSON.stringify({completion: "return", value: __ogmaResult, hostCalls: __ogmaHostCalls}));\n'
        + "} catch (__ogmaThrown) {\n"
        + '  print(JSON.stringify({completion: "throw", value: __ogmaThrown, hostCalls: __ogmaHostCalls}));\n'
        + "}\n"
    )
    with tempfile.TemporaryDirectory(prefix="ogma-differential-") as directory:
        path = Path(directory) / "case.js"
        path.write_text(script, encoding="utf-8", newline="\n")
        completed = subprocess.run(
            [str(reference), str(path)],
            text=True,
            capture_output=True,
            check=False,
            timeout=timeout_ms / 1000 + 2,
        )
    if completed.returncode != 0:
        raise RuntimeError(f"QuickJS-NG exited {completed.returncode}: {completed.stderr[-2000:]}")
    lines = [line for line in completed.stdout.splitlines() if line.strip()]
    if len(lines) != 1:
        raise RuntimeError(f"QuickJS-NG returned {len(lines)} observable output lines, expected one")
    result = json.loads(lines[0])
    if not isinstance(result, dict) or result.get("completion") not in ("return", "throw"):
        raise RuntimeError(f"QuickJS-NG returned an invalid completion record: {result!r}")
    return result


def write_junit(path: Path, results: list[dict]) -> None:
    suite = ET.Element(
        "testsuite",
        name="ogma-quickjs-differential-smoke",
        tests=str(len(results)),
        failures=str(sum(item["status"] == "FAIL" for item in results)),
        errors=str(sum(item["status"] == "ERROR" for item in results)),
        skipped="0",
    )
    for item in results:
        case = ET.SubElement(suite, "testcase", name=item["id"], classname="ogma.differential")
        if item["status"] == "FAIL":
            ET.SubElement(case, "failure", message=item.get("reason", "observable mismatch")).text = json.dumps(item, sort_keys=True)
        elif item["status"] == "ERROR":
            ET.SubElement(case, "error", message=item.get("reason", "differential harness error")).text = json.dumps(item, sort_keys=True)
    path.parent.mkdir(parents=True, exist_ok=True)
    ET.ElementTree(suite).write(path, encoding="utf-8", xml_declaration=True)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--adapter", type=Path, required=True)
    parser.add_argument("--reference", type=Path, required=True)
    parser.add_argument("--reference-root", type=Path, required=True)
    parser.add_argument("--fixtures", type=Path, default=Path("tests/differential/fixtures.json"))
    parser.add_argument("--baseline", type=Path, default=Path("conformance/baseline.json"))
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--junit", type=Path, required=True)
    parser.add_argument("--timeout-ms", type=int, default=5000)
    args = parser.parse_args()

    baseline = test_runner.load_baseline(args.baseline)
    reference_source = baseline["repositories"][QUICKJS_REVISION_KEY]
    revision = subprocess.run(
        ["git", "-C", str(args.reference_root.resolve(strict=True)), "rev-parse", "HEAD"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()
    if revision != reference_source["revision"]:
        raise test_runner.RunnerError(
            f"QuickJS-NG revision mismatch: expected {reference_source['revision']}, got {revision}"
        )

    fixture_document = test_runner.read_json(args.fixtures)
    fixtures = fixture_document.get("fixtures")
    if type(fixture_document.get("schemaVersion")) is not int or fixture_document["schemaVersion"] != 1:
        raise test_runner.RunnerError("unsupported differential fixture schemaVersion")
    if not isinstance(fixtures, list) or not fixtures:
        raise test_runner.RunnerError("differential fixture list must be non-empty")

    adapter = args.adapter.resolve(strict=True)
    reference = args.reference.resolve(strict=True)
    results = []
    for fixture in fixtures:
        item = {"id": fixture["id"], "status": "ERROR"}
        try:
            ogma = run_ogma(adapter, fixture, args.timeout_ms)
            if ogma.get("outcome") not in ("completed", "threw"):
                raise RuntimeError(f"Ogma did not produce an observable completion: {ogma}")
            ogma_completion = ogma.get("completion")
            if ogma_completion not in ("return", "throw"):
                raise RuntimeError(f"Ogma returned an invalid completion kind: {ogma}")
            ogma_value = ogma.get("value")
            if not isinstance(ogma_value, dict) or "type" not in ogma_value:
                raise RuntimeError(f"Ogma returned an invalid observable value: {ogma_value!r}")
            quickjs_result = run_quickjs(reference, fixture, args.timeout_ms)
            expected_completion = fixture.get("expectedCompletion", "return")
            expected = fixture["expected"]
            expected_host_calls = fixture.get("expectedHostCalls", [])
            ogma_host_calls = ogma.get("hostCalls", [])
            quickjs_host_calls = quickjs_result.get("hostCalls", [])
            ogma_observable = ogma_value.get("value")
            item.update({
                "ogmaCompletion": ogma_completion,
                "ogmaValue": ogma_observable,
                "ogmaHostCalls": ogma_host_calls,
                "quickjsCompletion": quickjs_result["completion"],
                "quickjsValue": quickjs_result.get("value"),
                "quickjsHostCalls": quickjs_host_calls,
                "expectedCompletion": expected_completion,
                "expected": expected,
                "expectedHostCalls": expected_host_calls,
            })
            if (
                ogma_completion != quickjs_result["completion"]
                or ogma_completion != expected_completion
                or ogma_observable != quickjs_result.get("value")
                or quickjs_result.get("value") != expected
                or ogma_host_calls != quickjs_host_calls
                or quickjs_host_calls != expected_host_calls
            ):
                item["status"] = "FAIL"
                item["reason"] = (
                    "observable completion, value, host-call side-effect trace, "
                    "or fixture expectation differs"
                )
            else:
                item["status"] = "PASS"
        except (OSError, RuntimeError, ValueError, KeyError, subprocess.SubprocessError) as error:
            item["reason"] = str(error)
        results.append(item)

    report = {
        "schemaVersion": 1,
        "suite": "ogma-differential-smoke",
        "reference": {
            "repository": reference_source["url"],
            "revision": revision,
        },
        "fixtures": str(args.fixtures),
        "results": results,
        "summary": {
            "total": len(results),
            "PASS": sum(item["status"] == "PASS" for item in results),
            "FAIL": sum(item["status"] == "FAIL" for item in results),
            "ERROR": sum(item["status"] == "ERROR" for item in results),
        },
    }
    test_runner.write_json(args.report, report)
    write_junit(args.junit, results)
    print(json.dumps(report["summary"], sort_keys=True))
    return 0 if all(item["status"] == "PASS" for item in results) else 1


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, test_runner.RunnerError, subprocess.SubprocessError) as error:
        print(f"Ogma differential smoke failed: {error}", file=sys.stderr)
        raise SystemExit(2)
