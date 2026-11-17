#!/usr/bin/env python3
"""Stack/register bytecode prototype, encoder/decoder and interpreters for OGM-16."""

from __future__ import annotations

from dataclasses import dataclass, field
import hashlib
import importlib.util
import json
from pathlib import Path
import platform
import struct
import sys
import time
from typing import Any, Callable

ROOT = Path(__file__).resolve().parents[2]
VERIFY_OGMB = ROOT / ".github" / "scripts" / "verify_ogmb.py"
if not VERIFY_OGMB.is_file():
    raise RuntimeError("OGM-16 must run from a complete Ogma checkout with .github/scripts/verify_ogmb.py")

_spec = importlib.util.spec_from_file_location("ogma_verify_ogmb", VERIFY_OGMB)
if _spec is None or _spec.loader is None:
    raise RuntimeError("cannot load the repository .ogmb verifier")
_verifier_module = importlib.util.module_from_spec(_spec)
sys.modules[_spec.name] = _verifier_module
_spec.loader.exec_module(_verifier_module)
ContainerReader = _verifier_module.ContainerReader
ProgramVerifier = _verifier_module.ProgramVerifier

HEADER = struct.Struct("<4sHHIQQQIII")
DIRECTORY_ENTRY = struct.Struct("<IIQQII")
MAX_STACK = 4096
MAX_FRAME = 4096

STACK_OPCODES = {
    "CONST": 0x01,
    "UNARY": 0x02,
    "BINARY": 0x03,
    "CALL": 0x04,
    "LOAD_LOCAL": 0x05,
    "STORE_LOCAL": 0x06,
    "JUMP": 0x07,
    "BRANCH": 0x08,
    "RETURN": 0x09,
    "THROW": 0x0A,
}
REGISTER_OPCODES = {
    "CONST": 0x11,
    "UNARY": 0x12,
    "BINARY": 0x13,
    "CALL": 0x14,
    "JUMP": 0x17,
    "BRANCH": 0x18,
    "RETURN": 0x19,
    "THROW": 0x1A,
}
UNARY_SUBOPS = {"copy": 0, "make_closure": 1}
BINARY_SUBOPS = {"add": 0, "strict_equal": 1}
UNARY_NAMES = {value: key for key, value in UNARY_SUBOPS.items()}
BINARY_NAMES = {value: key for key, value in BINARY_SUBOPS.items()}
RESOURCE_LIMITS = {
    "max_code_bytes": 64 * 1024 * 1024,
    "max_instructions": 100_000,
    "max_constants": 10_000,
    "max_stack": MAX_STACK,
    "max_registers": MAX_FRAME,
    "max_locals": MAX_FRAME,
    "max_call_depth": 256,
    "max_exception_regions": 1_000,
}


@dataclass
class PlanOp:
    kind: str
    fields: dict[str, Any]
    may_throw: bool = False
    source_id: str = ""
    block_id: str = ""
    source: dict[str, Any] = field(default_factory=dict)

    def size(self, model: str) -> int:
        if model == "stack":
            return {
                "CONST": 6,
                "UNARY": 3,
                "BINARY": 3,
                "CALL": 4,
                "LOAD_LOCAL": 6,
                "STORE_LOCAL": 6,
                "JUMP": 6,
                "BRANCH": 10,
                "RETURN": 2,
                "THROW": 2,
            }[self.kind]
        if self.kind == "CONST":
            return 8
        if self.kind == "UNARY":
            return 7
        if self.kind == "BINARY":
            return 9
        if self.kind == "CALL":
            return 6 + 2 * len(self.fields["sources"])
        return {
            "JUMP": 6,
            "BRANCH": 12,
            "RETURN": 4,
            "THROW": 4,
        }[self.kind]


@dataclass
class DecodedInstruction:
    offset: int
    size: int
    opcode: int
    operation: str
    operands: list[int]
    constant_refs: list[int]
    successors: list[int]
    may_throw: bool
    may_suspend: bool
    stack_effect: dict[str, int] | None
    register_effects: dict[str, list[int]] | None
    argument_count: int | None
    details: dict[str, Any]


@dataclass
class CompiledFunction:
    function_id: str
    name: str
    model: str
    code: bytes
    container: bytes
    constants: list[Any]
    instructions: list[DecodedInstruction]
    verification_input: dict[str, Any]
    artifact: dict[str, Any]
    local_count: int
    register_count: int
    parameter_count: int
    debug_map: list[dict[str, Any]]
    code_generation_ns: int


class JSException(Exception):
    def __init__(self, value: Any):
        super().__init__("JavaScript exception")
        self.value = value


@dataclass
class Closure:
    function_id: str


@dataclass
class RuntimeState:
    functions: dict[str, CompiledFunction]
    host_trace: list[dict[str, Any]] = field(default_factory=list)
    dispatches: int = 0
    calls: int = 0
    call_depth: int = 0


UNINITIALIZED = object()


def canonical_json(value: Any) -> bytes:
    return json.dumps(
        value, sort_keys=True, separators=(",", ":"),
        ensure_ascii=False, allow_nan=False,
    ).encode("utf-8")


def u16(value: int) -> bytes:
    return struct.pack("<H", value)


def u32(value: int) -> bytes:
    return struct.pack("<I", value)


def json_key(value: Any) -> str:
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False, allow_nan=False)


def add_constant(constants: list[Any], indexes: dict[str, int], value: Any) -> int:
    key = json_key(value)
    if key not in indexes:
        indexes[key] = len(constants)
        constants.append(value)
    return indexes[key]


def effects_may_throw(ir_instruction: dict[str, Any]) -> bool:
    return bool(ir_instruction["effects"]["may_throw"])


