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
            {"total": 28, "passed": 28, "failed": 0, "accepted": 7, "rejected": 21},
        )

    def test_stack_register_and_exception_contracts_are_accepted(self):
        cases = (
            ("positive-stack.ogmb", "positive-stack.json"),
            ("positive-register.ogmb", "positive-register.json"),
            ("positive-exception.ogmb", "positive-exception.json"),
            ("positive-register-exception.ogmb", "positive-register-exception.json"),
            ("positive-suspension.ogmb", "positive-suspension.json"),
            ("positive-source-map.ogmb", "positive-stack.json"),
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

    def test_suspension_metadata_must_match_resume_control_flow(self):
        artifact, errors = load_artifact("negative-suspension-target.ogmb")
        self.assertEqual(errors, [])
        actual = ProgramVerifier().verify(artifact, load_fixture("positive-suspension.json"))
        self.assertIn("suspension.resume_target", codes(actual))

    def test_frame_layout_limits_actual_local_and_register_windows(self):
        artifact, errors = load_artifact("negative-local-bounds.ogmb")
        self.assertEqual(errors, [])
        local_program = load_fixture("negative-local-bounds.json")
        self.assertIn("operand.local_bounds", codes(ProgramVerifier().verify(artifact, local_program)))
        register_artifact, errors = load_artifact("positive-register.ogmb")
        self.assertEqual(errors, [])
        register_program = load_fixture("positive-register.json")
        register_program["instructions"][2]["register_effects"]["reads"] = [0, 3]
        self.assertIn("register.bounds", codes(ProgramVerifier().verify(register_artifact, register_program)))

    def test_function_metadata_schema_is_present_and_versioned(self):
        schema = load_json(ROOT / "contracts" / "ogmb" / "v1" / "manifest-metadata.schema.json")
        self.assertEqual(schema["$id"], "https://ogma.dev/contracts/ogmb/v1/manifest-metadata.schema.json")
        self.assertIn("frame_layout", schema["required"])
        self.assertIn("suspension_points", schema["required"])
        self.assertIn("code_generation", schema["required"])
        self.assertIn("code_cache", schema["required"])

    def test_source_map_references_are_verified(self):
        artifact, errors = load_artifact("negative-source-map-reference.ogmb")
        self.assertIsNone(artifact)
        self.assertIn("source_map.file_ref", codes(errors))

    def test_source_map_schema_is_versioned(self):
        schema = load_json(ROOT / "contracts" / "ogmb" / "v1" / "source-map.schema.json")
        self.assertEqual(schema["properties"]["version"]["const"], "1.0.0")
        self.assertIn("lexical_scopes", schema["required"])
        self.assertIn("variable_locations", schema["required"])

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

    def test_control_flow_cannot_reenter_function_entry(self):
        artifact, errors = load_artifact("positive-stack.ogmb")
        self.assertEqual(errors, [])
        program = load_fixture("positive-stack.json")
        program["instructions"][0].update(
            operation="jump", operands=[0], constant_refs=[], successors=[0],
            stack_effect={"pop": 0, "push": 0}
        )
        actual = ProgramVerifier().verify(artifact, program)
        self.assertIn("cfg.entry_predecessor", codes(actual))

    def test_verifier_summary_is_recomputed_not_trusted(self):
        artifact, errors = load_artifact("positive-stack.ogmb")
        self.assertEqual(errors, [])
        artifact["manifest"]["metadata"]["verifier_summary"]["instruction_count"] += 1
        actual = ProgramVerifier().verify(artifact, load_fixture("positive-stack.json"))
        self.assertIn("verifier_summary.instruction_count", codes(actual))

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

    def test_duplicate_exception_regions_are_rejected(self):
        artifact, errors = load_artifact("positive-exception.ogmb")
        self.assertEqual(errors, [])
        artifact["exceptions"].append(dict(artifact["exceptions"][0]))
        actual = ProgramVerifier().verify(artifact, load_fixture("positive-exception.json"))
        self.assertIn("exception.duplicate_region", codes(actual))

    def test_verification_schema_is_valid_json_and_names_the_contract(self):
        schema = load_json(ROOT / "contracts" / "ogmb" / "v1" / "verification-input.schema.json")
        self.assertEqual(schema["properties"]["schema"]["const"], "ogma.bytecode-verification")
        self.assertEqual(schema["properties"]["version"]["const"], "1.0.0")
        self.assertEqual(schema["properties"]["execution_model"]["enum"], ["stack", "register"])

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

    def test_exception_handler_registers_must_fit_function_frame(self):
        artifact, errors = load_artifact("negative-register-exception-frame.ogmb")
        self.assertEqual(errors, [])
        actual = ProgramVerifier().verify(artifact, load_fixture("positive-register-exception.json"))
        self.assertIn("exception.handler_registers", codes(actual))

    def test_verifier_summary_register_index_must_fit_function_frame(self):
        artifact, errors = load_artifact("negative-register-summary-frame.ogmb")
        self.assertIsNone(artifact)
        self.assertIn("metadata.verifier_summary_register", codes(errors))

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
        deeply_nested = "[" * 1500 + "0" + "]" * 1500
        with self.assertRaisesRegex(ValueError, "JSON nesting"):
            strict_json_loads(deeply_nested)
        at_limit = "[" * 32 + "0" + "]" * 32
        strict_json_loads(at_limit)
        embedded_text = 'brackets ]]} {{{[ and quote " plus slash ' + chr(92)
        embedded_brackets = json.dumps({"text": embedded_text})
        self.assertEqual(strict_json_loads(embedded_brackets)["text"], embedded_text)

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
