#!/usr/bin/env python3
"""Run the exact profile-gated Test262 M1 smoke through Ogma's real Runtime adapter."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

import test_runner


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--corpus", type=Path, required=True)
    parser.add_argument("--adapter", type=Path, required=True)
    parser.add_argument("--baseline", type=Path, default=Path("conformance/baseline.json"))
    parser.add_argument("--profile", type=Path, default=Path("conformance/profile.json"))
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--junit", type=Path, required=True)
    parser.add_argument("--timeout-ms", type=int, default=5000)
    args = parser.parse_args()

    if args.timeout_ms < 1:
        raise test_runner.RunnerError("--timeout-ms must be positive")
    baseline = test_runner.load_baseline(args.baseline)
    profile = test_runner.load_profile(args.profile)
    source = test_runner._pinned_source(baseline, "test262")
    corpus = args.corpus.resolve(strict=True)
    test_runner._load_and_check_revision(corpus, source["revision"], "test262")
    allowlist = profile.get("test262Allowlist")
    if not isinstance(allowlist, list) or not allowlist:
        raise test_runner.RunnerError("the runtime smoke requires a non-empty exact test262Allowlist")
    if profile["execution"]["state"] != "ready":
        raise test_runner.RunnerError("the Runtime execution profile is not ready")

    adapter = args.adapter.resolve(strict=True)
    command = [str(adapter)]
    results = []
    worker_error = None
    try:
        with test_runner.AdapterSession(command) as session:
            for test_id in allowlist:
                result = test_runner.run_one_test(
                    corpus,
                    "test262",
                    test_id,
                    profile,
                    command,
                    args.timeout_ms,
                    args.timeout_ms,
                    adapter_session=session,
                )
                results.append(result)
    except (OSError, test_runner.RunnerError) as error:
        worker_error = str(error)

    summary = test_runner.summarize(results)
    report = {
        "schemaVersion": 1,
        "suite": "test262",
        "source": source,
        "profile": {
            "id": profile["profileId"],
            "version": profile["version"],
            "sha256": test_runner.sha256_json(profile),
        },
        "adapter": str(adapter),
        "results": results,
        "summary": summary,
        "workerError": worker_error,
    }
    test_runner.write_json(args.report, report)
    test_runner.write_junit(
        args.junit,
        "ogma-m1-test262-smoke",
        results,
        [worker_error] if worker_error else [],
    )
    print(test_runner.json.dumps(summary, sort_keys=True))
    if worker_error or any(result["status"] != "PASS" for result in results):
        return 1
    print(f"Ogma M1 Test262 smoke passed: {len(results)} tests at {source['revision']}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, test_runner.RunnerError) as error:
        print(f"Ogma runtime corpus smoke failed: {error}", file=sys.stderr)
        raise SystemExit(2)