def core_ir_plan(
    function_doc: dict[str, Any],
    model: str,
    constants: list[Any],
    constant_indexes: dict[str, int],
) -> tuple[list[PlanOp], dict[str, int], dict[str, int], int, int, list[dict[str, Any]]]:
    fn = function_doc["function"]
    params = fn["parameters"]
    bindings = fn["environment"]["bindings"]
    blocks = fn["blocks"]
    blocks_by_id = {block["id"]: block for block in blocks}
    values: dict[str, int] = {}
    binding_locations: dict[str, int] = {}
    next_location = 0

    for param in params:
        values[param["id"]] = next_location
        binding_locations[param["binding"]] = next_location
        next_location += 1

    for lexical_binding in bindings:
        if lexical_binding["id"] not in binding_locations:
            binding_locations[lexical_binding["id"]] = next_location
            next_location += 1

    # Core IR block parameters are real SSA definitions with edge-specific incoming
    # values. Give them stable frame locations before allocating instruction results.
    for block in blocks:
        for block_parameter in block["parameters"]:
            if block_parameter["kind"] == "value":
                values[block_parameter["id"]] = next_location
                next_location += 1

    # Allocate one stable frame location per SSA definition. Environment stores alias
    # their assigned value, while the lexical binding itself has its own mutable slot.
    for block in blocks:
        for ir_instruction in block["instructions"]:
            result = ir_instruction["result"]
            if result is None:
                continue
            result_id = result["id"]
            if ir_instruction["op"] == "env.store":
                values[result_id] = values[ir_instruction["operands"][0]]
            else:
                values[result_id] = next_location
                next_location += 1

    plans: list[PlanOp] = []
    block_plans: dict[str, list[PlanOp]] = {}
    debug_map: list[dict[str, Any]] = []
    synthetic_blocks: list[str] = []

    def emit(block_id: str, kind: str, fields: dict[str, Any], source_id: str,
             source: dict[str, Any], may_throw: bool = False) -> None:
        item = PlanOp(kind, fields, may_throw, source_id, block_id, source)
        block_plans[block_id].append(item)

    def const_id(value: Any) -> int:
        return add_constant(constants, constant_indexes, value)

    def distinct_register_sources(block_id: str, locations: list[int], source_id: str,
                                  src: dict[str, Any]) -> list[int]:
        nonlocal next_location
        if model != "register":
            return locations
        seen: set[int] = set()
        normalized: list[int] = []
        for index, location in enumerate(locations):
            if location in seen:
                temporary = next_location
                next_location += 1
                emit(
                    block_id, "UNARY",
                    {"dest": temporary, "source": location, "subop": "copy"},
                    f"{source_id}:duplicate-read:{index}", src,
                )
                normalized.append(temporary)
            else:
                normalized.append(location)
                seen.add(location)
        return normalized

    def edge_target(source_block: str, edge: dict[str, Any], suffix: str,
                    term_source: dict[str, Any]) -> str:
        nonlocal next_location
        target_id = edge["target"]
        target = blocks_by_id[target_id]
        arguments = edge["arguments"]
        if len(arguments) != len(target["parameters"]):
            raise ValueError(
                f"{fn['id']}: edge {source_block}->{target_id} has {len(arguments)} arguments "
                f"for {len(target['parameters'])} block parameters"
            )

        assignments: list[tuple[int, int]] = []
        for block_parameter, argument_id in zip(target["parameters"], arguments):
            if block_parameter["kind"] == "effect":
                continue
            assignments.append((values[argument_id], values[block_parameter["id"]]))

        if not assignments:
            return target_id

        # Edge arguments have parallel-copy semantics. Stage all sources first so
        # swaps/self-loop-carried values cannot be corrupted by sequential writes.
        bridge_id = f"__ogm16_edge_{source_block}_{suffix}"
        if bridge_id in block_plans or bridge_id in blocks_by_id:
            raise ValueError(f"{fn['id']}: synthetic edge block ID collision: {bridge_id}")
        block_plans[bridge_id] = []
        synthetic_blocks.append(bridge_id)
        staged: list[tuple[int, int]] = []
        for index, (source_location, target_location) in enumerate(assignments):
            temporary = next_location
            next_location += 1
            staged.append((temporary, target_location))
            if model == "stack":
                emit(bridge_id, "LOAD_LOCAL", {"local": source_location},
                     f"edge:{source_block}->{target_id}:{suffix}:stage:{index}", term_source)
                emit(bridge_id, "STORE_LOCAL", {"local": temporary},
                     f"edge:{source_block}->{target_id}:{suffix}:stage:{index}", term_source)
            else:
                emit(bridge_id, "UNARY", {"dest": temporary, "source": source_location, "subop": "copy"},
                     f"edge:{source_block}->{target_id}:{suffix}:stage:{index}", term_source)

        for index, (temporary, target_location) in enumerate(staged):
            if model == "stack":
                emit(bridge_id, "LOAD_LOCAL", {"local": temporary},
                     f"edge:{source_block}->{target_id}:{suffix}:commit:{index}", term_source)
                emit(bridge_id, "STORE_LOCAL", {"local": target_location},
                     f"edge:{source_block}->{target_id}:{suffix}:commit:{index}", term_source)
            else:
                emit(bridge_id, "UNARY", {"dest": target_location, "source": temporary, "subop": "copy"},
                     f"edge:{source_block}->{target_id}:{suffix}:commit:{index}", term_source)

        emit(bridge_id, "JUMP", {"target_label": target_id},
             f"edge:{source_block}->{target_id}:{suffix}:jump", term_source)
        return bridge_id

    for block in blocks:
        block_id = block["id"]
        block_plans[block_id] = []
        for ir_instruction in block["instructions"]:
            op = ir_instruction["op"]
            operands = ir_instruction["operands"]
            result = ir_instruction["result"]
            result_loc = None if result is None else values[result["id"]]
            src = ir_instruction["source"]
            may_throw = effects_may_throw(ir_instruction)
            source_id = ir_instruction["id"]

            if op == "js.constant":
                constant_index = const_id(ir_instruction["attributes"]["value"])
                if model == "stack":
                    emit(block_id, "CONST", {"constant_index": constant_index}, source_id, src)
                    emit(block_id, "STORE_LOCAL", {"local": result_loc}, source_id, src)
                else:
                    emit(block_id, "CONST", {"dest": result_loc, "constant_index": constant_index}, source_id, src)

            elif op in {"js.add", "js.strict_equal"}:
                subop = "add" if op == "js.add" else "strict_equal"
                if model == "stack":
                    for operand in operands:
                        emit(block_id, "LOAD_LOCAL", {"local": values[operand]}, source_id, src)
                    emit(block_id, "BINARY", {"subop": subop}, source_id, src, may_throw)
                    emit(block_id, "STORE_LOCAL", {"local": result_loc}, source_id, src)
                else:
                    sources = distinct_register_sources(
                        block_id, [values[operands[0]], values[operands[1]]], source_id, src,
                    )
                    emit(
                        block_id, "BINARY",
                        {"dest": result_loc, "left": sources[0], "right": sources[1], "subop": subop},
                        source_id, src, may_throw,
                    )

            elif op == "env.load":
                binding_loc = binding_locations[ir_instruction["attributes"]["binding"]]
                if model == "stack":
                    emit(block_id, "LOAD_LOCAL", {"local": binding_loc}, source_id, src, may_throw)
                    emit(block_id, "STORE_LOCAL", {"local": result_loc}, source_id, src)
                else:
                    emit(block_id, "UNARY", {"dest": result_loc, "source": binding_loc, "subop": "copy"}, source_id, src, may_throw)

            elif op == "env.store":
                source_loc = values[operands[0]]
                binding_loc = binding_locations[ir_instruction["attributes"]["binding"]]
                if model == "stack":
                    emit(block_id, "LOAD_LOCAL", {"local": source_loc}, source_id, src)
                    emit(block_id, "STORE_LOCAL", {"local": binding_loc}, source_id, src, may_throw)
                else:
                    emit(block_id, "UNARY", {"dest": binding_loc, "source": source_loc, "subop": "copy"}, source_id, src, may_throw)

            elif op == "closure.create":
                function_id = ir_instruction["attributes"]["function"]
                constant_index = const_id(function_id)
                if model == "stack":
                    emit(block_id, "CONST", {"constant_index": constant_index}, source_id, src)
                    emit(block_id, "UNARY", {"subop": "make_closure"}, source_id, src)
                    emit(block_id, "STORE_LOCAL", {"local": result_loc}, source_id, src)
                else:
                    temporary = next_location
                    next_location += 1
                    emit(block_id, "CONST", {"dest": temporary, "constant_index": constant_index}, source_id, src)
                    emit(block_id, "UNARY", {"dest": result_loc, "source": temporary, "subop": "make_closure"}, source_id, src)

            elif op == "js.call":
                sources = [values[item] for item in operands]
                argument_count = len(sources) - 1
                if model == "stack":
                    for location in sources:
                        emit(block_id, "LOAD_LOCAL", {"local": location}, source_id, src)
                    emit(block_id, "CALL", {"argument_count": argument_count}, source_id, src, may_throw)
                    emit(block_id, "STORE_LOCAL", {"local": result_loc}, source_id, src)
                else:
                    sources = distinct_register_sources(block_id, sources, source_id, src)
                    emit(
                        block_id, "CALL",
                        {"dest": result_loc, "sources": sources, "argument_count": argument_count},
                        source_id, src, may_throw,
                    )

            elif op == "host.call":
                symbol = ir_instruction["attributes"]["symbol"]
                host_constant = const_id("__host__:" + symbol)
                argument_sources = [values[item] for item in operands]
                argument_count = len(argument_sources)
                if model == "stack":
                    emit(block_id, "CONST", {"constant_index": host_constant}, source_id, src)
                    for location in argument_sources:
                        emit(block_id, "LOAD_LOCAL", {"local": location}, source_id, src)
                    emit(block_id, "CALL", {"argument_count": argument_count}, source_id, src, may_throw)
                    emit(block_id, "STORE_LOCAL", {"local": result_loc}, source_id, src)
                else:
                    temporary = next_location
                    next_location += 1
                    emit(block_id, "CONST", {"dest": temporary, "constant_index": host_constant}, source_id, src)
                    call_sources = distinct_register_sources(
                        block_id, [temporary, *argument_sources], source_id, src,
                    )
                    emit(
                        block_id, "CALL",
                        {"dest": result_loc, "sources": call_sources, "argument_count": argument_count},
                        source_id, src, may_throw,
                    )

            elif op == "js.throw":
                source_loc = values[operands[0]]
                if model == "stack":
                    emit(block_id, "LOAD_LOCAL", {"local": source_loc}, source_id, src)
                    emit(block_id, "THROW", {}, source_id, src, True)
                else:
                    emit(block_id, "THROW", {"source": source_loc}, source_id, src, True)

            else:
                raise ValueError(f"OGM-16 prototype does not implement Core IR operation {op!r}")

        terminator = block["terminator"]
        kind = terminator["kind"]
        term_source = terminator["source"]
        source_id = f"terminator:{block_id}:{kind}"
        if kind == "jump":
            target = edge_target(block_id, {
                "target": terminator["target"],
                "arguments": terminator["arguments"],
            }, "jump", term_source)
            emit(block_id, "JUMP", {"target_label": target}, source_id, term_source)
        elif kind == "branch":
            condition = values[terminator["condition"]]
            then_target = edge_target(block_id, terminator["then"], "then", term_source)
            else_target = edge_target(block_id, terminator["else"], "else", term_source)
            if model == "stack":
                emit(block_id, "LOAD_LOCAL", {"local": condition}, source_id, term_source)
            emit(
                block_id, "BRANCH",
                {
                    "condition": condition,
                    "then_label": then_target,
                    "else_label": else_target,
                },
                source_id, term_source,
            )
        elif kind == "return":
            location = values[terminator["value"]]
            if model == "stack":
                emit(block_id, "LOAD_LOCAL", {"local": location}, source_id, term_source)
                emit(block_id, "RETURN", {}, source_id, term_source)
            else:
                emit(block_id, "RETURN", {"source": location}, source_id, term_source)
        elif kind == "unreachable":
            if not block_plans[block_id] or block_plans[block_id][-1].kind != "THROW":
                raise ValueError(f"{fn['id']}: unreachable terminator is not preceded by js.throw")
        else:
            raise ValueError(f"{fn['id']}: unsupported Core IR terminator {kind!r}")

    ordered_block_ids = [block["id"] for block in blocks] + synthetic_blocks
    local_count = next_location if model == "stack" else 0
    register_count = next_location if model == "register" else 0
    if max(local_count, register_count) > MAX_FRAME:
        raise ValueError(f"{fn['id']}: frame exceeds prototype limit {MAX_FRAME}")

    for block_id in ordered_block_ids:
        plans.extend(block_plans[block_id])

    # Layout uses fixed-width absolute branch offsets, so one pass is sufficient.
    offsets: dict[int, int] = {}
    block_offsets: dict[str, int] = {}
    cursor = 0
    for block_id in ordered_block_ids:
        block_offsets[block_id] = cursor
        for item in block_plans[block_id]:
            offsets[id(item)] = cursor
            cursor += item.size(model)

    for item in plans:
        offset = offsets[id(item)]
        details = dict(item.fields)
        if "target_label" in details:
            details["target"] = block_offsets[details.pop("target_label")]
        if "then_label" in details:
            details["then_target"] = block_offsets[details.pop("then_label")]
            details["else_target"] = block_offsets[details.pop("else_label")]
        debug_map.append({
            "offset": offset,
            "block": item.block_id,
            "core_ir": item.source_id,
            "generated_opcode": item.kind.lower(),
            "source": item.source,
        })

    return plans, offsets, block_offsets, local_count, register_count, debug_map

