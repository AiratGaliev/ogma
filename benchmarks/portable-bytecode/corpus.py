#!/usr/bin/env python3
"""Deterministic Core IR v1 workload corpus for OGM-16."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
FIXTURE_ROOT = Path(__file__).resolve().parent / "fixtures"

FALSE_EFFECTS = {
    "may_allocate": False,
    "may_gc": False,
    "may_throw": False,
    "may_suspend": False,
    "may_call_host": False,
    "may_reenter": False,
    "may_mutate_shape": False,
}

FULL_DYNAMIC_EFFECTS = {
    "may_allocate": True,
    "may_gc": True,
    "may_throw": True,
    "may_suspend": False,
    "may_call_host": True,
    "may_reenter": True,
    "may_mutate_shape": True,
}


def source() -> dict[str, Any]:
    return {
        "origin": "generated",
        "reason": "OGM-16 deterministic portable bytecode workload",
        "scope": None,
        "bindings": [],
        "suspension": None,
        "inline_chain": [],
    }


def effects(**overrides: bool) -> dict[str, bool]:
    result = dict(FALSE_EFFECTS)
    result.update(overrides)
    return result


def instruction(
    ident: str,
    op: str,
    operands: list[str],
    result: dict[str, Any] | None = None,
    *,
    attributes: dict[str, Any] | None = None,
    effect_in: str | None = None,
    effect_out: str | None = None,
    effect_flags: dict[str, bool] | None = None,
    exception: dict[str, Any] | None = None,
) -> dict[str, Any]:
    return {
        "id": ident,
        "op": op,
        "operands": operands,
        "result": result,
        "attributes": {} if attributes is None else attributes,
        "effects": dict(FALSE_EFFECTS) if effect_flags is None else effect_flags,
        "source": source(),
        "effect_in": effect_in,
        "effect_out": effect_out,
        "exception": exception,
    }


def value(ident: str, semantic_type: str, representation: str) -> dict[str, Any]:
    return {
        "id": ident,
        "kind": "value",
        "semantic_type": semantic_type,
        "representation": representation,
    }


def parameter(ident: str, binding: str, name: str, semantic_type: str, representation: str) -> dict[str, Any]:
    return {
        **value(ident, semantic_type, representation),
        "name": name,
        "binding": binding,
    }


def binding(ident: str, name: str, kind: str, semantic_type: str, representation: str) -> dict[str, Any]:
    return {
        "id": ident,
        "name": name,
        "kind": kind,
        "mutable": kind in {"var", "let", "parameter", "catch"},
        "semantic_type": semantic_type,
        "representation": representation,
    }


def effect_parameter(ident: str) -> dict[str, str]:
    return {"id": ident, "kind": "effect"}


def basic_block(
    ident: str,
    instructions: list[dict[str, Any]],
    terminator: dict[str, Any],
    effect_id: str | None = None,
) -> dict[str, Any]:
    params = [] if effect_id is None else [effect_parameter(effect_id)]
    return {
        "id": ident,
        "parameters": params,
        "exception_parameter": None,
        "instructions": instructions,
        "terminator": {**terminator, "source": source()},
    }


def function(
    ident: str,
    name: str,
    parameters: list[dict[str, Any]],
    return_type: dict[str, str] | None,
    bindings: list[dict[str, Any]],
    blocks: list[dict[str, Any]],
    *,
    effect_entry: str | None,
) -> dict[str, Any]:
    return {
        "contract": "ogma.core-ir",
        "version": "1.0.0",
        "function": {
            "id": ident,
            "name": name,
            "parameters": parameters,
            "return": return_type,
            "source_files": [],
            "environment": {
                "id": f"env_{ident}",
                "parent": None,
                "bindings": bindings,
            },
            "captures": [],
            "entry_block": blocks[0]["id"],
            "effect_entry": effect_entry,
            "blocks": blocks,
        },
    }


def const(ident: str, literal: str, payload: Any, semantic_type: str, representation: str) -> dict[str, Any]:
    return instruction(
        ident,
        "js.constant",
        [],
        value(ident + "_value", semantic_type, representation),
        attributes={"literal": literal, "value": payload},
    )


def edge(target: str, arguments: list[str] | None = None) -> dict[str, Any]:
    return {"target": target, "arguments": [] if arguments is None else arguments}


def build_workloads() -> list[dict[str, Any]]:
    workloads: list[dict[str, Any]] = []

    # Dynamic addition exercises ECMAScript coercion rather than a numeric-only ALU path.
    dynamic_add = function(
        "fn_dynamic_add",
        "dynamic_add",
        [],
        {"semantic_type": "dynamic", "representation": "boxed"},
        [],
        [
            basic_block(
                "entry",
                [
                    const("left", "string", "value ", "string", "string_ref"),
                    const("right", "number", 42, "number", "f64"),
                    instruction(
                        "dynamic_plus",
                        "js.add",
                        ["left_value", "right_value"],
                        value("sum", "dynamic", "boxed"),
                        effect_in="e0",
                        effect_out="e1",
                        effect_flags=dict(FULL_DYNAMIC_EFFECTS),
                        exception={"kind": "propagate"},
                    ),
                ],
                {"kind": "return", "value": "sum"},
                "e0",
            )
        ],
        effect_entry="e0",
    )
    workloads.append({
        "id": "dynamic-add",
        "entry": "fn_dynamic_add",
        "functions": [dynamic_add],
        "cases": [{"args": [], "expected": {"return": "value 42", "host_trace": []}}],
        "description": "Dynamic JS addition with string/number coercion",
    })

    # Branches are actual CFG edges and are executed with both conditions.
    branch = function(
        "fn_branch",
        "branch_result",
        [parameter("flag", "bind_flag", "flag", "boolean", "i1")],
        {"semantic_type": "string", "representation": "string_ref"},
        [binding("bind_flag", "flag", "parameter", "boolean", "i1")],
        [
            basic_block(
                "entry",
                [],
                {
                    "kind": "branch",
                    "condition": "flag",
                    "then": edge("when_true"),
                    "else": edge("when_false"),
                },
            ),
            basic_block(
                "when_true",
                [const("true_value", "string", "then", "string", "string_ref")],
                {"kind": "return", "value": "true_value_value"},
            ),
            basic_block(
                "when_false",
                [const("false_value", "string", "else", "string", "string_ref")],
                {"kind": "return", "value": "false_value_value"},
            ),
        ],
        effect_entry=None,
    )
    workloads.append({
        "id": "control-flow",
        "entry": "fn_branch",
        "functions": [branch],
        "cases": [
            {"args": [True], "expected": {"return": "then", "host_trace": []}},
            {"args": [False], "expected": {"return": "else", "host_trace": []}},
        ],
        "description": "Two-way CFG branch with distinct return blocks",
    })

    # Mutable lexical storage is loop-carried through the environment, not a phi shortcut.
    loop = function(
        "fn_countdown",
        "countdown",
        [parameter("initial_value", "bind_initial", "initial", "dynamic", "boxed")],
        {"semantic_type": "dynamic", "representation": "boxed"},
        [
            binding("bind_initial", "initial", "parameter", "dynamic", "boxed"),
            binding("bind_counter", "counter", "let", "dynamic", "boxed"),
        ],
        [
            basic_block(
                "entry",
                [
                    instruction(
                        "initialize_counter",
                        "env.store",
                        ["initial_value"],
                        value("stored_initial", "dynamic", "boxed"),
                        attributes={"binding": "bind_counter"},
                        effect_in="e_entry",
                        effect_out="e_entry_next",
                        effect_flags=effects(may_throw=True),
                        exception={"kind": "propagate"},
                    ),
                ],
                {"kind": "jump", "target": "condition", "arguments": ["e_entry_next"]},
                "e_entry",
            ),
            basic_block(
                "condition",
                [
                    instruction(
                        "read_counter",
                        "env.load",
                        [],
                        value("current", "dynamic", "boxed"),
                        attributes={"binding": "bind_counter"},
                        effect_in="e_condition",
                        effect_out="e_condition_next",
                        effect_flags=effects(may_throw=True),
                        exception={"kind": "propagate"},
                    ),
                    const("zero", "number", 0, "number", "f64"),
                    instruction(
                        "is_zero",
                        "js.strict_equal",
                        ["current", "zero_value"],
                        value("finished", "boolean", "i1"),
                    ),
                ],
                {
                    "kind": "branch",
                    "condition": "finished",
                    "then": edge("exit", ["e_condition_next"]),
                    "else": edge("body", ["e_condition_next"]),
                },
                "e_condition",
            ),
            basic_block(
                "body",
                [
                    const("minus_one", "number", -1, "number", "f64"),
                    instruction(
                        "decrement",
                        "js.add",
                        ["current", "minus_one_value"],
                        value("next_counter", "dynamic", "boxed"),
                        effect_in="e_body",
                        effect_out="e_body_after_add",
                        effect_flags=dict(FULL_DYNAMIC_EFFECTS),
                        exception={"kind": "propagate"},
                    ),
                    instruction(
                        "write_counter",
                        "env.store",
                        ["next_counter"],
                        value("stored_next", "dynamic", "boxed"),
                        attributes={"binding": "bind_counter"},
                        effect_in="e_body_after_add",
                        effect_out="e_body_next",
                        effect_flags=effects(may_throw=True),
                        exception={"kind": "propagate"},
                    ),
                ],
                {"kind": "jump", "target": "condition", "arguments": ["e_body_next"]},
                "e_body",
            ),
            basic_block(
                "exit",
                [],
                {"kind": "return", "value": "current"},
                "e_exit",
            ),
        ],
        effect_entry="e_entry",
    )
    workloads.append({
        "id": "mutable-loop-locals",
        "entry": "fn_countdown",
        "functions": [loop],
        "cases": [{"args": [5], "expected": {"return": 0, "host_trace": []}}],
        "description": "Loop with mutable lexical binding, loads/stores and back-edge",
    })

    # Calls use closure.create + js.call with explicit thisArg and ordinary arguments.
    callee = function(
        "fn_sum",
        "sum",
        [
            parameter("left_arg", "bind_left", "left", "dynamic", "boxed"),
            parameter("right_arg", "bind_right", "right", "dynamic", "boxed"),
        ],
        {"semantic_type": "dynamic", "representation": "boxed"},
        [
            binding("bind_left", "left", "parameter", "dynamic", "boxed"),
            binding("bind_right", "right", "parameter", "dynamic", "boxed"),
        ],
        [
            basic_block(
                "entry",
                [
                    instruction(
                        "callee_add",
                        "js.add",
                        ["left_arg", "right_arg"],
                        value("callee_sum", "dynamic", "boxed"),
                        effect_in="e_callee",
                        effect_out="e_callee_next",
                        effect_flags=dict(FULL_DYNAMIC_EFFECTS),
                        exception={"kind": "propagate"},
                    )
                ],
                {"kind": "return", "value": "callee_sum"},
                "e_callee",
            )
        ],
        effect_entry="e_callee",
    )
    caller = function(
        "fn_call_sum",
        "call_sum",
        [parameter("input_value", "bind_input", "input", "dynamic", "boxed")],
        {"semantic_type": "dynamic", "representation": "boxed"},
        [binding("bind_input", "input", "parameter", "dynamic", "boxed")],
        [
            basic_block(
                "entry",
                [
                    instruction(
                        "make_sum_closure",
                        "closure.create",
                        [],
                        value("sum_closure", "function", "function_ref"),
                        attributes={"function": "fn_sum", "captures": []},
                        effect_in="e_caller",
                        effect_out="e_after_closure",
                        effect_flags=effects(may_allocate=True, may_gc=True),
                    ),
                    const("receiver", "null", None, "null", "boxed"),
                    instruction(
                        "invoke_sum",
                        "js.call",
                        ["sum_closure", "receiver_value", "input_value", "input_value"],
                        value("call_result", "dynamic", "boxed"),
                        effect_in="e_after_closure",
                        effect_out="e_after_call",
                        effect_flags=dict(FULL_DYNAMIC_EFFECTS),
                        exception={"kind": "propagate"},
                    ),
                ],
                {"kind": "return", "value": "call_result"},
                "e_caller",
            )
        ],
        effect_entry="e_caller",
    )
    workloads.append({
        "id": "function-calls",
        "entry": "fn_call_sum",
        "functions": [caller, callee],
        "cases": [{"args": [4], "expected": {"return": 8, "host_trace": []}}],
        "description": "Closure creation, explicit receiver and nested JS function calls",
    })

    # Host calls record an observable trace in a registered capability, not a native address.
    host_call = function(
        "fn_host_trace",
        "host_trace",
        [parameter("message", "bind_message", "message", "string", "string_ref")],
        {"semantic_type": "dynamic", "representation": "boxed"},
        [binding("bind_message", "message", "parameter", "string", "string_ref")],
        [
            basic_block(
                "entry",
                [
                    instruction(
                        "send_trace",
                        "host.call",
                        ["message"],
                        value("host_result", "dynamic", "boxed"),
                        attributes={"symbol": "trace"},
                        effect_in="e_host",
                        effect_out="e_host_next",
                        effect_flags=dict(FULL_DYNAMIC_EFFECTS),
                        exception={"kind": "propagate"},
                    )
                ],
                {"kind": "return", "value": "host_result"},
                "e_host",
            )
        ],
        effect_entry="e_host",
    )
    workloads.append({
        "id": "host-calls",
        "entry": "fn_host_trace",
        "functions": [host_call],
        "cases": [{
            "args": ["signal"],
            "expected": {
                "return": "signal",
                "host_trace": [{"symbol": "trace", "args": ["signal"]}],
            },
        }],
        "description": "Capability-mediated host call with observable side-effect trace",
    })

    # Exceptions are observable completion records and are not Rust panics.
    thrower = function(
        "fn_throw",
        "throws_value",
        [],
        None,
        [],
        [
            basic_block(
                "entry",
                [
                    const("error_value", "string", "expected-error", "string", "string_ref"),
                    instruction(
                        "throw_error",
                        "js.throw",
                        ["error_value_value"],
                        effect_in="e_throw",
                        effect_out="e_throw_next",
                        effect_flags=effects(may_throw=True),
                        exception={"kind": "propagate"},
                    ),
                ],
                {"kind": "unreachable", "reason": "after_throw"},
                "e_throw",
            )
        ],
        effect_entry="e_throw",
    )
    workloads.append({
        "id": "exceptions",
        "entry": "fn_throw",
        "functions": [thrower],
        "cases": [{"args": [], "expected": {"throw": "expected-error", "host_trace": []}}],
        "description": "Explicit JS throw and exceptional completion propagation",
    })

    return workloads


def write_corpus() -> dict[str, Any]:
    FIXTURE_ROOT.mkdir(parents=True, exist_ok=True)
    workloads = build_workloads()
    fixture_entries: list[dict[str, str]] = []
    public_workloads: list[dict[str, Any]] = []
    runtime_workloads: list[dict[str, Any]] = []

    for workload in workloads:
        workload_dir = FIXTURE_ROOT / workload["id"]
        workload_dir.mkdir(parents=True, exist_ok=True)
        paths: list[str] = []
        for document in workload["functions"]:
            function_id = document["function"]["id"]
            relative = Path("benchmarks/portable-bytecode/fixtures") / workload["id"] / f"{function_id}.json"
            destination = ROOT / relative
            destination.write_text(
                json.dumps(document, indent=2, sort_keys=True, ensure_ascii=False, allow_nan=False) + "\n",
                encoding="utf-8",
                newline="\n",
            )
            fixture_entries.append({"path": relative.as_posix(), "expect": "accepted"})
            paths.append(relative.as_posix())

        public_workloads.append({
            "id": workload["id"],
            "entry": workload["entry"],
            "functions": paths,
            "cases": workload["cases"],
            "description": workload["description"],
        })
        runtime_workloads.append({
            "id": workload["id"],
            "entry": workload["entry"],
            "functions": workload["functions"],
            "cases": workload["cases"],
            "description": workload["description"],
        })

    manifest = {
        "contract": "ogma.core-ir",
        "version": "1.0.0",
        "fixtures": fixture_entries,
    }
    (FIXTURE_ROOT / "manifest.json").write_text(
        json.dumps(manifest, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
        newline="\n",
    )
    workloads_doc = {
        "schema": "ogma.portable-bytecode-workloads",
        "version": "1.0.0",
        "workloads": public_workloads,
    }
    (FIXTURE_ROOT / "workloads.json").write_text(
        json.dumps(workloads_doc, indent=2, sort_keys=True, ensure_ascii=False) + "\n",
        encoding="utf-8",
        newline="\n",
    )
    return {"schema": workloads_doc["schema"], "version": workloads_doc["version"], "workloads": runtime_workloads}


if __name__ == "__main__":
    result = write_corpus()
    print(f"Generated {len(result['workloads'])} workloads from the Core IR v1 contract.")
