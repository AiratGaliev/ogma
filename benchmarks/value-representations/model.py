#!/usr/bin/env python3
"""Executable semantic and cost model for OGM-17 Value/SSA representation candidates.

This is a research model, not a native runtime implementation. NaN-box layouts are
modeled as 64-bit words with synthetic heap handles; Python object storage and timings
must not be interpreted as Rust runtime performance.
"""
from __future__ import annotations

from dataclasses import dataclass
from enum import Enum
import math
import struct
from typing import Any, Iterable


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


def js_string(value: str) -> JSValue:
    return JSValue(Tag.STRING, value)


def bigint(value: int) -> JSValue:
    return JSValue(Tag.BIGINT, int(value))


def object_value(identity: int) -> JSValue:
    return JSValue(Tag.OBJECT, identity)


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
        # ECMAScript String(BigInt) has no trailing "n".
        return str(value.payload)
    if value.tag is Tag.STRING:
        return value.payload
    if value.tag is Tag.OBJECT:
        return "[object Object]"
    if value.tag is Tag.SYMBOL:
        raise TypeError("Cannot convert a Symbol value to a string")
    raise AssertionError(value.tag)


def js_add(left: JSValue, right: JSValue) -> JSValue:
    # This deliberately models only primitive values in the benchmark corpus.
    # BigInt arithmetic cannot mix with Number, while String concatenation can
    # stringify either primitive after ToPrimitive.
    if (left.tag is Tag.BIGINT and right.tag is Tag.NUMBER) or (right.tag is Tag.BIGINT and left.tag is Tag.NUMBER):
        raise TypeError("Cannot mix BigInt and other types, use explicit conversions")
    if left.tag is Tag.BIGINT and right.tag is Tag.BIGINT:
        return bigint(left.payload + right.payload)
    if left.tag is Tag.STRING or right.tag is Tag.STRING:
        return js_string(to_js_string(left) + to_js_string(right))
    if left.tag is Tag.OBJECT or right.tag is Tag.OBJECT:
        raise NotImplementedError("object ToPrimitive is outside this M0 fixture")
    return number(to_number(left) + to_number(right))


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


def strict_equal(left: JSValue, right: JSValue) -> bool:
    if left.tag is not right.tag:
        return False
    if left.tag is Tag.NUMBER:
        a, b = float(left.payload), float(right.payload)
        return not (math.isnan(a) or math.isnan(b)) and a == b
    if left.tag is Tag.OBJECT:
        return left.payload == right.payload
    return left.payload == right.payload


# NaN-box reserved words use a quiet-NaN prefix. Heap values are synthetic handles;
# production pointer canonicalization, address-space constraints and GC are not modeled.
NAN_PREFIX = 0x7FF8000000000000
TAG_SHIFT = 48
PAYLOAD_MASK = (1 << 48) - 1
NAN_TAGS = {
    Tag.UNDEFINED: 0x7FF9,
    Tag.NULL: 0x7FFA,
    Tag.BOOLEAN: 0x7FFB,
    Tag.STRING: 0x7FFC,
    Tag.BIGINT: 0x7FFD,
    Tag.OBJECT: 0x7FFE,
    Tag.SYMBOL: 0x7FFF,
}


def f64_bits(value: float) -> int:
    return struct.unpack("<Q", struct.pack("<d", value))[0]


def bits_f64(bits: int) -> float:
    return struct.unpack("<d", struct.pack("<Q", bits))[0]


def encode_nan_box(value: JSValue) -> int:
    if value.tag is Tag.NUMBER:
        raw = f64_bits(float(value.payload))
        # Canonicalize NaNs so they cannot collide with reserved tag space.
        return 0x7FF8000000000000 if math.isnan(float(value.payload)) else raw
    if value.tag is Tag.BOOLEAN:
        payload = 1 if value.payload else 0
    elif value.tag is Tag.UNDEFINED or value.tag is Tag.NULL:
        payload = 0
    elif value.tag in (Tag.STRING, Tag.BIGINT, Tag.OBJECT, Tag.SYMBOL):
        payload = int(value.payload) if value.tag is not Tag.STRING else hash(value.payload) & PAYLOAD_MASK
    else:
        raise AssertionError(value.tag)
    return (NAN_TAGS[value.tag] << 48) | (payload & PAYLOAD_MASK)


def decode_nan_box(word: int) -> JSValue:
    top = word >> 48
    for tag, encoded in NAN_TAGS.items():
        if top == encoded:
            payload = word & PAYLOAD_MASK
            if tag is Tag.UNDEFINED:
                return UNDEFINED
            if tag is Tag.NULL:
                return NULL
            if tag is Tag.BOOLEAN:
                return JSValue(Tag.BOOLEAN, bool(payload))
            # Heap/string handles need a real heap table; retain an opaque identity.
            return JSValue(tag, payload)
    return number(bits_f64(word))