def encode_instruction(item: PlanOp, offset: int, model: str, offsets: dict[int, int],
                       block_offsets: dict[str, int]) -> bytes:
    fields = item.fields
    flags = (1 if item.may_throw else 0)
    if model == "stack":
        opcode = STACK_OPCODES[item.kind]
        prefix = bytes((opcode, flags))
        if item.kind == "CONST":
            return prefix + u32(fields["constant_index"])
        if item.kind in {"LOAD_LOCAL", "STORE_LOCAL"}:
            return prefix + u32(fields["local"])
        if item.kind == "UNARY":
            return prefix + bytes((UNARY_SUBOPS[fields["subop"]],))
        if item.kind == "BINARY":
            return prefix + bytes((BINARY_SUBOPS[fields["subop"]],))
        if item.kind == "CALL":
            return prefix + u16(fields["argument_count"])
        if item.kind == "JUMP":
            target = block_offsets[fields["target_label"]]
            return prefix + u32(target)
        if item.kind == "BRANCH":
            then_target = block_offsets[fields["then_label"]]
            else_target = block_offsets[fields["else_label"]]
            return prefix + u32(then_target) + u32(else_target)
        if item.kind in {"RETURN", "THROW"}:
            return prefix
        raise ValueError(item.kind)

    opcode = REGISTER_OPCODES[item.kind]
    prefix = bytes((opcode, flags))
    if item.kind == "CONST":
        return prefix + u16(fields["dest"]) + u32(fields["constant_index"])
    if item.kind == "UNARY":
        return prefix + u16(fields["dest"]) + u16(fields["source"]) + bytes((UNARY_SUBOPS[fields["subop"]],))
    if item.kind == "BINARY":
        return (
            prefix + u16(fields["dest"]) + u16(fields["left"]) + u16(fields["right"])
            + bytes((BINARY_SUBOPS[fields["subop"]],))
        )
    if item.kind == "CALL":
        sources = fields["sources"]
        return prefix + u16(fields["dest"]) + u16(fields["argument_count"]) + b"".join(u16(r) for r in sources)
    if item.kind == "JUMP":
        return prefix + u32(block_offsets[fields["target_label"]])
    if item.kind == "BRANCH":
        return (
            prefix + u16(fields["condition"]) + u32(block_offsets[fields["then_label"]])
            + u32(block_offsets[fields["else_label"]])
        )
    if item.kind in {"RETURN", "THROW"}:
        return prefix + u16(fields["source"])
    raise ValueError(item.kind)


