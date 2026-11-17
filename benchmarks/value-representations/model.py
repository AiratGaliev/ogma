"""Core IR-driven Value layout and representation-aware SSA proof model."""
from __future__ import annotations

from collections import Counter
from dataclasses import dataclass
from enum import Enum
import math
import struct
from typing import Any

from corpus import Corpus


class Tag(str, Enum):
    UNDEFINED = "undefined"
    NULL = "null"
    BOOLEAN = "boolean"
    NUMBER = "number"
    STRING = "string"
    BIGINT = "bigint"
    OBJECT = "object"
    SYMBOL = "symbol"


@dataclass(frozen=True)
class JSValue:
    tag: Tag
    payload: Any = None


UNDEFINED = JSValue(Tag.UNDEFINED)
NULL = JSValue(Tag.NULL)


def number(value: float | int) -> JSValue:
    return JSValue(Tag.NUMBER, float(value))


def boolean(value: bool) -> JSValue:
    if not isinstance(value, bool):
        raise TypeError("boolean payload must be bool")
    return JSValue(Tag.BOOLEAN, value)


def js_string(value: str) -> JSValue:
    if not isinstance(value, str):
        raise TypeError("string payload must be str")
    return JSValue(Tag.STRING, value)


def bigint(value: int) -> JSValue:
    if isinstance(value, bool) or not isinstance(value, int):
        raise TypeError("BigInt payload must be int")
    return JSValue(Tag.BIGINT, value)


def object_value(identity: int) -> JSValue:
    if isinstance(identity, bool) or not isinstance(identity, int) or identity < 0:
        raise ValueError("object identity must be a non-negative integer")
    return JSValue(Tag.OBJECT, identity)


def symbol_value(identity: int) -> JSValue:
    if isinstance(identity, bool) or not isinstance(identity, int) or identity < 0:
        raise ValueError("symbol identity must be a non-negative integer")
    return JSValue(Tag.SYMBOL, identity)


def to_js_string(value: JSValue) -> str:
    if value.tag is Tag.UNDEFINED:
        return "undefined"
    if value.tag is Tag.NULL:
        return "null"
    if value.tag is Tag.BOOLEAN:
        return "true" if value.payload else "false"
    if value.tag is Tag.NUMBER:
        n = float(value.payload)
        if math.isnan(n):
            return "NaN"
        if math.isinf(n):
            return "Infinity" if n > 0 else "-Infinity"
        if n == 0:
            return "0"
        if n.is_integer() and abs(n) < 1e21:
            return str(int(n))
        return format(n, ".15g")
    if value.tag is Tag.BIGINT:
        return str(value.payload)
    if value.tag is Tag.STRING:
        return value.payload
    if value.tag is Tag.OBJECT:
        return "[object Object]"
    if value.tag is Tag.SYMBOL:
        raise TypeError("Cannot convert a Symbol value to a string")
    raise AssertionError(value.tag)


def to_number(value: JSValue) -> float:
    if value.tag is Tag.NUMBER:
        return float(value.payload)
    if value.tag is Tag.BOOLEAN:
        return 1.0 if value.payload else 0.0
    if value.tag is Tag.NULL:
        return 0.0
    if value.tag is Tag.UNDEFINED:
        return math.nan
    if value.tag is Tag.STRING:
        raw = value.payload.strip()
        if not raw:
            return 0.0
        try:
            return float(raw)
        except ValueError:
            return math.nan
    if value.tag is Tag.BIGINT:
        raise TypeError("Cannot convert a BigInt value to a number")
    raise TypeError("Cannot convert value to a number")


