"""Tests for the OGM-38 / OGM-31 machine-report comparison."""

from __future__ import annotations

import gzip
import hashlib
import json
import math
import unittest
from pathlib import Path

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
                        "native_value_dispatches": 1,
                        "reference_values_resolved_after_forced_gc": 1,
                    }
                    for fixture, path in proof.EXPECTED_FIXTURE_PATHS.items()
                ],
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
    def test_committed_primary_native_samples_are_complete_and_pinned(self):
        path = (
            Path(__file__).parent
            / "results"
            / "ogm38-primary-benchmark-samples.json"
        )
        report = json.loads(path.read_text(encoding="utf-8"))

        self.assertEqual("ogma.ogm38.primary-benchmark-samples", report["schema"])
        self.assertEqual(1, report["version"])
        self.assertEqual(
            "dfea7ebc25eec61665c93cfff5470251e00519df",
            report["source_revision"],
        )
        self.assertEqual(report["source_revision"], report["tested_revision"])
        self.assertEqual(101, report["samples_per_metric"])
        self.assertEqual(3, report["warmups"])
        self.assertEqual(1000, report["iterations_per_throughput_sample"])
        self.assertEqual("x86_64-unknown-linux-gnu", report["target"])

        expected_candidates = {
            "tagged-16-boxed",
            "tagged-16-representation-aware",
            "nanbox-8-boxed",
            "nanbox-8-representation-aware",
        }
        candidates = report["candidates"]
        self.assertEqual(
            expected_candidates,
            {candidate["name"] for candidate in candidates},
        )
        self.assertEqual(4, len(candidates))
        for candidate in candidates:
            with self.subTest(candidate=candidate["name"]):
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

        self.assertTrue(report["run_url"].endswith("/44730458468"))
        self.assertEqual(
            "https://github.com/AiratGaliev/ogma/actions/runs/44730458468/artifacts/14625194465",
            report["full_report_artifact_url"],
        )
        self.assertEqual(
            "05de9c347b24ea27f3a91bae604c5a768519cfd2d92d6e28544b4b44ec2c49a1",
            report["full_report_sha256"],
        )

    def test_committed_runtime_raw_samples_cover_gc_and_allocator(self):
        path = (
            Path(__file__).parent
            / "results"
            / "ogm38-runtime-raw-samples.json.gz"
        )
        raw_bytes = path.read_bytes()
        self.assertEqual(
            "e250675df6142ef5c9e21fad0645d4986d9de4bdafd5ad1a2dbfc799c54dd32e",
            hashlib.sha256(raw_bytes).hexdigest(),
        )
        report = json.loads(gzip.decompress(raw_bytes).decode("utf-8"))

        self.assertEqual("ogma.ogm38.runtime-raw-samples", report["schema"])
        self.assertEqual(1, report["version"])
        self.assertEqual(
            "dfea7ebc25eec61665c93cfff5470251e00519df",
            report["source_revision"],
        )
        self.assertEqual(report["source_revision"], report["tested_revision"])
        self.assertEqual(101, report["sample_count"])
        self.assertEqual(3, report["warmups"])
        self.assertEqual("x86_64-unknown-linux-gnu", report["target"])

        expected_candidates = {
            "tagged-16-boxed",
            "tagged-16-representation-aware",
            "nanbox-8-boxed",
            "nanbox-8-representation-aware",
        }
        candidates = report["candidates"]
        self.assertEqual(expected_candidates, {item["name"] for item in candidates})
        self.assertEqual(4, len(candidates))

        expected_collectors = {"non-moving-mark-sweep", "rc-cycle-removal"}
        expected_allocators = {
            "system-allocator",
            "per-isolate-exact-reuse",
            "per-isolate-size-class-pool",
        }
        for candidate in candidates:
            with self.subTest(candidate=candidate["name"]):
                self.assertEqual(expected_collectors, set(candidate["gc"]))
                self.assertEqual(expected_allocators, set(candidate["allocator"]))

                for collector, metrics in candidate["gc"].items():
                    for metric in (
                        "collection_ns",
                        "root_scan_ns",
                        "release_collection_ns",
                    ):
                        samples = metrics[metric]
                        self.assertEqual(101, len(samples), f"{collector}.{metric}")
                        self.assertTrue(all(value > 0 for value in samples))
                    self.assertGreater(metrics["peak_logical_heap_bytes"], 0)

                for strategy, metrics in candidate["allocator"].items():
                    for metric in ("allocation_ns", "deallocation_ns"):
                        samples = metrics[metric]
                        self.assertEqual(101, len(samples), f"{strategy}.{metric}")
                        self.assertTrue(all(value > 0 for value in samples))
                    self.assertEqual(0, metrics["allocation_failures"])
                    self.assertGreater(metrics["peak_reserved_bytes"], 0)
                    self.assertGreaterEqual(
                        metrics["peak_internal_fragmentation_bytes"],
                        0,
                    )

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
