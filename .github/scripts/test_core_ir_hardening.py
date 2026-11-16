#!/usr/bin/env python3
"""Focused regressions for Core IR invariants found during independent PR review."""
import json
import unittest
from pathlib import Path

from verify_core_ir import Verifier, load

ROOT = Path(__file__).resolve().parents[2]
SCHEMA = load(ROOT / "contracts/core-ir/v1/schema.json")


def fixture(name):
    return load(ROOT / "tests/core-ir/fixtures" / name)


def codes(doc):
    return {item[0] for item in Verifier(SCHEMA).verify(doc)}


def append_constant(doc, literal, value, semantic_type, representation, ident):
    block = doc["function"]["blocks"][0]
    template = block["instructions"][0]
    block["instructions"].append({
        "id": "review_" + ident, "op": "js.constant", "operands": [],
        "result": {"id": "review_value_" + ident, "kind": "value",
                   "semantic_type": semantic_type, "representation": representation},
        "attributes": {"literal": literal, "value": value},
        "effects": {key: False for key in template["effects"]},
        "source": template["source"], "effect_in": None, "effect_out": None,
        "exception": None,
    })


class CoreIRHardeningTests(unittest.TestCase):
    def test_declaration_mutability_matches_kind_in_both_directions(self):
        doc = fixture("positive-ssa-branch.json")
        doc["function"]["environment"]["bindings"][0]["mutable"] = False
        self.assertIn("environment.binding_mutability", codes(doc))

    def test_numeric_constant_rejects_string_payload(self):
        doc = fixture("positive-ssa-branch.json")
        append_constant(doc, "number", "not-a-number", "number", "f64", "bad_number")
        self.assertIn("operation.literal_value", codes(doc))

    def test_valid_ecmascript_number_special_values_are_representable(self):
        for value in (1.25, "NaN", "Infinity", "-Infinity", "-0"):
            with self.subTest(value=value):
                doc = fixture("positive-ssa-branch.json")
                append_constant(doc, "number", value, "number", "f64", "number")
                self.assertNotIn("operation.literal_value", codes(doc))

    def test_bigint_payload_must_be_canonical_decimal(self):
        doc = fixture("positive-ssa-branch.json")
        append_constant(doc, "bigint", "001", "bigint", "bigint_ref", "bad_bigint")
        self.assertIn("operation.literal_value", codes(doc))

    def test_duplicate_suspension_points_are_rejected(self):
        doc = fixture("positive-async-suspension.json")
        block = doc["function"]["blocks"][0]
        duplicate = json.loads(json.dumps(block["instructions"][0]))
        duplicate["id"] = "await_again"
        duplicate["result"]["id"] = "resolved_again"
        duplicate["effect_in"], duplicate["effect_out"] = "e1", "e2"
        block["instructions"].append(duplicate)
        self.assertIn("source.suspension_duplicate", codes(doc))

    def test_duplicate_imported_function_capture_is_rejected(self):
        doc = fixture("positive-closure-source.json")
        capture = json.loads(json.dumps(doc["function"]["captures"][0]))
        doc["function"]["captures"].append(capture)
        self.assertIn("closure.duplicate_capture", codes(doc))

    def test_capture_namespace_cannot_collide_with_local_binding(self):
        doc = fixture("positive-closure-source.json")
        doc["function"]["environment"]["bindings"].append({
            "id": "capture_total", "name": "shadowed_local", "kind": "const",
            "mutable": False, "semantic_type": "number", "representation": "f64"
        })
        self.assertIn("closure.capture_identifier_collision", codes(doc))

    def test_closure_cannot_capture_same_imported_binding_through_aliases(self):
        doc = fixture("positive-closure-source.json")
        closure = next(i for b in doc["function"]["blocks"] for i in b["instructions"]
                       if i["op"] == "closure.create")
        closure["attributes"]["captures"].append({"binding": "bind_total", "mode": "cell"})
        self.assertIn("closure.duplicate_capture", codes(doc))

    def test_nullable_identifier_fields_require_stable_ids(self):
        cases = (
            ("positive-ssa-branch.json", ("function", "environment", "parent")),
            ("positive-ssa-branch.json", ("function", "blocks", 0, "instructions", 0, "source", "scope")),
            ("positive-async-suspension.json", ("function", "blocks", 0, "instructions", 0, "source", "suspension")),
            ("positive-async-suspension.json", ("function", "blocks", 0, "instructions", 0, "effect_in")),
            ("positive-async-suspension.json", ("function", "effect_entry")),
        )
        for name, path in cases:
            with self.subTest(path=path):
                doc = fixture(name)
                target = doc
                for key in path[:-1]:
                    target = target[key]
                target[path[-1]] = "not a valid id"
                self.assertTrue({"schema.oneOf", "schema.pattern"} & codes(doc))

    def test_malformed_environment_binding_reference_does_not_crash(self):
        doc = fixture("positive-closure-source.json")
        instruction = next(i for b in doc["function"]["blocks"] for i in b["instructions"]
                           if i["op"] == "env.load")
        instruction["attributes"]["binding"] = []
        self.assertIn("environment.binding_id", codes(doc))

    def test_closure_target_must_be_a_stable_function_identifier(self):
        doc = fixture("positive-closure-source.json")
        closure = next(i for b in doc["function"]["blocks"] for i in b["instructions"]
                       if i["op"] == "closure.create")
        closure["attributes"]["function"] = "invalid function id"
        self.assertIn("closure.function_id", codes(doc))

    def test_host_symbol_is_a_capability_identifier_not_free_form_text(self):
        doc = fixture("positive-dynamic-exception.json")
        instruction = doc["function"]["blocks"][0]["instructions"][0]
        instruction["op"] = "host.call"
        instruction["attributes"] = {"symbol": "not a capability name"}
        self.assertIn("host.symbol_id", codes(doc))

    def test_closure_capture_list_must_be_an_array(self):
        doc = fixture("positive-closure-source.json")
        closure = next(i for b in doc["function"]["blocks"] for i in b["instructions"]
                       if i["op"] == "closure.create")
        closure["attributes"]["captures"] = {"binding": "capture_total", "mode": "cell"}
        self.assertIn("closure.capture_list", codes(doc))

    def test_capture_descriptor_requires_exact_shape_and_mode(self):
        doc = fixture("positive-closure-source.json")
        closure = next(i for b in doc["function"]["blocks"] for i in b["instructions"]
                       if i["op"] == "closure.create")
        closure["attributes"]["captures"][0]["surprise"] = True
        self.assertIn("closure.capture_descriptor", codes(doc))

    def test_non_string_literal_tag_is_rejected_without_type_error(self):
        doc = fixture("positive-ssa-branch.json")
        append_constant(doc, [], 1, "number", "f64", "bad_tag")
        self.assertIn("operation.literal_type", codes(doc))

    def test_imprecise_integer_number_payload_is_rejected(self):
        doc = fixture("positive-ssa-branch.json")
        append_constant(doc, "number", 9007199254740993, "number", "f64", "imprecise")
        self.assertIn("operation.literal_value", codes(doc))

    def test_oversized_integer_is_rejected_without_overflow(self):
        doc = fixture("positive-ssa-branch.json")
        append_constant(doc, "number", 10 ** 1000, "number", "f64", "oversized")
        self.assertIn("schema.type", codes(doc))

    def test_lexical_environment_cannot_parent_itself(self):
        doc = fixture("positive-ssa-branch.json")
        env = doc["function"]["environment"]
        env["parent"] = env["id"]
        self.assertIn("environment.parent_cycle", codes(doc))


if __name__ == "__main__":
    unittest.main()
