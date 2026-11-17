"""Generate deterministic binary and JSON fixtures for the .ogmb v1 contract."""
from __future__ import annotations

import hashlib
import json
import struct
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
OUTPUT = ROOT / "tests" / "ogmb" / ".generated"
FIXTURES = OUTPUT / "fixtures"
HEADER = struct.Struct("<4sHHIQQQIII")
ENTRY = struct.Struct("<IIQQII")
LIMITS = {
    "max_code_bytes": 1024,
    "max_instructions": 100,
    "max_constants": 20,
    "max_stack": 16,
    "max_registers": 16,
    "max_locals": 16,
    "max_call_depth": 16,
    "max_exception_regions": 16,
}
ENCODING_ID = "ogma.test-records.v1"


def write_json(path: Path, value: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8", newline="\n")


def build_container(
    name: str,
    code: bytes,
    constants: list[object] | None = None,
    exceptions: list[dict[str, object]] | None = None,
    *,
    version: tuple[int, int] = (1, 0),
    feature_flags: int = 0,
    digest_override: str | None = None,
    extra_sections: list[tuple[int, int, bytes]] | None = None,
    execution_model: str = "stack",
    local_count: int = 0,
    register_count: int = 0,
    max_stack_depth: int = 2,
    max_register_index: int | None = None,
    suspension_points: list[dict[str, object]] | None = None,
    environment_metadata: dict[str, object] | None = None,
) -> Path:
    constants = [] if constants is None else constants
    exceptions = [] if exceptions is None else exceptions
    code_digest = hashlib.sha256(code).hexdigest()
    safe_name = "".join(ch if ch.isalnum() or ch == "_" else "_" for ch in Path(name).stem)
    metadata = {
        "execution_model": execution_model,
        "function": {"id": "fixture_" + safe_name, "name": Path(name).stem},
        "code_generation": {"id": "ogma_fixture_codegen", "version": "1"},
        "code_cache": {"id": "cache_" + hashlib.sha256((name + code_digest).encode()).hexdigest()[:24], "version": "1"},
        "frame_layout": {"parameter_count": 0, "local_count": local_count, "register_count": register_count},
        "environment": {"id": "env_" + safe_name, "parent": None, "bindings": [], "captures": [], "locations": []} if environment_metadata is None else environment_metadata,
        "suspension_points": [] if suspension_points is None else suspension_points,
        "inline_cache_slots": [],
        "verifier_summary": {
            "schema": "ogma.bytecode-verification-summary",
            "version": "1.0.0",
            "code_sha256": code_digest,
            "instruction_count": len(code),
            "max_stack_depth": max_stack_depth if execution_model == "stack" else 0,
            "max_register_index": max_register_index if execution_model == "register" else None,
            "exception_region_count": len(exceptions),
            "constant_count": len(constants),
        },
    }
    manifest = {
        "format": "ogma.ogmb",
        "version": "1.0",
        "encoding_id": ENCODING_ID,
        "code_sha256": digest_override or code_digest,
        "resource_limits": LIMITS,
        "metadata": metadata,
    }
    sections = [
        (1, 0, json.dumps(manifest, sort_keys=True, separators=(",", ":")).encode()),
        (2, 0, code),
        (3, 0, json.dumps(constants, sort_keys=True, separators=(",", ":")).encode()),
        (4, 0, json.dumps(exceptions, sort_keys=True, separators=(",", ":")).encode()),
    ]
    if extra_sections:
        sections.extend(extra_sections)

    directory_end = 48 + len(sections) * ENTRY.size
    data = bytearray(b"\0" * directory_end)
    directory: list[tuple[int, int, int, int, int, int]] = []
    for section_type, flags, payload in sections:
        aligned = (len(data) + 7) & ~7
        data.extend(b"\0" * (aligned - len(data)))
        offset = len(data)
        data.extend(payload)
        directory.append((section_type, flags, offset, len(payload), 8, 0))

    HEADER.pack_into(
        data, 0, b"OGMB", version[0], version[1], 48, feature_flags,
        len(data), 48, len(directory), ENTRY.size, 0,
    )
    for index, entry in enumerate(directory):
        ENTRY.pack_into(data, 48 + index * ENTRY.size, *entry)

    path = FIXTURES / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)
    return path


