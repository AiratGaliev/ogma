"""Tests for the OGM-38 / OGM-31 machine-report comparison."""

from __future__ import annotations

import gzip
import hashlib
import json
import os
import math
import unittest
from pathlib import Path

import export_ogm38_raw_samples as exporter
import validate_ogm38_ogm31 as proof


def make_reports():
    native = {
        "task": "OGM-38",
        "schema_version": 1,
        "tested_revision": "candidate-revision",
        "candidates": [],
    }
    expected = {
        "branch_equal": 7.0,
        "branch_unequal": 8.0,
        "loop_integer": 5.0,
        "loop_fractional": 3.5,
        "dynamic_add_normal": 15.0,
    }
    dual = {
        "task": "OGM-31",
        "schema": "ogma.dual-backend-proof-report",
        "tested_revision": "oracle-revision",
        "source_fixtures": [{"id": item} for item in sorted(proof.EXPECTED_FIXTURES)],
        "differential_observations": {
            "all_equal": True,
            "portable_register": {
                **{name: {"return": value, "host_trace": []} for name, value in expected.items()},
                "dynamic_add_throw": {
                    "throw": "TypeError: Cannot convert a Symbol value to a number",
                    "host_trace": [],
                },
                "call_boundary": {
                    "return": 8,
                    "host_trace": [{
                        "symbol": "benchmark.host.call_target",
                        "args": [2, "item=5"],
                    }],
                },
                "host_call": {
                    "return": 42,
                    "host_trace": [{
                        "symbol": "benchmark.host.echo",
                        "args": [31],
                    }],
                },
            },
        },
    }
    for name in sorted(proof.EXPECTED_CANDIDATES):
        native["candidates"].append({
            "name": name,
            "native_core_ir_corpus": {
                "canonical_fixture_count": len(proof.EXPECTED_FIXTURE_PATHS),
                "fixtures": [
                    {
                        "fixture": fixture,
                        "path": path,
                        "native_batch_invocations": 1,
                        "native_value_dispatches": 1,
                        "reference_values_resolved_after_forced_gc": 1,
                    }
                    for fixture, path in proof.EXPECTED_FIXTURE_PATHS.items()
                ],
                "native_batch_invocations": len(proof.EXPECTED_FIXTURE_PATHS),
                "native_value_dispatches": len(proof.EXPECTED_FIXTURE_PATHS),
                "reference_values_resolved_after_forced_gc": len(proof.EXPECTED_FIXTURE_PATHS),
                "all_values_roundtripped_and_dispatched": True,
            },
            "native_code": {
                "symbol": "ogma_value_loop",
                "numeric_conversion_instructions": 2,
                "call_instructions": 0,
            },
            "modeled_ir_value_conversion_ops_per_loop_iteration": 5,
            "modeled_ir_value_conversion_ops_per_invocation": 0,
            "native_core_ir_execution": {
                "strict_equal_native_code": {
                    "symbol": "ogma_value_strict_equal",
                    "numeric_conversion_instructions": 0,
                    "call_instructions": 1,
                },
                "branch_equal_result_bits": proof.number_to_bits(7.0),
                "branch_unequal_result_bits": proof.number_to_bits(8.0),
                "loop_integer_result_bits": proof.number_to_bits(5.0),
                "loop_fractional_result_bits": proof.number_to_bits(3.5),
                "dynamic_add_result_bits": proof.number_to_bits(15.0),
                "dynamic_add_throw_status": 2,
                "symbol_handle_survived_forced_collection": True,
                "all_native_fixture_cases_match_expected": True,
            },
            "native_value_semantics": {
                "every_case_matches": True,
                "cases": [
                    {
                        "name": "nan-strict-equality",
                        "matches": True,
                        "native_strict_equal_executed": True,
                        "native_reference_identity_preserved": True,
                        "native_reference_equality_result": False,
                    },
                    {
                        "name": "signed-zero-strict-equality",
                        "matches": True,
                        "native_strict_equal_executed": True,
                        "native_reference_identity_preserved": True,
                        "native_reference_equality_result": True,
                    },
                    {
                        "name": "object-identity-same",
                        "matches": True,
                        "native_strict_equal_executed": True,
                        "native_reference_identity_preserved": True,
                        "native_reference_equality_result": True,
                    },
                    {
                        "name": "object-identity-distinct",
                        "matches": True,
                        "native_strict_equal_executed": True,
                        "native_reference_identity_preserved": True,
                        "native_reference_equality_result": False,
                    },
                    {
                        "name": "symbol-identity-same",
                        "matches": True,
                        "native_strict_equal_executed": True,
                        "native_reference_identity_preserved": True,
                        "native_reference_equality_result": True,
                    },
                    {
                        "name": "symbol-identity-distinct",
                        "matches": True,
                        "native_strict_equal_executed": True,
                        "native_reference_identity_preserved": True,
                        "native_reference_equality_result": False,
                    },
                    {
                        "name": "undefined-strict-equality",
                        "matches": True,
                        "native_strict_equal_executed": True,
                        "native_reference_identity_preserved": True,
                        "native_reference_equality_result": True,
                    },
                    {
                        "name": "null-strict-equality",
                        "matches": True,
                        "native_strict_equal_executed": True,
                        "native_reference_identity_preserved": True,
                        "native_reference_equality_result": True,
                    },
                    {
                        "name": "undefined-null-strict-equality",
                        "matches": True,
                        "native_strict_equal_executed": True,
                        "native_reference_identity_preserved": True,
                        "native_reference_equality_result": False,
                    },
                    {
                        "name": "boolean-strict-equality-same",
                        "matches": True,
                        "native_strict_equal_executed": True,
                        "native_reference_identity_preserved": True,
                        "native_reference_equality_result": True,
                    },
                    {
                        "name": "boolean-strict-equality-distinct",
                        "matches": True,
                        "native_strict_equal_executed": True,
                        "native_reference_identity_preserved": True,
                        "native_reference_equality_result": False,
                    },
                    {
                        "name": "positive-infinity-strict-equality",
                        "matches": True,
                        "native_strict_equal_executed": True,
                        "native_reference_identity_preserved": True,
                        "native_reference_equality_result": True,
                    },
                    {
                        "name": "negative-infinity-strict-equality",
                        "matches": True,
                        "native_strict_equal_executed": True,
                        "native_reference_identity_preserved": True,
                        "native_reference_equality_result": True,
                    },
                    {
                        "name": "opposite-infinity-strict-equality",
                        "matches": True,
                        "native_strict_equal_executed": True,
                        "native_reference_identity_preserved": True,
                        "native_reference_equality_result": False,
                    },
                    {
                        "name": "string-strict-equality-same",
                        "matches": True,
                        "native_strict_equal_executed": True,
                        "native_reference_value_comparison_executed": True,
                        "native_reference_identity_preserved": True,
                        "native_reference_equality_result": True,
                    },
                    {
                        "name": "string-strict-equality-distinct",
                        "matches": True,
                        "native_strict_equal_executed": True,
                        "native_reference_value_comparison_executed": True,
                        "native_reference_identity_preserved": True,
                        "native_reference_equality_result": False,
                    },
                    {
                        "name": "bigint-strict-equality-same",
                        "matches": True,
                        "native_strict_equal_executed": True,
                        "native_reference_value_comparison_executed": True,
                        "native_reference_identity_preserved": True,
                        "native_reference_equality_result": True,
                    },
                    {
                        "name": "bigint-strict-equality-distinct",
                        "matches": True,
                        "native_strict_equal_executed": True,
                        "native_reference_value_comparison_executed": True,
                        "native_reference_identity_preserved": True,
                        "native_reference_equality_result": False,
                    },
                    {
                        "name": "call-boundary",
                        "matches": True,
                        "native_strict_equal_executed": True,
                        "native_reference_equality_result": True,
                        "actual_host_trace": ["benchmark.host.call_target([2,\"item=5\"])"],
                        "native_host_callback": {
                            "callee_resolved": True,
                            "receiver": 2.0,
                            "argument": "item=5",
                            "result_bits": proof.number_to_bits(8.0),
                            "host_callback_calls": 1,
                        },
                    },
                    {
                        "name": "host-call-echo",
                        "matches": True,
                        "actual_host_trace": ["benchmark.host.echo([31])"],
                        "native_host_callback": {
                            "symbol": "benchmark.host.echo",
                            "arguments": ["31"],
                            "result_bits": proof.number_to_bits(42.0),
                            "host_callback_calls": 1,
                        },
                    },
                ],
            },
            "native_shadow_roots": {
                "all_roots_survived": True,
                "all_roots_reclaimed": True,
            },
        })
    return native, dual


