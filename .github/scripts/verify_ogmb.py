#!/usr/bin/env python3
"""Dependency-free reference verifier for the Ogma .ogmb v1 M0 contract."""
from __future__ import annotations

import argparse
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
    return json.loads(raw, object_pairs_hook=_object_pairs, parse_constant=_reject_constant)


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
        if not isinstance(manifest["metadata"], dict) or not bounded_json_value(manifest["metadata"]):
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
        if model not in ("stack", "register"):
            self.err("program.execution_model", "$.execution_model", "execution_model must be stack or register")
        instructions = program["instructions"]
        if not isinstance(instructions, list) or not instructions:
            self.err("program.instructions", "$.instructions", "decoded instruction stream must be a non-empty array")
            return self.errors
        limits = manifest["resource_limits"]
        if len(instructions) > limits["max_instructions"]:
            self.err("resource.instruction_count", "$.instructions", "instruction count exceeds declared limit")
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
        elif any(not is_uint(reg) or reg >= limits["max_registers"] for reg in entry_registers):
            self.err("register.entry_bounds", "$.entry_registers", "entry register is outside declared register window")
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
                "successors", "may_throw", "stack_effect", "register_effects", "argument_count",
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
                                     "return": 0, "throw": 0}.get(ins["operation"])
                if expected_operands is not None and len(operands) != expected_operands:
                    self.err("operand.arity", path + ".operands", f"{ins['operation']} requires {expected_operands} immediate operands")
                if ins["operation"] in ("load_local", "store_local") and len(operands) == 1 and operands[0] >= limits["max_locals"]:
                    self.err("operand.local_bounds", path + ".operands[0]", "local slot index is outside declared max_locals")
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
            if model == "stack":
                if ins["register_effects"] is not None:
                    self.err("stack.register_effects", path + ".register_effects", "stack program must not declare register effects")
                self._check_stack_effect(ins, path)
            elif model == "register":
                if ins["stack_effect"] is not None:
                    self.err("register.stack_effect", path + ".stack_effect", "register program must not declare stack effects")
                self._check_register_effect(ins, path, limits["max_registers"])
        if previous_end != code_size:
            self.err("instruction.coverage", "$.instructions", "decoded instructions must cover the code section exactly")
        if entry not in by_offset:
            self.err("program.entry_target", "$.entry", "entry does not point to an instruction boundary")
        if self.errors:
            return self._sorted_errors()

        boundaries = set(by_offset)
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

        reachable = self._reachable(entry, normal_edges, exception_edges)
        if reachable != boundaries:
            self.err("cfg.unreachable", "$.instructions", f"instructions unreachable from entry: {sorted(boundaries - reachable)[:16]}")
        if model == "stack":
            self._verify_stack(entry, by_offset, normal_edges, exception_edges, limits["max_stack"])
        else:
            self._verify_registers(entry, entry_registers, by_offset, normal_edges, exception_edges, limits["max_registers"])
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
        ranges: list[tuple[int, int, int]] = []
        expected_state = "handler_stack_depth" if model == "stack" else "handler_registers"
        exact = {"start", "end", "handler", "kind", expected_state}
        for index, region in enumerate(regions):
            path = f"$.sections.exceptions[{index}]"
            if not isinstance(region, dict) or set(region) != exact:
                self.err("exception.fields", path, f"exception region must define exactly {sorted(exact)}")
                continue
            start, end, handler = region["start"], region["end"], region["handler"]
            if not all(is_uint(v) for v in (start, end, handler)):
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
            if model == "stack":
                depth = region[expected_state]
                if not is_uint(depth) or depth > limits["max_stack"]:
                    self.err("exception.handler_stack", path + "." + expected_state, "handler stack depth exceeds declared stack bounds")
            else:
                regs = region[expected_state]
                if not isinstance(regs, list) or any(not is_uint(r) or r >= limits["max_registers"] for r in regs):
                    self.err("exception.handler_registers", path + "." + expected_state, "handler registers must fit declared register bounds")
                elif len(set(regs)) != len(regs):
                    self.err("exception.handler_registers", path + "." + expected_state, "handler registers must be unique")
            covered = [offset for offset, ins in by_offset.items() if start <= offset < end and ins["may_throw"]]
            if not covered:
                self.err("exception.no_throw_site", path, "protected range must cover at least one may_throw instruction")
            else:
                for offset in covered:
                    edges.setdefault(offset, []).append((handler, region))
            ranges.append((start, end, index))
        ordered = sorted(ranges)
        for i, left in enumerate(ordered):
            for right in ordered[i + 1:]:
                if right[0] >= left[1]:
                    break
                # Disjoint and nested regions are valid; partial overlap is ambiguous.
                if not (left[0] <= right[0] and right[1] <= left[1]) and not (right[0] <= left[0] and left[1] <= right[1]):
                    self.err("exception.overlap", f"$.sections.exceptions[{right[2]}]", f"exception region partially overlaps region {left[2]}")
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
                      maximum: int) -> None:
        depths: dict[int, int] = {entry: 0}
        pending = [entry]
        while pending:
            offset = pending.pop(0)
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

    def _merge_stack(self, depths: dict[int, int], pending: list[int], target: int, incoming: int) -> None:
        existing = depths.get(target)
        if existing is None:
            depths[target] = incoming
            pending.append(target)
        elif existing != incoming:
            self.err("stack.merge_depth", f"$.instructions[{target}]", f"control-flow merge has inconsistent stack depths {existing} and {incoming}")

    def _verify_registers(self, entry: int, entry_registers: list[int], by_offset: dict[int, dict[str, Any]],
                          normal: dict[int, list[int]], exceptional: dict[int, list[tuple[int, dict[str, Any]]]],
                          maximum: int) -> None:
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
        pending = [entry]
        queued = {entry}
        processed_out: dict[int, int] = {}
        while pending:
            source = pending.pop(0)
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
