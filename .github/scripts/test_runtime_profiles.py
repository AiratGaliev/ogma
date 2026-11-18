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
                "debug-hybrid-native",
                "development-test",
                "development-hybrid-native",
                "portable-interpreter-release",
                "hybrid-native-release",
                "node-compatibility-portable-release",
                "node-compatibility-hybrid-native-release",
            },
        )
        for plan in first.values():
            self.assertIn("ecmascript-semantics", plan["includedComponents"])
            self.assertIn("portable-interpreter", plan["includedComponents"])
            self.assertIn("host-module-registry", plan["includedComponents"])

    def test_debug_and_development_profiles_are_orthogonal_to_execution_backend(self) -> None:
        profiles = self.document["profiles"]
        for mode in ("debug", "development"):
            backends = {
                profile["executionBackend"]
                for profile in profiles
                if profile["buildMode"] == mode and profile["hostCompatibility"] == "none"
            }
            self.assertEqual(backends, {"portable-interpreter", "hybrid-native"})

    def test_node_compatibility_layer_is_available_for_both_execution_backends(self) -> None:
        profiles = [
            profile for profile in self.document["profiles"]
            if profile["hostCompatibility"] == "node"
        ]
        self.assertEqual(
            {profile["executionBackend"] for profile in profiles},
            {"portable-interpreter", "hybrid-native"},
        )
        plans = self.validate()
        for profile in profiles:
            included = set(plans[profile["id"]]["includedComponents"])
            self.assertTrue({"node-module-resolver", "node-compat-api"}.issubset(included))

    def test_hybrid_debug_profile_keeps_debugger_on_interpreter_fallback(self) -> None:
        profile = next(item for item in self.document["profiles"] if item["id"] == "debug-hybrid-native")
        included = set(self.validate()["debug-hybrid-native"]["includedComponents"])
        self.assertIn("native-aot-execution", included)
        self.assertIn("portable-interpreter", included)
        self.assertIn("debugger-api", included)
        self.assertIn("interpreter-debug-hooks", included)
        self.assertIn("source-debug-metadata", included)
        self.assertEqual(profile["metadata"]["debugSymbols"], "embedded")
        self.assertIn("native AOT stepping remains a later milestone", profile["purpose"])

    def test_development_profiles_require_dap_profiler_and_hot_reload(self) -> None:
        invalid = copy.deepcopy(self.document)
        profile = next(item for item in invalid["profiles"] if item["id"] == "development-hybrid-native")
        profile["includeRoots"].remove("hot-reload")
        profile["featureFlags"].remove("tools.hot-reload")
        profile["tooling"]["hotReload"] = False
        with self.assertRaisesRegex(ProfileValidationError, "must include DAP, profiler hooks and Hot Reload"):
            self.validate(invalid)

    def test_debug_and_development_profiles_require_embedded_source_maps(self) -> None:
        invalid = copy.deepcopy(self.document)
        profile = next(item for item in invalid["profiles"] if item["id"] == "debug-hybrid-native")
        profile["metadata"]["sourceMaps"] = "external"
        with self.assertRaisesRegex(ProfileValidationError, "must embed source maps"):
            self.validate(invalid)

    def test_profile_matrix_rejects_a_missing_backend_mode_combination(self) -> None:
        invalid = copy.deepcopy(self.document)
        invalid["profiles"] = [
            profile for profile in invalid["profiles"]
            if profile["id"] != "debug-hybrid-native"
        ]
        with self.assertRaisesRegex(ProfileValidationError, "missing required build/backend/host combinations"):
            self.validate(invalid)

    def test_rust_build_settings_are_explicit_and_match_the_profile_mode(self) -> None:
        plans = self.validate()
        self.assertEqual(plans["debug-interpreter"]["rustBuild"]["optLevel"], "0")
        self.assertEqual(plans["development-test"]["rustBuild"]["optLevel"], "1")
        self.assertEqual(plans["hybrid-native-release"]["rustBuild"]["optLevel"], "3")
        invalid = copy.deepcopy(self.document)
        release = next(item for item in invalid["profiles"] if item["id"] == "hybrid-native-release")
        release["rustBuild"]["optLevel"] = "1"
        with self.assertRaisesRegex(ProfileValidationError, "rustBuild settings conflict"):
            self.validate(invalid)

    def test_aot_optimization_and_symbol_policy_are_backend_specific(self) -> None:
        plans = self.validate()
        self.assertFalse(plans["debug-interpreter"]["aotBuild"]["enabled"])
        self.assertEqual(plans["debug-hybrid-native"]["aotBuild"]["optimization"], "minimal")
        self.assertEqual(plans["development-hybrid-native"]["aotBuild"]["optimization"], "balanced")
        self.assertEqual(plans["hybrid-native-release"]["aotBuild"]["optimization"], "aggressive")
        self.assertEqual(plans["hybrid-native-release"]["aotBuild"]["symbols"], "external")
        self.assertEqual(plans["node-compatibility-hybrid-native-release"]["aotBuild"]["symbols"], "external")

        invalid = copy.deepcopy(self.document)
        profile = next(item for item in invalid["profiles"] if item["id"] == "hybrid-native-release")
        profile["aotBuild"]["optimization"] = "minimal"
        with self.assertRaisesRegex(ProfileValidationError, "AOT settings conflict"):
            self.validate(invalid)

    def test_release_symbol_policy_matches_compiler_stripping(self) -> None:
        plans = self.validate()
        portable = plans["portable-interpreter-release"]
        native = plans["hybrid-native-release"]
        self.assertEqual(portable["metadataPolicy"]["debugSymbols"], "not-applicable")
        self.assertEqual(portable["rustBuild"]["strip"], "symbols")
        self.assertEqual(native["metadataPolicy"]["debugSymbols"], "external")
        self.assertEqual(native["rustBuild"]["strip"], "debuginfo")

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

    def test_json_schema_constant_comparison_keeps_boolean_distinct_from_integer(self) -> None:
        invalid = copy.deepcopy(self.document)
        invalid["schemaVersion"] = True
        with self.assertRaisesRegex(ProfileValidationError, "expected type integer"):
            self.validate(invalid)

    def test_identifier_patterns_reject_trailing_line_terminators(self) -> None:
        invalid = copy.deepcopy(self.document)
        invalid["components"][0]["id"] += "\\n"
        with self.assertRaisesRegex(ProfileValidationError, "does not match pattern"):
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