def instruction(
    offset: int,
    opcode: int,
    operation: str,
    successors: list[int],
    stack_effect: dict[str, int] | None,
    register_effects: dict[str, list[int]] | None,
    *,
    operands: list[int] | None = None,
    constant_refs: list[int] | None = None,
    may_throw: bool = False,
    argument_count: int | None = None,
    may_suspend: bool = False,
) -> dict[str, object]:
    return {
        "offset": offset,
        "size": 1,
        "opcode": opcode,
        "operation": operation,
        "operands": [] if operands is None else operands,
        "constant_refs": [] if constant_refs is None else constant_refs,
        "successors": successors,
        "may_throw": may_throw,
        "may_suspend": may_suspend,
        "stack_effect": stack_effect,
        "register_effects": register_effects,
        "argument_count": argument_count,
    }


def program(code: bytes, model: str, instructions: list[dict[str, object]],
            entry_registers: list[int] | None = None) -> dict[str, object]:
    return {
        "schema": "ogma.bytecode-verification",
        "version": "1.0.0",
        "encoding_id": ENCODING_ID,
        "code_sha256": hashlib.sha256(code).hexdigest(),
        "execution_model": model,
        "entry": 0,
        "entry_registers": [] if entry_registers is None else entry_registers,
        "instructions": instructions,
    }


def write_program(name: str, value: dict[str, object]) -> None:
    write_json(FIXTURES / name, value)