def js_add(left: JSValue, right: JSValue) -> JSValue:
    # Restricted primitive-only ToPrimitive subset for the M0 fixtures.
    if (left.tag is Tag.BIGINT and right.tag is Tag.NUMBER) or (
        right.tag is Tag.BIGINT and left.tag is Tag.NUMBER
    ):
        raise TypeError("Cannot mix BigInt and other types, use explicit conversions")
    if left.tag is Tag.BIGINT and right.tag is Tag.BIGINT:
        return bigint(left.payload + right.payload)
    if left.tag is Tag.STRING or right.tag is Tag.STRING:
        return js_string(to_js_string(left) + to_js_string(right))
    if left.tag is Tag.OBJECT or right.tag is Tag.OBJECT:
        raise NotImplementedError("object ToPrimitive is outside this M0 semantic subset")
    return number(to_number(left) + to_number(right))


def strict_equal(left: JSValue, right: JSValue) -> bool:
    if left.tag is not right.tag:
        return False
    if left.tag is Tag.NUMBER:
        a, b = float(left.payload), float(right.payload)
        return not (math.isnan(a) or math.isnan(b)) and a == b
    if left.tag in (Tag.OBJECT, Tag.SYMBOL):
        return left.payload == right.payload
    return left.payload == right.payload


CANONICAL_NAN = 0x7FF8000000000000
TAG_SHIFT = 48
PAYLOAD_MASK = (1 << TAG_SHIFT) - 1
NAN_TAGS = {
    Tag.UNDEFINED: 0x7FF9,
    Tag.NULL: 0x7FFA,
    Tag.BOOLEAN: 0x7FFB,
    Tag.STRING: 0x7FFC,
    Tag.BIGINT: 0x7FFD,
    Tag.OBJECT: 0x7FFE,
    Tag.SYMBOL: 0x7FFF,
}
REVERSE_NAN_TAGS = {encoded: tag for tag, encoded in NAN_TAGS.items()}


def f64_bits(value: float) -> int:
    return struct.unpack("<Q", struct.pack("<d", value))[0]


def bits_f64(bits: int) -> float:
    return struct.unpack("<d", struct.pack("<Q", bits))[0]


class HeapHandles:
    """Deterministic synthetic heap table; handles are not production GC pointers."""

    def __init__(self, max_handle: int = PAYLOAD_MASK):
        if isinstance(max_handle, bool) or not 1 <= max_handle <= PAYLOAD_MASK:
            raise ValueError("max_handle must fit in the 48-bit payload")
        self.max_handle = max_handle
        self._next = 1
        self._by_handle: dict[int, JSValue] = {}
        self._by_value: dict[JSValue, int] = {}

    def intern(self, value: JSValue) -> int:
        handle = self._by_value.get(value)
        if handle is not None:
            return handle
        handle = self._next
        if handle > self.max_handle:
            raise OverflowError("NaN-box synthetic handle space exhausted")
        self._next += 1
        self._by_value[value] = handle
        self._by_handle[handle] = value
        return handle

    def resolve(self, handle: int) -> JSValue:
        try:
            return self._by_handle[handle]
        except KeyError as exc:
            raise ValueError(f"unknown NaN-box synthetic handle {handle}") from exc


class NaNBoxCodec:
    def __init__(self, heap: HeapHandles | None = None):
        self.heap = heap or HeapHandles()

    def encode(self, value: JSValue) -> int:
        if value.tag is Tag.NUMBER:
            number_value = float(value.payload)
            return CANONICAL_NAN if math.isnan(number_value) else f64_bits(number_value)
        if value.tag is Tag.BOOLEAN:
            if not isinstance(value.payload, bool):
                raise TypeError("boolean payload must be bool")
            payload = int(value.payload)
        elif value.tag in (Tag.UNDEFINED, Tag.NULL):
            payload = 0
        elif value.tag in (Tag.STRING, Tag.BIGINT, Tag.OBJECT, Tag.SYMBOL):
            payload = self.heap.intern(value)
        else:
            raise AssertionError(value.tag)
        return (NAN_TAGS[value.tag] << TAG_SHIFT) | payload

    def decode(self, word: int) -> JSValue:
        if isinstance(word, bool) or not isinstance(word, int) or not 0 <= word < (1 << 64):
            raise ValueError("NaN-box word must be an unsigned 64-bit integer")
        tag = REVERSE_NAN_TAGS.get(word >> TAG_SHIFT)
        payload = word & PAYLOAD_MASK
        if tag is Tag.UNDEFINED:
            if payload != 0:
                raise ValueError("undefined tag has non-zero payload")
            return UNDEFINED
        if tag is Tag.NULL:
            if payload != 0:
                raise ValueError("null tag has non-zero payload")
            return NULL
        if tag is Tag.BOOLEAN:
            if payload not in (0, 1):
                raise ValueError("boolean tag payload must be 0 or 1")
            return boolean(bool(payload))
        if tag is not None:
            return self.heap.resolve(payload)
        decoded = bits_f64(word)
        if math.isnan(decoded) and word != CANONICAL_NAN:
            raise ValueError("non-canonical NaN-box word")
        return number(decoded)


