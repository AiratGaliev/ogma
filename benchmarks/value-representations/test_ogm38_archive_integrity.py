"""Integrity checks tying the OGM-38 raw archive to its complete benchmark matrix."""

from __future__ import annotations

import hashlib
import json
import os
import unittest
from pathlib import Path

import decode_ogm38_raw_archive as raw_archive


HERE = Path(__file__).resolve().parent
RESULTS = HERE / "results"
EXPECTED_CANDIDATES = {
    "tagged-16-boxed",
    "tagged-16-representation-aware",
    "nanbox-8-boxed",
    "nanbox-8-representation-aware",
}
EXPECTED_COLLECTORS = {"rc-cycle-removal", "non-moving-mark-sweep"}
EXPECTED_ALLOCATORS = {
    "system-allocator",
    "per-isolate-exact-reuse",
    "per-isolate-size-class-pool",
}
COLLECTOR_METRICS = {
    "build_ns",
    "collection_ns",
    "root_scan_ns",
    "cleanup_collection_ns",
}
ALLOCATOR_METRICS = {"allocation_ns", "deallocation_ns"}
SEMANTIC_ADD_CASES = {
    "number-add",
    "string-concat",
    "bigint-add",
    "object-to-primitive-add",
    "symbol-add-throws",
    "bigint-number-add-throws",
}


