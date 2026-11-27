#!/usr/bin/env python3
"""Run a bounded, pinned parser-corpus smoke through the real Rust adapter."""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import xml.etree.ElementTree as ET
from pathlib import Path

import test_runner

CASES = (
    ("pass/005dc7dff71d4b97.js", "parsed", "script", "005dc7dff71d4b97.js", "pass"),
    ("pass-explicit/005dc7dff71d4b97.js", "parsed", "script", "005dc7dff71d4b97.js", "pass-explicit"),
    ("pass/0140c25a4177e5f7.module.js", "parsed", "module", "0140c25a4177e5f7.module.js", "pass"),
    ("pass-explicit/0140c25a4177e5f7.module.js", "parsed", "module", "0140c25a4177e5f7.module.js", "pass-explicit"),
    ("early/0049fa147d7edac0.js", "rejected", "script", None, None),
    ("fail/0053737b6145994c.js", "rejected", "script", None, None),
)


def build_request(
    corpus: Path,
    case: tuple[str, str, str, str | None, str | None],
    request_id: int,
) -> dict:
    relative, expected, source_type, equivalence_group, role = case
    source_path = (corpus / relative).resolve(strict=True)
    source_path.relative_to(corpus.resolve(strict=True))
    source = source_path.read_text(encoding="utf-8")
    return {
        "protocol": "ogma-test-runner-adapter",
        "version": 1,
        "requestId": request_id,
        "test": {
            "id": relative,
            "suite": "test262-parser-tests",
            "source": source,
            "metadata": {},
            "mode": source_type,
            "sourceType": source_type,
            "raw": True,
            "async": False,
            "flags": [],
            "features": [],
            "featureRequirements": [],
            "includes": [],
            "negative": None,
            "parseExpectation": {
                "outcome": expected,
                "phase": "early" if relative.startswith("early/") else "parse",
            },
            "equivalenceGroup": equivalence_group,
            "equivalenceRole": role,
            "corpusRoot": str(corpus.resolve()),
            "testPath": relative,
            "testDirectory": str(source_path.parent),
        },
        "timeoutMs": 5000,
    }


def run_cases(adapter: Path, corpus: Path) -> list[dict]:
    requests = [build_request(corpus, case, index) for index, case in enumerate(CASES, 1)]
    payload = "".join(json.dumps(request, ensure_ascii=False) + "\n" for request in requests)
    completed = subprocess.run(
        [str(adapter.resolve())],
        input=payload,
        text=True,
        capture_output=True,
        check=False,
        timeout=30,
    )
    if completed.returncode != 0:
        raise RuntimeError(
            f"parser adapter exited {completed.returncode}: {completed.stderr[-2000:]}"
        )

    lines = completed.stdout.splitlines()
    if len(lines) != len(CASES):
        raise RuntimeError(
            f"expected {len(CASES)} JSONL responses from one worker, got {len(lines)}"
        )

    responses = []
    for index, (case, line) in enumerate(zip(CASES, lines, strict=True), 1):
        relative, expected, _source_type, _group, _role = case
        try:
            response = json.loads(line)
        except json.JSONDecodeError as error:
            raise RuntimeError(f"{relative}: adapter returned invalid JSON: {error}") from error
        if (
            response.get("protocol") != "ogma-test-runner-adapter"
            or response.get("version") != 1
            or response.get("requestId") != index
        ):
            raise RuntimeError(f"{relative}: invalid adapter response envelope or requestId")
        if response.get("outcome") != expected:
            raise RuntimeError(
                f"{relative}: expected {expected}, got {response.get('outcome')} "
                f"({response.get('phase')}); {response.get('reason', '')}"
            )
        diagnostics = response.get("diagnostics")
        if not isinstance(diagnostics, list):
            raise RuntimeError(f"{relative}: diagnostics must be an array")
        if expected == "rejected":
            expected_phase = "early" if relative.startswith("early/") else "parse"
            if response.get("phase") != expected_phase:
                raise RuntimeError(
                    f"{relative}: expected rejection phase {expected_phase}, got {response.get('phase')}"
                )
            if not diagnostics:
                raise RuntimeError(f"{relative}: rejected input must include diagnostics")
            if any(item.get("phase") not in {"parse", "early"} for item in diagnostics):
                raise RuntimeError(f"{relative}: diagnostic has an invalid source phase")
        responses.append(response)
    return responses

def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--corpus", type=Path, required=True)
    parser.add_argument("--adapter", type=Path, required=True)
    parser.add_argument("--baseline", type=Path, default=Path("conformance/baseline.json"))
    parser.add_argument("--report", type=Path)
    parser.add_argument("--junit", type=Path)
    args = parser.parse_args()

    baseline = test_runner.load_baseline(args.baseline)
    pinned = test_runner._pinned_source(baseline, "test262-parser-tests")
    corpus = args.corpus.resolve(strict=True)
    test_runner._load_and_check_revision(corpus, pinned["revision"], "test262-parser-tests")
    revision = pinned["revision"]

    fingerprints: dict[str, str] = {}
    passed = 0
    rejected = 0
    results: list[dict] = []
    for case, response in zip(CASES, run_cases(args.adapter, corpus), strict=True):
        if case[1] == "parsed":
            passed += 1
        else:
            rejected += 1
        results.append({
            "id": case[0],
            "status": "PASS",
            "expected": case[1],
            "actual": response.get("outcome"),
            "phase": response.get("phase"),
            "normalizedAstSha256": response.get("normalizedAstSha256"),
        })
        if case[3] is not None:
            fingerprint = response.get("normalizedAstSha256")
            if (
                not isinstance(fingerprint, str)
                or len(fingerprint) != 64
                or any(character not in "0123456789abcdef" for character in fingerprint)
            ):
                raise RuntimeError(f"{case[0]}: missing normalized AST SHA-256")
            key = case[3]
            previous = fingerprints.setdefault(key, fingerprint)
            if previous != fingerprint:
                raise RuntimeError(f"{case[0]}: pass/pass-explicit AST fingerprint mismatch")

    report = {
        "schemaVersion": 1,
        "suite": "test262-parser-tests",
        "source": pinned,
        "adapter": str(args.adapter.resolve()),
        "cases": results,
        "summary": {
            "total": len(results),
            "PASS": len(results),
            "parsed": passed,
            "rejected": rejected,
            "astEquivalencePairs": len(fingerprints),
        },
    }
    if args.report:
        test_runner.write_json(args.report, report)
    if args.junit:
        suite = ET.Element(
            "testsuite",
            name="ogma-pinned-parser-smoke",
            tests=str(len(results)),
            failures="0",
            errors="0",
            skipped="0",
        )
        for item in results:
            ET.SubElement(suite, "testcase", name=item["id"], classname="ogma.parser")
        args.junit.parent.mkdir(parents=True, exist_ok=True)
        ET.ElementTree(suite).write(args.junit, encoding="utf-8", xml_declaration=True)
    print(
        f"Ogma parser corpus smoke passed: {passed} valid, {rejected} rejected, "
        f"{len(fingerprints)} AST equivalence pairs; revision {revision}"
    )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError, ValueError, json.JSONDecodeError, subprocess.SubprocessError) as error:
        print(f"parser corpus smoke failed: {error}", file=sys.stderr)
        raise SystemExit(1)
