"""Regression tests for the backend-neutral Core IR v1 verifier."""
import json
from pathlib import Path
import tempfile
import unittest

from verify_core_ir import Verifier, load, run_corpus

ROOT = Path(__file__).resolve().parents[2]
SCHEMA = load(ROOT / "contracts/core-ir/v1/schema.json")
MANIFEST = ROOT / "tests/core-ir/manifest.json"
FIXTURES = ROOT / "tests/core-ir/fixtures"
EFFECTS = ("may_allocate", "may_gc", "may_throw", "may_suspend", "may_call_host", "may_reenter", "may_mutate_shape")


def fixture(name):
    return load(FIXTURES / name)


def codes(doc):
    return {item[0] for item in Verifier(SCHEMA).verify(doc)}


class CoreIRVerifierTests(unittest.TestCase):
    def test_positive_and_negative_fixture_corpus(self):
        report = run_corpus(ROOT, MANIFEST, ROOT / "contracts/core-ir/v1/schema.json")
        self.assertEqual(report["summary"], {"total": 27, "passed": 27, "failed": 0, "accepted": 6, "rejected": 21})

    def test_all_positive_fixtures_are_accepted(self):
        manifest = load(MANIFEST)
        for item in manifest["fixtures"]:
            if item["expect"] != "accepted":
                continue
            name = Path(item["path"]).name
            with self.subTest(name=name):
                self.assertEqual(codes(fixture(name)), set())

    def test_all_negative_fixtures_are_rejected_with_declared_codes(self):
        manifest = load(MANIFEST)
        for item in manifest["fixtures"]:
            if item["expect"] != "rejected": continue
            actual = codes(fixture(Path(item["path"]).name))
            with self.subTest(path=item["path"]):
                self.assertTrue(actual)
                self.assertTrue(set(item.get("diagnostics", [])) <= actual)

    def test_backend_specific_fields_are_rejected(self):
        doc = fixture("positive-ssa-branch.json"); doc["function"]["backend"] = "llvm"
        self.assertIn("schema.additional_property", codes(doc))

    def test_unknown_operation_is_rejected(self):
        doc = fixture("positive-ssa-branch.json"); doc["function"]["blocks"][0]["instructions"][0]["op"] = "llvm.add"
        self.assertIn("operation.unsupported", codes(doc))

    def test_use_before_definition_is_rejected(self):
        doc = fixture("positive-ssa-branch.json"); doc["function"]["blocks"][0]["instructions"][0]["operands"] = ["same", "arg_left"]
        self.assertIn("ssa.use_before_definition", codes(doc))

    def test_non_dominating_definition_is_rejected(self):
        doc = fixture("positive-ssa-branch.json")
        blocks = doc["function"]["blocks"]
        blocks[1]["instructions"] = [dict(blocks[0]["instructions"][0], id="left_cmp", result={"id":"left_only","kind":"value","semantic_type":"boolean","representation":"i1"})]
        blocks[1]["terminator"] = {"kind":"return","value":"left_only","source":blocks[1]["terminator"]["source"]}
        blocks[2]["terminator"]["value"] = "left_only"
        self.assertIn("ssa.dominance", codes(doc))

    def test_effect_chain_must_be_contiguous(self):
        doc = fixture("positive-dynamic-exception.json"); doc["function"]["blocks"][0]["instructions"][0]["effect_in"] = "wrong_token"
        self.assertIn("effect.chain", codes(doc))

    def test_exception_edge_forwards_effect_out(self):
        doc = fixture("positive-dynamic-exception.json"); doc["function"]["blocks"][0]["instructions"][0]["exception"]["arguments"] = ["e0"]
        self.assertIn("effect.exception_edge", codes(doc))
    def test_exception_handler_cannot_use_a_later_definition_from_throwing_block(self):
        doc = fixture("positive-dynamic-exception.json")
        blocks = {block["id"]: block for block in doc["function"]["blocks"]}
        source = blocks["entry"]["instructions"][0]["source"]
        blocks["entry"]["instructions"].append({
            "id": "after_throwing_site", "op": "js.to_boolean", "operands": ["lhs"],
            "result": {"id": "after_throw_value", "kind": "value", "semantic_type": "boolean", "representation": "i1"},
            "attributes": {}, "effects": {key: False for key in EFFECTS}, "source": source,
            "effect_in": None, "effect_out": None, "exception": None,
        })
        handler_source = blocks["handler"]["instructions"][0]["source"]
        blocks["handler"]["instructions"].insert(0, {
            "id": "handler_use", "op": "js.to_boolean", "operands": ["after_throw_value"],
            "result": {"id": "handler_result", "kind": "value", "semantic_type": "boolean", "representation": "i1"},
            "attributes": {}, "effects": {key: False for key in EFFECTS}, "source": handler_source,
            "effect_in": None, "effect_out": None, "exception": None,
        })
        self.assertIn("ssa.dominance", codes(doc))

    def test_exception_edge_cannot_pass_the_throwing_instruction_result(self):
        doc = fixture("positive-dynamic-exception.json")
        blocks = {block["id"]: block for block in doc["function"]["blocks"]}
        blocks["handler"]["parameters"].insert(0, {
            "id": "exception_value", "kind": "value", "semantic_type": "dynamic", "representation": "boxed"
        })
        blocks["entry"]["instructions"][0]["exception"]["arguments"] = ["sum", "e1"]
        self.assertIn("ssa.use_before_definition", codes(doc))

    def test_exception_edge_can_forward_value_defined_before_throw(self):
        doc = fixture("positive-dynamic-exception.json")
        blocks = {block["id"]: block for block in doc["function"]["blocks"]}
        blocks["handler"]["parameters"].insert(0, {
            "id": "pre_throw_value", "kind": "value", "semantic_type": "dynamic", "representation": "boxed"
        })
        blocks["entry"]["instructions"][0]["exception"]["arguments"] = ["lhs", "e1"]
        self.assertNotIn("ssa.dominance", codes(doc))
        self.assertNotIn("ssa.use_before_definition", codes(doc))


    def test_normal_edge_forwards_current_effect_token(self):
        doc = fixture("positive-dynamic-exception.json"); doc["function"]["blocks"][0]["terminator"]["arguments"][-1] = "e0"
        self.assertIn("effect.normal_edge", codes(doc))

    def test_mutable_capture_must_be_by_cell(self):
        doc = fixture("positive-closure-source.json"); doc["function"]["blocks"][0]["instructions"][0]["attributes"]["captures"][0]["mode"] = "value"
        self.assertIn("closure.capture_mode", codes(doc))

    def test_async_await_requires_suspension_provenance(self):
        doc = fixture("positive-async-suspension.json"); doc["function"]["blocks"][0]["instructions"][0]["source"]["suspension"] = None
        self.assertIn("source.suspension_missing", codes(doc))

    def test_duplicate_json_keys_are_rejected(self):
        with tempfile.TemporaryDirectory() as folder:
            path=Path(folder)/"duplicate.json"; path.write_text('{"contract":"ogma.core-ir","contract":"other"}')
            with self.assertRaisesRegex(ValueError, "duplicate JSON object key"): load(path)

    def test_non_finite_json_is_rejected(self):
        with tempfile.TemporaryDirectory() as folder:
            path=Path(folder)/"nonfinite.json"; path.write_text('{"value":NaN}')
            with self.assertRaisesRegex(ValueError, "non-finite JSON number"): load(path)

    def test_instruction_ids_are_function_unique(self):
        doc=fixture("positive-ssa-branch.json"); first=doc["function"]["blocks"][0]["instructions"][0]
        clone=json.loads(json.dumps(first)); clone["id"]="cmp"; clone["result"]["id"]="other"
        doc["function"]["blocks"][1]["instructions"]=[clone]
        self.assertIn("instruction.duplicate_id", codes(doc))

    def test_dynamic_result_must_remain_boxed(self):
        doc=fixture("positive-dynamic-exception.json"); doc["function"]["blocks"][0]["instructions"][0]["result"]["representation"]="f64"
        self.assertIn("operation.dynamic_representation", codes(doc))

    def test_unknown_effect_flag_is_rejected(self):
        doc=fixture("positive-dynamic-exception.json"); doc["function"]["blocks"][0]["instructions"][0]["effects"]["may_deopt"]=True
        self.assertIn("schema.additional_property", codes(doc))

    def test_proven_unreachable_requires_generated_source(self):
        doc=fixture("positive-ssa-branch.json"); term=doc["function"]["blocks"][1]["terminator"]
        term.update({"kind":"unreachable","reason":"proven_unreachable"}); term.pop("value")
        self.assertIn("cfg.unreachable_proof", codes(doc))

    def test_corpus_report_is_deterministic(self):
        a=run_corpus(ROOT,MANIFEST,ROOT/"contracts/core-ir/v1/schema.json")
        b=run_corpus(ROOT,MANIFEST,ROOT/"contracts/core-ir/v1/schema.json")
        self.assertEqual(json.dumps(a,sort_keys=True),json.dumps(b,sort_keys=True))

    def test_manifest_has_expected_coverage(self):
        m=load(MANIFEST); self.assertEqual(sum(x["expect"]=="accepted" for x in m["fixtures"]),6)
        self.assertEqual(sum(x["expect"]=="rejected" for x in m["fixtures"]),21)

    def test_environment_store_requires_matching_value_type(self):
        doc=fixture("positive-closure-source.json")
        store=next(i for b in doc["function"]["blocks"] for i in b["instructions"] if i["op"]=="env.store")
        store["operands"][0]="wrong_type_value"
        for b in doc["function"]["blocks"]:
            if b["id"]=="entry":
                b["instructions"].insert(0, {"id":"wrong_type_def","op":"js.constant","operands":[],"result":{"id":"wrong_type_value","kind":"value","semantic_type":"string","representation":"string_ref"},"attributes":{"literal":"string","value":"x"},"effects":{k:False for k in EFFECTS},"source":b["instructions"][0]["source"],"effect_in":None,"effect_out":None,"exception":None})
                break
        self.assertIn("environment.store_type", codes(doc))

    def test_closure_capture_descriptors_are_exact(self):
        doc=fixture("positive-closure-source.json")
        closure=next(i for b in doc["function"]["blocks"] for i in b["instructions"] if i["op"]=="closure.create")
        closure["attributes"]["captures"][0]["surprise"]="invalid"
        self.assertIn("closure.capture_descriptor", codes(doc))

    def test_nested_mutable_cell_capture_preserves_cell_mode(self):
        doc=fixture("positive-closure-source.json")
        closure=next(i for b in doc["function"]["blocks"] for i in b["instructions"] if i["op"]=="closure.create")
        closure["attributes"]["captures"][0]["mode"]="value"
        self.assertIn("closure.capture_mode", codes(doc))

    def test_pure_function_cannot_smuggle_effect_tokens(self):
        doc=fixture("positive-ssa-branch.json")
        ins=doc["function"]["blocks"][0]["instructions"][0]
        ins["effect_in"]="forged"
        self.assertIn("effect.pure_token", codes(doc))


if __name__ == "__main__":
    unittest.main()