def decode_code(code: bytes, model: str) -> list[DecodedInstruction]:
    result: list[DecodedInstruction] = []
    cursor = 0
    reverse = {value: key for key, value in (STACK_OPCODES if model == "stack" else REGISTER_OPCODES).items()}

    def read_u16(position: int) -> int:
        return struct.unpack_from("<H", code, position)[0]

    def read_u32(position: int) -> int:
        return struct.unpack_from("<I", code, position)[0]

    while cursor < len(code):
        start = cursor
        opcode = code[cursor]
        flags = code[cursor + 1]
        cursor += 2
        if flags & ~0x03:
            raise ValueError(f"unknown instruction flags at byte offset {start}")
        kind = reverse.get(opcode)
        if kind is None:
            raise ValueError(f"unknown {model} opcode 0x{opcode:02x} at byte offset {start}")
        may_throw = bool(flags & 0x01)
        details: dict[str, Any] = {}
        operands: list[int] = []
        constant_refs: list[int] = []
        argument_count: int | None = None

        if model == "stack":
            if kind == "CONST":
                constant_index = read_u32(cursor); cursor += 4
                operation, constant_refs = "constant", [constant_index]
                details["constant_index"] = constant_index
            elif kind in {"LOAD_LOCAL", "STORE_LOCAL"}:
                local = read_u32(cursor); cursor += 4
                operation = "load_local" if kind == "LOAD_LOCAL" else "store_local"
                operands = [local]; details["local"] = local
            elif kind == "UNARY":
                subop = code[cursor]; cursor += 1
                if subop not in UNARY_NAMES:
                    raise ValueError(f"unknown unary sub-operation {subop}")
                operation = "unary"; details["subop"] = UNARY_NAMES[subop]
            elif kind == "BINARY":
                subop = code[cursor]; cursor += 1
                if subop not in BINARY_NAMES:
                    raise ValueError(f"unknown binary sub-operation {subop}")
                operation = "binary"; details["subop"] = BINARY_NAMES[subop]
            elif kind == "CALL":
                argument_count = read_u16(cursor); cursor += 2
                operation = "call"; details["argument_count"] = argument_count
            elif kind == "JUMP":
                target = read_u32(cursor); cursor += 4
                operation = "jump"; operands = [target]; details["target"] = target
            elif kind == "BRANCH":
                then_target = read_u32(cursor); else_target = read_u32(cursor + 4); cursor += 8
                operation = "branch"; operands = [then_target, else_target]
                details.update({"then_target": then_target, "else_target": else_target})
            elif kind == "RETURN":
                operation = "return"
            elif kind == "THROW":
                operation = "throw"
            else:
                raise ValueError(kind)

            stack_effects = {
                "constant": {"pop": 0, "push": 1},
                "load_local": {"pop": 0, "push": 1},
                "store_local": {"pop": 1, "push": 0},
                "unary": {"pop": 1, "push": 1},
                "binary": {"pop": 2, "push": 1},
                "call": {"pop": (argument_count or 0) + 1, "push": 1},
                "jump": {"pop": 0, "push": 0},
                "branch": {"pop": 1, "push": 0},
                "return": {"pop": 1, "push": 0},
                "throw": {"pop": 1, "push": 0},
            }
            stack_effect = stack_effects[operation]
            register_effects = None
        else:
            if kind == "CONST":
                dest = read_u16(cursor); constant_index = read_u32(cursor + 2); cursor += 6
                operation, constant_refs = "constant", [constant_index]
                details.update({"dest": dest, "constant_index": constant_index})
                register_effects = {"reads": [], "writes": [dest]}
            elif kind == "UNARY":
                dest = read_u16(cursor); source = read_u16(cursor + 2); subop = code[cursor + 4]; cursor += 5
                if subop not in UNARY_NAMES:
                    raise ValueError(f"unknown unary sub-operation {subop}")
                operation = "unary"; details.update({"dest": dest, "source": source, "subop": UNARY_NAMES[subop]})
                register_effects = {"reads": [source], "writes": [dest]}
            elif kind == "BINARY":
                dest = read_u16(cursor); left = read_u16(cursor + 2); right = read_u16(cursor + 4); subop = code[cursor + 6]; cursor += 7
                if subop not in BINARY_NAMES:
                    raise ValueError(f"unknown binary sub-operation {subop}")
                operation = "binary"; details.update({"dest": dest, "left": left, "right": right, "subop": BINARY_NAMES[subop]})
                register_effects = {"reads": [left, right], "writes": [dest]}
            elif kind == "CALL":
                dest = read_u16(cursor); argument_count = read_u16(cursor + 2); cursor += 4
                sources = [read_u16(cursor + 2 * index) for index in range(argument_count + 1)]
                cursor += 2 * (argument_count + 1)
                operation = "call"; details.update({"dest": dest, "sources": sources, "argument_count": argument_count})
                register_effects = {"reads": sources, "writes": [dest]}
            elif kind == "JUMP":
                target = read_u32(cursor); cursor += 4
                operation = "jump"; operands = [target]; details["target"] = target
                register_effects = {"reads": [], "writes": []}
            elif kind == "BRANCH":
                condition = read_u16(cursor); then_target = read_u32(cursor + 2); else_target = read_u32(cursor + 6); cursor += 10
                operation = "branch"; operands = [then_target, else_target]
                details.update({"condition": condition, "then_target": then_target, "else_target": else_target})
                register_effects = {"reads": [condition], "writes": []}
            elif kind == "RETURN":
                source = read_u16(cursor); cursor += 2
                operation = "return"; details["source"] = source
                register_effects = {"reads": [source], "writes": []}
            elif kind == "THROW":
                source = read_u16(cursor); cursor += 2
                operation = "throw"; details["source"] = source
                register_effects = {"reads": [source], "writes": []}
            else:
                raise ValueError(kind)
            stack_effect = None

        size = cursor - start
        if cursor > len(code):
            raise ValueError(f"truncated instruction at byte offset {start}")
        if model == "stack":
            register_effects = None
            details["kind"] = kind
        else:
            details["kind"] = kind
        result.append(DecodedInstruction(
            offset=start,
            size=size,
            opcode=opcode,
            operation=operation,
            operands=operands,
            constant_refs=constant_refs,
            successors=[],
            may_throw=may_throw,
            may_suspend=bool(flags & 0x02),
            stack_effect=stack_effect,
            register_effects=register_effects,
            argument_count=argument_count,
            details=details,
        ))

    for index, instruction in enumerate(result):
        next_offset = instruction.offset + instruction.size
        if instruction.operation in {"jump", "branch"}:
            instruction.successors = list(instruction.operands)
        elif instruction.operation in {"return", "throw"}:
            instruction.successors = []
        else:
            instruction.successors = [next_offset] if next_offset < len(code) else []
    return result


