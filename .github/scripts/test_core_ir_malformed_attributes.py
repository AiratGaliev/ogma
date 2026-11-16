"""Additional negative-path guards for attributes that bypass JSON Schema."""
import unittest
from pathlib import Path
from verify_core_ir import Verifier, load

ROOT = Path(__file__).resolve().parents[2]
SCHEMA = load(ROOT / "contracts/core-ir/v1/schema.json")

class CoreIRMalformedAttributeTests(unittest.TestCase):
    def test_environment_store_with_non_identifier_reference_is_rejected(self):
        doc = load(ROOT / "tests/core-ir/fixtures/positive-closure-source.json")
        instruction = next(i for b in doc["function"]["blocks"] for i in b["instructions"] if i["op"] == "env.store")
        instruction["attributes"]["binding"] = []
        codes = {e[0] for e in Verifier(SCHEMA).verify(doc)}
        self.assertIn("environment.binding_id", codes)

    def test_distinct_capture_id_cannot_reimport_same_binding(self):
        doc = load(ROOT / "tests/core-ir/fixtures/positive-closure-source.json")
        capture = dict(doc["function"]["captures"][0])
        capture["id"] = "another_capture_alias"
        doc["function"]["captures"].append(capture)
        codes = {e[0] for e in Verifier(SCHEMA).verify(doc)}
        self.assertIn("closure.duplicate_capture", codes)

if __name__ == "__main__":
    unittest.main()
