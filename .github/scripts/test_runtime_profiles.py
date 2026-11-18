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
        for profile_id in ("portable-interpreter-release", "hybrid-native-release", "node-compatibility-portable-release"):
            plan = plans[profile_id]
            self.assertNotIn("dap-transport", plan["includedComponents"])
            self.assertNotIn("inspector-ui", plan["includedComponents"])
            self.assertEqual(plan["metadataPolicy"]["sourceDebugMetadata"], "stripped")

    def test_release_can_explicitly_opt_in_to_debugger_transport_and_metadata(self) -> None:
        configured = copy.deepcopy(self.document)
        profile = next(item for item in configured["profiles"] if item["id"] == "portable-interpreter-release")
        profile["excludeComponents"] = [
            item for item in profile["excludeComponents"]
            if item not in {"source-debug-metadata", "debugger-api", "dap-transport"}
        ]
        profile["includeRoots"].append("dap-transport")
        profile["tooling"]["debuggerApi"] = True
        profile["tooling"]["dapTransport"] = True
        profile["metadata"]["sourceDebugMetadata"] = "embedded"
        profile["metadata"]["sourceMaps"] = "embedded"
        profile["featureFlags"].extend(["debug.api", "debug.source-metadata", "tools.dap-transport"])
        profile["featureFlags"].sort()
        plan = self.validate(configured)["portable-interpreter-release"]
        self.assertIn("dap-transport", plan["includedComponents"])
        self.assertIn("debugger-api", plan["includedComponents"])
        self.assertIn("source-debug-metadata", plan["includedComponents"])

    def test_node_compatibility_requires_explicit_resolver_and_adapter(self) -> None:
        invalid = copy.deepcopy(self.document)
        profile = next(item for item in invalid["profiles"] if item["id"] == "portable-interpreter-release")
        profile["hostCompatibility"] = "node"
        with self.assertRaisesRegex(ProfileValidationError, "without both resolver and API adapter"):
            self.validate(invalid)

    def test_node_components_are_rejected_without_node_profile(self) -> None:
        invalid = copy.deepcopy(self.document)
        profile = next(item for item in invalid["profiles"] if item["id"] == "portable-interpreter-release")
        profile["excludeComponents"] = [
            item for item in profile["excludeComponents"]
            if item not in {"node-module-resolver", "node-compat-api"}
        ]
        profile["includeRoots"].append("node-compat-api")
        with self.assertRaisesRegex(ProfileValidationError, "without selecting hostCompatibility=node"):
            self.validate(invalid)

    def test_debug_and_development_profiles_retain_source_metadata(self) -> None:
        invalid = copy.deepcopy(self.document)
        profile = next(item for item in invalid["profiles"] if item["id"] == "development-test")
        profile["metadata"]["sourceDebugMetadata"] = "stripped"
        with self.assertRaisesRegex(ProfileValidationError, "must embed source/debug metadata"):
            self.validate(invalid)

    def test_debugger_transport_and_ui_are_orthogonal_to_runtime_semantics(self) -> None:
        plans = self.validate()
        debug = plans["debug-interpreter"]["includedComponents"]
        release = plans["portable-interpreter-release"]["includedComponents"]
        self.assertIn("dap-transport", debug)
        self.assertIn("inspector-ui", debug)
        self.assertNotIn("dap-transport", release)
        self.assertNotIn("inspector-ui", release)
        self.assertEqual(self.document["languageSemantics"]["dynamicCode"], "preserved")
        self.assertEqual(self.document["ecmaEdition"], "ECMAScript-2026")

    def test_mandatory_verifier_and_runtime_metadata_cannot_be_stripped(self) -> None:
        invalid = copy.deepcopy(self.document)
        invalid["mandatoryRuntimeMetadata"].remove("verifier-summary")
        with self.assertRaisesRegex(ProfileValidationError, "mandatoryRuntimeMetadata"):
            self.validate(invalid)

    def test_debugger_and_tooling_components_must_be_semantics_neutral(self) -> None:
        invalid = copy.deepcopy(self.document)
        transport = next(item for item in invalid["components"] if item["id"] == "dap-transport")
        transport["semanticEffects"] = True
        with self.assertRaisesRegex(ProfileValidationError, "must not have ECMAScript semantic effects"):
            self.validate(invalid)

    def test_feature_flags_must_match_the_resolved_component_closure(self) -> None:
        invalid = copy.deepcopy(self.document)
        profile = next(item for item in invalid["profiles"] if item["id"] == "hybrid-native-release")
        profile["featureFlags"].remove("execution.native-aot")
        with self.assertRaisesRegex(ProfileValidationError, "featureFlags do not match"):
            self.validate(invalid)


if __name__ == "__main__":
    unittest.main()