def max_stack_depth(instructions: list[DecodedInstruction]) -> int:
    by_offset = {item.offset: item for item in instructions}
    depths = {0: 0}
    pending = [0]
    while pending:
        offset = pending.pop(0)
        item = by_offset[offset]
        assert item.stack_effect is not None
        incoming = depths[offset]
        effect = item.stack_effect
        if incoming < effect["pop"]:
            raise ValueError(f"stack underflow at byte offset {offset}")
        outgoing = incoming - effect["pop"] + effect["push"]
        if item.operation in {"return", "throw"} and outgoing != 0:
            raise ValueError(f"non-empty stack at terminal offset {offset}")
        for target in item.successors:
            existing = depths.get(target)
            if existing is None:
                depths[target] = outgoing
                pending.append(target)
            elif existing != outgoing:
                raise ValueError(f"inconsistent stack merge at byte offset {target}")
    if set(depths) != set(by_offset):
        raise ValueError("stack candidate contains unreachable bytecode")
    return max(depths.values(), default=0)


def max_register_index(instructions: list[DecodedInstruction], entry_registers: list[int]) -> int | None:
    used = set(entry_registers)
    for item in instructions:
        assert item.register_effects is not None
        used.update(item.register_effects["reads"])
        used.update(item.register_effects["writes"])
    return max(used) if used else None