@dataclass(frozen=True)
class RuntimeLayout:
    name: str
    bytes_per_value: int


LAYOUTS = (
    RuntimeLayout("tagged-16", 16),
    RuntimeLayout("nanbox-8", 8),
)

REPRESENTATION_BYTES = {
    "boxed": None,
    "i1": 1,
    "i32": 4,
    "u32": 4,
    "f64": 8,
    "string_ref": 8,
    "symbol_ref": 8,
    "bigint_ref": 8,
    "object_ref": 8,
    "array_ref": 8,
    "function_ref": 8,
    "exception_ref": 8,
}

DYNAMIC_HELPER_OPS = {
    "js.add", "js.get_property", "js.set_property", "js.get_element",
    "js.set_element", "js.call", "js.construct", "js.to_number",
    "js.to_string", "host.call", "async.await",
}
SPECIALIZABLE_OPS = {"js.strict_equal", "js.to_boolean", "js.typeof"}
ALWAYS_HELPER_OPS = {"env.load", "env.store", "closure.create", "js.throw"}


def _value_definitions(function: dict[str, Any]) -> list[dict[str, str]]:
    values: list[dict[str, str]] = []

    def add(item: dict[str, Any], origin: str) -> None:
        values.append({
            "id": item["id"],
            "semantic_type": item["semantic_type"],
            "representation": item["representation"],
            "origin": origin,
        })

    for parameter in function["parameters"]:
        add(parameter, "function_parameter")
    for block in function["blocks"]:
        for parameter in block["parameters"]:
            if parameter["kind"] == "value":
                add(parameter, "block_parameter")
        if block.get("exception_parameter"):
            add(block["exception_parameter"], "exception_parameter")
        for instruction in block["instructions"]:
            if instruction.get("result"):
                add(instruction["result"], "instruction_result")

    identities = [item["id"] for item in values]
    if len(identities) != len(set(identities)):
        raise ValueError(f"duplicate SSA definition in function {function['id']}")
    return values


def _normal_edges(block: dict[str, Any]) -> list[dict[str, Any]]:
    terminator = block["terminator"]
    if terminator["kind"] == "jump":
        return [terminator]
    if terminator["kind"] == "branch":
        return [terminator["then"], terminator["else"]]
    return []


def _all_edges(function: dict[str, Any]) -> list[tuple[str, str, list[str], str]]:
    edges = []
    for block in function["blocks"]:
        for edge in _normal_edges(block):
            edges.append((block["id"], edge["target"], edge["arguments"], "normal"))
        for instruction in block["instructions"]:
            exception = instruction.get("exception")
            if exception and exception["kind"] == "edge":
                edges.append((block["id"], exception["target"], exception["arguments"], "exception"))
    return edges


