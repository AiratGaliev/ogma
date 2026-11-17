from __future__ import annotations

import sys
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))

from tools import test_runner


FIXTURES = ROOT / "tests" / "test-runner" / "fixtures"
TEST262_ROOT = FIXTURES / "test262"
PARSER_ROOT = FIXTURES / "parser-tests"
BASELINE = test_runner.load_baseline(ROOT / "conformance" / "baseline.json")
FAKE_ADAPTER = [sys.executable, str(ROOT / "tests" / "test-runner" / "fake_adapter.py")]


def ready_profile() -> dict:
    return {
        "schemaVersion": 1,
        "profileId": "self-test",
        "version": "1.0.0",
        "execution": {"state": "ready"},
        "parserStatus": "ready",
        "defaultFeatureStatus": "supported",
        "featureStatus": {"Array": "supported"},
        "exclusions": [],
    }


def manifest_for(suite: str, root: Path, profile: dict, shard_count: int = 1):
    return test_runner.build_manifest(
        suite,
        test_runner.discover_test_ids(root, suite),
        BASELINE,
        profile,
        shard_count,
    )


class Test262MetadataTests(unittest.TestCase):
    def test_metadata_includes_features_flags_and_both_default_modes(self) -> None:
        test_id = "runner/strict-default.js"
        descriptor = test_runner._read_test262_descriptor(TEST262_ROOT, test_id)
        self.assertEqual(descriptor["features"], ["Array"])
        self.assertEqual(descriptor["modes"], ["sloppy", "strict"])
        self.assertEqual(descriptor["sourceType"], "script")

    def test_harness_includes_are_loaded_in_test262_order(self) -> None:
        descriptor = test_runner._read_test262_descriptor(
            TEST262_ROOT, "runner/basic-async.js"
        )
        self.assertEqual(
            [item["name"] for item in descriptor["includes"]],
            ["sta.js", "assert.js", "helper.js"],
        )
        self.assertTrue(descriptor["async"])

    def test_raw_tests_do_not_receive_harness_includes(self) -> None:
        descriptor = test_runner._read_test262_descriptor(TEST262_ROOT, "runner/raw.js")
        self.assertTrue(descriptor["raw"])
        self.assertEqual(descriptor["includes"], [])
        self.assertEqual(descriptor["modes"], ["sloppy"])

    def test_conflicting_strictness_flags_are_a_harness_error(self) -> None:
        result = test_runner.run_one_test(
            TEST262_ROOT,
            "test262",
            "runner/invalid-flags.js",
            ready_profile(),
            FAKE_ADAPTER,
            1000,
            1000,
        )
        self.assertEqual(result["status"], "HARNESS ERROR")
        self.assertIn("mutually exclusive", result["reason"])

    def test_negative_parse_phase_and_error_type_are_checked(self) -> None:
        result = test_runner.run_one_test(
            TEST262_ROOT,
            "test262",
            "runner/negative-parse.js",
            ready_profile(),
            FAKE_ADAPTER,
            1000,
            1000,
        )
        self.assertEqual(result["status"], "PASS")
        self.assertEqual(result["modes"], ["strict"])
        self.assertEqual(result["variants"][0]["status"], "PASS")

    def test_module_tests_use_one_module_variant(self) -> None:
        result = test_runner.run_one_test(
            TEST262_ROOT,
            "test262",
            "runner/module.js",
            ready_profile(),
            FAKE_ADAPTER,
            1000,
            1000,
        )
        self.assertEqual(result["status"], "PASS")
        self.assertEqual(result["modes"], ["module"])

    def test_async_completion_is_required(self) -> None:
        result = test_runner.run_one_test(
            TEST262_ROOT,
            "test262",
            "runner/async-incomplete.js",
            ready_profile(),
            FAKE_ADAPTER,
            1000,
            1000,
        )
        self.assertEqual(result["status"], "FAIL")
        self.assertEqual(result["variants"][0]["failureKind"], "ASYNC_NOT_COMPLETED")

    def test_timeout_is_a_failure_not_a_pass(self) -> None:
        result = test_runner.run_one_test(
            TEST262_ROOT,
            "test262",
            "runner/timeout.js",
            ready_profile(),
            FAKE_ADAPTER,
            50,
            50,
        )
        self.assertEqual(result["status"], "FAIL")
        self.assertEqual(result["variants"][0]["failureKind"], "TIMEOUT")

    def test_unsupported_feature_is_not_counted_as_pass(self) -> None:
        profile = ready_profile()
        profile["featureStatus"]["Array"] = "deferred"
        result = test_runner.run_one_test(
            TEST262_ROOT,
            "test262",
            "runner/strict-default.js",
            profile,
            FAKE_ADAPTER,
            1000,
            1000,
        )
        self.assertEqual(result["status"], "UNSUPPORTED")
        self.assertEqual(result["variants"], [])

    def test_probe_detects_unexpected_pass_for_unsupported_feature(self) -> None:
        profile = ready_profile()
        profile["featureStatus"]["Array"] = "deferred"
        result = test_runner.run_one_test(
            TEST262_ROOT,
            "test262",
            "runner/strict-default.js",
            profile,
            FAKE_ADAPTER,
            1000,
            1000,
            probe_unsupported=True,
        )
        self.assertEqual(result["status"], "UNEXPECTED PASS")

    def test_json_and_junit_reports_preserve_classifications(self) -> None:
        results = [
            {"id": "pass.js", "status": "PASS", "variants": []},
            {"id": "unsupported.js", "status": "UNSUPPORTED", "reason": "deferred", "variants": []},
            {"id": "broken.js", "status": "HARNESS ERROR", "reason": "bad metadata", "variants": []},
            {"id": "newly-supported.js", "status": "UNEXPECTED PASS", "reason": "review", "variants": []},
        ]
        summary = test_runner.summarize(results)
        self.assertEqual(summary["PASS"], 1)
        self.assertEqual(summary["UNSUPPORTED"], 1)
        self.assertEqual(summary["HARNESS ERROR"], 1)
        self.assertEqual(summary["UNEXPECTED PASS"], 1)
        report_path = Path(self.id().replace(".", "_") + ".xml")
        try:
            test_runner.write_junit(report_path, "test262", results)
            root = ET.parse(report_path).getroot()
            self.assertEqual(root.attrib["tests"], "4")
            self.assertEqual(root.attrib["errors"], "1")
            self.assertEqual(root.attrib["failures"], "1")
            self.assertEqual(root.attrib["skipped"], "1")
        finally:
            report_path.unlink(missing_ok=True)