def build_ogmb(code: bytes, constants: list[Any], manifest: dict[str, Any],
               exceptions: list[dict[str, Any]] | None = None) -> bytes:
    sections = [
        (1, 0, canonical_json(manifest)),
        (2, 0, code),
        (3, 0, canonical_json(constants)),
        (4, 0, canonical_json([] if exceptions is None else exceptions)),
    ]
    directory_end = HEADER.size + len(sections) * DIRECTORY_ENTRY.size
    data = bytearray(b"\x00" * directory_end)
    entries: list[tuple[int, int, int, int, int, int]] = []
    for section_type, flags, payload in sections:
        aligned = (len(data) + 7) & ~7
        data.extend(b"\x00" * (aligned - len(data)))
        offset = len(data)
        data.extend(payload)
        entries.append((section_type, flags, offset, len(payload), 8, 0))

    HEADER.pack_into(
        data, 0, b"OGMB", 1, 0, HEADER.size, 0, len(data),
        HEADER.size, len(entries), DIRECTORY_ENTRY.size, 0,
    )
    for index, entry in enumerate(entries):
        DIRECTORY_ENTRY.pack_into(data, HEADER.size + index * DIRECTORY_ENTRY.size, *entry)
    return bytes(data)


def frame_metadata(fn: dict[str, Any], model: str, local_count: int, register_count: int,
                   max_depth: int, max_register: int | None, code_digest: str,
                   instruction_count: int, constant_count: int,
                   binding_locations: dict[str, int]) -> dict[str, Any]:
    environment = fn["environment"]
    locations = []
    for item in environment["bindings"]:
        locations.append({
            "binding": item["id"],
            "kind": "local" if model == "stack" else "register",
            "index": binding_locations[item["id"]],
        })
    return {
        "execution_model": model,
        "function": {"id": fn["id"], "name": fn["name"]},
        "code_generation": {"id": "ogma_" + model + "_prototype", "version": "1.0.0"},
        "code_cache": {"id": "cache_" + fn["id"], "version": "1.0.0"},
        "frame_layout": {
            "parameter_count": len(fn["parameters"]),
            "local_count": local_count,
            "register_count": register_count,
        },
        "environment": {
            "id": environment["id"],
            "parent": environment["parent"],
            "bindings": environment["bindings"],
            "captures": fn["captures"],
            "locations": locations,
        },
        "suspension_points": [],
        "inline_cache_slots": [],
        "verifier_summary": {
            "schema": "ogma.bytecode-verification-summary",
            "version": "1.0.0",
            "code_sha256": code_digest,
            "instruction_count": instruction_count,
            "max_stack_depth": max_depth,
            "max_register_index": max_register,
            "exception_region_count": 0,
            "constant_count": constant_count,
        },
    }


def normalized_input(model: str, encoding_id: str, code_digest: str,
                     instructions: list[DecodedInstruction], parameter_count: int) -> dict[str, Any]:
    return {
        "schema": "ogma.bytecode-verification",
        "version": "1.0.0",
        "encoding_id": encoding_id,
        "code_sha256": code_digest,
        "execution_model": model,
        "entry": 0,
        "entry_registers": list(range(parameter_count)) if model == "register" else [],
        "instructions": [
            {
                "offset": item.offset,
                "size": item.size,
                "opcode": item.opcode,
                "operation": item.operation,
                "operands": item.operands,
                "constant_refs": item.constant_refs,
                "successors": item.successors,
                "may_throw": item.may_throw,
                "may_suspend": item.may_suspend,
                "stack_effect": item.stack_effect,
                "register_effects": item.register_effects,
                "argument_count": item.argument_count,
            }
            for item in instructions
        ],
    }


def compile_function(function_doc: dict[str, Any], model: str) -> CompiledFunction:
    started = time.perf_counter_ns()
    fn = function_doc["function"]
    constants: list[Any] = []
    constant_indexes: dict[str, int] = {}
    plans, offsets, block_offsets, local_count, register_count, debug_map = core_ir_plan(
        function_doc, model, constants, constant_indexes,
    )
    encoded = b"".join(
        encode_instruction(item, offsets[id(item)], model, offsets, block_offsets)
        for item in plans
    )
    decoded = decode_code(encoded, model)
    if not decoded or decoded[0].offset != 0 or decoded[-1].offset + decoded[-1].size != len(encoded):
        raise ValueError(f"{fn['id']}: decoded bytecode does not cover the encoded code section")

    if model == "stack":
        depth = max_stack_depth(decoded)
        register_index = None
    else:
        depth = 0
        register_index = max_register_index(decoded, list(range(len(fn["parameters"]))))
        if register_index is None:
            raise ValueError(f"{fn['id']}: register function produced no virtual registers")

    digest = hashlib.sha256(encoded).hexdigest()
    encoding_id = "ogma." + model + ".prototype.v1"
    # Recover the Core IR binding map using the exact allocation policy used by core_ir_plan.
    binding_locations: dict[str, int] = {}
    slot = 0
    for param in fn["parameters"]:
        binding_locations[param["binding"]] = slot
        slot += 1
    for lexical_binding in fn["environment"]["bindings"]:
        if lexical_binding["id"] not in binding_locations:
            binding_locations[lexical_binding["id"]] = slot
            slot += 1
    # The register planner allocates scratch operands after the initial frame. Environment
    # bindings are always in the stable parameter/local prefix.
    metadata = frame_metadata(
        fn, model, local_count, register_count, depth, register_index,
        digest, len(decoded), len(constants), binding_locations,
    )
    manifest = {
        "format": "ogma.ogmb",
        "version": "1.0",
        "encoding_id": encoding_id,
        "code_sha256": digest,
        "resource_limits": RESOURCE_LIMITS,
        "metadata": metadata,
    }
    container = build_ogmb(encoded, constants, manifest)
    code_generation_ns = time.perf_counter_ns() - started
    reader = ContainerReader()
    artifact = reader.read(container)
    if artifact is None:
        raise ValueError(f"{fn['id']}: .ogmb container rejected: {reader.errors}")
    verification_input = normalized_input(model, encoding_id, digest, decoded, len(fn["parameters"]))
    verifier = ProgramVerifier()
    errors = verifier.verify(artifact, verification_input)
    if errors:
        raise ValueError(f"{fn['id']}: {model} bytecode rejected by OGM-36 verifier: {errors}")

    return CompiledFunction(
        function_id=fn["id"],
        name=fn["name"],
        model=model,
        code=encoded,
        container=container,
        constants=constants,
        instructions=decoded,
        verification_input=verification_input,
        artifact=artifact,
        local_count=local_count,
        register_count=register_count,
        parameter_count=len(fn["parameters"]),
        debug_map=debug_map,
        code_generation_ns=code_generation_ns,
    )