def _dominators(function: dict[str, Any], edges: list[tuple[str, str, list[str], str]]) -> dict[str, set[str]]:
    block_ids = [block["id"] for block in function["blocks"]]
    entry = function["entry_block"]
    predecessors = {block_id: set() for block_id in block_ids}
    for source, target, _, _ in edges:
        predecessors[target].add(source)
    all_blocks = set(block_ids)
    dominators = {
        block_id: ({block_id} if block_id == entry else set(all_blocks))
        for block_id in block_ids
    }
    changed = True
    while changed:
        changed = False
        for block_id in block_ids:
            if block_id == entry:
                continue
            preds = predecessors[block_id]
            common = set.intersection(*(dominators[pred] for pred in preds)) if preds else set()
            updated = {block_id} | common
            if updated != dominators[block_id]:
                dominators[block_id] = updated
                changed = True
    return dominators


def _size_of(rep: str, boxed_bytes: int) -> int:
    if rep == "boxed":
        return boxed_bytes
    size = REPRESENTATION_BYTES.get(rep)
    if size is None:
        raise ValueError(f"unsupported physical representation in proof model: {rep}")
    return size


def _peak_liveness(
    function: dict[str, Any],
    value_representations: dict[str, str],
    boxed_bytes: int,
) -> tuple[int, int]:
    blocks = {block["id"]: block for block in function["blocks"]}
    formal_values = {
        block_id: {
            parameter["id"]
            for parameter in block["parameters"]
            if parameter["kind"] == "value"
        } | ({block["exception_parameter"]["id"]} if block.get("exception_parameter") else set())
        for block_id, block in blocks.items()
    }
    live_after_parameters = {block_id: set() for block_id in blocks}
    peak_count = 0
    peak_bytes = 0

    def observe(values: set[str]) -> None:
        nonlocal peak_count, peak_bytes
        actual = values.intersection(value_representations)
        peak_count = max(peak_count, len(actual))
        peak_bytes = max(
            peak_bytes,
            sum(_size_of(value_representations[value_id], boxed_bytes) for value_id in actual),
        )

    def edge_values(target: str, arguments: list[str]) -> set[str]:
        target_live = live_after_parameters[target]
        target_block = blocks[target]
        mapped: set[str] = set()
        params = target_block["parameters"]
        for parameter, argument in zip(params, arguments):
            if parameter["kind"] == "value" and parameter["id"] in target_live:
                if argument in value_representations:
                    mapped.add(argument)
        # Values defined outside the target block remain live through the edge.
        local_defs = formal_values[target]
        mapped.update(
            value_id for value_id in target_live
            if value_id not in local_defs and value_id in value_representations
        )
        return mapped

    def transfer(block: dict[str, Any]) -> tuple[set[str], set[str]]:
        live: set[str] = set()
        terminator = block["terminator"]
        for edge in _normal_edges(block):
            live.update(edge_values(edge["target"], edge["arguments"]))
        if terminator["kind"] == "branch":
            live.add(terminator["condition"])
        elif terminator["kind"] == "return" and terminator.get("value") is not None:
            live.add(terminator["value"])
        observe(live)

        for instruction in reversed(block["instructions"]):
            exception = instruction.get("exception")
            if exception and exception["kind"] == "edge":
                live.update(edge_values(exception["target"], exception["arguments"]))
            result = instruction.get("result")
            if result:
                live.discard(result["id"])
            live.update(instruction["operands"])
            observe(live)

        after_parameters = live.intersection(value_representations)
        external_before_parameters = after_parameters - formal_values[block["id"]]
        return after_parameters, external_before_parameters

    changed = True
    while changed:
        changed = False
        for block in reversed(list(blocks.values())):
            after_parameters, _ = transfer(block)
            if after_parameters != live_after_parameters[block["id"]]:
                live_after_parameters[block["id"]] = after_parameters
                changed = True

    peak_count = 0
    peak_bytes = 0
    for block in blocks.values():
        _, _ = transfer(block)
    return peak_count, peak_bytes


