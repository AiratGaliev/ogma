#!/usr/bin/env python3
"""Validate OGM-28 runtime profile contracts and produce a deterministic build-plan report.

This is an M0 architectural proof. Component identifiers describe the intended build
boundary; they are not claims that production Rust crates already exist.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import sys
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_MANIFEST = ROOT / "contracts" / "runtime-profiles" / "v1" / "profiles.json"
DEFAULT_SCHEMA = ROOT / "contracts" / "runtime-profiles" / "v1" / "profile-set.schema.json"

REQUIRED_RUNTIME_METADATA = (
    "function-identity",
    "code-generation-cache-identity",
    "frame-layout",
    "environment-capture-descriptors",
    "suspension-state",
    "verifier-summary",
)
EXPECTED_LANGUAGE_SEMANTICS = {
    "dynamicCode": "preserved",
    "featureGating": "host-and-tooling-only",
}
TOOLING_COMPONENTS = {
    "debuggerApi": "debugger-api",
    "interpreterHooks": "interpreter-debug-hooks",
    "dapTransport": "dap-transport",
    "inspector": "runtime-inspector",
    "inspectorUi": "inspector-ui",
    "repl": "repl",
    "profilerHooks": "profiler-hooks",
    "hotReload": "hot-reload",
}
NODE_COMPONENTS = {"node-module-resolver", "node-compat-api"}
DEBUG_REQUIRED_COMPONENTS = {
    "debugger-api",
    "interpreter-debug-hooks",
    "source-debug-metadata",
}

DEVELOPMENT_REQUIRED_COMPONENTS = {
    "dap-transport",
    "profiler-hooks",
    "hot-reload",
}
REQUIRED_PROFILE_DIMENSIONS = {
    ("debug", "portable-interpreter", "none"),
    ("debug", "hybrid-native", "none"),
    ("development", "portable-interpreter", "none"),
    ("development", "hybrid-native", "none"),
    ("release", "portable-interpreter", "none"),
    ("release", "hybrid-native", "none"),
    ("release", "portable-interpreter", "node"),
    ("release", "hybrid-native", "node"),
}


class ProfileValidationError(ValueError):
    """A stable, actionable profile or dependency-graph validation failure."""


def _fail(message: str) -> None:
    raise ProfileValidationError(message)


def _matches_type(value: Any, expected: str) -> bool:
    if expected == "object":
        return isinstance(value, dict)
    if expected == "array":
        return isinstance(value, list)
    if expected == "string":
        return isinstance(value, str)
    if expected == "boolean":
        return type(value) is bool
    if expected == "integer":
        return type(value) is int
    if expected == "number":
        return type(value) in (int, float)
    if expected == "null":
        return value is None
    return False


def validate_schema_instance(value: Any, schema: dict[str, Any], location: str = "$") -> None:
    """Small dependency-free validator for the JSON Schema keywords used by this contract."""
    expected = schema.get("type")
    if expected is not None:
        allowed = expected if isinstance(expected, list) else [expected]
        if not any(_matches_type(value, item) for item in allowed):
            _fail(f"{location}: expected type {' or '.join(allowed)}")
    if "const" in schema and json.dumps(value, sort_keys=True) != json.dumps(schema["const"], sort_keys=True):
        _fail(f"{location}: expected constant {schema['const']!r}")
    if "enum" in schema and value not in schema["enum"]:
        _fail(f"{location}: value {value!r} is outside enum")

    if isinstance(value, str):
        if len(value) < schema.get("minLength", 0):
            _fail(f"{location}: string is shorter than minLength")
        if len(value) > schema.get("maxLength", float("inf")):
            _fail(f"{location}: string exceeds maxLength")
        pattern = schema.get("pattern")
        if pattern and re.fullmatch(pattern, value) is None:
            _fail(f"{location}: string does not match pattern {pattern!r}")

    if isinstance(value, list):
        if len(value) < schema.get("minItems", 0):
            _fail(f"{location}: array has too few items")
        if len(value) > schema.get("maxItems", float("inf")):
            _fail(f"{location}: array has too many items")
        if schema.get("uniqueItems"):
            canonical = [json.dumps(item, sort_keys=True, ensure_ascii=False) for item in value]
            if len(canonical) != len(set(canonical)):
                _fail(f"{location}: array items must be unique")
        item_schema = schema.get("items")
        if item_schema:
            for index, item in enumerate(value):
                validate_schema_instance(item, item_schema, f"{location}[{index}]")

    if isinstance(value, dict):
        for key in schema.get("required", []):
            if key not in value:
                _fail(f"{location}: required property {key!r} is missing")
        properties = schema.get("properties", {})
        if schema.get("additionalProperties") is False:
            unexpected = sorted(set(value) - set(properties))
            if unexpected:
                _fail(f"{location}: unexpected properties: {', '.join(unexpected)}")
        for key, item in value.items():
            child_schema = properties.get(key)
            if child_schema is not None:
                validate_schema_instance(item, child_schema, f"{location}.{key}")


def _component_index(document: dict[str, Any]) -> dict[str, dict[str, Any]]:
    components: dict[str, dict[str, Any]] = {}
    flags: dict[str, str] = {}
    for component in document["components"]:
        component_id = component["id"]
        if component_id in components:
            _fail(f"duplicate component id: {component_id}")
        components[component_id] = component
        flag = component["featureFlag"]
        if flag is not None:
            if flag in flags:
                _fail(f"feature flag {flag!r} is shared by {flags[flag]!r} and {component_id!r}")
            flags[flag] = component_id
        if component["kind"] == "developer-tooling" and component["semanticEffects"]:
            _fail(f"developer tooling component {component_id!r} must not have ECMAScript semantic effects")
        if component["packaging"] == "tooling-only" and component["semanticEffects"]:
            _fail(f"tooling-only component {component_id!r} must not have ECMAScript semantic effects")
        if component["stripPolicy"] == "never" and component["kind"] != "semantic-core":
            _fail(f"only semantic-core components may use stripPolicy=never ({component_id!r})")

    semantics = components.get("ecmascript-semantics")
    if semantics is None or semantics["kind"] != "semantic-core" or semantics["stripPolicy"] != "never":
        _fail("ecmascript-semantics must be the non-strippable semantic-core component")
    if semantics["featureFlag"] is not None:
        _fail("ECMAScript semantics must not be controlled by a selectable feature flag")

    for component in components.values():
        for dependency in component["requires"]:
            if dependency not in components:
                _fail(f"component {component['id']!r} requires unknown component {dependency!r}")
    return components


def _dependency_closure(
    roots: list[str],
    components: dict[str, dict[str, Any]],
    profile_id: str,
) -> set[str]:
    included: set[str] = set()
    visiting: list[str] = []

    def visit(component_id: str) -> None:
        if component_id not in components:
            _fail(f"profile {profile_id!r} selects unknown component {component_id!r}")
        if component_id in visiting:
            cycle = " -> ".join([*visiting, component_id])
            _fail(f"profile {profile_id!r} contains component dependency cycle: {cycle}")
        if component_id in included:
            return
        visiting.append(component_id)
        for dependency in components[component_id]["requires"]:
            visit(dependency)
        visiting.pop()
        included.add(component_id)

    for root in roots:
        visit(root)
    return included


def _validate_rust_build_settings(profile_id: str, profile: dict[str, Any]) -> None:
    """Keep Rust compiler optimisation/debug/stripping flags explicit and mode-consistent."""
    mode = profile["buildMode"]
    backend = profile["executionBackend"]
    build = profile["rustBuild"]

    expected: dict[str, Any] = {
        "panic": "unwind",
    }
    if mode == "debug":
        expected.update({
            "optLevel": "0",
            "debug": True,
            "debugAssertions": True,
            "overflowChecks": True,
            "lto": "off",
            "codegenUnits": 256,
            "strip": "none",
        })
    elif mode == "development":
        expected.update({
            "optLevel": "1",
            "debug": True,
            "debugAssertions": True,
            "overflowChecks": True,
            "lto": "off",
            "codegenUnits": 16,
            "strip": "none",
        })
    else:
        expected.update({
            "optLevel": "3",
            "debug": backend == "hybrid-native",
            "debugAssertions": False,
            "overflowChecks": False,
            "lto": "thin",
            "codegenUnits": 1,
            "strip": "debuginfo" if backend == "hybrid-native" else "symbols",
        })

    mismatches = {
        key: (build[key], value)
        for key, value in expected.items()
        if build[key] != value
    }
    if mismatches:
        _fail(f"profile {profile_id!r} rustBuild settings conflict with {mode}/{backend}: {mismatches}")


def build_profile_plans(document: dict[str, Any], schema: dict[str, Any]) -> dict[str, dict[str, Any]]:
    validate_schema_instance(document, schema)
    if document["languageSemantics"] != EXPECTED_LANGUAGE_SEMANTICS:
        _fail("languageSemantics must preserve ECMAScript dynamic-code semantics; runtime profiles may gate only host/tooling features")
    if tuple(document["mandatoryRuntimeMetadata"]) != REQUIRED_RUNTIME_METADATA:
        _fail("mandatoryRuntimeMetadata differs from the verifier/runtime contract; required metadata cannot be stripped")

    components = _component_index(document)
    component_ids = set(components)
    # Reject malformed cycles even in currently unused components; a later profile
    # must not make a previously latent cycle part of a build plan.
    for component_id in sorted(components):
        _dependency_closure([component_id], components, "component-registry")
    profiles_by_id: dict[str, dict[str, Any]] = {}
    plans: dict[str, dict[str, Any]] = {}

    for profile in document["profiles"]:
        profile_id = profile["id"]
        if profile_id in profiles_by_id:
            _fail(f"duplicate profile id: {profile_id}")
        profiles_by_id[profile_id] = profile

    for profile_id, profile in profiles_by_id.items():
        _validate_rust_build_settings(profile_id, profile)
        unknown_exclusions = sorted(set(profile["excludeComponents"]) - component_ids)
        if unknown_exclusions:
            _fail(f"profile {profile_id!r} excludes unknown components: {unknown_exclusions}")

        included = _dependency_closure(profile["includeRoots"], components, profile_id)
        excluded = set(profile["excludeComponents"])
        conflicts = sorted(included & excluded)
        if conflicts:
            _fail(
                f"profile {profile_id!r} cannot strip components required by its dependency closure: "
                + ", ".join(conflicts)
            )

        required_components = {
            component_id
            for component_id, component in components.items()
            if component["stripPolicy"] in ("never", "retain")
        }
        missing_required = sorted(required_components - included)
        if missing_required:
            _fail(
                f"profile {profile_id!r} strips components with a non-strippable policy: "
                + ", ".join(missing_required)
            )

        if "ecmascript-semantics" not in included:
            _fail(f"profile {profile_id!r} does not retain ECMAScript semantics")
        if "portable-interpreter" not in included:
            _fail(f"profile {profile_id!r} must retain the portable interpreter")
        if "host-module-registry" not in included:
            _fail(f"profile {profile_id!r} must retain the declared host-module registration boundary")

        if profile["executionBackend"] == "portable-interpreter":
            if "native-aot-execution" in included or "native-root-maps" in included:
                _fail(f"profile {profile_id!r} selects portable interpretation but includes native AOT components")
        else:
            required_native = {"native-aot-execution", "native-root-maps"}
            missing_native = sorted(required_native - included)
            if missing_native:
                _fail(f"profile {profile_id!r} is hybrid-native but misses {missing_native}")
            if "portable-interpreter" not in included:
                _fail(f"profile {profile_id!r} must retain interpreter fallback for hybrid execution")

        node_included = NODE_COMPONENTS & included
        if profile["hostCompatibility"] == "node":
            if not NODE_COMPONENTS.issubset(included):
                _fail(f"profile {profile_id!r} selects Node compatibility without both resolver and API adapter")
        elif node_included:
            _fail(f"profile {profile_id!r} includes Node components without selecting hostCompatibility=node")

        tooling = profile["tooling"]
        for field, component_id in TOOLING_COMPONENTS.items():
            actual = component_id in included
            if tooling[field] is not actual:
                _fail(
                    f"profile {profile_id!r} tooling.{field}={tooling[field]!r} "
                    f"does not match component {component_id!r} inclusion ({actual})"
                )

        if profile["buildMode"] in ("debug", "development"):
            missing_debug = sorted(DEBUG_REQUIRED_COMPONENTS - included)
            if missing_debug:
                _fail(f"profile {profile_id!r} must preserve debug API/hooks/source metadata: {missing_debug}")
            if profile["metadata"]["sourceDebugMetadata"] != "embedded":
                _fail(f"profile {profile_id!r} must embed source/debug metadata")
            if profile["metadata"]["sourceMaps"] != "embedded":
                _fail(f"profile {profile_id!r} must embed source maps in Debug/Development")
        if profile["buildMode"] == "development":
            missing_development = sorted(DEVELOPMENT_REQUIRED_COMPONENTS - included)
            if missing_development:
                _fail(f"profile {profile_id!r} must include DAP, profiler hooks and Hot Reload: {missing_development}")

        source_metadata_included = "source-debug-metadata" in included
        if source_metadata_included != (profile["metadata"]["sourceDebugMetadata"] == "embedded"):
            _fail(f"profile {profile_id!r} source metadata policy disagrees with its component graph")
        if profile["metadata"]["sourceMaps"] == "embedded" and not source_metadata_included:
            _fail(f"profile {profile_id!r} cannot embed source maps without source/debug metadata")
        if profile["executionBackend"] == "portable-interpreter" and profile["metadata"]["debugSymbols"] != "not-applicable":
            _fail(f"profile {profile_id!r} has native debug-symbol policy without a native backend")
        if profile["executionBackend"] == "hybrid-native":
            expected_symbols = "embedded" if profile["buildMode"] in ("debug", "development") else "external"
            if profile["metadata"]["debugSymbols"] != expected_symbols:
                _fail(f"profile {profile_id!r} must use {expected_symbols} native debug symbols for {profile['buildMode']} mode")

        expected_flags = sorted(
            component["featureFlag"]
            for component_id, component in components.items()
            if component_id in included and component["featureFlag"] is not None
        )
        if sorted(profile["featureFlags"]) != expected_flags:
            missing = sorted(set(expected_flags) - set(profile["featureFlags"]))
            extra = sorted(set(profile["featureFlags"]) - set(expected_flags))
            _fail(f"profile {profile_id!r} featureFlags do not match graph closure (missing={missing}, extra={extra})")
        if any("dynamic-code" in flag or flag in ("language.eval", "language.function-constructor") for flag in profile["featureFlags"]):
            _fail(f"profile {profile_id!r} must not feature-gate ECMAScript dynamic-code semantics")

        linked = sorted(component_id for component_id in included if components[component_id]["packaging"] == "linked-runtime")
        runtime_modules = sorted(component_id for component_id in included if components[component_id]["packaging"] == "runtime-module")
        tooling_only = sorted(component_id for component_id in included if components[component_id]["packaging"] == "tooling-only")
        metadata = sorted(component_id for component_id in included if components[component_id]["packaging"] == "metadata")
        stripped = sorted(component_ids - included)
        plans[profile_id] = {
            "buildMode": profile["buildMode"],
            "executionBackend": profile["executionBackend"],
            "hostCompatibility": profile["hostCompatibility"],
            "rustBuild": profile["rustBuild"],
            "includedComponents": sorted(included),
            "strippedComponents": stripped,
            "linkedRuntimeComponents": linked,
            "runtimeModuleComponents": runtime_modules,
            "toolingOnlyComponents": tooling_only,
            "metadataComponents": metadata,
            "featureFlags": expected_flags,
            "metadataPolicy": profile["metadata"],
        }

    actual_dimensions = {
        (profile["buildMode"], profile["executionBackend"], profile["hostCompatibility"])
        for profile in profiles_by_id.values()
    }
    missing_dimensions = sorted(REQUIRED_PROFILE_DIMENSIONS - actual_dimensions)
    if missing_dimensions:
        _fail(f"runtime profile matrix is missing required build/backend/host combinations: {missing_dimensions}")

    if len(actual_dimensions) != len(profiles_by_id):
        _fail("runtime profile matrix contains duplicate buildMode/executionBackend/hostCompatibility combinations")

    return plans


def build_report(
    document: dict[str, Any],
    schema: dict[str, Any],
    plans: dict[str, dict[str, Any]],
    manifest_path: Path,
    schema_path: Path,
    proof_binary: Path | None,
) -> dict[str, Any]:
    binary_measurement: dict[str, Any]
    if proof_binary is not None:
        if not proof_binary.is_file():
            _fail(f"requested M0 proof binary does not exist: {proof_binary}")
        binary_bytes = proof_binary.stat().st_size
        binary_measurement = {
            "name": proof_binary.name,
            "bytes": binary_bytes,
            "sha256": hashlib.sha256(proof_binary.read_bytes()).hexdigest(),
            "scope": "OGM-25 typed-host-dispatch M0 proof executable; not an Ogma Runtime binary",
        }
    else:
        binary_measurement = {
            "status": "not-measured",
            "scope": "OGM-25 typed-host-dispatch M0 proof executable; not an Ogma Runtime binary",
        }

    return {
        "task": "OGM-28",
        "contractVersion": document["schemaVersion"],
        "result": "PASS",
        "executionBoundary": "M0 logical component/profile proof; no production Rust Runtime or Cargo workspace exists",
        "repositoryRevision": os.environ.get("GITHUB_SHA"),
        "toolchain": {
            "rustc": os.environ.get("RUSTC_VERSION"),
            "cargo": os.environ.get("CARGO_VERSION"),
            "python": platform.python_version(),
            "platform": platform.platform(),
        },
        "contractMeasurements": {
            "profileSetBytes": manifest_path.stat().st_size,
            "profileSchemaBytes": schema_path.stat().st_size,
            "logicalComponentCount": len(document["components"]),
            "profileCount": len(plans),
            "profiles": {
                profile_id: {
                    "includedComponentCount": len(plan["includedComponents"]),
                    "strippedComponentCount": len(plan["strippedComponents"]),
                    "linkedRuntimeComponentCount": len(plan["linkedRuntimeComponents"]),
                    "runtimeModuleComponentCount": len(plan["runtimeModuleComponents"]),
                    "toolingOnlyComponentCount": len(plan["toolingOnlyComponents"]),
                    "metadataComponentCount": len(plan["metadataComponents"]),
                }
                for profile_id, plan in plans.items()
            },
            "m0ProofExecutable": binary_measurement,
            "productionRuntimeExecutable": {
                "status": "not-implemented",
                "bytes": None,
                "reason": "The repository is still M0 bootstrap and has no production Runtime/workspace; a per-profile Runtime binary delta is not yet measurable.",
            },
        },
        "profilePlans": plans,
        "limitations": [
            "Logical component IDs are architectural build boundaries, not existing Cargo crate names or claims that the components are implemented.",
            "OGM-25 remains the authority for concrete host-module/native dependency closure and fail-closed runtime-import/native-module diagnostics.",
            "OGM-33 remains the authority for debugger DTO/source metadata and the executable-payload-preserving metadata-strip proof.",
            "The OGM-25 proof executable size is a measured tooling baseline only; it must not be presented as the size of any Ogma Runtime profile.",
        ],
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    parser.add_argument("--schema", type=Path, default=DEFAULT_SCHEMA)
    parser.add_argument("--report", type=Path)
    parser.add_argument("--proof-binary", type=Path)
    args = parser.parse_args()

    try:
        document = json.loads(args.manifest.read_text(encoding="utf-8"))
        schema = json.loads(args.schema.read_text(encoding="utf-8"))
        plans = build_profile_plans(document, schema)
        report = build_report(document, schema, plans, args.manifest, args.schema, args.proof_binary)
    except (OSError, json.JSONDecodeError, ProfileValidationError) as exc:
        print(f"Runtime profile validation failed: {exc}", file=sys.stderr)
        return 1

    if args.report:
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")

    print(
        f"Validated OGM-28 runtime profiles: {len(plans)} profiles, "
        f"{len(document['components'])} logical components."
    )
    if args.proof_binary:
        print(f"Measured OGM-25 M0 proof executable: {report['contractMeasurements']['m0ProofExecutable']['bytes']} bytes.")
    print("Production Runtime binary size is not measurable in the current M0 bootstrap.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