@dataclass(frozen=True)
class Layout:
    name: str
    bytes: int
    boxed: bool


TAGGED_LAYOUT = Layout("tagged-16", 16, True)
NANBOX_LAYOUT = Layout("nanbox-8", 8, True)


class Rep(str, Enum):
    BOXED = "boxed"
    F64 = "f64"
    I32 = "i32"
    REF = "ref"


@dataclass(frozen=True)
class ValueFact:
    semantic_type: str
    representation: Rep


@dataclass(frozen=True)
class LoweringCost:
    box: int = 0
    unbox: int = 0
    conversions: int = 0
    helper_calls: int = 0
    peak_live_values: int = 0


def lower_edge(source: ValueFact, target: ValueFact) -> LoweringCost:
    if source.representation is target.representation:
        return LoweringCost()
    if source.representation is Rep.BOXED:
        return LoweringCost(unbox=1, conversions=1, helper_calls=1 if target.representation is Rep.REF else 0)
    if target.representation is Rep.BOXED:
        return LoweringCost(box=1, conversions=1)
    # Numeric widening or numeric/reference boundary.
    return LoweringCost(conversions=1, helper_calls=1 if Rep.REF in (source.representation, target.representation) else 0)


def sum_cost(costs: Iterable[LoweringCost]) -> LoweringCost:
    return LoweringCost(
        box=sum(c.box for c in costs),
        unbox=sum(c.unbox for c in costs),
        conversions=sum(c.conversions for c in costs),
        helper_calls=sum(c.helper_calls for c in costs),
        peak_live_values=max((c.peak_live_values for c in costs), default=0),
    )


WORKLOADS = {
    "numeric": [("f64", "f64", "f64"), ("i32", "i32", "i32"), ("f64", "i32", "f64")],
    "string": [("ref", "ref", "ref"), ("boxed", "ref", "ref")],
    "object_identity": [("ref", "ref", "i32"), ("ref", "ref", "ref")],
    "mixed_dynamic": [("boxed", "f64", "boxed"), ("boxed", "ref", "boxed"), ("i32", "boxed", "boxed")],
    "call_heavy": [("f64", "f64", "boxed"), ("ref", "boxed", "ref"), ("boxed", "boxed", "boxed")],
    "phi_and_loops": [("f64", "i32", "f64"), ("ref", "ref", "boxed"), ("boxed", "f64", "boxed")],
}


def representation(name: str) -> Rep:
    return Rep(name)


def workload_cost(name: str, strategy: str) -> LoweringCost:
    rows = WORKLOADS[name]
    costs: list[LoweringCost] = []
    for left, right, result in rows:
        if strategy == "boxed":
            costs.extend([
                lower_edge(ValueFact(left, representation(left)), ValueFact("dynamic", Rep.BOXED)),
                lower_edge(ValueFact(right, representation(right)), ValueFact("dynamic", Rep.BOXED)),
                lower_edge(ValueFact("dynamic", Rep.BOXED), ValueFact(result, representation(result))),
            ])
        elif strategy == "representation-aware":
            # Operations remain in the narrowest proven representation; box only
            # at dynamic call/return/ABI boundaries represented by "boxed" operands.
            for item in (left, right):
                if item == "boxed":
                    costs.append(LoweringCost())
            if result == "boxed":
                costs.append(LoweringCost(box=1, conversions=1))
        else:
            raise ValueError(strategy)
    return sum_cost(costs)


def layout_report(value_count: int = 100_000) -> dict[str, dict[str, int]]:
    return {
        layout.name: {
            "bytes_per_value": layout.bytes,
            "payload_bytes_for_100k": layout.bytes * value_count,
            "payload_bytes_saved_vs_tagged": (TAGGED_LAYOUT.bytes - layout.bytes) * value_count,
        }
        for layout in (TAGGED_LAYOUT, NANBOX_LAYOUT)
    }


def report() -> dict[str, Any]:
    return {
        "model": "OGM-17 semantic/layout/SSA structural proof model; not native runtime benchmark",
        "runtime_layouts": layout_report(),
        "workloads": {
            name: {
                "operation_shapes": len(rows),
                "boxed_ssa": workload_cost(name, "boxed").__dict__,
                "representation_aware_ssa": workload_cost(name, "representation-aware").__dict__,
            }
            for name, rows in WORKLOADS.items()
        },
        "unmodeled": [
            "Rust allocation and pointer tagging",
            "GC/root scanning and write barriers",
            "native register allocation/spills and machine code size",
            "native throughput and cache/TLB behavior",
            "full ECMAScript ToPrimitive and object coercion",
            "actual stack maps, deoptimization and ABI integration",
        ],
    }


if __name__ == "__main__":
    import json
    print(json.dumps(report(), indent=2, sort_keys=True))