def _workload_labels(function: dict[str, Any], definitions: list[dict[str, str]], back_edges: int) -> set[str]:
    semantic = {item["semantic_type"] for item in definitions}
    reps = {item["representation"] for item in definitions}
    operations = {
        instruction["op"]
        for block in function["blocks"]
        for instruction in block["instructions"]
    }
    labels: set[str] = set()
    if "number" in semantic:
        labels.add("numeric")
    if "string" in semantic or "string_ref" in reps:
        labels.add("string")
    if "object" in semantic and "js.strict_equal" in operations:
        labels.add("object_identity")
    if "dynamic" in semantic and any(rep != "boxed" for rep in reps):
        labels.add("mixed_dynamic")
    if operations.intersection({"js.call", "js.construct", "host.call"}):
        labels.add("call_heavy")
    if any(p["kind"] == "value" for b in function["blocks"] for p in b["parameters"]):
        labels.add("phi_block_arguments")
    if back_edges:
        labels.add("loops")
    return labels


def _candidate_metrics(function: dict[str, Any], boxed_bytes: int, strategy: str) -> dict[str, Any]:
    definitions = _value_definitions(function)
    source_reps = {item["id"]: item["representation"] for item in definitions}
    if strategy == "boxed":
        reps = {value_id: "boxed" for value_id in source_reps}
    elif strategy == "representation-aware":
        reps = source_reps
    else:
        raise ValueError(f"unknown strategy {strategy}")

    box_sites: list[str] = []
    unbox_sites: list[str] = []
    helper_sites: list[str] = []
    box_categories: Counter[str] = Counter()
    operation_counts: Counter[str] = Counter()

    if strategy == "representation-aware":
        for parameter in function["parameters"]:
            if parameter["representation"] != "boxed":
                unbox_sites.append(f"parameter:{parameter['id']}")
        signature = function.get("return")
        if signature and signature["representation"] != "boxed":
            box_sites.append("return")
            box_categories["return_boundary"] += 1

    for block in function["blocks"]:
        for instruction in block["instructions"]:
            op = instruction["op"]
            operation_counts[op] += 1
            operand_reps = [reps[operand] for operand in instruction["operands"] if operand in reps]

            if op in DYNAMIC_HELPER_OPS:
                helper_sites.append(instruction["id"])
                if strategy == "representation-aware":
                    for operand in instruction["operands"]:
                        rep = reps.get(operand)
                        if rep and rep != "boxed":
                            box_sites.append(f"{instruction['id']}:{operand}")
                            box_categories["dynamic_semantic_operands"] += 1
            elif op in SPECIALIZABLE_OPS:
                if any(rep == "boxed" for rep in operand_reps):
                    helper_sites.append(instruction["id"])
            elif op in ALWAYS_HELPER_OPS:
                helper_sites.append(instruction["id"])
                if strategy == "representation-aware" and op == "js.throw":
                    for operand in instruction["operands"]:
                        rep = reps.get(operand)
                        if rep and rep != "boxed":
                            box_sites.append(f"{instruction['id']}:{operand}")
                            box_categories["exception_operands"] += 1

    edges = _all_edges(function)
    dominators = _dominators(function, edges)
    back_edges = sum(1 for source, target, _, _ in edges if target in dominators[source])
    block_value_params = sum(
        1 for block in function["blocks"]
        for parameter in block["parameters"]
        if parameter["kind"] == "value"
    )
    edge_value_arguments = sum(
        1 for _, _, arguments, _ in edges
        for argument in arguments
        if argument in reps
    )
    environment = list(function["environment"]["bindings"]) + list(function["captures"])
    ssa_payload = sum(_size_of(reps[item["id"]], boxed_bytes) for item in definitions)
    environment_payload = sum(
        _size_of("boxed" if strategy == "boxed" else item["representation"], boxed_bytes)
        for item in environment
    )
    conversion_temporary_payload = (
        len(box_sites) * boxed_bytes
        + sum(_size_of(source_reps[site.split(":", 1)[1]], boxed_bytes) for site in unbox_sites)
    )
    peak_live_values, peak_live_bytes = _peak_liveness(function, reps, boxed_bytes)
    instruction_count = sum(len(block["instructions"]) for block in function["blocks"])
    conversion_count = len(box_sites) + len(unbox_sites)

    return {
        "strategy": strategy,
        "ssa_value_definition_count": len(definitions),
        "function_parameter_count": len(function["parameters"]),
        "block_value_parameter_count": block_value_params,
        "phi_value_edge_argument_count": edge_value_arguments,
        "normal_and_exception_edge_count": len(edges),
        "loop_back_edge_count": back_edges,
        "instruction_count": instruction_count,
        "operation_counts": dict(sorted(operation_counts.items())),
        "helper_call_sites": len(helper_sites),
        "helper_sites": helper_sites,
        "box_conversion_count": len(box_sites),
        "unbox_conversion_count": len(unbox_sites),
        "representation_conversion_count": conversion_count,
        "box_conversion_sites": box_sites,
        "unbox_conversion_sites": unbox_sites,
        "conversion_breakdown": {
            "function_parameters": {
                "box": 0,
                "unbox": len(unbox_sites),
            },
            "returns": {
                "box": box_categories["return_boundary"],
                "unbox": 0,
            },
            "lexical_environment_locals": {
                "box": 0,
                "unbox": 0,
                "slot_count": len(environment),
                "reason": "env.load/env.store must preserve the declared binding representation",
            },
            "dynamic_semantic_operands": {
                "box": box_categories["dynamic_semantic_operands"],
                "unbox": 0,
            },
            "exception_operands": {
                "box": box_categories["exception_operands"],
                "unbox": 0,
            },
            "phi_block_arguments": {
                "box": 0,
                "unbox": 0,
                "edge_argument_count": edge_value_arguments,
                "reason": "Core IR verifier requires matching type and representation on CFG edges",
            },
            "loop_carried_edges": {
                "box": 0,
                "unbox": 0,
                "back_edge_count": back_edges,
                "reason": "loop-carried block arguments retain the same verified representation",
            },
        },
        "peak_live_ssa_values": peak_live_values,
        "peak_live_ssa_payload_bytes": peak_live_bytes,
        "ssa_slot_payload_bytes": ssa_payload,
        "environment_slot_count": len(environment),
        "environment_slot_payload_bytes": environment_payload,
        "conversion_temporary_payload_bytes": conversion_temporary_payload,
        "logical_payload_estimate_bytes": ssa_payload + environment_payload + conversion_temporary_payload,
        "lowered_instruction_count_proxy": instruction_count + conversion_count + len(helper_sites),
    }


