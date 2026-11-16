#!/usr/bin/env python3
"""Dependency-free reference verifier for the Ogma .ogmb v1 M0 contract."""
from __future__ import annotations

import argparse
from bisect import bisect_left
from collections import deque
import hashlib
import json
import math
import re
import struct
import sys
from pathlib import Path, PurePosixPath
from typing import Any

FORMAT = "ogma.ogmb"
FORMAT_VERSION = "1.0"
VERIFIER_SCHEMA = "ogma.bytecode-verification"
VERIFIER_VERSION = "1.0.0"
MAGIC = b"OGMB"
HEADER = struct.Struct("<4sHHIQQQIII")
DIRECTORY_ENTRY = struct.Struct("<IIQQII")
HEADER_SIZE = 48
ENTRY_SIZE = 32
MAX_FILE_BYTES = 64 * 1024 * 1024
MAX_JSON_BYTES = 64 * 1024 * 1024
MAX_SECTIONS = 64
MAX_FIXTURES = 10_000
MAX_EXCEPTION_EDGES = 1_000_000
MAX_ALIGN = 64
SECTION_NAMES = {1: "manifest", 2: "code", 3: "constants", 4: "exceptions", 5: "source_map"}
REQUIRED_SECTIONS = {1, 2, 3, 4}
OPTIONAL_FLAG = 1
RESOURCE_KEYS = (
    "max_code_bytes", "max_instructions", "max_constants", "max_stack",
    "max_registers", "max_locals", "max_call_depth", "max_exception_regions",
)
HARD_LIMITS = {
    "max_code_bytes": MAX_FILE_BYTES,
    "max_instructions": 1_000_000,
    "max_constants": 262_144,
    "max_stack": 65_536,
    "max_registers": 65_536,
    "max_locals": 65_536,
    "max_call_depth": 4_096,
    "max_exception_regions": 65_536,
}
ID_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_.:-]{0,127}$")
HEX_RE = re.compile(r"^[0-9a-f]{64}$")

# (stack pop, stack push, register reads, register writes)
OPERATION_EFFECTS = {
    "nop": (0, 0, 0, 0),
    "constant": (0, 1, 0, 1),
    "unary": (1, 1, 1, 1),
    "binary": (2, 1, 2, 1),
    "call": (None, 1, None, 1),
    "load_local": (0, 1, 0, 1),
    "store_local": (1, 0, 1, 0),
    "jump": (0, 0, 0, 0),
    "branch": (1, 0, 1, 0),
    "return": (1, 0, 1, 0),
    "throw": (1, 0, 1, 0),
    "suspend": (1, 1, 1, 1),
}


class DuplicateKeyError(ValueError):
    pass


