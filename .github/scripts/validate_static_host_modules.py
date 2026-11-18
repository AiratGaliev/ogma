#!/usr/bin/env python3
"""Validate OGM-25 JSON Schemas and their checked-in fixtures."""

from __future__ import annotations

import argparse
import json
import re
from pathlib import Path
from typing import Any
from urllib.parse import urldefrag


ROOT = Path(__file__).resolve().parents[2]
CONTRACTS = ROOT / "contracts" / "host-modules" / "v1"
FIXTURES = ROOT / "benchmarks" / "typed-host-dispatch" / "tests" / "fixtures"


def load_json(path: Path) -> Any:
    with path.open("r", encoding="utf-8") as stream:
        return json.load(stream)


def resolve_pointer(document: Any, pointer: str) -> Any:
    if not pointer:
        return document
    current = document
    for raw_part in pointer.lstrip("/").split("/"):
        part = raw_part.replace("~1", "/").replace("~0", "~")
        current = current[part] if isinstance(current, dict) else current[int(part)]
    return current


def validate(
    value: Any,
    schema: dict[str, Any],
    schema_path: Path,
    documents: dict[Path, Any],
    location: str = "$",
) -> None:
    reference = schema.get("$ref")
    if reference:
        target, fragment = urldefrag(reference)
        target_path = (schema_path.parent / target).resolve() if target else schema_path
        target_doc = documents.get(target_path)
        if target_doc is None:
            target_doc = load_json(target_path)
            documents[target_path] = target_doc
        resolved = resolve_pointer(target_doc, fragment)
        validate(value, resolved, target_path, documents, location)
        return

    expected_type = schema.get("type")
    if expected_type == "object" and not isinstance(value, dict):
        raise ValueError(f"{location}: expected object")
    if expected_type == "array" and not isinstance(value, list):
        raise ValueError(f"{location}: expected array")
    if expected_type == "string" and not isinstance(value, str):
        raise ValueError(f"{location}: expected string")
    if expected_type == "integer" and type(value) is not int:
        raise ValueError(f"{location}: expected integer")
    if expected_type == "boolean" and type(value) is not bool:
        raise ValueError(f"{location}: expected boolean")

    if "const" in schema and value != schema["const"]:
        raise ValueError(f"{location}: expected constant {schema['const']!r}")
    if "not" in schema and "enum" in schema["not"] and value in schema["not"]["enum"]:
        raise ValueError(f"{location}: value {value!r} is explicitly forbidden")
    if "enum" in schema and value not in schema["enum"]:
        raise ValueError(f"{location}: value {value!r} is outside enum")
    if isinstance(value, str):
        if len(value) < schema.get("minLength", 0):
            raise ValueError(f"{location}: string is shorter than minLength")
        if len(value) > schema.get("maxLength", float("inf")):
            raise ValueError(f"{location}: string exceeds maxLength")
        pattern = schema.get("pattern")
        if pattern and re.search(pattern, value) is None:
            raise ValueError(f"{location}: string does not match pattern {pattern!r}")
    if type(value) is int:
        if value < schema.get("minimum", float("-inf")):
            raise ValueError(f"{location}: integer is below minimum")
        if value > schema.get("maximum", float("inf")):
            raise ValueError(f"{location}: integer exceeds maximum")
    if isinstance(value, list):
        if len(value) < schema.get("minItems", 0):
            raise ValueError(f"{location}: array has too few items")
        if len(value) > schema.get("maxItems", float("inf")):
            raise ValueError(f"{location}: array has too many items")
        if schema.get("uniqueItems"):
            canonical = [json.dumps(item, sort_keys=True, ensure_ascii=False) for item in value]
            if len(canonical) != len(set(canonical)):
                raise ValueError(f"{location}: array items must be unique")
        item_schema = schema.get("items")
        if item_schema:
            for index, item in enumerate(value):
                validate(item, item_schema, schema_path, documents, f"{location}[{index}]")
    if isinstance(value, dict):
        for key in schema.get("required", []):
            if key not in value:
                raise ValueError(f"{location}: required property {key!r} is missing")
        properties = schema.get("properties", {})
        if schema.get("additionalProperties") is False:
            unexpected = sorted(set(value) - set(properties))
            if unexpected:
                raise ValueError(f"{location}: unexpected properties: {', '.join(unexpected)}")
        for key, item in value.items():
            child_schema = properties.get(key)
            if child_schema is not None:
                validate(item, child_schema, schema_path, documents, f"{location}.{key}")


def expect_invalid(
    value: Any,
    schema: dict[str, Any],
    schema_path: Path,
    documents: dict[Path, Any],
    location: str,
) -> None:
    try:
        validate(value, schema, schema_path, documents, location)
    except ValueError:
        return
    raise ValueError(f"{location}: invalid fixture was unexpectedly accepted")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--report", type=Path)
    args = parser.parse_args()

    descriptor_path = CONTRACTS / "descriptor.schema.json"
    profile_path = CONTRACTS / "profile.schema.json"
    descriptor_fixture_path = FIXTURES / "ogma-math.module.json"
    profile_fixture_path = FIXTURES / "ogma-game.profile.json"
    declaration_path = FIXTURES / "ogma-math.d.ts"

    documents = {
        descriptor_path.resolve(): load_json(descriptor_path),
        profile_path.resolve(): load_json(profile_path),
    }
    descriptor = load_json(descriptor_fixture_path)
    profile = load_json(profile_fixture_path)
    validate(descriptor, documents[descriptor_path.resolve()], descriptor_path.resolve(), documents)
    validate(profile, documents[profile_path.resolve()], profile_path.resolve(), documents)

    for restricted_name in ("arguments", "eval"):
        invalid_descriptor = json.loads(json.dumps(descriptor))
        invalid_descriptor["exports"][0]["name"] = restricted_name
        expect_invalid(
            invalid_descriptor,
            documents[descriptor_path.resolve()],
            descriptor_path.resolve(),
            documents,
            f"$.exports[0].name({restricted_name})",
        )

    exports = descriptor["exports"]
    names = [item["name"] for item in exports]
    if names != ["add", "log"]:
        raise ValueError(f"unexpected descriptor fixture exports: {names!r}")
    if profile["runtimeImports"][0]["specifier"] not in profile["enabledModules"]:
        raise ValueError("profile fixture strips a referenced runtime import")
    exported_names = set(names)
    for runtime_import in profile["runtimeImports"]:
        if runtime_import["kind"] == "static-named-esm":
            unknown = sorted(set(runtime_import["importedNames"]) - exported_names)
            if unknown:
                raise ValueError(f"runtime import references unknown exports: {unknown!r}")
    if not declaration_path.is_file() or 'declare module "ogma:math"' not in declaration_path.read_text(encoding="utf-8"):
        raise ValueError("TypeScript declaration fixture is missing or malformed")

    report = {
        "task": "OGM-25",
        "descriptorSchemaVersion": documents[descriptor_path.resolve()]["properties"]["descriptorVersion"]["const"],
        "profileSchemaId": documents[profile_path.resolve()]["$id"],
        "descriptorFixture": descriptor_fixture_path.relative_to(ROOT).as_posix(),
        "profileFixture": profile_fixture_path.relative_to(ROOT).as_posix(),
        "exports": names,
        "typescriptFixture": declaration_path.relative_to(ROOT).as_posix(),
        "result": "PASS",
    }
    if args.report:
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(f"Validated OGM-25 descriptor/profile schemas and fixtures ({len(names)} exports).")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
