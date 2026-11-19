from __future__ import annotations

from pathlib import Path
import unittest

import run as footprint


ROOT = Path(__file__).resolve().parents[2]


class RuntimeFootprintTests(unittest.TestCase):
    def test_ogm28_profile_dependency_closures_are_distinct_and_fail_closed(self) -> None:
        document = footprint._read_json(
            ROOT / "contracts/runtime-profiles/v1/profiles.json"
        )
        portable = footprint._resolve_profile_plan(
            document, "portable-interpreter-release"
        )
        hybrid = footprint._resolve_profile_plan(document, "hybrid-native-release")

        self.assertIn("portable-interpreter", portable["resolved_components"])
        self.assertNotIn("native-aot-execution", portable["resolved_components"])
        self.assertIn("native-aot-execution", hybrid["resolved_components"])
        self.assertIn("portable-interpreter", hybrid["resolved_components"])
        self.assertIn("native-root-maps", hybrid["resolved_components"])
        self.assertNotIn("node-module-resolver", portable["resolved_components"])
        self.assertNotIn("node-module-resolver", hybrid["resolved_components"])
        self.assertNotEqual(
            set(portable["resolved_components"]), set(hybrid["resolved_components"])
        )

    def test_both_execution_paths_use_the_same_canonical_core_ir_fixtures(self) -> None:
        fixtures, _source_hashes, provenance = footprint._load_inputs()
        portable_paths = {
            item["core_ir"] for item in provenance["manifest"]["source_fixtures"]
        }
        native_paths = {
            item["fixture"] for item in provenance["native_manifest"]["workloads"]
        }
        self.assertEqual(5, len(fixtures))
        self.assertEqual(portable_paths, native_paths)
        self.assertEqual(
            {"branch", "loop", "dynamic_exception", "call_boundary", "host_call"},
            set(fixtures),
        )

    def test_latency_summary_retains_raw_samples_and_percentiles(self) -> None:
        result = footprint._distribution([10.0, 20.0, 30.0, 40.0, 50.0])
        self.assertEqual(5, result["sample_count"])
        self.assertEqual(30.0, result["p50_ns_per_call"])
        self.assertEqual([10.0, 20.0, 30.0, 40.0, 50.0], result["raw_ns_per_call_samples"])
        self.assertGreater(result["throughput_calls_per_second_from_p50"], 0)

    def test_native_host_call_overhead_uses_direct_helper_baseline(self) -> None:
        result = footprint._native_host_call_overhead({
            "workloads": {
                "core_ir_host_call": {
                    "p50_ns_per_call": 30.0,
                    "raw_ns_per_call_samples": [29.0, 30.0, 31.0],
                },
                "direct_host_helper": {
                    "p50_ns_per_call": 10.0,
                    "raw_ns_per_call_samples": [9.0, 10.0, 11.0],
                },
            }
        })
        self.assertEqual(20.0, result["estimated_dispatch_overhead_ns_per_call"])
        self.assertEqual(3.0, result["estimated_dispatch_overhead_ratio"])

    def test_time_report_requires_a_real_rss_measurement(self) -> None:
        import tempfile

        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "time.txt"
            path.write_text(
                "Maximum resident set size (kbytes): 1234\n"
                "Elapsed (wall clock) time (h:mm:ss or m:ss): 0:01.23\n",
                encoding="utf-8",
            )
            result = footprint._parse_time_report(path)
        self.assertEqual(1234 * 1024, result["peak_rss_bytes"])
        self.assertEqual("0:01.23", result["elapsed_wall_clock"])

    def test_native_runtime_metrics_are_not_fabricated_by_profile_contract(self) -> None:
        document = footprint._read_json(
            ROOT / "contracts/runtime-profiles/v1/profiles.json"
        )
        plan = footprint._resolve_profile_plan(document, "hybrid-native-release")
        self.assertEqual("logical-profile-contract-only", plan["runtime_package_graph_status"])
        self.assertIn("native-aot-execution", plan["resolved_components"])
        self.assertFalse(
            (ROOT / "Cargo.toml").is_file(),
            "M0 repository unexpectedly gained a root Cargo workspace; revisit the limitation before accepting this test.",
        )


if __name__ == "__main__":
    unittest.main()