def _object_pairs(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise DuplicateKeyError(f"duplicate JSON object key: {key}")
        result[key] = value
    return result


def _reject_constant(value: str) -> None:
    raise ValueError(f"non-finite JSON number is not permitted: {value}")


def strict_json_loads(raw: bytes | str) -> Any:
    if isinstance(raw, bytes):
        if len(raw) > MAX_JSON_BYTES:
            raise ValueError(f"JSON exceeds hard limit of {MAX_JSON_BYTES} bytes")
        raw = raw.decode("utf-8", errors="strict")
    elif len(raw.encode("utf-8")) > MAX_JSON_BYTES:
        raise ValueError(f"JSON exceeds hard limit of {MAX_JSON_BYTES} bytes")
    try:
        return json.loads(raw, object_pairs_hook=_object_pairs, parse_constant=_reject_constant)
    except RecursionError as exc:
        raise ValueError("JSON nesting exceeds parser limits") from exc


def load_json(path: Path) -> Any:
    if path.stat().st_size > MAX_JSON_BYTES:
        raise ValueError(f"JSON file exceeds hard limit: {path}")
    return strict_json_loads(path.read_bytes())


def is_uint(value: Any, maximum: int = 0xFFFFFFFF) -> bool:
    return isinstance(value, int) and not isinstance(value, bool) and 0 <= value <= maximum


def is_positive_uint(value: Any, maximum: int) -> bool:
    return is_uint(value, maximum) and value > 0


def bounded_json_value(value: Any, depth: int = 0) -> bool:
    if depth > 32:
        return False
    if value is None or isinstance(value, (str, bool)):
        return len(value) <= 65_536 if isinstance(value, str) else True
    if isinstance(value, int):
        return -(1 << 53) < value < (1 << 53)
    if isinstance(value, float):
        return math.isfinite(value)
    if isinstance(value, list):
        return len(value) <= 65_536 and all(bounded_json_value(v, depth + 1) for v in value)
    if isinstance(value, dict):
        return len(value) <= 65_536 and all(
            isinstance(k, str) and len(k) <= 256 and bounded_json_value(v, depth + 1)
            for k, v in value.items()
        )
    return False


def diagnostic(code: str, path: str, message: str) -> dict[str, str]:
    return {"code": code, "path": path, "message": message}


def repository_path(root: Path, relative: Any) -> Path:
    if not isinstance(relative, str) or not relative or "\\" in relative:
        raise ValueError("fixture path must be a non-empty repository-relative POSIX path")
    posix = PurePosixPath(relative)
    if posix.is_absolute() or ".." in posix.parts or posix.as_posix() != relative:
        raise ValueError("fixture path must be normalized and confined to the repository")
    base = root.resolve()
    target = (base / Path(*posix.parts)).resolve()
    target.relative_to(base)
    return target


SEMANTIC_TYPES = {
    "dynamic", "undefined", "null", "boolean", "number", "string", "symbol",
    "bigint", "object", "array", "function", "exception",
}
REPRESENTATIONS = {
    "dynamic": {"boxed"}, "undefined": {"boxed"}, "null": {"boxed"},
    "boolean": {"boxed", "i1"}, "number": {"boxed", "i32", "u32", "f64"},
    "string": {"boxed", "string_ref"}, "symbol": {"boxed", "symbol_ref"},
    "bigint": {"boxed", "bigint_ref"}, "object": {"boxed", "object_ref"},
    "array": {"boxed", "array_ref", "object_ref"},
    "function": {"boxed", "function_ref", "object_ref"},
    "exception": {"boxed", "exception_ref"},
}
BINDING_KINDS = {"var", "let", "const", "parameter", "catch", "import"}
MUTABLE_BINDING_KINDS = {"var", "let", "parameter", "catch"}
LOCATION_KINDS = {"local", "register", "environment", "capture"}


def validate_function_metadata(metadata: Any, limits: dict[str, int], code_digest: str) -> list[dict[str, str]]:
    errors: list[dict[str, str]] = []

    def err(code: str, path: str, message: str) -> None:
        errors.append(diagnostic(code, path, message))

    def stable_id(value: Any) -> bool:
        return isinstance(value, str) and ID_RE.fullmatch(value) is not None

    required = {
        "execution_model", "function", "code_generation", "code_cache", "frame_layout",
        "environment", "suspension_points", "inline_cache_slots", "verifier_summary",
    }
    if not isinstance(metadata, dict) or set(metadata) != required:
        err("metadata.fields", "$.manifest.metadata", "function metadata must define the exact v1 field set")
        return errors
    if metadata["execution_model"] not in ("stack", "register"):
        err("metadata.execution_model", "$.manifest.metadata.execution_model", "execution_model must be stack or register")

    function = metadata["function"]
    if not isinstance(function, dict) or set(function) != {"id", "name"}:
        err("metadata.function", "$.manifest.metadata.function", "function identity must define id and name")
    else:
        if not stable_id(function["id"]):
            err("metadata.function_id", "$.manifest.metadata.function.id", "function id must use the stable ASCII identifier grammar")
        if not isinstance(function["name"], str) or not function["name"] or len(function["name"]) > 512:
            err("metadata.function_name", "$.manifest.metadata.function.name", "function name must be a non-empty string of at most 512 characters")

    for key in ("code_generation", "code_cache"):
        value = metadata[key]
        path = f"$.manifest.metadata.{key}"
        if not isinstance(value, dict) or set(value) != {"id", "version"}:
            err("metadata.identity", path, f"{key} must define exactly id and version")
        else:
            if not stable_id(value["id"]):
                err("metadata.identity_id", path + ".id", "identity must use the stable ASCII identifier grammar")
            if not isinstance(value["version"], str) or not value["version"] or len(value["version"]) > 128:
                err("metadata.identity_version", path + ".version", "version must be a non-empty string of at most 128 characters")

    frame = metadata["frame_layout"]
    frame_fields = {"parameter_count", "local_count", "register_count"}
    if not isinstance(frame, dict) or set(frame) != frame_fields:
        err("metadata.frame_layout", "$.manifest.metadata.frame_layout", "frame_layout must define parameter_count, local_count and register_count")
        frame = {}
    else:
        for key, maximum in (
            ("parameter_count", min(limits["max_locals"], limits["max_registers"])),
            ("local_count", limits["max_locals"]),
            ("register_count", limits["max_registers"]),
        ):
            if not is_uint(frame[key], maximum):
                err("metadata.frame_bounds", f"$.manifest.metadata.frame_layout.{key}", f"must be an unsigned integer no greater than {maximum}")
        parameter_window = frame.get("register_count", 0) if metadata["execution_model"] == "register" else frame.get("local_count", 0)
        if is_uint(frame.get("parameter_count")) and is_uint(parameter_window) and frame["parameter_count"] > parameter_window:
            err("metadata.parameter_count", "$.manifest.metadata.frame_layout.parameter_count", "parameter_count exceeds the selected execution model's parameter storage window")

    environment = metadata["environment"]
    env_fields = {"id", "parent", "bindings", "captures", "locations"}
    if not isinstance(environment, dict) or set(environment) != env_fields:
        err("metadata.environment", "$.manifest.metadata.environment", "environment must define id, parent, bindings, captures and locations")
        environment = {}
    else:
        if not stable_id(environment["id"]):
            err("metadata.environment_id", "$.manifest.metadata.environment.id", "environment id must use the stable ASCII identifier grammar")
        parent = environment["parent"]
        if parent is not None and not stable_id(parent):
            err("metadata.environment_parent", "$.manifest.metadata.environment.parent", "parent must be null or a stable environment identifier")
        if parent == environment["id"]:
            err("metadata.environment_cycle", "$.manifest.metadata.environment.parent", "environment cannot be its own parent")

    bindings_by_id: dict[str, dict[str, Any]] = {}
    captures_by_id: dict[str, dict[str, Any]] = {}
    if not isinstance(environment.get("bindings"), list):
        err("metadata.bindings_shape", "$.manifest.metadata.environment.bindings", "bindings must be an array")
    if not isinstance(environment.get("captures"), list):
        err("metadata.captures_shape", "$.manifest.metadata.environment.captures", "captures must be an array")
    if not isinstance(environment.get("locations"), list):
        err("metadata.locations_shape", "$.manifest.metadata.environment.locations", "locations must be an array")
    if isinstance(environment.get("bindings"), list):
        if len(environment["bindings"]) > 65_536:
            err("resource.binding_count", "$.manifest.metadata.environment.bindings", "binding count exceeds hard verifier limit 65536")
        for index, binding in enumerate(environment["bindings"][:65_536]):
            path = f"$.manifest.metadata.environment.bindings[{index}]"
            fields = {"id", "name", "kind", "mutable", "semantic_type", "representation"}
            if not isinstance(binding, dict) or set(binding) != fields:
                err("metadata.binding_fields", path, "binding must define the exact portable Core IR binding fields")
                continue
            ident = binding["id"]
            if not stable_id(ident) or ident in bindings_by_id:
                err("metadata.binding_id", path + ".id", "binding id must be valid and unique")
            else:
                bindings_by_id[ident] = binding
            if not isinstance(binding["name"], str) or not binding["name"] or len(binding["name"]) > 512:
                err("metadata.binding_name", path + ".name", "binding name must be a non-empty string of at most 512 characters")
            if binding["kind"] not in BINDING_KINDS:
                err("metadata.binding_kind", path + ".kind", "unsupported lexical binding kind")
            if not isinstance(binding["mutable"], bool):
                err("metadata.binding_mutable", path + ".mutable", "binding mutability must be boolean")
            elif binding["kind"] in BINDING_KINDS and binding["mutable"] != (binding["kind"] in MUTABLE_BINDING_KINDS):
                err("metadata.binding_mutability", path + ".mutable", "binding mutability must match its declaration kind")
            semantic_type = binding["semantic_type"]
            representation = binding["representation"]
            if not isinstance(semantic_type, str) or not isinstance(representation, str) or semantic_type not in SEMANTIC_TYPES or representation not in REPRESENTATIONS.get(semantic_type, set()):
                err("metadata.binding_representation", path, "binding semantic type and representation are incompatible")

    if isinstance(environment.get("captures"), list):
        if len(environment["captures"]) > 65_536:
            err("resource.capture_count", "$.manifest.metadata.environment.captures", "capture count exceeds hard verifier limit 65536")
        seen_bindings: set[str] = set()
        for index, capture in enumerate(environment["captures"][:65_536]):
            path = f"$.manifest.metadata.environment.captures[{index}]"
            fields = {"id", "source_environment", "binding", "name", "mutable", "semantic_type", "representation", "mode"}
            if not isinstance(capture, dict) or set(capture) != fields:
                err("metadata.capture_fields", path, "capture must define the exact portable Core IR capture fields")
                continue
            ident, binding_id = capture["id"], capture["binding"]
            if not stable_id(ident) or ident in captures_by_id:
                err("metadata.capture_id", path + ".id", "capture id must be valid and unique")
            else:
                captures_by_id[ident] = capture
            if not stable_id(capture["source_environment"]) or capture["source_environment"] == environment.get("id"):
                err("metadata.capture_origin", path + ".source_environment", "capture must originate in a different stable environment")
            if not stable_id(binding_id) or binding_id in seen_bindings:
                err("metadata.capture_binding", path + ".binding", "imported binding must be valid and captured once")
            seen_bindings.add(binding_id)
            if not isinstance(capture["name"], str) or not capture["name"] or len(capture["name"]) > 512:
                err("metadata.capture_name", path + ".name", "capture name must be a non-empty string of at most 512 characters")
            if not isinstance(capture["mutable"], bool):
                err("metadata.capture_mutable", path + ".mutable", "capture mutability must be boolean")
            if capture["mode"] not in ("cell", "value"):
                err("metadata.capture_mode", path + ".mode", "capture mode must be cell or value")
            if capture["mutable"] is True and capture["mode"] != "cell":
                err("metadata.capture_mode", path + ".mode", "mutable captures must use cell mode")
            semantic_type = capture["semantic_type"]
            if not isinstance(semantic_type, str) or not isinstance(capture["representation"], str) or semantic_type not in SEMANTIC_TYPES or capture["representation"] not in REPRESENTATIONS.get(semantic_type, set()):
                err("metadata.capture_representation", path, "capture semantic type and representation are incompatible")

    if isinstance(environment.get("locations"), list):
        if len(environment["locations"]) > 131_072:
            err("resource.binding_location_count", "$.manifest.metadata.environment.locations", "binding location count exceeds hard verifier limit")
        located: set[str] = set()
        for index, location in enumerate(environment["locations"][:131_072]):
            path = f"$.manifest.metadata.environment.locations[{index}]"
            if not isinstance(location, dict) or set(location) != {"binding", "kind", "index"}:
                err("metadata.binding_location", path, "binding location must define binding, kind and index")
                continue
            binding_id = location["binding"]
            if not isinstance(binding_id, str) or binding_id not in bindings_by_id:
                err("metadata.binding_location_ref", path + ".binding", "binding location references an unknown lexical binding")
            if binding_id in located:
                err("metadata.binding_location_duplicate", path + ".binding", "lexical binding must have exactly one storage location")
            located.add(binding_id)
            kind, slot = location["kind"], location["index"]
            if kind not in LOCATION_KINDS:
                err("metadata.binding_location_kind", path + ".kind", "unsupported binding storage location")
            maximum = {
                "local": frame.get("local_count", 0),
                "register": frame.get("register_count", 0),
                "environment": len(environment.get("bindings", [])),
                "capture": len(environment.get("captures", [])),
            }.get(kind, 0)
            if not is_uint(slot) or slot >= maximum:
                err("metadata.binding_location_bounds", path + ".index", "binding storage index is outside its declared frame/environment window")
        if located != set(bindings_by_id):
            err("metadata.binding_location_coverage", "$.manifest.metadata.environment.locations", "every lexical binding must have exactly one storage location")

    suspension_points = metadata["suspension_points"]
    if not isinstance(suspension_points, list) or len(suspension_points) > 65_536:
        err("metadata.suspension_points", "$.manifest.metadata.suspension_points", "suspension_points must be a bounded array")
    else:
        seen: set[str] = set()
        offsets: set[int] = set()
        for index, point in enumerate(suspension_points):
            path = f"$.manifest.metadata.suspension_points[{index}]"
            fields = {"id", "kind", "offset", "resume_offset", "live_locals", "stack_depth", "live_registers"}
            if not isinstance(point, dict) or set(point) != fields:
                err("metadata.suspension_fields", path, "suspension point has missing or unknown fields")
                continue
            if not stable_id(point["id"]) or point["id"] in seen:
                err("metadata.suspension_id", path + ".id", "suspension point id must be valid and unique")
            seen.add(point["id"])
            if point["kind"] not in ("await", "yield", "yield_delegate"):
                err("metadata.suspension_kind", path + ".kind", "unsupported suspension kind")
            if not is_uint(point["offset"]) or not is_uint(point["resume_offset"]):
                err("metadata.suspension_offset", path, "suspension offsets must be uint32 byte offsets")
            elif point["offset"] in offsets:
                err("metadata.suspension_duplicate", path + ".offset", "only one suspension point may own a bytecode offset")
            else:
                offsets.add(point["offset"])
            live_locals = point["live_locals"]
            if not isinstance(live_locals, list) or len(live_locals) > frame.get("local_count", 0) or any(
                not is_uint(slot) or slot >= frame.get("local_count", 0) for slot in live_locals
            ) or (isinstance(live_locals, list) and len(set(live_locals)) != len(live_locals)):
                err("metadata.suspension_locals", path + ".live_locals", "live_locals must be unique indexes within local_count")
            if metadata["execution_model"] == "stack":
                if not is_uint(point["stack_depth"], limits["max_stack"]) or point["live_registers"] is not None:
                    err("metadata.suspension_stack_state", path, "stack model requires bounded stack_depth and null live_registers")
            elif metadata["execution_model"] == "register":
                registers = point["live_registers"]
                if point["stack_depth"] is not None or not isinstance(registers, list) or len(registers) > frame.get("register_count", 0) or any(
                    not is_uint(reg) or reg >= frame.get("register_count", 0) for reg in registers
                ) or (isinstance(registers, list) and len(set(registers)) != len(registers)):
                    err("metadata.suspension_register_state", path, "register model requires null stack_depth and unique bounded live_registers")

    inline_slots = metadata["inline_cache_slots"]
    if not isinstance(inline_slots, list) or len(inline_slots) > 65_536:
        err("metadata.inline_cache_slots", "$.manifest.metadata.inline_cache_slots", "inline_cache_slots must be a bounded array")
    else:
        seen_ids: set[str] = set()
        seen_slots: set[int] = set()
        for index, slot in enumerate(inline_slots):
            path = f"$.manifest.metadata.inline_cache_slots[{index}]"
            if not isinstance(slot, dict) or set(slot) != {"id", "offset", "slot", "kind"}:
                err("metadata.inline_cache_fields", path, "inline-cache slot must define id, offset, slot and kind")
                continue
            if not stable_id(slot["id"]) or slot["id"] in seen_ids:
                err("metadata.inline_cache_id", path + ".id", "inline-cache id must be valid and unique")
            seen_ids.add(slot["id"])
            if not is_uint(slot["offset"]) or not is_uint(slot["slot"]):
                err("metadata.inline_cache_index", path, "inline-cache offset and slot must be uint32")
            elif slot["slot"] in seen_slots:
                err("metadata.inline_cache_duplicate", path + ".slot", "inline-cache slot indexes must be unique")
            else:
                seen_slots.add(slot["slot"])
            if not isinstance(slot["kind"], str) or slot["kind"] not in ("property", "element", "call", "construct"):
                err("metadata.inline_cache_kind", path + ".kind", "unsupported inline-cache metadata kind")

    summary = metadata["verifier_summary"]
    summary_fields = {
        "schema", "version", "code_sha256", "instruction_count", "max_stack_depth",
        "max_register_index", "exception_region_count", "constant_count",
    }
    if not isinstance(summary, dict) or set(summary) != summary_fields:
        err("metadata.verifier_summary", "$.manifest.metadata.verifier_summary", "verifier_summary has missing or unknown fields")
    else:
        if summary["schema"] != "ogma.bytecode-verification-summary" or summary["version"] != "1.0.0":
            err("metadata.verifier_summary_version", "$.manifest.metadata.verifier_summary", "unsupported verifier summary version")
        if summary["code_sha256"] != code_digest:
            err("metadata.verifier_summary_digest", "$.manifest.metadata.verifier_summary.code_sha256", "verifier summary digest does not match code section")
        for key, maximum in (
            ("instruction_count", limits["max_instructions"]),
            ("max_stack_depth", limits["max_stack"]),
            ("exception_region_count", limits["max_exception_regions"]),
            ("constant_count", limits["max_constants"]),
        ):
            if not is_uint(summary[key], maximum):
                err("metadata.verifier_summary_range", f"$.manifest.metadata.verifier_summary.{key}", "summary count/depth exceeds the declared resource limit")
        register_index = summary["max_register_index"]
        if register_index is not None and not is_uint(register_index, max(0, limits["max_registers"] - 1)):
            err("metadata.verifier_summary_register", "$.manifest.metadata.verifier_summary.max_register_index", "max_register_index is outside declared register bounds")
        if metadata["execution_model"] == "stack" and register_index is not None:
            err("metadata.verifier_summary_model", "$.manifest.metadata.verifier_summary.max_register_index", "stack model requires null max_register_index")
        if metadata["execution_model"] == "register" and register_index is None:
            err("metadata.verifier_summary_model", "$.manifest.metadata.verifier_summary.max_register_index", "register model requires a maximum register index")
        if metadata["execution_model"] == "register" and summary["max_stack_depth"] != 0:
            err("metadata.verifier_summary_model", "$.manifest.metadata.verifier_summary.max_stack_depth", "register model requires max_stack_depth zero")

    return errors


def validate_source_map(source_map: Any, metadata: dict[str, Any], code_size: int) -> list[dict[str, str]]:
    errors: list[dict[str, str]] = []

    def err(code: str, path: str, message: str) -> None:
        errors.append(diagnostic(code, path, message))

    if not isinstance(source_map, dict) or set(source_map) != {
        "version", "files", "locations", "lexical_scopes", "variable_locations"
    }:
        return [diagnostic("source_map.fields", "$.source_map", "source_map must define the exact v1 field set")]
    if source_map["version"] != "1.0.0":
        err("source_map.version", "$.source_map.version", "unsupported source-map version")
    files = source_map["files"]
    if not isinstance(files, list) or len(files) > 65_536:
        err("source_map.files", "$.source_map.files", "files must be a bounded array")
        files = []
    file_ids: set[str] = set()
    for index, file in enumerate(files):
        path = f"$.source_map.files[{index}]"
        if not isinstance(file, dict) or set(file) not in (
            {"id", "path", "language"},
            {"id", "path", "language", "sha256"},
        ):
            err("source_map.file_fields", path, "source file must define id, path, language and optional sha256")
            continue
        ident = file["id"]
        if not isinstance(ident, str) or not ID_RE.fullmatch(ident) or ident in file_ids:
            err("source_map.file_id", path + ".id", "source file id must be stable and unique")
        file_ids.add(ident)
        name = file["path"]
        if not isinstance(name, str) or not name or "\\" in name or PurePosixPath(name).is_absolute() or ".." in PurePosixPath(name).parts or PurePosixPath(name).as_posix() != name:
            err("source_map.file_path", path + ".path", "source path must be normalized repository-relative POSIX")
        if file["language"] not in ("javascript", "typescript", "json", "synthetic"):
            err("source_map.file_language", path + ".language", "unsupported source language")
        if "sha256" in file and (not isinstance(file["sha256"], str) or not HEX_RE.fullmatch(file["sha256"])):
            err("source_map.file_digest", path + ".sha256", "source digest must be lowercase SHA-256 hex")

    meta_env = metadata["environment"]
    binding_ids = {b["id"] for b in meta_env["bindings"]}
    scope_ids: set[str] = set()
    scopes = source_map["lexical_scopes"]
    if not isinstance(scopes, list) or len(scopes) > 65_536:
        err("source_map.scopes", "$.source_map.lexical_scopes", "lexical_scopes must be a bounded array")
        scopes = []
    for index, scope in enumerate(scopes):
        path = f"$.source_map.lexical_scopes[{index}]"
        if not isinstance(scope, dict) or set(scope) != {"id", "parent", "start_offset", "end_offset", "bindings"}:
            err("source_map.scope_fields", path, "lexical scope has missing or unknown fields")
            continue
        if not isinstance(scope["id"], str) or not ID_RE.fullmatch(scope["id"]) or scope["id"] in scope_ids:
            err("source_map.scope_id", path + ".id", "scope id must be stable and unique")
        scope_ids.add(scope["id"])
        if scope["parent"] is not None and not isinstance(scope["parent"], str):
            err("source_map.scope_parent", path + ".parent", "scope parent must be null or an identifier")
        if not is_uint(scope["start_offset"]) or not is_uint(scope["end_offset"]) or not (scope["start_offset"] < scope["end_offset"] <= code_size):
            err("source_map.scope_range", path, "scope range must be a non-empty code byte range")
        if not isinstance(scope["bindings"], list) or any(binding not in binding_ids for binding in scope["bindings"]):
            err("source_map.scope_bindings", path + ".bindings", "scope bindings must reference declared lexical bindings")
    for index, scope in enumerate(scopes):
        if isinstance(scope, dict) and scope.get("parent") is not None and scope["parent"] not in scope_ids:
            err("source_map.scope_parent", f"$.source_map.lexical_scopes[{index}].parent", "scope parent is unknown")

    suspensions = {point["id"]: point for point in metadata["suspension_points"]}
    locations = source_map["locations"]
    if not isinstance(locations, list) or len(locations) > 1_000_000:
        err("source_map.locations", "$.source_map.locations", "locations must be a bounded array")
        locations = []
    seen_offsets: set[int] = set()
    for index, location in enumerate(locations):
        path = f"$.source_map.locations[{index}]"
        if not isinstance(location, dict) or set(location) != {"offset", "source"}:
            err("source_map.location_fields", path, "location must define offset and source")
            continue
        if not is_uint(location["offset"]) or location["offset"] >= code_size or location["offset"] in seen_offsets:
            err("source_map.location_offset", path + ".offset", "source map offset must be unique and inside code")
        seen_offsets.add(location["offset"])
        source = location["source"]
        if not isinstance(source, dict) or source.get("origin") not in ("user", "generated"):
            err("source_map.source", path + ".source", "source origin must be user or generated")
            continue
        common = {"origin", "scope", "bindings", "suspension", "inline_chain"}
        if source["origin"] == "user":
            expected = common | {"file", "start", "end"}
            if set(source) != expected:
                err("source_map.user_fields", path + ".source", "user source must contain file, start/end and common provenance fields")
                continue
            if source["file"] not in file_ids:
                err("source_map.file_ref", path + ".source.file", "source references an unknown source file")
            for pos_name in ("start", "end"):
                pos = source[pos_name]
                if not isinstance(pos, dict) or set(pos) != {"line", "column"} or not is_positive_uint(pos.get("line"), 0xFFFFFFFF) or not is_uint(pos.get("column")):
                    err("source_map.position", path + ".source." + pos_name, "position must define line >= 1 and uint32 column")
            if isinstance(source["start"], dict) and isinstance(source["end"], dict):
                begin, finish = source["start"], source["end"]
                if (finish.get("line", 0), finish.get("column", 0)) < (begin.get("line", 0), begin.get("column", 0)):
                    err("source_map.position_order", path + ".source", "source end must not precede source start")
        else:
            if set(source) != common | {"reason"} or not isinstance(source.get("reason"), str) or not source["reason"]:
                err("source_map.generated_fields", path + ".source", "generated source requires a non-empty reason and common provenance fields")
                continue
        if source.get("scope") is not None and source["scope"] not in scope_ids:
            err("source_map.scope_ref", path + ".source.scope", "source references an unknown lexical scope")
        if not isinstance(source.get("bindings"), list) or any(binding not in binding_ids for binding in source["bindings"]):
            err("source_map.binding_ref", path + ".source.bindings", "source references an unknown lexical binding")
        if source.get("suspension") is not None and source["suspension"] not in suspensions:
            err("source_map.suspension_ref", path + ".source.suspension", "source references an unknown suspension point")
        if not isinstance(source.get("inline_chain"), list) or len(source["inline_chain"]) > 64:
            err("source_map.inline_chain", path + ".source.inline_chain", "inline chain must be a bounded array")
        else:
            for frame_index, frame in enumerate(source["inline_chain"]):
                fp = f"{path}.source.inline_chain[{frame_index}]"
                if not isinstance(frame, dict) or set(frame) != {"function", "file", "start", "end"}:
                    err("source_map.inline_frame", fp, "inline frame must define function, file, start and end")
                    continue
                if not isinstance(frame["function"], str) or not ID_RE.fullmatch(frame["function"]) or frame["file"] not in file_ids:
                    err("source_map.inline_frame_ref", fp, "inline frame function or file reference is invalid")
                for pos_name in ("start", "end"):
                    pos = frame[pos_name]
                    if not isinstance(pos, dict) or set(pos) != {"line", "column"} or not is_positive_uint(pos.get("line"), 0xFFFFFFFF) or not is_uint(pos.get("column")):
                        err("source_map.inline_position", fp + "." + pos_name, "inline source position is invalid")

    variable_locations = source_map["variable_locations"]
    if not isinstance(variable_locations, list) or len(variable_locations) > 131_072:
        err("source_map.variable_locations", "$.source_map.variable_locations", "variable_locations must be a bounded array")
        variable_locations = []
    meta_locations = {
        location["binding"]: location
        for location in meta_env["locations"]
    }
    seen_variables: set[tuple[str, int, int]] = set()
    for index, variable in enumerate(variable_locations):
        path = f"$.source_map.variable_locations[{index}]"
        fields = {"binding", "kind", "index", "start_offset", "end_offset"}
        if not isinstance(variable, dict) or set(variable) != fields:
            err("source_map.variable_fields", path, "variable location has missing or unknown fields")
            continue
        binding = variable["binding"]
        key = (binding, variable["start_offset"], variable["end_offset"])
        if binding not in binding_ids or key in seen_variables:
            err("source_map.variable_binding", path + ".binding", "variable location references unknown/duplicate binding")
        seen_variables.add(key)
        expected = meta_locations.get(binding)
        if expected and (variable["kind"], variable["index"]) != (expected["kind"], expected["index"]):
            err("source_map.variable_storage", path, "debug variable location differs from canonical frame/environment binding location")
        if not is_uint(variable["start_offset"]) or not is_uint(variable["end_offset"]) or not (variable["start_offset"] < variable["end_offset"] <= code_size):
            err("source_map.variable_range", path, "variable location range must be a non-empty code byte range")

    return errors


class ContainerReader:
    """Validate the fixed .ogmb binary envelope without loading executable code."""

    def __init__(self) -> None:
        self.errors: list[dict[str, str]] = []

    def err(self, code: str, path: str, message: str) -> None:
        self.errors.append(diagnostic(code, path, message))

    def read(self, raw: bytes) -> dict[str, Any] | None:
        self.errors = []
        if not isinstance(raw, bytes) or len(raw) < HEADER_SIZE:
            self.err("container.truncated_header", "$", "container is shorter than the 48-byte header")
            return None
        if len(raw) > MAX_FILE_BYTES:
            self.err("resource.file_size", "$", f"container exceeds hard limit of {MAX_FILE_BYTES} bytes")
            return None

        (magic, major, minor, header_size, feature_flags, file_size, directory_offset,
         section_count, entry_size, reserved) = HEADER.unpack_from(raw)
        if magic != MAGIC:
            self.err("container.magic", "$.header.magic", "expected OGMB magic")
        if (major, minor) != (1, 0):
            self.err("container.version", "$.header.version", f"unsupported format version {major}.{minor}; supported version is 1.0")
        if header_size != HEADER_SIZE:
            self.err("container.header_size", "$.header.header_size", "v1 header_size must be exactly 48")
        if reserved != 0:
            self.err("container.reserved", "$.header.reserved", "reserved header bits must be zero")
        if feature_flags != 0:
            self.err("container.unknown_features", "$.header.feature_flags", "v1 defines no feature flags; unknown bits fail closed")
        if file_size != len(raw):
            self.err("container.file_size", "$.header.file_size", "declared file size does not match actual byte length")
        if directory_offset != HEADER_SIZE:
            self.err("container.directory_offset", "$.header.directory_offset", "v1 directory must immediately follow the header")
        if entry_size != ENTRY_SIZE:
            self.err("container.directory_entry_size", "$.header.directory_entry_size", "v1 directory entries must be exactly 32 bytes")
        if section_count > MAX_SECTIONS:
            self.err("container.section_count", "$.header.section_count", f"section count exceeds hard limit {MAX_SECTIONS}")
        if self.errors:
            return None

        directory_end = directory_offset + section_count * entry_size
        if directory_end > len(raw):
            self.err("container.truncated_directory", "$.directory", "section directory exceeds container bounds")
            return None

        entries: list[tuple[int, int, int, int, int, int]] = []
        seen: set[int] = set()
        intervals: list[tuple[int, int, int]] = []
        for index in range(section_count):
            base = directory_offset + index * entry_size
            section_type, flags, offset, size, alignment, entry_reserved = DIRECTORY_ENTRY.unpack_from(raw, base)
            path = f"$.directory[{index}]"
            if entry_reserved != 0:
                self.err("container.reserved", path + ".reserved", "reserved directory bits must be zero")
            if flags & ~OPTIONAL_FLAG:
                self.err("container.section_flags", path + ".flags", "unknown section flag bits are not permitted")
            if section_type in REQUIRED_SECTIONS and flags & OPTIONAL_FLAG:
                self.err("container.required_section_optional", path + ".flags", "required sections cannot be marked optional")
            if section_type == 5 and not (flags & OPTIONAL_FLAG):
                self.err("container.optional_section_required", path + ".flags", "source_map must be marked optional in v1")
            if section_type in seen:
                self.err("container.duplicate_section", path + ".type", f"section type {section_type} appears more than once")
            seen.add(section_type)
            if section_type not in SECTION_NAMES and not (flags & OPTIONAL_FLAG):
                self.err("container.unknown_required_section", path + ".type", f"unknown required section type {section_type}")
            if not is_positive_uint(alignment, MAX_ALIGN) or alignment & (alignment - 1):
                self.err("container.section_alignment", path + ".alignment", "alignment must be a power of two from 1 through 64")
            elif offset % alignment:
                self.err("container.section_alignment", path + ".offset", "section offset does not satisfy declared alignment")
            if offset < directory_end or offset > len(raw) or size > len(raw) - min(offset, len(raw)):
                self.err("container.section_bounds", path, "section overlaps the header/directory or exceeds file bounds")
            elif size == 0 and section_type in REQUIRED_SECTIONS:
                self.err("container.empty_required_section", path + ".size", "required sections must not be empty")
            else:
                intervals.append((offset, offset + size, index))
            entries.append((section_type, flags, offset, size, alignment, entry_reserved))

        for left, right in zip(sorted(intervals), sorted(intervals)[1:]):
            if right[0] < left[1]:
                self.err("container.section_overlap", f"$.directory[{right[2]}]", f"section overlaps directory entry {left[2]}")
        missing = REQUIRED_SECTIONS - seen
        for section_type in sorted(missing):
            self.err("container.missing_section", "$.directory", f"required section {SECTION_NAMES[section_type]} is missing")
        if self.errors:
            return None

        sections: dict[int, bytes] = {}
        for section_type, flags, offset, size, _alignment, _reserved in entries:
            if section_type in SECTION_NAMES:
                sections[section_type] = raw[offset:offset + size]

        decoded: dict[str, Any] = {
            "sections": sections,
            "feature_flags": feature_flags,
            "file_size": len(raw),
        }
        for section_type in (1, 3, 4, 5):
            if section_type not in sections:
                continue
            try:
                decoded[SECTION_NAMES[section_type]] = strict_json_loads(sections[section_type])
            except (UnicodeDecodeError, json.JSONDecodeError, DuplicateKeyError, ValueError) as exc:
                self.err("container.section_json", f"$.sections.{SECTION_NAMES[section_type]}", f"invalid strict UTF-8 JSON: {exc}")
        if self.errors:
            return None

        manifest = decoded.get("manifest")
        if not isinstance(manifest, dict):
            self.err("manifest.shape", "$.manifest", "manifest section must contain a JSON object")
            return None
        expected_manifest = {"format", "version", "encoding_id", "code_sha256", "resource_limits", "metadata"}
        if set(manifest) != expected_manifest:
            self.err("manifest.fields", "$.manifest", "manifest has missing or unknown fields")
            return None
        if manifest["format"] != FORMAT or manifest["version"] != FORMAT_VERSION:
            self.err("manifest.version", "$.manifest.version", "manifest format/version does not match ogma.ogmb 1.0")
        if not isinstance(manifest["encoding_id"], str) or not ID_RE.fullmatch(manifest["encoding_id"]):
            self.err("manifest.encoding_id", "$.manifest.encoding_id", "encoding_id must be a stable ASCII identifier")
        if not isinstance(manifest["code_sha256"], str) or not HEX_RE.fullmatch(manifest["code_sha256"]):
            self.err("manifest.code_digest", "$.manifest.code_sha256", "code_sha256 must be lowercase SHA-256 hex")
        limits = manifest["resource_limits"]
        if not isinstance(limits, dict) or set(limits) != set(RESOURCE_KEYS):
            self.err("resource.limits_shape", "$.manifest.resource_limits", "resource_limits must define every required limit exactly once")
        else:
            for key in RESOURCE_KEYS:
                if not is_positive_uint(limits[key], HARD_LIMITS[key]):
                    self.err("resource.limit_range", f"$.manifest.resource_limits.{key}", f"must be an integer from 1 through {HARD_LIMITS[key]}")
        if not bounded_json_value(manifest["metadata"]):
            self.err("manifest.metadata", "$.manifest.metadata", "metadata must be a bounded JSON object")
        constants = decoded.get("constants")
        if not isinstance(constants, list) or any(
            not isinstance(value, (str, bool, int, float)) and value is not None
            for value in constants
        ):
            self.err("constant_pool.shape", "$.sections.constants", "constant pool must be a JSON array of scalar values")
        elif any(not bounded_json_value(value) for value in constants):
            self.err("constant_pool.value", "$.sections.constants", "constant-pool values must be bounded JSON scalars")
        if not isinstance(decoded.get("exceptions"), list):
            self.err("exception.shape", "$.sections.exceptions", "exception section must be a JSON array")

        code = sections[2]
        if isinstance(limits, dict) and is_positive_uint(limits.get("max_code_bytes"), HARD_LIMITS["max_code_bytes"]):
            if len(code) > limits["max_code_bytes"]:
                self.err("resource.code_size", "$.sections.code", "code section exceeds declared max_code_bytes")
        if isinstance(manifest["code_sha256"], str) and HEX_RE.fullmatch(manifest["code_sha256"]):
            if manifest["code_sha256"] != hashlib.sha256(code).hexdigest():
                self.err("container.code_digest", "$.manifest.code_sha256", "manifest code digest does not match code section")
        actual_digest = hashlib.sha256(code).hexdigest()
        if isinstance(manifest["resource_limits"], dict) and bounded_json_value(manifest["metadata"]):
            try:
                metadata_errors = validate_function_metadata(manifest["metadata"], limits, actual_digest)
                self.errors.extend(metadata_errors)
            except (KeyError, TypeError, ValueError, IndexError) as exc:
                metadata_errors = [diagnostic("metadata.malformed", "$.manifest.metadata", f"malformed function metadata: {type(exc).__name__}")]
                self.errors.extend(metadata_errors)
            if 5 in sections and not metadata_errors:
                try:
                    self.errors.extend(validate_source_map(decoded.get("source_map"), manifest["metadata"], len(code)))
                except (KeyError, TypeError, ValueError, IndexError) as exc:
                    self.err("source_map.malformed", "$.source_map", f"malformed source map: {type(exc).__name__}")
            elif 5 in sections:
                self.err("source_map.metadata", "$.source_map", "source map cannot be validated while function metadata is invalid")
        if self.errors:
            return None
        decoded["code"] = code
        decoded["code_sha256"] = hashlib.sha256(code).hexdigest()
        decoded["constants"] = constants
        decoded["exceptions"] = decoded["exceptions"]
        return decoded


class ProgramVerifier:
    """Verify decoder facts independently from the physical instruction encoding."""

    def __init__(self) -> None:
        self.errors: list[dict[str, str]] = []

    def err(self, code: str, path: str, message: str) -> None:
        self.errors.append(diagnostic(code, path, message))

    def verify(self, artifact: dict[str, Any], program: Any) -> list[dict[str, str]]:
        try:
            return self._verify(artifact, program)
        except (KeyError, TypeError, ValueError, IndexError, OverflowError) as exc:
            self.errors = [diagnostic("program.malformed", "$", f"malformed decoder verification input: {type(exc).__name__}")]
            return self.errors

    def _verify(self, artifact: dict[str, Any], program: Any) -> list[dict[str, str]]:
        self.errors = []
        if not isinstance(program, dict):
            self.err("program.shape", "$", "decoder verification input must be a JSON object")
            return self.errors
        required = {"schema", "version", "encoding_id", "code_sha256", "execution_model", "entry", "entry_registers", "instructions"}
        if set(program) != required:
            self.err("program.fields", "$", "verification input has missing or unknown fields")
            return self.errors
        if program["schema"] != VERIFIER_SCHEMA or program["version"] != VERIFIER_VERSION:
            self.err("program.version", "$.version", "unsupported decoder verification input schema/version")
        manifest = artifact["manifest"]
        if program["encoding_id"] != manifest["encoding_id"]:
            self.err("program.encoding_id", "$.encoding_id", "decoder encoding_id does not match artifact manifest")
        if program["code_sha256"] != artifact["code_sha256"]:
            self.err("program.code_digest", "$.code_sha256", "decoder output is not bound to this exact code section")
        model = program["execution_model"]
        metadata = manifest["metadata"]
        frame = metadata["frame_layout"]
        if model not in ("stack", "register"):
            self.err("program.execution_model", "$.execution_model", "execution_model must be stack or register")
        if model != metadata["execution_model"]:
            self.err("program.execution_model_mismatch", "$.execution_model", "decoder execution model does not match the artifact frame metadata")
        if model == "stack" and frame["register_count"] != 0:
            self.err("frame.stack_registers", "$.manifest.metadata.frame_layout.register_count", "stack model must declare zero registers")
        if model == "register" and frame["register_count"] == 0:
            self.err("frame.register_count", "$.manifest.metadata.frame_layout.register_count", "register model must declare at least one register")
        instructions = program["instructions"]
        if not isinstance(instructions, list) or not instructions:
            self.err("program.instructions", "$.instructions", "decoded instruction stream must be a non-empty array")
            return self.errors
        limits = manifest["resource_limits"]
        if len(instructions) > limits["max_instructions"]:
            self.err("resource.instruction_count", "$.instructions", "instruction count exceeds declared limit")
            return self._sorted_errors()
        if len(artifact["constants"]) > limits["max_constants"]:
            self.err("resource.constant_count", "$.sections.constants", "constant count exceeds declared limit")
        if len(artifact["exceptions"]) > limits["max_exception_regions"]:
            self.err("resource.exception_count", "$.sections.exceptions", "exception region count exceeds declared limit")

        entry = program["entry"]
        if not is_uint(entry):
            self.err("program.entry", "$.entry", "entry must be an unsigned 32-bit byte offset")
        entry_registers = program["entry_registers"]
        if not isinstance(entry_registers, list):
            self.err("register.entry_state", "$.entry_registers", "entry_registers must be an array")
            entry_registers = []
        elif len(entry_registers) > frame["register_count"] or any(not is_uint(reg) or reg >= frame["register_count"] for reg in entry_registers):
            self.err("register.entry_bounds", "$.entry_registers", "entry register is outside the function's declared register window")
        elif len(set(entry_registers)) != len(entry_registers):
            self.err("register.entry_duplicate", "$.entry_registers", "entry registers must be unique")
        if model == "stack" and entry_registers:
            self.err("stack.entry_registers", "$.entry_registers", "stack model must not declare entry registers")
        if self.errors:
            return self.errors

        code_size = len(artifact["code"])
        by_offset: dict[int, dict[str, Any]] = {}
        previous_end = 0
        for index, ins in enumerate(instructions):
            path = f"$.instructions[{index}]"
            fields = {
                "offset", "size", "opcode", "operation", "operands", "constant_refs",
                "successors", "may_throw", "may_suspend", "stack_effect", "register_effects", "argument_count",
            }
            if not isinstance(ins, dict) or set(ins) != fields:
                self.err("instruction.fields", path, "instruction has missing or unknown fields")
                continue
            offset, size, opcode = ins["offset"], ins["size"], ins["opcode"]
            if not is_uint(offset) or not is_positive_uint(size, 0xFFFFFFFF) or not is_uint(opcode):
                self.err("instruction.header", path, "offset/opcode must be uint32 and size must be positive uint32")
                continue
            if offset != previous_end:
                self.err("instruction.layout", path + ".offset", "instructions must form a contiguous non-overlapping partition starting at offset zero")
            previous_end = offset + size
            if previous_end > code_size:
                self.err("instruction.bounds", path, "instruction extends beyond code section")
            if offset in by_offset:
                self.err("instruction.duplicate_offset", path + ".offset", "instruction offsets must be unique")
            by_offset[offset] = ins
            if not isinstance(ins["operation"], str) or ins["operation"] not in OPERATION_EFFECTS:
                self.err("opcode.unsupported", path + ".operation", "decoder returned an unsupported semantic operation class")
                continue
            operands = ins["operands"]
            refs = ins["constant_refs"]
            successors = ins["successors"]
            if not isinstance(operands, list) or len(operands) > 64 or any(not is_uint(x) for x in operands):
                self.err("operand.range", path + ".operands", "operands must be at most 64 unsigned 32-bit integers")
            elif isinstance(ins["operation"], str):
                expected_operands = {"nop": 0, "constant": 0, "unary": 0, "binary": 0, "call": 0,
                                     "load_local": 1, "store_local": 1, "jump": 1, "branch": 2,
                                     "return": 0, "throw": 0, "suspend": 0}.get(ins["operation"])
                if expected_operands is not None and len(operands) != expected_operands:
                    self.err("operand.arity", path + ".operands", f"{ins['operation']} requires {expected_operands} immediate operands")
                if ins["operation"] in ("load_local", "store_local") and len(operands) == 1 and operands[0] >= frame["local_count"]:
                    self.err("operand.local_bounds", path + ".operands[0]", "local slot index is outside declared function local_count")
            if not isinstance(refs, list) or len(refs) > 64 or any(not is_uint(x) for x in refs):
                self.err("constant.reference_shape", path + ".constant_refs", "constant references must be at most 64 uint32 indexes")
            elif any(ref >= len(artifact["constants"]) for ref in refs):
                self.err("constant.reference_bounds", path + ".constant_refs", "constant-pool reference is out of bounds")
            elif isinstance(ins["operation"], str):
                expected_refs = 1 if ins["operation"] == "constant" else 0
                if len(refs) != expected_refs:
                    self.err("constant.reference_arity", path + ".constant_refs", f"{ins['operation']} requires {expected_refs} constant references")
            if not isinstance(successors, list) or len(successors) > 2 or any(not is_uint(x) for x in successors):
                self.err("cfg.successor_shape", path + ".successors", "successors must contain at most two uint32 byte offsets")
            if not isinstance(ins["may_throw"], bool):
                self.err("exception.throw_flag", path + ".may_throw", "may_throw must be boolean")
            if not isinstance(ins["may_suspend"], bool):
                self.err("suspension.flag", path + ".may_suspend", "may_suspend must be boolean")
            if ins["operation"] == "suspend" and ins["may_suspend"] is not True:
                self.err("suspension.operation_flag", path + ".may_suspend", "suspend operation must declare may_suspend=true")
            if model == "stack":
                if ins["register_effects"] is not None:
                    self.err("stack.register_effects", path + ".register_effects", "stack program must not declare register effects")
                self._check_stack_effect(ins, path)
            elif model == "register":
                if ins["stack_effect"] is not None:
                    self.err("register.stack_effect", path + ".stack_effect", "register program must not declare stack effects")
                self._check_register_effect(ins, path, frame["register_count"])
        if previous_end != code_size:
            self.err("instruction.coverage", "$.instructions", "decoded instructions must cover the code section exactly")
        if entry not in by_offset:
            self.err("program.entry_target", "$.entry", "entry does not point to an instruction boundary")
        if self.errors:
            return self._sorted_errors()

        boundaries = set(by_offset)
        for index, slot in enumerate(metadata["inline_cache_slots"]):
            if slot["offset"] not in boundaries:
                self.err("inline_cache.offset", f"$.manifest.metadata.inline_cache_slots[{index}].offset", "inline-cache offset must reference an instruction boundary")
        source_map = artifact.get("source_map")
        if isinstance(source_map, dict):
            for index, location in enumerate(source_map["locations"]):
                if location["offset"] not in boundaries:
                    self.err("source_map.instruction_boundary", f"$.source_map.locations[{index}].offset", "source-map location must reference an instruction boundary")
            for index, scope in enumerate(source_map["lexical_scopes"]):
                if scope["start_offset"] not in boundaries or (scope["end_offset"] != code_size and scope["end_offset"] not in boundaries):
                    self.err("source_map.scope_boundary", f"$.source_map.lexical_scopes[{index}]", "lexical scope offsets must align to instruction boundaries")
            for index, variable in enumerate(source_map["variable_locations"]):
                if variable["start_offset"] not in boundaries or (variable["end_offset"] != code_size and variable["end_offset"] not in boundaries):
                    self.err("source_map.variable_boundary", f"$.source_map.variable_locations[{index}]", "variable location offsets must align to instruction boundaries")
        normal_edges: dict[int, list[int]] = {}
        for offset, ins in by_offset.items():
            operation = ins["operation"]
            succ = ins["successors"]
            operands = ins["operands"]
            path = f"$.instructions[{offset}]"
            next_offset = offset + ins["size"]
            if operation == "jump":
                if len(operands) != 1 or succ != operands:
                    self.err("cfg.jump_target", path, "jump must have one operand matching its sole successor")
            elif operation == "branch":
                if len(operands) != 2 or succ != operands:
                    self.err("cfg.branch_targets", path, "branch must have two ordered operands matching its successors")
            elif operation in ("return", "throw"):
                if succ:
                    self.err("cfg.terminal_successor", path + ".successors", "return/throw must not have normal successors")
            elif succ != [next_offset] or next_offset not in boundaries:
                self.err("cfg.fallthrough", path + ".successors", "ordinary operation must fall through to the immediately following instruction")
            if len(set(succ)) != len(succ):
                self.err("cfg.duplicate_successor", path + ".successors", "successor targets must be unique")
            for target in succ:
                if target not in boundaries:
                    self.err("cfg.target", path + ".successors", f"control-flow target {target} is not an instruction boundary")
            normal_edges[offset] = [target for target in succ if target in boundaries]
        if self.errors:
            return self._sorted_errors()

        exception_edges = self._verify_exceptions(artifact["exceptions"], by_offset, boundaries, code_size, limits, model)
        if self.errors:
            return self._sorted_errors()

        has_entry_predecessor = any(entry in targets for targets in normal_edges.values()) or any(
            handler == entry
            for handlers in exception_edges.values()
            for handler, _region in handlers
        )
        if has_entry_predecessor:
            self.err("cfg.entry_predecessor", "$.entry", "function entry must not be the target of normal or exceptional control flow in v1")
        if self.errors:
            return self._sorted_errors()

        reachable = self._reachable(entry, normal_edges, exception_edges)
        if reachable != boundaries:
            self.err("cfg.unreachable", "$.instructions", f"instructions unreachable from entry: {sorted(boundaries - reachable)[:16]}")
        suspensions = metadata["suspension_points"]
        suspension_by_offset = {point["offset"]: point for point in suspensions}
        flagged_suspensions = {offset for offset, ins in by_offset.items() if ins["may_suspend"]}
        if flagged_suspensions != set(suspension_by_offset):
            self.err("suspension.metadata_coverage", "$.manifest.metadata.suspension_points", "every may_suspend instruction must have exactly one suspension metadata record, and no other offset may be listed")
        for offset, point in suspension_by_offset.items():
            ins = by_offset.get(offset)
            if ins is None or not ins["may_suspend"] or ins["operation"] != "suspend":
                self.err("suspension.instruction", f"$.manifest.metadata.suspension_points[{offset}]", "suspension record must reference a suspend operation marked may_suspend")
                continue
            if point["resume_offset"] not in ins["successors"]:
                self.err("suspension.resume_target", f"$.manifest.metadata.suspension_points[{offset}].resume_offset", "resume_offset must be a normal successor of the suspend operation")
            if ins["successors"] and ins["successors"][0] != point["resume_offset"]:
                self.err("suspension.resume_order", f"$.manifest.metadata.suspension_points[{offset}].resume_offset", "v1 suspend operation must resume at its sole fallthrough successor")
        if model == "stack":
            max_stack_depth = self._verify_stack(entry, by_offset, normal_edges, exception_edges, limits["max_stack"])
            max_register_index = None
        else:
            max_stack_depth = 0
            max_register_index = self._verify_registers(entry, entry_registers, by_offset, normal_edges, exception_edges, frame["register_count"])
        summary = metadata["verifier_summary"]
        if summary["instruction_count"] != len(instructions):
            self.err("verifier_summary.instruction_count", "$.manifest.metadata.verifier_summary.instruction_count", "summary instruction count does not match decoded stream")
        if summary["max_stack_depth"] != max_stack_depth:
            self.err("verifier_summary.stack_depth", "$.manifest.metadata.verifier_summary.max_stack_depth", "summary stack depth does not match verifier result")
        if summary["max_register_index"] != max_register_index:
            self.err("verifier_summary.register_index", "$.manifest.metadata.verifier_summary.max_register_index", "summary register index does not match verifier result")
        if summary["exception_region_count"] != len(artifact["exceptions"]):
            self.err("verifier_summary.exception_count", "$.manifest.metadata.verifier_summary.exception_region_count", "summary exception region count does not match container")
        if summary["constant_count"] != len(artifact["constants"]):
            self.err("verifier_summary.constant_count", "$.manifest.metadata.verifier_summary.constant_count", "summary constant count does not match container")
        return self._sorted_errors()

    def _sorted_errors(self) -> list[dict[str, str]]:
        return sorted(self.errors, key=lambda item: (item["path"], item["code"], item["message"]))

    def _check_stack_effect(self, ins: dict[str, Any], path: str) -> None:
        effect = ins["stack_effect"]
        if not isinstance(effect, dict) or set(effect) != {"pop", "push"} or not all(is_uint(effect[k]) for k in ("pop", "push")):
            self.err("stack.effect_shape", path + ".stack_effect", "stack_effect must define uint32 pop and push")
            return
        expected = OPERATION_EFFECTS.get(ins["operation"])
        if expected is None:
            return
        pop, push = expected[:2]
        if ins["operation"] == "call":
            argc = ins["argument_count"]
            if not is_uint(argc, 1024):
                self.err("operand.call_arity", path + ".argument_count", "call argument_count must be from 0 through 1024")
                return
            pop = argc + 1
        elif ins["argument_count"] is not None:
            self.err("operand.unexpected_call_arity", path + ".argument_count", "argument_count is only valid for call")
        if (effect["pop"], effect["push"]) != (pop, push):
            self.err("stack.effect_mismatch", path + ".stack_effect", f"{ins['operation']} requires pop={pop}, push={push}")

    def _check_register_effect(self, ins: dict[str, Any], path: str, maximum: int) -> None:
        effect = ins["register_effects"]
        if not isinstance(effect, dict) or set(effect) != {"reads", "writes"}:
            self.err("register.effect_shape", path + ".register_effects", "register_effects must define reads and writes")
            return
        reads, writes = effect["reads"], effect["writes"]
        if not isinstance(reads, list) or not isinstance(writes, list):
            self.err("register.effect_shape", path + ".register_effects", "reads and writes must be arrays")
            return
        if len(reads) > 1025 or len(writes) > 1:
            self.err("register.effect_size", path + ".register_effects", "register effects exceed the v1 call/read/write arity limits")
            return
        valid_registers = all(is_uint(r) and r < maximum for r in reads + writes)
        if not valid_registers:
            self.err("register.bounds", path + ".register_effects", "register index is outside declared register window")
        elif len(set(reads)) != len(reads) or len(set(writes)) != len(writes):
            self.err("register.effect_duplicate", path + ".register_effects", "register read/write lists must not contain duplicates")
        expected = OPERATION_EFFECTS.get(ins["operation"])
        if expected is None:
            return
        expected_reads, expected_writes = expected[2:]
        if ins["operation"] == "call":
            argc = ins["argument_count"]
            if not is_uint(argc, 1024):
                self.err("operand.call_arity", path + ".argument_count", "call argument_count must be from 0 through 1024")
                return
            expected_reads = argc + 1
        elif ins["argument_count"] is not None:
            self.err("operand.unexpected_call_arity", path + ".argument_count", "argument_count is only valid for call")
        if len(reads) != expected_reads or len(writes) != expected_writes:
            self.err("register.effect_mismatch", path + ".register_effects", f"{ins['operation']} requires {expected_reads} reads and {expected_writes} writes")

    def _verify_exceptions(self, regions: Any, by_offset: dict[int, dict[str, Any]], boundaries: set[int],
                           code_size: int, limits: dict[str, int], model: str) -> dict[int, list[tuple[int, dict[str, Any]]]]:
        edges: dict[int, list[tuple[int, dict[str, Any]]]] = {}
        if not isinstance(regions, list):
            self.err("exception.shape", "$.sections.exceptions", "exception metadata must be an array")
            return edges
        if len(regions) > limits["max_exception_regions"]:
            self.err("resource.exception_count", "$.sections.exceptions", "exception region count exceeds declared limit")

        # Index throwing instructions once. Avoid scanning the full instruction table for each region.
        throw_offsets = sorted(offset for offset, ins in by_offset.items() if ins["may_throw"])
        ranges: list[tuple[int, int, int]] = []
        seen_ranges: set[tuple[int, int]] = set()
        expected_state = "handler_stack_depth" if model == "stack" else "handler_registers"
        exact = {"start", "end", "handler", "kind", expected_state}
        edge_count = 0

        for index, region in enumerate(regions):
            path = f"$.sections.exceptions[{index}]"
            if not isinstance(region, dict) or set(region) != exact:
                self.err("exception.fields", path, f"exception region must define exactly {sorted(exact)}")
                continue
            start, end, handler = region["start"], region["end"], region["handler"]
            if not all(is_uint(value) for value in (start, end, handler)):
                self.err("exception.range_type", path, "start, end and handler must be uint32 byte offsets")
                continue
            if start >= end or end > code_size or start not in boundaries or (end != code_size and end not in boundaries):
                self.err("exception.range", path, "protected range must be non-empty and aligned to instruction boundaries")
                continue
            if handler not in boundaries or start <= handler < end:
                self.err("exception.handler", path + ".handler", "handler must be an instruction boundary outside its own protected range")
                continue
            if region["kind"] not in ("catch", "finally"):
                self.err("exception.kind", path + ".kind", "exception kind must be catch or finally")

            if (start, end) in seen_ranges:
                self.err("exception.duplicate_region", path, "duplicate protected ranges are ambiguous")
            seen_ranges.add((start, end))

            if model == "stack":
                depth = region[expected_state]
                if not is_uint(depth) or depth > limits["max_stack"]:
                    self.err("exception.handler_stack", path + "." + expected_state, "handler stack depth exceeds declared stack bounds")
            else:
                registers = region[expected_state]
                if not isinstance(registers, list) or any(
                    not is_uint(register) or register >= limits["max_registers"] for register in registers
                ):
                    self.err("exception.handler_registers", path + "." + expected_state, "handler registers must fit declared register bounds")
                elif len(set(registers)) != len(registers):
                    self.err("exception.handler_registers", path + "." + expected_state, "handler registers must be unique")

            first = bisect_left(throw_offsets, start)
            last = bisect_left(throw_offsets, end)
            covered_count = last - first
            if covered_count == 0:
                self.err("exception.no_throw_site", path, "protected range must cover at least one may_throw instruction")
            elif edge_count + covered_count > MAX_EXCEPTION_EDGES:
                self.err("resource.exception_edges", "$.sections.exceptions",
                         f"exception control-flow edges exceed hard verifier limit {MAX_EXCEPTION_EDGES}")
                return edges
            else:
                edge_count += covered_count
                for offset in throw_offsets[first:last]:
                    edges.setdefault(offset, []).append((handler, region))
            ranges.append((start, end, index))

        # Disjoint and nested regions form a laminar set. Reject partial overlap in O(n log n).
        active: list[tuple[int, int, int]] = []
        for current in sorted(ranges, key=lambda item: (item[0], -item[1], item[2])):
            start, end, index = current
            while active and start >= active[-1][1]:
                active.pop()
            if active and end > active[-1][1]:
                self.err("exception.overlap", f"$.sections.exceptions[{index}]",
                         f"exception region partially overlaps region {active[-1][2]}")
            active.append(current)
        return edges

    @staticmethod
    def _reachable(entry: int, normal: dict[int, list[int]],
                   exceptional: dict[int, list[tuple[int, dict[str, Any]]]]) -> set[int]:
        reached: set[int] = set()
        pending = [entry]
        while pending:
            current = pending.pop()
            if current in reached:
                continue
            reached.add(current)
            pending.extend(normal.get(current, []))
            pending.extend(handler for handler, _region in exceptional.get(current, []))
        return reached

    def _verify_stack(self, entry: int, by_offset: dict[int, dict[str, Any]],
                      normal: dict[int, list[int]], exceptional: dict[int, list[tuple[int, dict[str, Any]]]],
                      maximum: int) -> int:
        depths: dict[int, int] = {entry: 0}
        pending = deque([entry])
        while pending:
            offset = pending.popleft()
            ins = by_offset[offset]
            effect = ins["stack_effect"]
            depth = depths[offset]
            if depth < effect["pop"]:
                self.err("stack.underflow", f"$.instructions[{offset}].stack_effect", "instruction consumes more values than are available")
                continue
            out_depth = depth - effect["pop"] + effect["push"]
            if out_depth > maximum:
                self.err("stack.depth_limit", f"$.instructions[{offset}].stack_effect", "stack depth exceeds declared max_stack")
            if ins["operation"] in ("return", "throw") and out_depth != 0:
                self.err("stack.terminal_depth", f"$.instructions[{offset}]", "terminal instruction must leave no residual stack values")
            for target in normal.get(offset, []):
                self._merge_stack(depths, pending, target, out_depth)
            for handler, region in exceptional.get(offset, []):
                self._merge_stack(depths, pending, handler, region["handler_stack_depth"])
        return max(depths.values(), default=0)

    def _merge_stack(self, depths: dict[int, int], pending: deque[int], target: int, incoming: int) -> None:
        existing = depths.get(target)
        if existing is None:
            depths[target] = incoming
            pending.append(target)
        elif existing != incoming:
            self.err("stack.merge_depth", f"$.instructions[{target}]", f"control-flow merge has inconsistent stack depths {existing} and {incoming}")

    def _verify_registers(self, entry: int, entry_registers: list[int], by_offset: dict[int, dict[str, Any]],
                          normal: dict[int, list[int]], exceptional: dict[int, list[tuple[int, dict[str, Any]]]],
                          maximum: int) -> int | None:
        # Use Python integer bitsets rather than per-instruction sets sized to max_registers.
        # This keeps verifier memory bounded even when artifacts declare large register windows.
        def mask(registers: list[int]) -> int:
            value = 0
            for register in registers:
                value |= 1 << register
            return value

        successors: dict[int, list[tuple[int, int | None]]] = {node: [] for node in by_offset}
        for source, targets in normal.items():
            for target in targets:
                successors[source].append((target, None))
        for source, handlers in exceptional.items():
            for target, region in handlers:
                successors[source].append((target, mask(region["handler_registers"])))

        incoming: dict[int, int] = {entry: mask(entry_registers)}
        pending = deque([entry])
        queued = {entry}
        processed_out: dict[int, int] = {}
        while pending:
            source = pending.popleft()
            queued.discard(source)
            in_state = incoming[source]
            write_state = mask(by_offset[source]["register_effects"]["writes"])
            out_state = in_state | write_state
            if processed_out.get(source) == out_state and source in processed_out:
                continue
            processed_out[source] = out_state
            for target, handler_state in successors[source]:
                candidate = handler_state if handler_state is not None else out_state
                existing = incoming.get(target)
                merged = candidate if existing is None else existing & candidate
                if existing is None or merged != existing:
                    incoming[target] = merged
                    if target not in queued:
                        pending.append(target)
                        queued.add(target)

        for offset, ins in by_offset.items():
            state = incoming.get(offset, 0)
            reads = ins["register_effects"]["reads"]
            missing = [register for register in reads if not (state & (1 << register))]
            if missing:
                self.err("register.uninitialized", f"$.instructions[{offset}].register_effects.reads",
                         f"registers read before definite initialization: {missing}")

            # An instruction that throws may not have committed its destination writes.
            # Exception handler live-ins must therefore exist in the pre-instruction state.
            for _handler, region in exceptional.get(offset, []):
                required = region["handler_registers"]
                missing_handler = [register for register in required if not (state & (1 << register))]
                if missing_handler:
                    self.err("exception.handler_register_state", "$.sections.exceptions",
                             f"handler requires registers not initialized before throw site: {missing_handler}")

        used = set(entry_registers)
        for ins in by_offset.values():
            used.update(ins["register_effects"]["reads"])
            used.update(ins["register_effects"]["writes"])
        return max(used) if used else None


def verify_one(root: Path, item: Any) -> dict[str, Any]:
    result: dict[str, Any] = {
        "artifact": item.get("artifact") if isinstance(item, dict) else None,
        "program": item.get("program") if isinstance(item, dict) else None,
        "expected": item.get("expect") if isinstance(item, dict) else None,
        "actual": "rejected",
        "passed": False,
        "diagnostics": [],
    }
    if not isinstance(item, dict):
        result["diagnostics"] = [diagnostic("manifest.fixture_shape", "$.fixtures", "fixture entry must be an object")]
        return result
    if item.get("expect") not in ("accepted", "rejected"):
        result["diagnostics"] = [diagnostic("manifest.expect", "$.fixtures", "expect must be accepted or rejected")]
        return result
    try:
        artifact_path = repository_path(root, item.get("artifact"))
        if artifact_path.stat().st_size > MAX_FILE_BYTES:
            raise ValueError(f"artifact exceeds hard limit of {MAX_FILE_BYTES} bytes")
        reader = ContainerReader()
        artifact = reader.read(artifact_path.read_bytes())
        errors = list(reader.errors)
    except (OSError, ValueError) as exc:
        result["diagnostics"] = [diagnostic("manifest.artifact_load", str(item.get("artifact")), str(exc))]
        return result

    if artifact is not None and item.get("program") is not None:
        try:
            program_path = repository_path(root, item["program"])
            program = load_json(program_path)
            errors.extend(ProgramVerifier().verify(artifact, program))
        except (OSError, UnicodeDecodeError, json.JSONDecodeError, DuplicateKeyError, ValueError, TypeError) as exc:
            errors.append(diagnostic("manifest.program_load", str(item["program"]), str(exc)))
    elif artifact is not None and item.get("program_required", True):
        errors.append(diagnostic("program.missing", "$.program", "decoder verification input is required before an artifact can be accepted for execution"))

    errors = sorted(errors, key=lambda entry: (entry["path"], entry["code"], entry["message"]))
    result["diagnostics"] = errors
    result["actual"] = "accepted" if not errors else "rejected"
    expected_codes = set(item.get("diagnostics", []))
    actual_codes = {entry["code"] for entry in errors}
    result["passed"] = (result["actual"] == item["expect"] and expected_codes <= actual_codes)
    return result


def run_corpus(root: Path, manifest_path: Path) -> dict[str, Any]:
    manifest = load_json(manifest_path)
    if not isinstance(manifest, dict) or manifest.get("contract") != FORMAT or manifest.get("version") != FORMAT_VERSION:
        raise ValueError("invalid .ogmb fixture corpus manifest")
    fixtures = manifest.get("fixtures")
    if not isinstance(fixtures, list) or len(fixtures) > MAX_FIXTURES:
        raise ValueError("fixture manifest must contain a bounded fixtures array")
    results = [verify_one(root, item) for item in fixtures]
    passed = sum(1 for item in results if item["passed"])
    accepted = sum(1 for item in results if item["actual"] == "accepted")
    rejected = sum(1 for item in results if item["actual"] == "rejected")
    return {
        "contract": FORMAT,
        "contract_version": FORMAT_VERSION,
        "verifier_schema": VERIFIER_SCHEMA,
        "verifier_version": VERIFIER_VERSION,
        "fixtures": results,
        "summary": {
            "total": len(results), "passed": passed, "failed": len(results) - passed,
            "accepted": accepted, "rejected": rejected,
        },
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, help="fixture corpus manifest")
    parser.add_argument("--report", type=Path, help="write deterministic JSON report")
    parser.add_argument("--container", type=Path, help="verify one .ogmb artifact")
    parser.add_argument("--program", type=Path, help="decoder verification input for --container")
    args = parser.parse_args()
    if bool(args.manifest) == bool(args.container):
        parser.error("provide exactly one of --manifest or --container")
    try:
        if args.manifest:
            report = run_corpus(Path.cwd(), args.manifest)
            code = 0 if report["summary"]["failed"] == 0 else 1
        else:
            if args.program is None:
                parser.error("--program is required with --container")
            if args.container.stat().st_size > MAX_FILE_BYTES:
                raise ValueError("artifact exceeds hard byte limit")
            reader = ContainerReader()
            artifact = reader.read(args.container.read_bytes())
            errors = list(reader.errors)
            if artifact is not None:
                errors.extend(ProgramVerifier().verify(artifact, load_json(args.program)))
            report = {
                "contract": FORMAT, "contract_version": FORMAT_VERSION,
                "verifier_schema": VERIFIER_SCHEMA, "verifier_version": VERIFIER_VERSION,
                "accepted": not errors,
                "diagnostics": sorted(errors, key=lambda item: (item["path"], item["code"], item["message"])),
            }
            code = 0 if not errors else 1
        rendered = json.dumps(report, indent=2, sort_keys=True, ensure_ascii=False) + "\n"
        if args.report:
            args.report.parent.mkdir(parents=True, exist_ok=True)
            args.report.write_text(rendered, encoding="utf-8", newline="\n")
        sys.stdout.write(rendered)
        return code
    except (OSError, UnicodeDecodeError, json.JSONDecodeError, DuplicateKeyError, ValueError, KeyError, TypeError) as exc:
        print(f"OGMB verifier error: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