def _aggregate(fixtures: list[dict[str, Any]]) -> dict[str, Any]:
    sum_fields = (
        "ssa_value_definition_count", "function_parameter_count",
        "block_value_parameter_count", "phi_value_edge_argument_count",
        "normal_and_exception_edge_count", "loop_back_edge_count",
        "instruction_count", "helper_call_sites", "box_conversion_count",
        "unbox_conversion_count", "representation_conversion_count",
        "ssa_slot_payload_bytes", "environment_slot_count",
        "environment_slot_payload_bytes", "conversion_temporary_payload_bytes",
        "logical_payload_estimate_bytes", "lowered_instruction_count_proxy",
    )
    result = {field: sum(fixture[field] for fixture in fixtures) for field in sum_fields}
    result["fixture_count"] = len(fixtures)
    result["peak_live_ssa_values"] = max(f["peak_live_ssa_values"] for f in fixtures)
    result["peak_live_ssa_payload_bytes"] = max(f["peak_live_ssa_payload_bytes"] for f in fixtures)
    ops: Counter[str] = Counter()
    for fixture in fixtures:
        ops.update(fixture["operation_counts"])
    result["operation_counts"] = dict(sorted(ops.items()))
    return result


def analyze_corpus(corpus: Corpus) -> dict[str, Any]:
    prepared = []
    coverage: dict[str, list[str]] = {
        name: [] for name in (
            "numeric", "string", "object_identity", "mixed_dynamic",
            "call_heavy", "phi_block_arguments", "loops",
        )
    }
    for fixture in corpus.fixtures:
        function = fixture.document["function"]
        definitions = _value_definitions(function)
        edges = _all_edges(function)
        dom = _dominators(function, edges)
        back_edges = sum(1 for source, target, _, _ in edges if target in dom[source])
        labels = _workload_labels(function, definitions, back_edges)
        for label in labels:
            coverage[label].append(fixture.path)
        prepared.append((fixture, function, definitions, labels))

    matrix = []
    for layout in LAYOUTS:
        for strategy in ("boxed", "representation-aware"):
            per_fixture = []
            for fixture, function, definitions, labels in prepared:
                metrics = _candidate_metrics(function, layout.bytes_per_value, strategy)
                metrics["fixture"] = fixture.path
                metrics["fixture_sha256"] = fixture.sha256
                metrics["workload_labels"] = sorted(labels)
                metrics["source_value_representations"] = dict(
                    sorted((item["id"], item["representation"]) for item in definitions)
                )
                per_fixture.append(metrics)
            matrix.append({
                "candidate_id": f"{layout.name}+{strategy}-ssa",
                "runtime_value_layout": layout.name,
                "bytes_per_boxed_value": layout.bytes_per_value,
                "ssa_strategy": strategy,
                "corpus_sha256": corpus.corpus_sha256,
                "per_fixture": per_fixture,
                "aggregate": _aggregate(per_fixture),
            })

    return {
        "model": "OGM-17 canonical Core IR fixture-driven structural proof",
        "corpus": {
            "manifest": corpus.manifest,
            "manifest_sha256": corpus.manifest_sha256,
            "corpus_sha256": corpus.corpus_sha256,
            "accepted_fixture_count": len(corpus.fixtures),
            "fixtures": [
                {"path": fixture.path, "sha256": fixture.sha256}
                for fixture in corpus.fixtures
            ],
            "workload_coverage": {key: sorted(paths) for key, paths in coverage.items()},
        },
        "runtime_layout_estimate": {
            "value_count": 100000,
            "layouts": {
                layout.name: {
                    "bytes_per_value": layout.bytes_per_value,
                    "payload_bytes_for_100k": 100000 * layout.bytes_per_value,
                }
                for layout in LAYOUTS
            },
            "payload_difference_bytes_for_100k": 800000,
            "note": "Arithmetic payload estimate only; excludes heap headers, alignment, allocator and GC metadata.",
        },
        "candidate_matrix": matrix,
        "metric_contract": {
            "derived_from_core_ir": [
                "SSA definitions, function/block parameters and edge arguments",
                "normal/exception edges and CFG loop back-edges",
                "modeled boxing/unboxing at dynamic semantic and public ABI boundaries",
                "generic helper-call sites versus representation-specialized operations",
                "peak live SSA values and abstract live payload bytes",
                "logical SSA/environment slot footprint and conversion temporaries",
            ],
            "proxies_not_native_metrics": [
                "lowered instruction count is a compiler structural proxy, not machine-code size",
                "peak live SSA values are not physical register pressure or spill count",
                "logical payload bytes are not allocation size, peak RSS or GC heap usage",
            ],
        },
        "not_measured": {
            "native_object_code_size_bytes": "No Ogma Rust runtime or native AOT code generator exists in the repository.",
            "native_throughput": "No executable Ogma backend exists; Python model timings are not runtime throughput.",
            "physical_register_pressure_and_spills": "No native lowering or register allocator exists.",
            "allocator_gc_costs_and_peak_rss": "No Ogma heap, allocator or garbage collector exists.",
            "observable_candidate_differential_execution": "No interpreter or AOT backend exists to execute the same input programs.",
        },
        "recommendation": (
            "Keep the public Runtime Value ABI explicitly tagged and architecture-neutral. "
            "Preserve representation-aware SSA as the compiler direction and box only at "
            "dynamic semantic and external ABI boundaries. Keep NaN-boxing unselected until "
            "Rust heap, GC, ABI and native benchmark evidence exists."
        ),
    }
