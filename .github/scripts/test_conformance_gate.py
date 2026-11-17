from __future__ import annotations

import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))

from tools import test_runner


TEST262_ROOT = ROOT / "tests" / "test-runner" / "fixtures" / "test262"
BASELINE = test_runner.load_baseline(ROOT / "conformance" / "baseline.json")


def ready_profile() -> dict:
    return {
        "schemaVersion": 1,
        "profileId": "promotion-gate-self-test",
        "version": "1.0.0",
        "execution": {"state": "ready"},
        "parserStatus": "ready",
        "defaultFeatureStatus": "supported",
        "featureStatus": {},
        "exclusions": [],
    }


def build_manifest(shard_count: int) -> dict:
    profile = ready_profile()
    return test_runner.build_manifest(
        "test262",
        test_runner.discover_test_ids(TEST262_ROOT, "test262"),
        BASELINE,
        profile,
        shard_count,
    )


def report_for(manifest: dict, shard_index: int, status: str = "PASS") -> dict:
    shard = manifest["shards"][shard_index]
    return {
        "schemaVersion": 1,
        "suite": manifest["suite"],
        "source": manifest["source"],
        "profile": manifest["profile"],
        "manifestSha256": manifest["manifestSha256"],
        "shardIndex": shard_index,
        "shardCount": manifest["shardCount"],
        "results": [
            {"id": test_id, "status": status, "variants": []}
            for test_id in shard["testIds"]
        ],
        "workerError": None,
    }


class ConformancePromotionGateTests(unittest.TestCase):
    def test_complete_successful_shards_allow_aggregate(self) -> None:
        manifest = build_manifest(shard_count=2)
        reports = [
            report_for(manifest, shard["index"])
            for shard in manifest["shards"]
        ]

        aggregate = test_runner.aggregate_reports(
            manifest, reports, BASELINE, ready_profile(), TEST262_ROOT
        )

        self.assertTrue(aggregate["aggregation"]["complete"])
        self.assertEqual(
            aggregate["summary"]["PASS"],
            len(manifest["tests"]),
        )

    def test_failed_shard_result_blocks_aggregate(self) -> None:
        manifest = build_manifest(shard_count=1)
        report = report_for(manifest, shard_index=0, status="FAIL")

        aggregate = test_runner.aggregate_reports(
            manifest, [report], BASELINE, ready_profile(), TEST262_ROOT
        )

        self.assertFalse(aggregate["aggregation"]["complete"])
        self.assertTrue(
            any("blocking aggregate status FAIL" in error
                for error in aggregate["aggregation"]["errors"])
        )

    def test_missing_shard_result_blocks_aggregate(self) -> None:
        manifest = build_manifest(shard_count=2)
        report = report_for(manifest, shard_index=0)

        aggregate = test_runner.aggregate_reports(
            manifest, [report], BASELINE, ready_profile(), TEST262_ROOT
        )

        self.assertFalse(aggregate["aggregation"]["complete"])
        self.assertEqual(aggregate["aggregation"]["missingShards"], [1])

    def test_adapter_shutdown_error_blocks_aggregate(self) -> None:
        manifest = build_manifest(shard_count=1)
        report = report_for(manifest, shard_index=0)
        report["workerError"] = "adapter exited with code 9 after the final response"

        aggregate = test_runner.aggregate_reports(
            manifest, [report], BASELINE, ready_profile(), TEST262_ROOT
        )

        self.assertFalse(aggregate["aggregation"]["complete"])
        self.assertTrue(
            any("adapter worker error" in error
                for error in aggregate["aggregation"]["errors"])
        )

    def test_unimplemented_runtime_profile_rejects_apparently_passing_shards(self) -> None:
        profile = ready_profile()
        profile["execution"] = {"state": "not_implemented"}
        manifest = test_runner.build_manifest(
            "test262",
            test_runner.discover_test_ids(TEST262_ROOT, "test262"),
            BASELINE,
            profile,
            1,
        )
        report = report_for(manifest, shard_index=0)

        aggregate = test_runner.aggregate_reports(
            manifest, [report], BASELINE, profile, TEST262_ROOT
        )

        self.assertFalse(aggregate["aggregation"]["complete"])
        self.assertTrue(
            any("requires an implemented execution adapter" in error
                for error in aggregate["aggregation"]["errors"])
        )


if __name__ == "__main__":
    unittest.main()