class Ogm38ArchiveIntegrityTests(unittest.TestCase):
    def test_archive_covers_the_complete_candidate_gc_allocator_matrix(self):
        compressed, report = raw_archive.load_committed_archive()
        candidate_names = {item["name"] for item in report["candidates"]}
        self.assertEqual(EXPECTED_CANDIDATES, candidate_names)
        self.assertEqual(4, len(report["candidates"]))

        collector_workers = [item for item in report["workers"] if item["type"] == "collector"]
        allocator_workers = [item for item in report["workers"] if item["type"] == "allocator"]
        expected_gc = {
            (candidate, collector, allocator)
            for candidate in EXPECTED_CANDIDATES
            for collector in EXPECTED_COLLECTORS
            for allocator in EXPECTED_ALLOCATORS
        }
        actual_gc = {
            (item["candidate"], item["collector"], item["allocator"])
            for item in collector_workers
        }
        self.assertEqual(expected_gc, actual_gc)
        self.assertEqual(len(expected_gc), len(collector_workers))

        expected_alloc = {
            (candidate, allocator)
            for candidate in EXPECTED_CANDIDATES
            for allocator in EXPECTED_ALLOCATORS
        }
        actual_alloc = {
            (item["candidate"], item["allocator"])
            for item in allocator_workers
        }
        self.assertEqual(expected_alloc, actual_alloc)
        self.assertEqual(len(expected_alloc), len(allocator_workers))

        expected_metric_names = set()
        for candidate in report["candidates"]:
            name = candidate["name"]
            self.assertEqual(31, candidate["native_semantic_case_count"])
            self.assertEqual(31, candidate["native_semantic_batch_dispatch_invocations"])
            self.assertEqual(77, candidate["native_value_dispatches"])
            self.assertEqual(SEMANTIC_ADD_CASES, set(candidate["semantic_add_cases"]))
            expected_metric_names.update({
                f"{name}|compile_ns",
                f"{name}|loop_ns_per_call",
                f"{name}|handle_dispatch_ns_per_call",
            })
            expected_metric_names.update(
                f"{name}|semantic_add|{case}" for case in SEMANTIC_ADD_CASES
            )

        for item in collector_workers:
            name = item["candidate"]
            self.assertEqual(COLLECTOR_METRICS, set(item["variable_fields"]))
            self.assertEqual(item["allocator"], item["constant_fields"]["allocator_strategy"])
            self.assertEqual(
                item["rss_baseline_bytes"] + item["rss_delta_bytes"],
                item["rss_peak_bytes"],
            )
            for metric in COLLECTOR_METRICS:
                expected_metric_names.add(
                    f"{name}|collector|{item['collector']}|{item['allocator']}|{metric}"
                )

        for item in allocator_workers:
            self.assertEqual(ALLOCATOR_METRICS, set(item["variable_fields"]))
            for metric in ALLOCATOR_METRICS:
                expected_metric_names.add(
                    f"{item['candidate']}|allocator|{item['allocator']}|{metric}"
                )

        actual_metric_names = {item[0] for item in report["metric_arrays"]}
        self.assertEqual(expected_metric_names, actual_metric_names)
        self.assertEqual(156, len(expected_metric_names))

        for metric_name, first, deltas in report["metric_arrays"]:
            samples = [first]
            for delta in deltas:
                samples.append(samples[-1] + delta)
            self.assertEqual(101, len(samples), metric_name)
            self.assertTrue(all(value >= 0 for value in samples), metric_name)

    def test_manifest_and_historical_projection_have_independent_provenance(self):
        compressed, report = raw_archive.load_committed_archive()
        raw_json = __import__("lzma").decompress(compressed)
        manifest_path = RESULTS / "ogm38-latest-evidence.json"
        manifest_bytes = manifest_path.read_bytes()
        manifest = json.loads(manifest_bytes)
        raw = manifest["raw_dataset"]

        self.assertEqual(report["source_revision"], manifest["source_revision"])
        self.assertEqual(report["tested_revision"], manifest["tested_revision"])
        self.assertEqual(raw_archive.EXPECTED_SOURCE_REVISION, report["source_revision"])
        self.assertEqual(hashlib.sha256(compressed).hexdigest(), raw["xz_sha256"])
        self.assertEqual(hashlib.sha256(raw_json).hexdigest(), raw["decoded_json_sha256"])
        self.assertEqual(len(compressed), raw["xz_bytes"])
        self.assertEqual(len(raw_json), raw["decoded_json_bytes"])
        self.assertEqual(len(report["metric_arrays"]), raw["series_count"])
        self.assertEqual(len(report["workers"]), raw["worker_count"])
        self.assertEqual(raw_archive.EXPECTED_PART_COUNT, raw["fragment_count"])
        self.assertEqual(raw_archive.EXPECTED_PART_LENGTHS, raw["fragment_lengths_bytes"])

        parts_dir = HERE / "results" / "ogm38-raw-native-samples.delta.json.xz.b64.parts"
        encoded = b"".join(
            (parts_dir / f"part-{index:02d}.b64").read_bytes()
            for index in range(raw_archive.EXPECTED_PART_COUNT)
        )
        self.assertEqual(raw_archive.EXPECTED_BASE64_BYTES, len(encoded))
        # The exporter stores a trailing newline after the Base64 payload.
        encoded_sha = hashlib.sha256(encoded + bytes([10])).hexdigest()
        self.assertEqual(encoded_sha, raw["base64_file_sha256"])
        self.assertEqual(encoded_sha, manifest["checksums"]["raw_delta_base64_file_sha256"])
        self.assertEqual(raw["xz_sha256"], manifest["checksums"]["raw_delta_xz_sha256"])
        self.assertEqual(
            raw["decoded_json_sha256"],
            manifest["checksums"]["raw_delta_json_sha256"],
        )

        snapshot_run = manifest["workflow_run"]
        self.assertEqual(45130525848, snapshot_run["id"])
        self.assertEqual("success", snapshot_run["jobs"]["fast_checks"])
        self.assertEqual("success", snapshot_run["jobs"]["native_value_ssa_evidence"])
        self.assertEqual(14798285147, snapshot_run["artifact"]["id"])
        self.assertEqual(report["source_revision"], snapshot_run["tested_revision"])

        active_results_env = os.environ.get("OGM38_RESULTS_DIR")
        if active_results_env:
            active_results = Path(active_results_env)
        elif os.environ.get("RUNNER_TEMP"):
            active_results = Path(os.environ["RUNNER_TEMP"]) / "ogm38-committed-results"
        else:
            active_results = None

        # This checksum is pinned to the immutable snapshot-producing run in
        # the manifest; each later CI run emits a new live projection in its artifact.
        self.assertRegex(
            manifest["checksums"]["primary_projection_sha256"],
            r"^[0-9a-f]{64}$",
        )

        primary_path = (
            active_results / "ogm38-primary-benchmark-samples.json"
            if active_results is not None
            else None
        )
        if primary_path is not None and primary_path.is_file():
            # The specialized CI job first runs this suite before exporting the
            # report, then runs it again after export. Validate the live projection
            # against this run's native report and workflow identity in the latter phase.
            primary = json.loads(primary_path.read_bytes())
            self.assertEqual(primary["source_revision"], primary["tested_revision"])
            expected_head_revision = os.environ.get("OGM38_EXPECTED_REVISION")
            event_path = os.environ.get("GITHUB_EVENT_PATH")
            if expected_head_revision is None and event_path:
                event_file = Path(event_path)
                if event_file.is_file():
                    event = json.loads(event_file.read_text(encoding="utf-8"))
                    expected_head_revision = (
                        event.get("pull_request", {}).get("head", {}).get("sha")
                    )
            if expected_head_revision:
                self.assertEqual(expected_head_revision, primary["source_revision"])
            current_run_id = os.environ.get("GITHUB_RUN_ID")
            server_url = os.environ.get("GITHUB_SERVER_URL")
            repository = os.environ.get("GITHUB_REPOSITORY")
            if current_run_id and server_url and repository:
                current_run_url = f"{server_url}/{repository}/actions/runs/{current_run_id}"
                self.assertEqual(current_run_url, primary["run_url"])
                self.assertEqual(
                    current_run_url + "/artifacts",
                    primary["full_report_artifact_url"],
                )
            self.assertEqual(
                EXPECTED_CANDIDATES,
                {item["name"] for item in primary["candidates"]},
            )
            self.assertEqual(101, primary["samples_per_metric"])
            for candidate in primary["candidates"]:
                self.assertEqual(101, len(candidate["compile_ns"]))
                self.assertEqual(101, len(candidate["native_loop_ns_per_call"]))
                self.assertTrue(all(value >= 0 for value in candidate["compile_ns"]))
                self.assertTrue(
                    all(value >= 0 for value in candidate["native_loop_ns_per_call"])
                )

        # The older primary projection is a separate historical snapshot and no
        # longer occupies the active projection path after raw-snapshot rotation.
        projection_meta = manifest["historical_primary_projection"]
        historical_primary_path = RESULTS / "historical" / "ogm38-primary-benchmark-samples-dfea7.json"
        historical_primary_bytes = historical_primary_path.read_bytes()
        historical_primary = json.loads(historical_primary_bytes)
        self.assertEqual(
            "reference-only-separate-snapshot",
            projection_meta["status"],
        )
        self.assertEqual(
            hashlib.sha256(historical_primary_bytes).hexdigest(),
            projection_meta["sha256"],
        )
        self.assertEqual(historical_primary["source_revision"], projection_meta["source_revision"])
        self.assertEqual(historical_primary["tested_revision"], projection_meta["tested_revision"])
        self.assertEqual(historical_primary["run_url"], projection_meta["run_url"])
        self.assertEqual(
            historical_primary["full_report_artifact_url"],
            projection_meta["artifact_url"],
        )
        self.assertEqual(44730458468, projection_meta["workflow_run_id"])
        self.assertEqual(14625194465, projection_meta["artifact_id"])

        historical_compressed, historical_report = raw_archive.load_committed_archive(
            raw_archive.HISTORICAL_PARTS_DIR,
            expectation=raw_archive.HISTORICAL_ARCHIVE,
        )
        historical_json = __import__("lzma").decompress(historical_compressed)
        historical = manifest["historical_raw_snapshot"]
        self.assertEqual("74991baa54b346389b6193c1f7075954055be181", historical_report["source_revision"])
        self.assertEqual(historical["source_revision"], historical_report["source_revision"])
        self.assertEqual(historical["tested_revision"], historical_report["tested_revision"])
        self.assertEqual(
            hashlib.sha256(historical_compressed).hexdigest(),
            historical["xz_sha256"],
        )
        self.assertEqual(
            hashlib.sha256(historical_json).hexdigest(),
            historical["decoded_json_sha256"],
        )
        historical_parts = raw_archive.HISTORICAL_PARTS_DIR
        historical_encoded = b"".join(
            (historical_parts / f"part-{index:02d}.b64").read_bytes()
            for index in range(raw_archive.HISTORICAL_ARCHIVE.part_count)
        )
        self.assertEqual(raw_archive.HISTORICAL_ARCHIVE.base64_bytes, len(historical_encoded))
        self.assertEqual(
            hashlib.sha256(historical_encoded + bytes([10])).hexdigest(),
            historical["base64_file_sha256"],
        )
        self.assertEqual(raw_archive.HISTORICAL_ARCHIVE.part_count, historical["fragment_count"])
        self.assertEqual(
            list(raw_archive.HISTORICAL_ARCHIVE.part_lengths),
            historical["fragment_lengths_bytes"],
        )
        self.assertNotEqual(report["source_revision"], historical_report["source_revision"])
        self.assertNotEqual(report["source_revision"], historical_primary["source_revision"])

    def test_manifest_records_the_pre_integrity_test_verification(self):
        manifest = json.loads((RESULTS / "ogm38-latest-evidence.json").read_text())
        verification = manifest["pre_integrity_test_verification"]
        self.assertEqual(45055209774, verification["id"])
        self.assertEqual("7f84ccba117047218fcbf2954f5463e0764c6692", verification["tested_revision"])
        self.assertEqual("success", verification["jobs"]["fast_checks"])
        self.assertEqual("success", verification["jobs"]["native_value_ssa_evidence"])
        self.assertEqual("passed", verification["fast_checks"]["result"])
        self.assertEqual(203, verification["fast_checks"]["python_tests"])
        self.assertEqual(42, verification["focused_python_tests"]["value_representation"]["ran"])
        self.assertEqual(8, verification["focused_python_tests"]["dual_backend"]["ran"])
        self.assertEqual(65, verification["rust_unit_tests"]["ran"])


if __name__ == "__main__":
    unittest.main()
