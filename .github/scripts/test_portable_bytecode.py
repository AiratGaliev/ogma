#!/usr/bin/env python3
"""Focused regression tests for the OGM-16 stack/register bytecode proof."""

from __future__ import annotations

import importlib.util
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
BENCHMARK_DIR = ROOT / "benchmarks" / "portable-bytecode"
sys.path.insert(0, str(BENCHMARK_DIR))

from corpus import write_corpus  # noqa: E402

_spec = importlib.util.spec_from_file_location("ogm16_prototype", BENCHMARK_DIR / "prototype.py")
if _spec is None or _spec.loader is None:
    raise RuntimeError("cannot import OGM-16 prototype")
_prototype = importlib.util.module_from_spec(_spec)
sys.modules[_spec.name] = _prototype
_spec.loader.exec_module(_prototype)

compile_workload = _prototype.compile_workload
verify_compiled = _prototype.verify_compiled
run_workload_correctness = _prototype.run_workload_correctness
ContainerReader = _prototype.ContainerReader


class PortableBytecodeProofTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.workload_document = write_corpus()
        cls.workloads = cls.workload_document["workloads"]
        cls.compiled = {
            workload["id"]: {
                model: compile_workload(workload, model)
                for model in ("stack", "register")
            }
            for workload in cls.workloads
        }

    def test_workload_corpus_covers_required_semantics(self) -> None:
        self.assertEqual(
            {
                "dynamic-add",
                "control-flow",
                "mutable-loop-locals",
                "function-calls",
                "host-calls",
                "exceptions",
            },
            {workload["id"] for workload in self.workloads},
        )

    def test_both_models_are_accepted_by_the_repository_container_and_program_verifiers(self) -> None:
        for workload_id, candidates in self.compiled.items():
            with self.subTest(workload=workload_id):
                self.assertEqual(len(candidates["stack"]), len(candidates["register"]))
                for model, functions in candidates.items():
                    for compiled in functions:
                        with self.subTest(workload=workload_id, model=model, function=compiled.function_id):
                            verify_compiled(compiled)

    def test_interpreters_match_expected_results_exceptions_and_host_traces(self) -> None:
        for workload in self.workloads:
            with self.subTest(workload=workload["id"]):
                observations = run_workload_correctness(
                    workload,
                    self.compiled[workload["id"]]["stack"],
                    self.compiled[workload["id"]]["register"],
                )
                self.assertEqual(len(workload["cases"]), len(observations))

    def test_register_encoding_reduces_dispatches_for_the_same_core_ir(self) -> None:
        stack_total = 0
        register_total = 0
        for workload in self.workloads:
            observations = run_workload_correctness(
                workload,
                self.compiled[workload["id"]]["stack"],
                self.compiled[workload["id"]]["register"],
            )
            stack_total += sum(item["stack_dispatches"] for item in observations)
            register_total += sum(item["register_dispatches"] for item in observations)
        self.assertLess(register_total, stack_total)

    def test_compilation_and_debug_mappings_are_deterministic(self) -> None:
        workload = self.workloads[0]
        first = compile_workload(workload, "register")
        second = compile_workload(workload, "register")
        self.assertEqual([item.code for item in first], [item.code for item in second])
        self.assertEqual([item.constants for item in first], [item.constants for item in second])
        self.assertEqual([item.debug_map for item in first], [item.debug_map for item in second])

    def test_container_digest_rejects_tampered_code(self) -> None:
        compiled = self.compiled["dynamic-add"]["stack"][0]
        tampered = bytearray(compiled.container)
        tampered[48 + 4 * 32 + 8] ^= 0x01
        reader = ContainerReader()
        self.assertIsNone(reader.read(bytes(tampered)))
        self.assertTrue(reader.errors)


if __name__ == "__main__":
    unittest.main()
