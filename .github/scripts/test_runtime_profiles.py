from __future__ import annotations

import copy
import json
from pathlib import Path
import sys
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / ".github" / "scripts"))

from validate_runtime_profiles import (
    ProfileValidationError,
    build_profile_plans,
    DEFAULT_MANIFEST,
    DEFAULT_SCHEMA,
)


class RuntimeProfileContractTests(unittest.TestCase):
    def setUp(self) -> None:
        self.document = json.loads(DEFAULT_MANIFEST.read_text(encoding="utf-8"))
        self.schema = json.loads(DEFAULT_SCHEMA.read_text(encoding="utf-8"))

    def validate(self, document=None):
        return build_profile_plans(self.document if document is None else document, self.schema)

    def test_all_profiles_produce_deterministic_dependency_closures(self) -> None:
        first = self.validate()
        second = self.validate()
        self.assertEqual(first, second)
        self.assertEqual(
            set(first),
            {
                "debug-interpreter",
                "development-test",
                "portable-interpreter-release",
                "hybrid-native-release",
                "node-compatibility-portable-release",
            },
        )
        for plan in first.values():
            self.assertIn("ecmascript-semantics", plan["includedComponents"])
            self.assertIn("portable-interpreter", plan["includedComponents"])
            self.assertIn("host-module-registry", plan["includedComponents"])

    def test_dependency_required_by_an_included_component_cannot_be_stripped(self) -> None:
        invalid = copy.deepcopy(self.document)
        profile = next(item for item in invalid["profiles"] if item["id"] == "portable-interpreter-release")
        profile["excludeComponents"].append("runtime-core")
        with self.assertRaisesRegex(ProfileValidationError, "dependency closure"):
            self.validate(invalid)

    def test_non_strippable_components_are_retained_in_every_profile(self) -> None:
        invalid = copy.deepcopy(self.document)
        profile = next(item for item in invalid["profiles"] if item["id"] == "portable-interpreter-release")
        profile["includeRoots"] = ["host-module-registry"]
        with self.assertRaisesRegex(ProfileValidationError, "non-strippable policy"):
            self.validate(invalid)

    def test_unknown_component_dependency_fails_closed(self) -> None:
        invalid = copy.deepcopy(self.document)
        invalid["components"][0]["requires"].append("missing-component")
        with self.assertRaisesRegex(ProfileValidationError, "unknown component"):
            self.validate(invalid)

    def test_component_dependency_cycles_fail_closed(self) -> None:
        invalid = copy.deepcopy(self.document)
        semantic_core = next(item for item in invalid["components"] if item["id"] == "ecmascript-semantics")
        semantic_core["requires"].append("runtime-core")
        with self.assertRaisesRegex(ProfileValidationError, "dependency cycle"):
            self.validate(invalid)

    def test_dynamic_code_semantics_cannot_be_disabled_by_profiles(self) -> None:
        invalid = copy.deepcopy(self.document)
        invalid["languageSemantics"]["dynamicCode"] = "disabled"
        with self.assertRaisesRegex(ProfileValidationError, "expected constant"):
            self.validate(invalid)

    def test_release_profiles_strip_debugger_transport_by_default(self) -> None:
        plans = self.validate()
        for profile_id in (