def main() -> None:
    FIXTURES.mkdir(parents=True, exist_ok=True)

    # Stack candidate: constant, constant, binary, return.
    stack_code = b"\x01\x02\x03\x04"
    stack_instructions = [
        instruction(0, 1, "constant", [1], {"pop": 0, "push": 1}, None, constant_refs=[0]),
        instruction(1, 2, "constant", [2], {"pop": 0, "push": 1}, None, constant_refs=[1]),
        instruction(2, 3, "binary", [3], {"pop": 2, "push": 1}, None),
        instruction(3, 4, "return", [], {"pop": 1, "push": 0}, None),
    ]
    stack_program = program(stack_code, "stack", stack_instructions)
    build_container("positive-stack.ogmb", stack_code, ["left", "right"])
    write_program("positive-stack.json", stack_program)

    # Register candidate uses the same semantic operation sequence.
    register_code = b"\x11\x12\x13\x14"
    register_instructions = [
        instruction(0, 17, "constant", [1], None, {"reads": [], "writes": [0]}, constant_refs=[0]),
        instruction(1, 18, "constant", [2], None, {"reads": [], "writes": [1]}, constant_refs=[1]),
        instruction(2, 19, "binary", [3], None, {"reads": [0, 1], "writes": [2]}),
        instruction(3, 20, "return", [], None, {"reads": [2], "writes": []}),
    ]
    register_program = program(register_code, "register", register_instructions)
    build_container("positive-register.ogmb", register_code, ["left", "right"], execution_model="register", register_count=3, max_register_index=2)
    write_program("positive-register.json", register_program)

    # Exception edge transfers to an out-of-line handler.
    exception_code = b"\x21\x22\x23"
    exception_instructions = [
        instruction(0, 1, "constant", [1], {"pop": 0, "push": 1}, None, constant_refs=[0]),
        instruction(1, 2, "throw", [], {"pop": 1, "push": 0}, None, may_throw=True),
        instruction(2, 3, "return", [], {"pop": 1, "push": 0}, None),
    ]
    exception_program = program(exception_code, "stack", exception_instructions)
    exception_regions = [
        {"start": 1, "end": 2, "handler": 2, "kind": "catch", "handler_stack_depth": 1}
    ]
    build_container("positive-exception.ogmb", exception_code, ["value"], exception_regions, max_stack_depth=1)
    write_program("positive-exception.json", exception_program)

    register_exception_code = b"\x31\x32\x33"
    register_exception_instructions = [
        instruction(0, 1, "constant", [1], None, {"reads": [], "writes": [0]}, constant_refs=[0]),
        instruction(1, 2, "unary", [2], None, {"reads": [0], "writes": [1]}, may_throw=True),
        instruction(2, 3, "return", [], None, {"reads": [0], "writes": []}),
    ]
    register_exception_program = program(register_exception_code, "register", register_exception_instructions)
    register_exception_regions = [
        {"start": 1, "end": 2, "handler": 2, "kind": "catch", "handler_registers": [0]}
    ]
    build_container("positive-register-exception.ogmb", register_exception_code, ["value"], register_exception_regions, execution_model="register", register_count=2, max_register_index=1)
    write_program("positive-register-exception.json", register_exception_program)
    build_container("negative-register-exception-frame.ogmb", register_exception_code, ["value"], [
        {"start": 1, "end": 2, "handler": 2, "kind": "catch", "handler_registers": [2]}
    ], execution_model="register", register_count=2, max_register_index=1)
    build_container("negative-register-summary-frame.ogmb", register_exception_code, ["value"],
                    execution_model="register", register_count=2, max_register_index=2)

    # Portable suspension metadata is bound to an explicit suspend operation and resume edge.
    suspension_code = b"\x25\x26\x27"
    suspension_instructions = [
        instruction(0, 1, "constant", [1], {"pop": 0, "push": 1}, None, constant_refs=[0]),
        instruction(1, 2, "suspend", [2], {"pop": 1, "push": 1}, None, may_suspend=True),
        instruction(2, 3, "return", [], {"pop": 1, "push": 0}, None),
    ]
    suspension_program = program(suspension_code, "stack", suspension_instructions)
    suspension_metadata = [
        {"id": "suspend_await_0", "kind": "await", "offset": 1, "resume_offset": 2,
         "live_locals": [], "stack_depth": 1, "live_registers": None}
    ]
    build_container("positive-suspension.ogmb", suspension_code, ["awaited"], max_stack_depth=1,
                    suspension_points=suspension_metadata)
    write_program("positive-suspension.json", suspension_program)

    bad_suspension_metadata = [
        {"id": "suspend_await_0", "kind": "await", "offset": 1, "resume_offset": 0,
         "live_locals": [], "stack_depth": 1, "live_registers": None}
    ]
    build_container("negative-suspension-target.ogmb", suspension_code, ["awaited"], max_stack_depth=1,
                    suspension_points=bad_suspension_metadata)

    # Optional source map records stable source identity and instruction-level provenance.
    valid_source_map = {
        "version": "1.0.0",
        "files": [{"id": "file_main", "path": "src/main.js", "language": "javascript"}],
        "locations": [{
            "offset": 0,
            "source": {
                "origin": "user", "file": "file_main",
                "start": {"line": 1, "column": 0}, "end": {"line": 1, "column": 5},
                "scope": None, "bindings": [], "suspension": None, "inline_chain": []
            }
        }],
        "lexical_scopes": [],
        "variable_locations": []
    }
    build_container("positive-source-map.ogmb", stack_code, ["left", "right"],
                    max_stack_depth=2,
                    extra_sections=[(5, 1, json.dumps(valid_source_map, sort_keys=True, separators=(",", ":")).encode())])
    invalid_source_map = dict(valid_source_map)
    invalid_source_map["locations"] = [{
        "offset": 0,
        "source": dict(valid_source_map["locations"][0]["source"], file="missing_file")
    }]
    build_container("negative-source-map-reference.ogmb", stack_code, ["left", "right"],
                    max_stack_depth=2,
                    extra_sections=[(5, 1, json.dumps(invalid_source_map, sort_keys=True, separators=(",", ":")).encode())])

    # Envelope failures: truncated header, unknown version/flags, digest mismatch.
    (FIXTURES / "negative-truncated.ogmb").write_bytes(b"OGMB\x01\x00")
    build_container("negative-unknown-version.ogmb", stack_code, ["left", "right"], version=(2, 0))
    build_container("negative-unknown-feature.ogmb", stack_code, ["left", "right"], feature_flags=1)
    build_container("negative-code-digest.ogmb", stack_code, ["left", "right"], digest_override="0" * 64)

    # Required sections cannot overlap. Corrupt the code entry offset to equal manifest offset.
    overlap_path = build_container("negative-section-overlap.ogmb", stack_code, ["left", "right"])
    raw = bytearray(overlap_path.read_bytes())
    first = ENTRY.unpack_from(raw, 48)
    second = ENTRY.unpack_from(raw, 48 + ENTRY.size)
    ENTRY.pack_into(raw, 48 + ENTRY.size, second[0], second[1], first[2], second[3], second[4], second[5])
    overlap_path.write_bytes(raw)

    # Optional unknown section is ignored; unknown required section fails closed.
    build_container("positive-unknown-optional.ogmb", stack_code, ["left", "right"],
                    extra_sections=[(99, 1, b"future optional section")])
    build_container("negative-unknown-required.ogmb", stack_code, ["left", "right"],
                    extra_sections=[(99, 0, b"future required section")])

    # Jump/operand/constant/stack negatives.
    invalid_jump = [
        instruction(0, 9, "jump", [99], {"pop": 0, "push": 0}, None, operands=[99]),
        instruction(1, 2, "nop", [2], {"pop": 0, "push": 0}, None),
        instruction(2, 3, "nop", [3], {"pop": 0, "push": 0}, None),
        instruction(3, 4, "return", [], {"pop": 1, "push": 0}, None),
    ]
    write_program("negative-invalid-jump.json", program(stack_code, "stack", invalid_jump))
    overflow = [dict(item) for item in stack_instructions]
    overflow[0] = dict(overflow[0], operands=[2**32])
    write_program("negative-operand-overflow.json", program(stack_code, "stack", overflow))
    bad_constant = [dict(item) for item in stack_instructions]
    bad_constant[0] = dict(bad_constant[0], constant_refs=[99])
    write_program("negative-constant-reference.json", program(stack_code, "stack", bad_constant))

    wrong_effect = [dict(item) for item in stack_instructions]
    wrong_effect[2] = dict(wrong_effect[2], stack_effect={"pop": 1, "push": 1})
    write_program("negative-stack-effect.json", program(stack_code, "stack", wrong_effect))
    underflow_code = b"\x41\x42"
    underflow = [
        instruction(0, 1, "binary", [1], {"pop": 2, "push": 1}, None),
        instruction(1, 2, "return", [], {"pop": 1, "push": 0}, None),
    ]
    build_container("negative-stack-underflow.ogmb", underflow_code, ["value"], max_stack_depth=1)
    write_program("negative-stack-underflow.json", program(underflow_code, "stack", underflow))

    # Two paths reach offset 4 with different stack depths.
    merge_code = b"\x51\x52\x53\x54\x55"
    merge_instructions = [
        instruction(0, 1, "constant", [1], {"pop": 0, "push": 1}, None, constant_refs=[0]),
        instruction(1, 2, "branch", [2, 3], {"pop": 1, "push": 0}, None, operands=[2, 3]),
        instruction(2, 3, "constant", [3], {"pop": 0, "push": 1}, None, constant_refs=[1]),
        instruction(3, 4, "jump", [4], {"pop": 0, "push": 0}, None, operands=[4]),
        instruction(4, 5, "return", [], {"pop": 1, "push": 0}, None),
    ]
    build_container("stack-merge.ogmb", merge_code, ["left", "right"], max_stack_depth=2)
    write_program("negative-stack-merge.json", program(merge_code, "stack", merge_instructions))

    # Malformed exception boundaries.
    build_container("negative-exception-range.ogmb", exception_code, ["value"], [
        {"start": 2, "end": 1, "handler": 2, "kind": "catch", "handler_stack_depth": 1}
    ])

    # Register definite-initialization failure.
    register_bad_code = b"\x61\x62"
    register_bad = [
        instruction(0, 1, "unary", [1], None, {"reads": [0], "writes": [1]}),
        instruction(1, 2, "return", [], None, {"reads": [1], "writes": []}),
    ]
    build_container("register-uninitialized.ogmb", register_bad_code, ["value"], execution_model="register", register_count=2, max_register_index=1)
    write_program("negative-register-uninitialized.json", program(register_bad_code, "register", register_bad))

    # Local slot indexes are bounded by the manifest's max_locals.
    local_code = b"\x71\x72"
    local_program = program(local_code, "stack", [
        instruction(0, 1, "load_local", [1], {"pop": 0, "push": 1}, None, operands=[16]),
        instruction(1, 2, "return", [], {"pop": 1, "push": 0}, None),
    ])
    build_container("negative-local-bounds.ogmb", local_code, ["value"], local_count=16, max_stack_depth=1)
    write_program("negative-local-bounds.json", local_program)

    wrong_digest = dict(stack_program, code_sha256="f" * 64)
    write_program("negative-program-digest.json", wrong_digest)

    manifest = {
        "contract": "ogma.ogmb",
        "version": "1.0",
        "fixtures": [
            {"artifact": "tests/ogmb/.generated/fixtures/positive-stack.ogmb", "program": "tests/ogmb/.generated/fixtures/positive-stack.json", "expect": "accepted"},
            {"artifact": "tests/ogmb/.generated/fixtures/positive-register.ogmb", "program": "tests/ogmb/.generated/fixtures/positive-register.json", "expect": "accepted"},
            {"artifact": "tests/ogmb/.generated/fixtures/positive-exception.ogmb", "program": "tests/ogmb/.generated/fixtures/positive-exception.json", "expect": "accepted"},
            {"artifact": "tests/ogmb/.generated/fixtures/positive-register-exception.ogmb", "program": "tests/ogmb/.generated/fixtures/positive-register-exception.json", "expect": "accepted"},
            {"artifact": "tests/ogmb/.generated/fixtures/negative-register-exception-frame.ogmb", "program": "tests/ogmb/.generated/fixtures/positive-register-exception.json", "expect": "rejected", "diagnostics": ["exception.handler_registers"]},
            {"artifact": "tests/ogmb/.generated/fixtures/negative-register-summary-frame.ogmb", "expect": "rejected", "diagnostics": ["metadata.verifier_summary_register"]},
            {"artifact": "tests/ogmb/.generated/fixtures/positive-unknown-optional.ogmb", "program": "tests/ogmb/.generated/fixtures/positive-stack.json", "expect": "accepted"},
            {"artifact": "tests/ogmb/.generated/fixtures/negative-truncated.ogmb", "expect": "rejected", "diagnostics": ["container.truncated_header"]},
            {"artifact": "tests/ogmb/.generated/fixtures/negative-unknown-version.ogmb", "expect": "rejected", "diagnostics": ["container.version"]},
            {"artifact": "tests/ogmb/.generated/fixtures/negative-unknown-feature.ogmb", "expect": "rejected", "diagnostics": ["container.unknown_features"]},
            {"artifact": "tests/ogmb/.generated/fixtures/negative-section-overlap.ogmb", "expect": "rejected", "diagnostics": ["container.section_overlap"]},
            {"artifact": "tests/ogmb/.generated/fixtures/negative-code-digest.ogmb", "expect": "rejected", "diagnostics": ["container.code_digest"]},
            {"artifact": "tests/ogmb/.generated/fixtures/negative-unknown-required.ogmb", "expect": "rejected", "diagnostics": ["container.unknown_required_section"]},
            {"artifact": "tests/ogmb/.generated/fixtures/positive-stack.ogmb", "program": "tests/ogmb/.generated/fixtures/negative-invalid-jump.json", "expect": "rejected", "diagnostics": ["cfg.target"]},
            {"artifact": "tests/ogmb/.generated/fixtures/positive-stack.ogmb", "program": "tests/ogmb/.generated/fixtures/negative-operand-overflow.json", "expect": "rejected", "diagnostics": ["operand.range"]},
            {"artifact": "tests/ogmb/.generated/fixtures/positive-stack.ogmb", "program": "tests/ogmb/.generated/fixtures/negative-constant-reference.json", "expect": "rejected", "diagnostics": ["constant.reference_bounds"]},
            {"artifact": "tests/ogmb/.generated/fixtures/positive-suspension.ogmb", "program": "tests/ogmb/.generated/fixtures/positive-suspension.json", "expect": "accepted"},
            {"artifact": "tests/ogmb/.generated/fixtures/positive-source-map.ogmb", "program": "tests/ogmb/.generated/fixtures/positive-stack.json", "expect": "accepted"},
            {"artifact": "tests/ogmb/.generated/fixtures/negative-source-map-reference.ogmb", "expect": "rejected", "diagnostics": ["source_map.file_ref"]},
            {"artifact": "tests/ogmb/.generated/fixtures/negative-suspension-target.ogmb", "program": "tests/ogmb/.generated/fixtures/positive-suspension.json", "expect": "rejected", "diagnostics": ["suspension.resume_target"]},
            {"artifact": "tests/ogmb/.generated/fixtures/positive-stack.ogmb", "program": "tests/ogmb/.generated/fixtures/negative-stack-effect.json", "expect": "rejected", "diagnostics": ["stack.effect_mismatch"]},
            {"artifact": "tests/ogmb/.generated/fixtures/negative-stack-underflow.ogmb", "program": "tests/ogmb/.generated/fixtures/negative-stack-underflow.json", "expect": "rejected", "diagnostics": ["stack.underflow"]},
            {"artifact": "tests/ogmb/.generated/fixtures/negative-local-bounds.ogmb", "program": "tests/ogmb/.generated/fixtures/negative-local-bounds.json", "expect": "rejected", "diagnostics": ["operand.local_bounds"]},
            {"artifact": "tests/ogmb/.generated/fixtures/stack-merge.ogmb", "program": "tests/ogmb/.generated/fixtures/negative-stack-merge.json", "expect": "rejected", "diagnostics": ["stack.merge_depth"]},
            {"artifact": "tests/ogmb/.generated/fixtures/negative-exception-range.ogmb", "program": "tests/ogmb/.generated/fixtures/positive-exception.json", "expect": "rejected", "diagnostics": ["exception.range"]},
            {"artifact": "tests/ogmb/.generated/fixtures/register-uninitialized.ogmb", "program": "tests/ogmb/.generated/fixtures/negative-register-uninitialized.json", "expect": "rejected", "diagnostics": ["register.uninitialized"]},
            {"artifact": "tests/ogmb/.generated/fixtures/positive-stack.ogmb", "program": "tests/ogmb/.generated/fixtures/negative-program-digest.json", "expect": "rejected", "diagnostics": ["program.code_digest"]},
            {"artifact": "tests/ogmb/.generated/fixtures/positive-stack.ogmb", "expect": "rejected", "diagnostics": ["program.missing"]},
        ],
    }
    write_json(OUTPUT / "manifest.json", manifest)


if __name__ == "__main__":
    main()
