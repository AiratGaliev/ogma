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
    if "const" in schema and value != schema["const"]:
        _fail(f"{location}: expected constant {schema['const']!r}")
    if "enum" in schema and value not in schema["enum"]:
        _fail(f"{location}: value {value!r} is outside enum")

    if isinstance(value, str):
        if len(value) < schema.get("minLength", 0):
            _fail(f"{location}: string is shorter than minLength")
        if len(value) > schema.get("maxLength", float("inf")):
            _fail(f"{location}: string exceeds maxLength")
        pattern = schema.get("pattern")
        if pattern and re.search(pattern, value) is None:
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