class Ogm38Ogm31ValidationTests(unittest.TestCase):
    @unittest.skipUnless(os.environ.get("OGM38_RESULTS_DIR"), "requires freshly exported CI evidence")
    def test_committed_primary_native_samples_are_complete_and_pinned(self):
        results = Path(os.environ["OGM38_RESULTS_DIR"])
        report = json.loads(
            (results / "ogm38-primary-benchmark-samples.json").read_text(encoding="utf-8")
        )

        self.assertEqual("ogma.ogm38.primary-benchmark-samples", report["schema"])
        self.assertEqual(2, report["version"])
        self.assertEqual(report["source_revision"], report["tested_revision"])
        self.assertEqual(40, len(report["source_revision"]))
        self.assertEqual(101, report["samples_per_metric"])
        self.assertEqual(3, report["warmups"])
        self.assertEqual(1000, report["iterations_per_throughput_sample"])
        self.assertEqual("x86_64-unknown-linux-gnu", report["target"])
        self.assertTrue(report["run_url"].startswith("https://github.com/AiratGaliev/ogma/actions/runs/"))
        self.assertTrue(report["full_report_artifact_url"].startswith("https://github.com/AiratGaliev/ogma/actions/runs/"))
        self.assertEqual(64, len(report["full_report_sha256"]))
        self.assertEqual(64, len(report["differential_report_sha256"]))

        expected_candidates = {
            "tagged-16-boxed",
            "tagged-16-representation-aware",
            "nanbox-8-boxed",
            "nanbox-8-representation-aware",
        }
        candidates = report["candidates"]
        self.assertEqual(expected_candidates, {candidate["name"] for candidate in candidates})
        self.assertEqual(4, len(candidates))
        for candidate in candidates:
            with self.subTest(candidate=candidate["name"]):
                self.assertIn(candidate["runtime_value_bytes"], (8, 16))
                for metric in ("compile_ns", "native_loop_ns_per_call"):
                    samples = candidate[metric]
                    self.assertEqual(101, len(samples), metric)
                    self.assertTrue(
                        all(
                            isinstance(value, (int, float))
                            and not isinstance(value, bool)
                            and math.isfinite(value)
                            and value > 0
                            for value in samples
                        ),
                        metric,
                    )
                self.assertTrue(candidate["all_semantic_cases_match"])
                self.assertEqual(31, candidate["semantic_cases_passed"])
                self.assertEqual(5, candidate["native_batch_invocations"])
                self.assertEqual(77, candidate["native_value_dispatches"])
                self.assertEqual(20, candidate["reference_values_resolved_after_forced_gc"])

    @unittest.skipUnless(os.environ.get("OGM38_RESULTS_DIR"), "requires freshly exported CI evidence")
    def test_committed_runtime_raw_samples_cover_integrated_gc_allocator_and_peak_rss(self):
        path = Path(os.environ["OGM38_RESULTS_DIR"]) / "ogm38-runtime-raw-samples.json.gz"
        report = json.loads(gzip.decompress(path.read_bytes()).decode("utf-8"))

        self.assertEqual("ogma.ogm38.runtime-raw-samples", report["schema"])
        self.assertEqual(2, report["version"])
        self.assertEqual(report["source_revision"], report["tested_revision"])
        self.assertEqual(40, len(report["source_revision"]))
        self.assertEqual(101, report["sample_count"])
        self.assertEqual(3, report["warmups"])
        self.assertEqual("x86_64-unknown-linux-gnu", report["target"])
        self.assertIn("VmHWM", report["rss_method"])
        self.assertIn("isolated child", report["rss_method"])

        expected_candidates = {
            "tagged-16-boxed",
            "tagged-16-representation-aware",
            "nanbox-8-boxed",
            "nanbox-8-representation-aware",
        }
        expected_pairs = {
            (collector, allocator)
            for collector in ("rc-cycle-removal", "non-moving-mark-sweep")
            for allocator in (
                "system-allocator",
                "per-isolate-exact-reuse",
                "per-isolate-size-class-pool",
            )
        }
        candidates = report["candidates"]
        self.assertEqual(expected_candidates, {item["name"] for item in candidates})
        self.assertEqual(4, len(candidates))
        for candidate in candidates:
            with self.subTest(candidate=candidate["name"]):
                comparisons = candidate["collector_comparisons"]
                self.assertEqual(6, len(comparisons))
                self.assertEqual(
                    expected_pairs,
                    {(item["collector"], item["allocator_strategy"]) for item in comparisons},
                )
                for item in comparisons:
                    self.assertEqual(101, len(item["raw_samples"]))
                    self.assertGreater(item["process_peak_rss_baseline_bytes"], 0)
                    self.assertGreater(item["process_peak_rss_bytes"], 0)
                    self.assertGreaterEqual(
                        item["process_peak_rss_bytes"],
                        item["process_peak_rss_baseline_bytes"],
                    )
                    self.assertEqual(
                        item["process_peak_rss_bytes"] - item["process_peak_rss_baseline_bytes"],
                        item["process_peak_rss_delta_bytes"],
                    )
                    self.assertEqual(101, len(item["build_ns"]))
                    self.assertEqual(101, len(item["collection_ns"]))
                    self.assertEqual(101, len(item["root_scan_ns"]))
                    self.assertEqual(101, len(item["cleanup_collection_ns"]))
                    self.assertGreater(item["actual_value_storage_capacity_bytes"], 0)

                allocator = candidate["allocator_comparisons"]
                self.assertEqual(
                    {
                        "system-allocator",
                        "per-isolate-exact-reuse",
                        "per-isolate-size-class-pool",
                    },
                    {item["strategy"] for item in allocator},
                )
                for item in allocator:
                    self.assertEqual(101, len(item["raw_samples"]))
                    self.assertEqual(101, len(item["allocation_ns_per_sample"]))
                    self.assertEqual(101, len(item["deallocation_ns_per_sample"]))
                    self.assertEqual(0, item["allocation_failures_total"])

    @unittest.skipUnless(os.environ.get("OGM38_RESULTS_DIR"), "requires freshly exported CI evidence")
    def test_committed_full_native_and_differential_reports_match_the_projections(self):
        results = Path(__file__).parent / "results"
        primary = json.loads(
            (results / "ogm38-primary-benchmark-samples.json").read_text(encoding="utf-8")
        )
        native_bytes = gzip.decompress(
            (results / "ogm38-native-value-ssa-report.json.gz").read_bytes()
        )
        differential_bytes = (
            results / "ogm38-ogm31-differential-report.json"
        ).read_bytes()
        self.assertEqual(primary["full_report_sha256"], hashlib.sha256(native_bytes).hexdigest())
        self.assertEqual(
            primary["differential_report_sha256"],
            hashlib.sha256(differential_bytes).hexdigest(),
        )
        native = json.loads(native_bytes.decode("utf-8"))
        differential = json.loads(differential_bytes.decode("utf-8"))
        exporter.validate_reports(native, differential)
        self.assertEqual(primary["source_revision"], native["source_revision"])
        self.assertEqual(primary["source_revision"], differential["native_revision"])
    def test_all_four_candidates_match_the_same_ogm31_oracle(self):
        native, dual = make_reports()
        result = proof.validate_reports(native, dual)
        self.assertTrue(result["all_candidates_match_portable_ogm31_oracle"])
        self.assertEqual(4, result["candidate_count"])
        self.assertEqual("oracle-revision", result["oracle_revision"])

    def test_rejects_native_result_that_differs_from_portable_oracle(self):
        native, dual = make_reports()
        native["candidates"][0]["native_core_ir_execution"]["branch_equal_result_bits"] = (
            proof.number_to_bits(8.0)
        )
        with self.assertRaisesRegex(RuntimeError, "does not match OGM-31"):
            proof.validate_reports(native, dual)

    def test_rejects_a_missing_candidate_or_fixture(self):
        native, dual = make_reports()
        native["candidates"].pop()
        with self.assertRaisesRegex(RuntimeError, "candidate matrix differs"):
            proof.validate_reports(native, dual)

        native, dual = make_reports()
        dual["source_fixtures"].pop()
        with self.assertRaisesRegex(RuntimeError, "fixture set drifted"):
            proof.validate_reports(native, dual)

    def test_rejects_non_native_strict_equality_or_fixture_drift(self):
        native, dual = make_reports()
        first = native["candidates"][0]
        first["native_value_semantics"]["cases"][0]["native_strict_equal_executed"] = False
        with self.assertRaisesRegex(RuntimeError, "generated native strict equality"):
            proof.validate_reports(native, dual)

        native, dual = make_reports()
        first = native["candidates"][0]
        first["native_core_ir_corpus"]["fixtures"][0]["path"] = (
            "tests/core-ir/fixtures/unrelated.json"
        )
        with self.assertRaisesRegex(RuntimeError, "exact OGM-31 set"):
            proof.validate_reports(native, dual)

        native, dual = make_reports()
        first = native["candidates"][0]
        first["native_core_ir_execution"]["strict_equal_native_code"][
            "symbol"
        ] = "not-generated"
        with self.assertRaisesRegex(RuntimeError, "strict_equal"):
            proof.validate_reports(native, dual)

    def test_rejects_duplicate_semantic_case_names(self):
        native, dual = make_reports()
        cases = native["candidates"][0]["native_value_semantics"]["cases"]
        cases.append(dict(cases[0]))
        with self.assertRaisesRegex(RuntimeError, "semantic case names are duplicated"):
            proof.validate_reports(native, dual)

    def test_rejects_identity_only_string_and_bigint_equality(self):
        for case_name in (
            "string-strict-equality-same",
            "bigint-strict-equality-same",
        ):
            with self.subTest(case_name=case_name):
                native, dual = make_reports()
                semantic_cases = native["candidates"][0]["native_value_semantics"]["cases"]
                case = next(item for item in semantic_cases if item["name"] == case_name)
                case["native_reference_value_comparison_executed"] = False
                with self.assertRaisesRegex(RuntimeError, "did not compare primitive"):
                    proof.validate_reports(native, dual)

    def test_rejects_unbalanced_native_roots(self):
        native, dual = make_reports()
        native["candidates"][0]["native_shadow_roots"]["all_roots_reclaimed"] = False
        with self.assertRaisesRegex(RuntimeError, "roots were not balanced"):
            proof.validate_reports(native, dual)


if __name__ == "__main__":
    unittest.main()
