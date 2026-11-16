"""Regression and contract tests for the .ogmb v1 container/verifier."""
from __future__ import annotations

import json
from pathlib import Path
import unittest

from verify_ogmb import (
    ContainerReader,
    ProgramVerifier,
    load_json,
    run_corpus,
    strict_json_loads,
    verify_one,
)

ROOT = Path(__file__).resolve().parents[2]
GENERATED = ROOT / "tests" / "ogmb" / ".generated"
FIXTURES = GENERATED / "fixtures"
MANIFEST = GENERATED / "manifest.json"


def load_fixture(name: str):
    return load_json(FIXTURES / name)


def load_artifact(name: str):
    reader = ContainerReader()
    artifact = reader.read((FIXTURES / name).read_bytes())
    return artifact, reader.errors


def codes(items):
    return {entry["code"] for entry in items}


class OgmbContractTests(unittest.TestCase):
    def test_positive_and_negative_corpus(self):
        report = run_corpus(ROOT, MANIFEST)
        self.assertEqual(
            report["summary"],
            {"total": 22, "passed": 22, "failed": 0, "accepted": 5, "rejected": 17},
        )

    def test_stack_register_and_exception_contracts_are_accepted(self):
        cases = (
            ("positive-stack.ogmb", "positive-stack.json"),
            ("positive-register.ogmb", "positive-register.json"),
            ("positive-exception.ogmb", "positive-exception.json"),
            ("positive-register-exception.ogmb", "positive-register-exception.json"),
            ("positive-unknown-optional.ogmb", "positive-stack.json"),
        )
        for artifact_name, program_name in cases:
            with self.subTest(artifact=artifact_name):
                artifact, errors = load_artifact(artifact_name)
                self.assertEqual(errors, [])
                self.assertIsNotNone(artifact)
                self.assertEqual(ProgramVerifier().verify(artifact, load_fixture(program_name)), [])

    def test_truncated_unknown_version_and_unknown_feature_fail_closed(self):
        cases = (
            ("negative-truncated.ogmb", "container.truncated_header"),
            ("negative-unknown-version.ogmb", "container.version"),
            ("negative-unknown-feature.ogmb", "container.unknown_features"),
        )
        for name, expected in cases:
            with self.subTest(name=name):
                artifact, errors = load_artifact(name)
                self.assertIsNone(artifact)
                self.assertIn(expected, codes(errors))

    def test_section_overlap_and_unknown_required_section_are_rejected(self):
        for name, expected in (
            ("negative-section-overlap.ogmb", "container.section_overlap"),
            ("negative-unknown-required.ogmb", "container.unknown_required_section"),
        ):
            with self.subTest(name=name):
                artifact, errors = load_artifact(name)
                self.assertIsNone(artifact)
                self.assertIn(expected, codes(errors))

    def test_unknown_optional_section_is_ignored(self):
        artifact, errors = load_artifact("positive-unknown-optional.ogmb")
        self.assertEqual(errors, [])
        self.assertIsNotNone(artifact)

    def test_container_code_digest_is_verified(self):
        artifact, errors = load_artifact("negative-code-digest.ogmb")
        self.assertIsNone(artifact)
        self.assertIn("container.code_digest", codes(errors))

    def test_decoder_output_is_bound_to_exact_code_bytes(self):
        artifact, errors = load_artifact("positive-stack.ogmb")
        self.assertEqual(errors, [])
        actual = ProgramVerifier().verify(artifact, load_fixture("negative-program-digest.json"))
        self.assertIn("program.code_digest", codes(actual))

    def test_invalid_jump_operand_overflow_and_constant_reference_are_rejected(self):
        artifact, errors = load_artifact("positive-stack.ogmb")
        self.assertEqual(errors, [])
        cases = (
            ("negative-invalid-jump.json", "cfg.target"),
            ("negative-operand-overflow.json", "operand.range"),
            ("negative-constant-reference.json", "constant.reference_bounds"),
        )
        for name, expected in cases:
            with self.subTest(name=name):
                actual = ProgramVerifier().verify(artifact, load_fixture(name))
                self.assertIn(expected, codes(actual))

    def test_local_slot_operands_are_bounded(self):
        artifact, errors = load_artifact("negative-local-bounds.ogmb")
        self.assertEqual(errors, [])
        actual = ProgramVerifier().verify(artifact, load_fixture("negative-local-bounds.json"))
        self.assertIn("operand.local_bounds", codes(actual))

    def test_stack_underflow_and_inconsistent_merge_are_rejected(self):
        for artifact_name, program_name, expected in (
            ("negative-stack-underflow.ogmb", "negative-stack-underflow.json", "stack.underflow"),
            ("stack-merge.ogmb", "negative-stack-merge.json", "stack.merge_depth"),
        ):
            with self.subTest(artifact=artifact_name):
                artifact, errors = load_artifact(artifact_name)
                self.assertEqual(errors, [])
                actual = ProgramVerifier().verify(artifact, load_fixture(program_name))
                self.assertIn(expected, codes(actual))

    def test_wrong_stack_effect_is_rejected(self):
        artifact, errors = load_artifact("positive-stack.ogmb")
        self.assertEqual(errors, [])
        program = load_fixture("positive-stack.json")
        program["instructions"][2]["stack_effect"] = {"pop": 1, "push": 1}
        self.assertIn("stack.effect_mismatch", codes(ProgramVerifier().verify(artifact, program)))

    def test_malformed_exception_metadata_is_rejected(self):
        artifact, errors = load_artifact("negative-exception-range.ogmb")
        self.assertEqual(errors, [])
        actual = ProgramVerifier().verify(artifact, load_fixture("positive-exception.json"))
        self.assertIn("exception.range", codes(actual))

    def test_register_reads_require_definite_initialization(self):
        artifact, errors = load_artifact("register-uninitialized.ogmb")
        self.assertEqual(errors, [])
        actual = ProgramVerifier().verify(artifact, load_fixture("negative-register-uninitialized.json"))
        self.assertIn("register.uninitialized", codes(actual))

    def test_throwing_register_result_is_not_available_to_exception_handler(self):
        artifact, errors = load_artifact("positive-register-exception.ogmb")
        self.assertEqual(errors, [])
        artifact["exceptions"][0]["handler_registers"] = [1]
        actual = ProgramVerifier().verify(artifact, load_fixture("positive-register-exception.json"))
        self.assertIn("exception.handler_register_state", codes(actual))

    def test_artifact_without_decoder_output_is_not_accepted_for_execution(self):
        report = run_corpus(ROOT, MANIFEST)
        missing = next(item for item in report["fixtures"] if item["artifact"].endswith("positive-stack.ogmb") and item["program"] is None)
        self.assertEqual(missing["actual"], "rejected")
        self.assertIn("program.missing", codes(missing["diagnostics"]))

    def test_duplicate_keys_and_non_finite_json_values_are_rejected(self):
        with self.assertRaisesRegex(ValueError, "duplicate JSON object key"):
            strict_json_loads('{"v":1,"v":2}')
        with self.assertRaisesRegex(ValueError, "non-finite JSON number"):
            strict_json_loads('{"value":NaN}')

    def test_fixture_paths_cannot_escape_repository_root(self):
        result = verify_one(ROOT, {"artifact": "../outside.ogmb", "expect": "rejected"})
        self.assertIn("manifest.artifact_load", codes(result["diagnostics"]))

    def test_malformed_decoder_shapes_return_diagnostics_not_exceptions(self):
        artifact, errors = load_artifact("positive-stack.ogmb")
        self.assertEqual(errors, [])
        cases = []
        malformed_model = load_fixture("positive-stack.json")
        malformed_model["execution_model"] = []
        cases.append(malformed_model)
        malformed_operation = load_fixture("positive-stack.json")
        malformed_operation["instructions"][0]["operation"] = []
        cases.append(malformed_operation)
        malformed_registers = load_fixture("positive-register.json")
        malformed_registers["instructions"][0]["register_effects"]["reads"] = [{}]
        cases.append(malformed_registers)
        for program in cases:
            with self.subTest(model=program["execution_model"]):
                self.assertTrue(ProgramVerifier().verify(artifact, program))

    def test_boolean_is_not_an_unsigned_integer(self):
        artifact, errors = load_artifact("positive-stack.ogmb")
        self.assertEqual(errors, [])
        program = load_fixture("positive-stack.json")
        program["instructions"][2]["operands"] = [True]
        self.assertIn("operand.range", codes(ProgramVerifier().verify(artifact, program)))

    def test_unknown_program_fields_are_rejected(self):
        artifact, errors = load_artifact("positive-stack.ogmb")
        self.assertEqual(errors, [])
        program = load_fixture("positive-stack.json")
        program["native_address"] = 1234
        self.assertIn("program.fields", codes(ProgramVerifier().verify(artifact, program)))

    def test_machine_report_is_deterministic(self):
        first = run_corpus(ROOT, MANIFEST)
        second = run_corpus(ROOT, MANIFEST)
        self.assertEqual(json.dumps(first, sort_keys=True), json.dumps(second, sort_keys=True))


if __name__ == "__main__":
    unittest.main()
