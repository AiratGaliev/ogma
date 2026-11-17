"""Source/debug metadata lowering and production-profile stripping helpers."""

from __future__ import annotations

from typing import Any, Mapping, Sequence

from debugger_api import CodeGeneration, DebuggerError


def lower_core_ir_debug_metadata(
    core_ir: Mapping[str, Any],
    instruction_offsets: Mapping[str, int],
    code_size: int,
    code_generation: CodeGeneration,
    instruction_boundaries: Sequence[int],
    variable_locations: Sequence[Mapping[str, Any]],
) -> dict[str, Any]:
    """Project verified Core IR v1 provenance into the existing .ogmb source-map v1.

    Bytecode offsets and physical variable locations are encoder-owned inputs.
    This function deliberately does not infer VM slots from Core IR descriptor
    order, and it returns no pointers or runtime-specific data.
    """
    if core_ir.get("contract") != "ogma.core-ir" or core_ir.get("version") != "1.0.0":
        raise DebuggerError("expected verified Core IR v1")
    if not isinstance(code_size, int) or isinstance(code_size, bool) or code_size <= 0:
        raise DebuggerError("code_size must be a positive integer")

    function = core_ir.get("function")
    if not isinstance(function, Mapping):
        raise DebuggerError("Core IR function descriptor is missing")
    environment = function.get("environment")
    if not isinstance(environment, Mapping):
        raise DebuggerError("Core IR lexical environment is missing")

    source_entries: list[tuple[str, Mapping[str, Any]]] = []
    for block in function.get("blocks", []):
        for instruction in block.get("instructions", []):
            source_entries.append((instruction["id"], instruction["source"]))
        source_entries.append((f"terminator:{block['id']}", block["terminator"]["source"]))

    expected_ids = {ident for ident, _ in source_entries}
    if set(instruction_offsets) != expected_ids:
        missing = sorted(expected_ids - set(instruction_offsets))
        extra = sorted(set(instruction_offsets) - expected_ids)
        raise DebuggerError(f"IR/bytecode location map mismatch; missing={missing}, extra={extra}")

    raw_boundaries = list(instruction_boundaries)
    if any(not isinstance(item, int) or isinstance(item, bool) or item < 0 for item in raw_boundaries):
        raise DebuggerError("instruction boundaries must be non-negative integer offsets")
    boundaries = set(raw_boundaries)
    if len(boundaries) != len(raw_boundaries) or code_size not in boundaries:
        raise DebuggerError("instruction boundaries must be unique and include code_size")

    offsets = list(instruction_offsets.values())
    if any(
        not isinstance(offset, int) or isinstance(offset, bool)
        or offset < 0 or offset >= code_size or offset not in boundaries
        for offset in offsets
    ):
        raise DebuggerError("every Core IR location must map to an encoded instruction boundary")
    if len(set(offsets)) != len(offsets):
        raise DebuggerError("source-map v1 requires a distinct bytecode offset for each mapped IR location")

    files: list[dict[str, Any]] = []
    file_ids: set[str] = set()
    for source_file in function.get("source_files", []):
        item = {key: source_file[key] for key in ("id", "path", "language")}
        if source_file.get("sha256"):
            item["sha256"] = source_file["sha256"]
        if item["id"] in file_ids:
            raise DebuggerError(f"duplicate source file id {item['id']!r}")
        file_ids.add(item["id"])
        files.append(item)

    local_bindings = [item["id"] for item in environment.get("bindings", [])]
    captures = [item["id"] for item in function.get("captures", [])]
    if len(local_bindings + captures) != len(set(local_bindings + captures)):
        raise DebuggerError("function binding and capture IDs must be globally unique")
    allowed_bindings = set(local_bindings + captures)

    locations: list[dict[str, Any]] = []
    for ident, raw_source in source_entries:
        source = dict(raw_source)
        if source.get("origin") == "user":
            if source.get("file") not in file_ids:
                raise DebuggerError(f"IR location {ident!r} references an unknown source file")
        source_bindings = source.get("bindings", [])
        if any(binding not in allowed_bindings for binding in source_bindings):
            raise DebuggerError(f"IR location {ident!r} references a non-local binding/capture")
        if source.get("scope") != environment["id"]:
            # Outer lexical environments are exposed by capture descriptors,
            # not by a dangling cross-CodeBlock scope reference.
            source["scope"] = None
        locations.append({"offset": instruction_offsets[ident], "source": source})

    if not local_bindings and not captures:
        lexical_scopes: list[dict[str, Any]] = []
    else:
        lexical_scopes = [{
            "id": environment["id"],
            "parent": None,
            "start_offset": 0,
            "end_offset": code_size,
            "bindings": local_bindings + captures,
        }]

    variables: list[dict[str, Any]] = []
    seen_ranges: set[tuple[str, int, int]] = set()
    for descriptor in variable_locations:
        required = {"binding", "kind", "index", "start_offset", "end_offset"}
        if set(descriptor) != required:
            raise DebuggerError("variable location descriptor has missing/unknown fields")
        binding = descriptor["binding"]
        kind, index = descriptor["kind"], descriptor["index"]
        start, end = descriptor["start_offset"], descriptor["end_offset"]
        if binding not in allowed_bindings:
            raise DebuggerError(f"variable location references unknown binding/capture {binding!r}")
        if kind not in {"local", "register", "environment", "capture"}:
            raise DebuggerError(f"unsupported variable storage kind {kind!r}")
        if any(not isinstance(value, int) or isinstance(value, bool) for value in (index, start, end)):
            raise DebuggerError("variable storage index/range must be integer byte offsets")
        if index < 0 or not (0 <= start < end <= code_size):
            raise DebuggerError("variable storage index/range is out of bounds")
        if start not in boundaries or end not in boundaries:
            raise DebuggerError("variable location range must align to instruction boundaries")
        if binding in captures and kind != "capture":
            raise DebuggerError("captured bindings must use the capture storage kind")
        if binding in captures and index != captures.index(binding):
            raise DebuggerError("capture storage index must match the manifest capture descriptor order")
        key = (binding, start, end)
        if key in seen_ranges:
            raise DebuggerError("duplicate variable location range")
        seen_ranges.add(key)
        variables.append(dict(descriptor))

    variables.sort(key=lambda item: (item["binding"], item["start_offset"], item["end_offset"]))
    return {
        "code_generation": {"id": code_generation.id, "version": code_generation.version},
        "ir_to_bytecode": dict(sorted(instruction_offsets.items())),
        "source_map": {
            "version": "1.0.0",
            "files": files,
            "locations": sorted(locations, key=lambda item: item["offset"]),
            "lexical_scopes": lexical_scopes,
            "variable_locations": variables,
        },
    }


def strip_debug_metadata(program: Mapping[str, Any]) -> dict[str, Any]:
    """Remove optional source/debug additions while preserving executable payloads."""
    from copy import deepcopy

    result = deepcopy(dict(program))
    result.pop("source_files", None)
    result.pop("debug_metadata", None)
    functions = result.get("functions", [])
    for function in functions:
        function.pop("debug_metadata", None)
    return result