def js_truthy(value: Any) -> bool:
    if value is None or value is False:
        return False
    if isinstance(value, (int, float)) and (value == 0 or (isinstance(value, float) and value != value)):
        return False
    if isinstance(value, str) and value == "":
        return False
    return True


def js_to_string(value: Any) -> str:
    if value is None:
        return "null"
    if value is True:
        return "true"
    if value is False:
        return "false"
    if isinstance(value, str):
        return value
    if isinstance(value, float) and value.is_integer():
        return str(int(value))
    return str(value)


def js_to_number(value: Any) -> float | int:
    if value is None:
        return 0
    if value is True:
        return 1
    if value is False:
        return 0
    if isinstance(value, (int, float)):
        return value
    if isinstance(value, str):
        stripped = value.strip()
        if not stripped:
            return 0
        try:
            return float(stripped)
        except ValueError:
            return float("nan")
    return float("nan")


def js_add(left: Any, right: Any) -> Any:
    # Workloads are restricted to primitive values, so ToPrimitive is deterministic here.
    if isinstance(left, str) or isinstance(right, str):
        return js_to_string(left) + js_to_string(right)
    return js_to_number(left) + js_to_number(right)


def js_strict_equal(left: Any, right: Any) -> bool:
    if isinstance(left, bool) != isinstance(right, bool):
        return False
    if left is None or right is None:
        return left is None and right is None
    if isinstance(left, (int, float)) and isinstance(right, (int, float)):
        if isinstance(left, float) and left != left:
            return False
        if isinstance(right, float) and right != right:
            return False
        return left == right
    if type(left) is not type(right):
        return False
    return left == right


def invoke_call(values: list[Any], state: RuntimeState) -> Any:
    if not values:
        raise JSException("TypeError: missing callee")
    callee, arguments = values[0], values[1:]
    state.calls += 1
    if isinstance(callee, Closure):
        if not arguments:
            raise JSException("TypeError: missing thisArg")
        return execute_function(callee.function_id, arguments[1:], state)
    if isinstance(callee, str) and callee.startswith("__host__:"):
        symbol = callee.removeprefix("__host__:")
        if symbol != "trace":
            raise JSException(f"TypeError: unknown host capability {symbol}")
        state.host_trace.append({"symbol": symbol, "args": arguments})
        return arguments[0] if arguments else None
    raise JSException("TypeError: value is not callable")


def execute_function(function_id: str, arguments: list[Any], state: RuntimeState) -> Any:
    if state.call_depth >= 128:
        raise JSException("RangeError: call depth exceeded")
    if function_id not in state.functions:
        raise JSException(f"ReferenceError: missing function {function_id}")
    compiled = state.functions[function_id]
    if len(arguments) != compiled.parameter_count:
        raise JSException("TypeError: argument count mismatch")
    state.call_depth += 1
    try:
        by_offset = {item.offset: item for item in compiled.instructions}
        if compiled.model == "stack":
            locals_frame = [UNINITIALIZED] * compiled.local_count
            for index, argument in enumerate(arguments):
                locals_frame[index] = argument
            operand_stack: list[Any] = []
        else:
            registers = [UNINITIALIZED] * compiled.register_count
            for index, argument in enumerate(arguments):
                registers[index] = argument

        pc = 0
        while True:
            item = by_offset.get(pc)
            if item is None:
                raise JSException(f"InternalError: invalid instruction pointer {pc}")
            state.dispatches += 1
            op = item.operation
            details = item.details
            next_pc = item.offset + item.size

            if compiled.model == "stack":
                if op == "constant":
                    operand_stack.append(compiled.constants[details["constant_index"]])
                elif op == "load_local":
                    loaded = locals_frame[details["local"]]
                    if loaded is UNINITIALIZED:
                        raise JSException("ReferenceError: lexical binding is uninitialized")
                    operand_stack.append(loaded)
                elif op == "store_local":
                    locals_frame[details["local"]] = operand_stack.pop()
                elif op == "unary":
                    operand = operand_stack.pop()
                    if details["subop"] == "copy":
                        result = operand
                    elif details["subop"] == "make_closure":
                        result = Closure(str(operand))
                    else:
                        raise JSException("InternalError: unsupported unary operation")
                    operand_stack.append(result)
                elif op == "binary":
                    right = operand_stack.pop()
                    left = operand_stack.pop()
                    if details["subop"] == "add":
                        result = js_add(left, right)
                    elif details["subop"] == "strict_equal":
                        result = js_strict_equal(left, right)
                    else:
                        raise JSException("InternalError: unsupported binary operation")
                    operand_stack.append(result)
                elif op == "call":
                    count = (item.argument_count or 0) + 1
                    call_values = operand_stack[-count:]
                    del operand_stack[-count:]
                    operand_stack.append(invoke_call(call_values, state))
                elif op == "jump":
                    next_pc = details["target"]
                elif op == "branch":
                    condition = operand_stack.pop()
                    next_pc = details["then_target"] if js_truthy(condition) else details["else_target"]
                elif op == "return":
                    return operand_stack.pop()
                elif op == "throw":
                    raise JSException(operand_stack.pop())
                else:
                    raise JSException(f"InternalError: unsupported stack instruction {op}")

            else:
                if op == "constant":
                    registers[details["dest"]] = compiled.constants[details["constant_index"]]
                elif op == "unary":
                    operand = registers[details["source"]]
                    if operand is UNINITIALIZED:
                        raise JSException("ReferenceError: lexical binding is uninitialized")
                    if details["subop"] == "copy":
                        result = operand
                    elif details["subop"] == "make_closure":
                        result = Closure(str(operand))
                    else:
                        raise JSException("InternalError: unsupported unary operation")
                    registers[details["dest"]] = result
                elif op == "binary":
                    left = registers[details["left"]]
                    right = registers[details["right"]]
                    if left is UNINITIALIZED or right is UNINITIALIZED:
                        raise JSException("ReferenceError: uninitialized register")
                    if details["subop"] == "add":
                        registers[details["dest"]] = js_add(left, right)
                    elif details["subop"] == "strict_equal":
                        registers[details["dest"]] = js_strict_equal(left, right)
                    else:
                        raise JSException("InternalError: unsupported binary operation")
                elif op == "call":
                    call_values = [registers[index] for index in details["sources"]]
                    if any(value is UNINITIALIZED for value in call_values):
                        raise JSException("ReferenceError: uninitialized call operand")
                    registers[details["dest"]] = invoke_call(call_values, state)
                elif op == "jump":
                    next_pc = details["target"]
                elif op == "branch":
                    condition = registers[details["condition"]]
                    if condition is UNINITIALIZED:
                        raise JSException("ReferenceError: uninitialized branch condition")
                    next_pc = details["then_target"] if js_truthy(condition) else details["else_target"]
                elif op == "return":
                    result = registers[details["source"]]
                    if result is UNINITIALIZED:
                        raise JSException("ReferenceError: uninitialized return value")
                    return result
                elif op == "throw":
                    thrown = registers[details["source"]]
                    if thrown is UNINITIALIZED:
                        raise JSException("ReferenceError: uninitialized thrown value")
                    raise JSException(thrown)
                else:
                    raise JSException(f"InternalError: unsupported register instruction {op}")

            pc = next_pc
    finally:
        state.call_depth -= 1