class ParserAndShardingTests(unittest.TestCase):
    def test_parser_corpus_pass_fail_and_early_error_phases(self) -> None:
        manifest = manifest_for("test262-parser-tests", PARSER_ROOT, ready_profile())
        report = test_runner.run_manifest(
            manifest,
            PARSER_ROOT,
            ready_profile(),
            FAKE_ADAPTER,
            0,
            timeout_ms=1000,
            async_timeout_ms=1000,
            verify_corpus=True,
        )
        self.assertEqual(report["summary"]["PASS"], 3)
        self.assertEqual(report["summary"]["FAIL"], 0)

    def test_shard_manifest_is_deterministic_and_disjoint(self) -> None:
        profile = ready_profile()
        test_ids = test_runner.discover_test_ids(TEST262_ROOT, "test262")
        first = test_runner.build_manifest("test262", test_ids, BASELINE, profile, 3)
        second = test_runner.build_manifest("test262", list(reversed(test_ids)), BASELINE, profile, 3)
        self.assertEqual(first["manifestSha256"], second["manifestSha256"])
        flattened = [test_id for shard in first["shards"] for test_id in shard["testIds"]]
        self.assertEqual(sorted(flattened), sorted(test_ids))
        self.assertEqual(len(flattened), len(set(flattened)))
        test_runner.validate_manifest(first)

    def test_aggregate_success_requires_exactly_one_result_per_test(self) -> None:
        manifest = test_runner.build_manifest(
            "test262", ["a.js", "b.js", "c.js", "d.js"], BASELINE, ready_profile(), 2
        )
        reports = []
        for shard in manifest["shards"]:
            reports.append({
                "suite": "test262",
                "manifestSha256": manifest["manifestSha256"],
                "shardIndex": shard["index"],
                "shardCount": 2,
                "results": [
                    {"id": test_id, "status": "PASS", "variants": []}
                    for test_id in shard["testIds"]
                ],
            })
        aggregate = test_runner.aggregate_reports(manifest, reports)
        self.assertTrue(aggregate["aggregation"]["complete"])
        self.assertEqual(aggregate["summary"]["PASS"], 4)

    def test_aggregate_detects_missing_shard_and_missing_tests(self) -> None:
        manifest = test_runner.build_manifest(
            "test262", ["a.js", "b.js", "c.js"], BASELINE, ready_profile(), 2
        )
        first = manifest["shards"][0]
        report = {
            "suite": "test262",
            "manifestSha256": manifest["manifestSha256"],
            "shardIndex": first["index"],
            "shardCount": 2,
            "results": [],
        }
        aggregate = test_runner.aggregate_reports(manifest, [report])
        self.assertFalse(aggregate["aggregation"]["complete"])
        self.assertEqual(len(aggregate["aggregation"]["missingShards"]), 1)
        self.assertGreaterEqual(len(aggregate["aggregation"]["missingTests"]), 1)

    def test_aggregate_detects_duplicate_shards_and_duplicate_test_results(self) -> None:
        manifest = test_runner.build_manifest(
            "test262", ["a.js", "b.js"], BASELINE, ready_profile(), 1
        )
        test_id = manifest["tests"][0]["id"]
        report = {
            "suite": "test262",
            "manifestSha256": manifest["manifestSha256"],
            "shardIndex": 0,
            "shardCount": 1,
            "results": [
                {"id": test_id, "status": "PASS"},
                {"id": test_id, "status": "PASS"},
            ],
        }
        aggregate = test_runner.aggregate_reports(manifest, [report, report])
        self.assertFalse(aggregate["aggregation"]["complete"])
        self.assertEqual(aggregate["aggregation"]["duplicateShards"], [0])
        self.assertTrue(any("duplicate test result" in error for error in aggregate["aggregation"]["errors"]))


if __name__ == "__main__":
    unittest.main()
