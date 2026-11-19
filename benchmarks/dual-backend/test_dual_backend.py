"""Regression tests for the OGM-31 common-Core-IR differential proof."""

from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(Path(__file__).resolve().parent))

import run as proof  # noqa: E402
from prototype import JSException, JSSymbol, compile_function  # noqa: E402


class DualBackendProofTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.fixtures, cls.source_hashes = proof.load_canonical_fixtures()

    def test_same_canonical_fixture_set_is_pinned_for_portable_and_native(self) -> None:
        self.assertEqual(
            {"branch", "loop", "dynamic_exception", "call_boundary", "host_call"},
            set(self.fixtures),
        )
        self.assertEqual(5, len(self.source_hashes))
        native_manifest = proof.read_json(
            ROOT / "benchmarks/backend-codegen/fixtures/workloads.json"
        )
        native_paths = {item["fixture"] for item in native_manifest["workloads"]}
        proof_manifest = proof.read_json(ROOT / "benchmarks/dual-backend/manifest.json")
        self.assertEqual(
            native_paths,
            {item["core_ir"] for item in proof_manifest["source_fixtures"]},
        )

    def test_stack_and_register_interpreters_match_all_common_observations(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            register, _ = proof.run_portable_observations(
                self.fixtures, "register", Path(directory)
            )
        with tempfile.TemporaryDirectory() as directory:
            stack, _ = proof.run_portable_observations(
                self.fixtures, "stack", Path(directory)
            )
        self.assertEqual(register, stack)
        self.assertEqual({"return": 7.0, "host_trace": []}, register["branch_equal"])
        self.assertEqual({"return": 8.0, "host_trace": []}, register["branch_unequal"])
        self.assertEqual({"return": 5.0, "host_trace": []}, register["loop_integer"])
        self.assertEqual({"return": 3.5, "host_trace": []}, register["loop_fractional"])
        self.assertEqual({"return": 15, "host_trace": []}, register["dynamic_add_normal"])
        self.assertEqual(
            {
                "throw": "TypeError: Cannot convert a Symbol value to a number",
                "host_trace": [],
            },
            register["dynamic_add_throw"],
        )
        self.assertEqual(
            {
                "return": 8,
                "host_trace": [{
                    "symbol": "benchmark.host.call_target",
                    "args": [2, "item=5"],
                }],
            },
            register["call_boundary"],
        )
        self.assertEqual(
            {
                "return": 42,
                "host_trace": [{
                    "symbol": "benchmark.host.echo",
                    "args": [31],
                }],
            },
            register["host_call"],
        )

    def test_ogmb_is_verified_and_tampering_is_rejected(self) -> None:
        candidate = compile_function(self.fixtures["branch"], "register")
        result = proof.verify_invalid_bytecode(candidate)
        self.assertTrue(result["rejected"])

    def test_literal_constants_are_not_runtime_aot_operations(self) -> None:
        workload = next(
            item for item in proof.build_workloads() if item["id"] == "property-access"
        )
        supported = {
            operation
            for item in proof.read_json(
                ROOT / "benchmarks/backend-codegen/fixtures/workloads.json"
            )["workloads"]
            for operation in item["operations"]
        }
        unsupported = proof.unsupported_aot_operations(
            workload["functions"][0]["function"],
            supported,
        )
        self.assertEqual(["js.get_property", "js.set_property"], unsupported)

    def test_property_operations_use_verified_portable_fallback(self) -> None:
        result = proof.run_property_fallback()
        self.assertEqual(
            ["js.get_property", "js.set_property"],
            result["unsupported_operations"],
        )
        self.assertEqual("portable-register-bytecode-interpreter", result["selected_fallback"])
        self.assertEqual(result["expected"], result["observed"])
        self.assertTrue(result["stack_register_equivalence"])

    def test_aot_exception_handler_with_observable_work_is_rejected(self) -> None:
        document = json.loads(json.dumps(self.fixtures["dynamic_exception"]))
        handler = next(
            block for block in document["function"]["blocks"]
            if block.get("exception_parameter") is not None
        )
        handler["instructions"].append(dict(handler["instructions"][0]))
        with self.assertRaisesRegex(ValueError, "passthrough-rethrow handler"):
            compile_function(document, "register")

    def test_to_number_throws_for_symbol_and_converts_numeric_strings(self) -> None:
        from prototype import js_to_number

        self.assertEqual(4.5, js_to_number("4.5"))
        with self.assertRaises(JSException):
            js_to_number(proof.JSSymbol())


if __name__ == "__main__":
    unittest.main()