def execute_case(workload: dict[str, Any], compiled: list[CompiledFunction],
                 arguments: list[Any]) -> tuple[dict[str, Any], int]:
    functions = {item.function_id: item for item in compiled}
    state = RuntimeState(functions)
    try:
        result = execute_function(workload["entry"], arguments, state)
        outcome = {"return": result, "host_trace": state.host_trace}
    except JSException as exc:
        outcome = {"throw": exc.value, "host_trace": state.host_trace}
    return outcome, state.dispatches


def normalized_size(compiled: CompiledFunction) -> int:
    return len(canonical_json(compiled.verification_input))


def verifier_complexity(compiled: CompiledFunction) -> dict[str, int | None]:
    instructions = compiled.verification_input["instructions"]
    edges = sum(len(item["successors"]) for item in instructions)
    operands = sum(len(item["operands"]) + len(item["constant_refs"]) for item in instructions)
    reads = 0
    writes = 0
    stack_effects = 0
    if compiled.model == "register":
        for item in instructions:
            reads += len(item["register_effects"]["reads"])
            writes += len(item["register_effects"]["writes"])
    else:
        stack_effects = len(instructions)
    return {
        "instructions": len(instructions),
        "control_flow_edges": edges,
        "immediate_and_constant_operands": operands,
        "stack_effect_checks": stack_effects,
        "register_reads": reads,
        "register_writes": writes,
        "state_analysis_operations": stack_effects + reads + writes,
        "structural_work_units": len(instructions) + edges + operands + stack_effects + reads + writes,
        "verification_input_bytes": normalized_size(compiled),
    }


def verify_compiled(compiled: CompiledFunction) -> None:
    reader = ContainerReader()
    artifact = reader.read(compiled.container)
    if artifact is None:
        raise AssertionError(f"{compiled.function_id}: container verification failed: {reader.errors}")
    verifier = ProgramVerifier()
    errors = verifier.verify(artifact, compiled.verification_input)
    if errors:
        raise AssertionError(f"{compiled.function_id}: bytecode verification failed: {errors}")


def compile_workload(workload: dict[str, Any], model: str) -> list[CompiledFunction]:
    return [compile_function(document, model) for document in workload["functions"]]


def run_workload_correctness(workload: dict[str, Any], stack_code: list[CompiledFunction],
                             register_code: list[CompiledFunction]) -> list[dict[str, Any]]:
    observations: list[dict[str, Any]] = []
    for case_index, case in enumerate(workload["cases"]):
        stack_outcome, stack_dispatches = execute_case(workload, stack_code, case["args"])
        register_outcome, register_dispatches = execute_case(workload, register_code, case["args"])
        expected = case["expected"]
        if stack_outcome != expected:
            raise AssertionError(
                f"{workload['id']} case {case_index}: stack result {stack_outcome!r} != expected {expected!r}"
            )
        if register_outcome != expected:
            raise AssertionError(
                f"{workload['id']} case {case_index}: register result {register_outcome!r} != expected {expected!r}"
            )
        if stack_outcome != register_outcome:
            raise AssertionError(f"{workload['id']} case {case_index}: stack/register observations differ")
        observations.append({
            "case": case_index,
            "expected": expected,
            "stack": stack_outcome,
            "register": register_outcome,
            "stack_dispatches": stack_dispatches,
            "register_dispatches": register_dispatches,
        })
    return observations


def environment_snapshot() -> dict[str, Any]:
    cpu = platform.processor() or platform.machine()
    try:
        import os
        if Path("/proc/cpuinfo").exists():
            for line in Path("/proc/cpuinfo").read_text(errors="replace").splitlines():
                if line.lower().startswith("model name"):
                    cpu = line.split(":", 1)[-1].strip()
                    break
    except OSError:
        pass
    return {
        "python": sys.version.split()[0],
        "implementation": platform.python_implementation(),
        "platform": platform.platform(),
        "machine": platform.machine(),
        "processor": cpu,
    }
