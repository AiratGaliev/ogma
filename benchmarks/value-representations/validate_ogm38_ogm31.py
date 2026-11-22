#!/usr/bin/env python3
"""Validate OGM-38 native Value candidates against the OGM-31 portable oracle.

The candidate-specific Core IR native execution runs first. OGM-31 then supplies
an independent interpreter/native differential report from the same canonical
fixture inputs. This validator joins the two machine-readable reports and
rejects missing candidates, fixture drift, mismatched observations or
incomplete native handle/GC evidence.
"""

from __future__ import annotations

import argparse
import json
import struct
from pathlib import Path
from typing import Any

EXPECTED_CANDIDATES = {
    "tagged-16-boxed",
    "tagged-16-representation-aware",
    "nanbox-8-boxed",
    "nanbox-8-representation-aware",
}
EXPECTED_FIXTURE_PATHS = {
    "positive-ssa-branch": "tests/core-ir/fixtures/positive-ssa-branch.json",
    "positive-loop-carried-values": "tests/core-ir/fixtures/positive-loop-carried-values.json",
    "positive-dynamic-exception": "tests/core-ir/fixtures/positive-dynamic-exception.json",
    "positive-call-boundary": "tests/core-ir/fixtures/positive-call-boundary.json",
    "positive-host-call": "tests/core-ir/fixtures/positive-host-call.json",
}
EXPECTED_FIXTURES = {"branch", "loop", "dynamic_exception", "call_boundary", "host_call"}
STRICT_EQUALITY_CASES = {
    "nan-strict-equality": False,
    "signed-zero-strict-equality": True,
    "object-identity-same": True,
    "object-identity-distinct": False,
    "symbol-identity-same": True,
    "symbol-identity-distinct": False,
    "undefined-strict-equality": True,
    "null-strict-equality": True,
    "undefined-null-strict-equality": False,
    "boolean-strict-equality-same": True,
    "boolean-strict-equality-distinct": False,
    "positive-infinity-strict-equality": True,
    "negative-infinity-strict-equality": True,
    "opposite-infinity-strict-equality": False,
    "string-strict-equality-same": True,
    "string-strict-equality-distinct": False,
    "bigint-strict-equality-same": True,
    "bigint-strict-equality-distinct": False,
}
EXPECTED_SEMANTIC_CASES = {
    "number-add",
    "string-concat",
    "bigint-add",
    "object-to-primitive-add",
    "symbol-add-throws",
    "bigint-to-number-throws",
    "nan-strict-equality",
    "signed-zero-strict-equality",
    "object-identity-same",
    "object-identity-distinct",
    "symbol-identity-same",
    "symbol-identity-distinct",
    "undefined-strict-equality",
    "null-strict-equality",
    "undefined-null-strict-equality",
    "boolean-strict-equality-same",
    "boolean-strict-equality-distinct",
    "positive-infinity-strict-equality",
    "negative-infinity-strict-equality",
    "opposite-infinity-strict-equality",
    "string-strict-equality-same",
    "string-strict-equality-distinct",
    "bigint-strict-equality-same",
    "bigint-strict-equality-distinct",
    "type-transition-number-to-string",
    "exception-passthrough",
    "call-boundary",
    "host-call-generic-add",
    "host-call-echo",
    "host-call-reentrant",
    "host-call-throw",
}

NATIVE_RUNTIME_ADD_CASES = {
    "number-add",
    "string-concat",
    "bigint-add",
    "object-to-primitive-add",
    "symbol-add-throws",
}

NATIVE_TO_ORACLE_FIELDS = {
    "branch_equal_result_bits": ("branch_equal", 7.0),
    "branch_unequal_result_bits": ("branch_unequal", 8.0),
    "loop_integer_result_bits": ("loop_integer", 5.0),
    "loop_fractional_result_bits": ("loop_fractional", 3.5),
    "dynamic_add_result_bits": ("dynamic_add_normal", 15.0),
}


def read_json(path: Path) -> dict[str, Any]:
    try:
        document = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise RuntimeError(f"cannot read JSON report {path}: {error}") from error
    if not isinstance(document, dict):
        raise RuntimeError(f"report root must be an object: {path}")
    return document


def bits_to_number(bits: int) -> float:
    return struct.unpack(">d", int(bits).to_bytes(8, "big", signed=False))[0]


def number_to_bits(value: int | float) -> int:
    return int.from_bytes(struct.pack(">d", float(value)), "big", signed=False)


def expect_number(observation: Any, *, name: str) -> float:
    if not isinstance(observation, dict) or "return" not in observation:
        raise RuntimeError(f"OGM-31 {name} observation has no Normal return")
    value = observation["return"]
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise RuntimeError(f"OGM-31 {name} return is not numeric: {value!r}")
    return float(value)



def validate_semantic_case_evidence(
    semantic: Any,
    *,
    candidate: str,
) -> list[dict[str, Any]]:
    """Validate every serialized semantic result, not only the aggregate flag."""
    if not isinstance(semantic, dict) or semantic.get("every_case_matches") is not True:
        raise RuntimeError(f"{candidate}: native Value semantic matrix did not pass")

    cases = semantic.get("cases")
    if not isinstance(cases, list):
        raise RuntimeError(f"{candidate}: native Value semantic case list is malformed")
    if any(not isinstance(item, dict) or not isinstance(item.get("name"), str) for item in cases):
        raise RuntimeError(f"{candidate}: native Value semantic case list is malformed")

    names = [item["name"] for item in cases]
    duplicates = sorted({name for name in names if names.count(name) > 1})
    if duplicates:
        raise RuntimeError(f"{candidate}: semantic case names are duplicated: {duplicates}")
    if len(cases) != len(EXPECTED_SEMANTIC_CASES):
        raise RuntimeError(
            f"{candidate}: expected exactly {len(EXPECTED_SEMANTIC_CASES)} semantic cases"
        )
    if set(names) != EXPECTED_SEMANTIC_CASES:
        missing = sorted(EXPECTED_SEMANTIC_CASES - set(names))
        unexpected = sorted(set(names) - EXPECTED_SEMANTIC_CASES)
        raise RuntimeError(
            f"{candidate}: semantic case set differs (missing={missing}, unexpected={unexpected})"
        )

    native_add_code = semantic.get("native_semantic_add_code")
    if (
        not isinstance(native_add_code, dict)
        or native_add_code.get("symbol") != "ogma_value_semantic_add"
        or not isinstance(native_add_code.get("call_instructions"), int)
        or isinstance(native_add_code.get("call_instructions"), bool)
        or native_add_code["call_instructions"] < 1
    ):
        raise RuntimeError(f"{candidate}: generated native js.add entry point has no measured call instruction")

    for item in cases:
        case_name = item["name"]
        expected_native_add = case_name in NATIVE_RUNTIME_ADD_CASES
        actual_native_add = item.get("native_runtime_add_executed")
        native_add_calls = item.get("native_runtime_add_calls")
        if (
            actual_native_add is not expected_native_add
            or not isinstance(native_add_calls, int)
            or isinstance(native_add_calls, bool)
            or native_add_calls != (1 if expected_native_add else 0)
        ):
            raise RuntimeError(
                f"{candidate}: {case_name} native Runtime js.add evidence is missing or unexpected"
            )

        expected_completion = item.get("expected_completion")
        actual_completion = item.get("actual_completion")
        if not isinstance(expected_completion, str) or not expected_completion:
            raise RuntimeError(f"{candidate}: {case_name} expected completion is missing")
        if actual_completion != expected_completion:
            raise RuntimeError(
                f"{candidate}: {case_name} actual completion differs from expected completion"
            )

        expected_trace = item.get("expected_host_trace")
        actual_trace = item.get("actual_host_trace")
        if not isinstance(expected_trace, list) or actual_trace != expected_trace:
            raise RuntimeError(
                f"{candidate}: {case_name} actual host trace differs from expected host trace"
            )

        callbacks = item.get("native_callbacks")
        collections = item.get("forced_collections")
        if not isinstance(callbacks, int) or isinstance(callbacks, bool) or callbacks <= 0:
            raise RuntimeError(f"{candidate}: {case_name} native callback count is invalid")
        if not isinstance(collections, int) or isinstance(collections, bool) or collections < 0:
            raise RuntimeError(f"{candidate}: {case_name} forced collection count is invalid")
        if item.get("matches") is not True:
            raise RuntimeError(f"{candidate}: {case_name} semantic case did not match")
        if item.get("native_reference_identity_preserved") is not True:
            raise RuntimeError(f"{candidate}: {case_name} did not preserve reference identity")
        if item.get("native_store_load_transition") is not True:
            raise RuntimeError(f"{candidate}: {case_name} native Value transition did not pass")

    return cases


def validate_reports(native: dict[str, Any], dual: dict[str, Any]) -> dict[str, Any]:
    if native.get("task") != "OGM-38" or native.get("schema_version") != 1:
        raise RuntimeError("expected ogm38 native Value/SSA report schema version 1")
    if dual.get("task") != "OGM-31" or dual.get("schema") != "ogma.dual-backend-proof-report":
        raise RuntimeError("expected canonical OGM-31 dual-backend report")

    differential = dual.get("differential_observations")
    if not isinstance(differential, dict) or differential.get("all_equal") is not True:
        raise RuntimeError("OGM-31 did not prove equal portable/Cranelift/LLVM observations")
    source_fixtures = dual.get("source_fixtures", [])
    fixture_ids = {
        item.get("id") for item in source_fixtures if isinstance(item, dict)
    }
    if len(source_fixtures) != len(EXPECTED_FIXTURES) or fixture_ids != EXPECTED_FIXTURES:
        raise RuntimeError(
            f"OGM-31 fixture set drifted: expected {sorted(EXPECTED_FIXTURES)}, got {sorted(fixture_ids)}"
        )

    oracle = differential.get("portable_register")
    if not isinstance(oracle, dict):
        raise RuntimeError("OGM-31 portable register oracle is missing")

    candidates = native.get("candidates")
    if not isinstance(candidates, list):
        raise RuntimeError("OGM-38 candidate reports are missing")
    candidate_names = {item.get("name") for item in candidates}
    if candidate_names != EXPECTED_CANDIDATES or len(candidates) != 4:
        raise RuntimeError(
            f"OGM-38 candidate matrix differs: expected {sorted(EXPECTED_CANDIDATES)}, "
            f"got {sorted(candidate_names)}"
        )

    checks: list[dict[str, Any]] = []
    for candidate in candidates:
        name = candidate["name"]
        execution = candidate.get("native_core_ir_execution")
        if not isinstance(execution, dict) or execution.get("all_native_fixture_cases_match_expected") is not True:
            raise RuntimeError(f"{name}: native Core IR fixture execution did not pass")

        compared = []
        for field, (oracle_case, documented_expected) in NATIVE_TO_ORACLE_FIELDS.items():
            actual_bits = execution.get(field)
            if not isinstance(actual_bits, int) or isinstance(actual_bits, bool):
                raise RuntimeError(f"{name}: native fixture field {field} is missing or invalid")
            actual = bits_to_number(actual_bits)
            oracle_value = expect_number(oracle.get(oracle_case), name=oracle_case)
            expected_bits = number_to_bits(documented_expected)
            if actual_bits != expected_bits or actual != oracle_value:
                raise RuntimeError(
                    f"{name}: {field}={actual!r} (0x{actual_bits:016x}) does not match "
                    f"OGM-31 {oracle_case}={oracle_value!r} / expected {documented_expected!r}"
                )
            compared.append(
                {
                    "native_field": field,
                    "ogm31_case": oracle_case,
                    "result": actual,
                    "result_bits": f"0x{actual_bits:016x}",
                    "matches_ogm31": True,
                }
            )

        throw_observation = oracle.get("dynamic_add_throw")
        throw_name = throw_observation.get("throw") if isinstance(throw_observation, dict) else None
        if not isinstance(throw_name, str) or not throw_name.startswith("TypeError:"):
            raise RuntimeError("OGM-31 Symbol + Number reference did not throw TypeError")
        if execution.get("dynamic_add_throw_status") != 2:
            raise RuntimeError(f"{name}: native dynamic-add did not reject Symbol with TypeError")
        if execution.get("symbol_handle_survived_forced_collection") is not True:
            raise RuntimeError(f"{name}: Symbol HostHandle did not survive forced collection")

        semantic_case_items = validate_semantic_case_evidence(
            candidate.get("native_value_semantics"),
            candidate=name,
        )
        semantic_cases = {item["name"]: item for item in semantic_case_items}
        for case_name, expected_equal in STRICT_EQUALITY_CASES.items():
            equality_case = semantic_cases.get(case_name)
            if (
                not isinstance(equality_case, dict)
                or equality_case.get("matches") is not True
                or equality_case.get("native_strict_equal_executed") is not True
                or equality_case.get("native_reference_equality_result") is not expected_equal
            ):
                raise RuntimeError(
                    f"{name}: generated native strict equality for {case_name} "
                    f"did not return {expected_equal}"
                )
            if case_name in {
                "object-identity-same",
                "object-identity-distinct",
                "symbol-identity-same",
                "symbol-identity-distinct",
            } and equality_case.get("native_reference_identity_preserved") is not True:
                raise RuntimeError(
                    f"{name}: {case_name} did not preserve distinct/same HostHandle identity"
                )
            if case_name in {
                "string-strict-equality-same",
                "string-strict-equality-distinct",
                "bigint-strict-equality-same",
                "bigint-strict-equality-distinct",
            } and equality_case.get("native_reference_value_comparison_executed") is not True:
                raise RuntimeError(
                    f"{name}: {case_name} did not compare primitive String/BigInt contents across distinct handles"
                )
        call_boundary = semantic_cases.get("call-boundary")
        call_observation = oracle.get("call_boundary", {})
        call_trace = call_observation.get("host_trace", [])
        if (
            not isinstance(call_boundary, dict)
            or call_boundary.get("matches") is not True
            or call_boundary.get("actual_host_trace")
            != ["benchmark.host.call_target([2,\"item=5\"])"]
            or not isinstance(call_trace, list)
            or len(call_trace) != 1
            or call_trace[0].get("symbol") != "benchmark.host.call_target"
            or call_trace[0].get("args") != [2, "item=5"]
        ):
            raise RuntimeError(f"{name}: call-boundary value/callback trace differs from OGM-31")
        call_callback = call_boundary.get("native_host_callback")
        if (
            not isinstance(call_callback, dict)
            or call_callback.get("callee_resolved") is not True
            or call_callback.get("receiver") != 2.0
            or call_callback.get("argument") != "item=5"
            or call_callback.get("result_bits") != number_to_bits(8.0)
            or call_callback.get("host_callback_calls") != 1
        ):
            raise RuntimeError(f"{name}: generated native call-boundary callback evidence is incomplete")

        host_echo = semantic_cases.get("host-call-echo")
        host_observation = oracle.get("host_call", {})
        host_trace = host_observation.get("host_trace", [])
        if (
            not isinstance(host_echo, dict)
            or host_echo.get("matches") is not True
            or host_echo.get("actual_host_trace") != ["benchmark.host.echo([31])"]
            or not isinstance(host_trace, list)
            or len(host_trace) != 1
            or host_trace[0].get("symbol") != "benchmark.host.echo"
            or host_trace[0].get("args") != [31]
        ):
            raise RuntimeError(f"{name}: host-call value/trace differs from OGM-31")
        echo_callback = host_echo.get("native_host_callback")
        if (
            not isinstance(echo_callback, dict)
            or echo_callback.get("symbol") != "benchmark.host.echo"
            or echo_callback.get("arguments") != ["31"]
            or echo_callback.get("result_bits") != number_to_bits(42.0)
            or echo_callback.get("host_callback_calls") != 1
        ):
            raise RuntimeError(f"{name}: generated native host.echo callback evidence is incomplete")

        roots = candidate.get("native_shadow_roots")
        if (
            not isinstance(roots, dict)
            or roots.get("all_roots_survived") is not True
            or roots.get("all_roots_reclaimed") is not True
            or roots.get("candidate_values_survived_both_collections") is not True
            or roots.get("candidate_value_layout") not in {"tagged-16", "nanbox-8"}
        ):
            raise RuntimeError(f"{name}: OGM-27 candidate Value roots were not balanced")
        candidate_slots = roots.get("candidate_value_slots")
        candidate_handles = roots.get("candidate_reference_handles_resolved")
        candidate_tags = roots.get("candidate_reference_tags")
        if (
            not isinstance(candidate_slots, int)
            or isinstance(candidate_slots, bool)
            or candidate_slots <= 0
            or candidate_handles != candidate_slots
            or not isinstance(candidate_tags, list)
            or len(candidate_tags) != candidate_slots
            or set(candidate_tags) != {"String", "BigInt", "Object", "Symbol"}
            or roots.get("entry_safepoint") != 0
            or roots.get("call_boundary_safepoint") != 1
            or roots.get("reclaimed_after_release") != candidate_slots
        ):
            raise RuntimeError(f"{name}: candidate Value root-map scan/handle-lifetime evidence is incomplete")

        corpus = candidate.get("native_core_ir_corpus")
        if (
            not isinstance(corpus, dict)
            or corpus.get("canonical_fixture_count") != len(EXPECTED_FIXTURE_PATHS)
            or corpus.get("all_values_roundtripped_and_dispatched") is not True
        ):
            raise RuntimeError(f"{name}: candidate did not cover the OGM-31 canonical fixture manifest")
        fixture_reports = corpus.get("fixtures")
        if not isinstance(fixture_reports, list):
            raise RuntimeError(f"{name}: native Core IR fixture detail list is missing")
        actual_fixture_paths = {
            item.get("fixture"): item.get("path")
            for item in fixture_reports
            if isinstance(item, dict)
        }
        if actual_fixture_paths != EXPECTED_FIXTURE_PATHS or len(fixture_reports) != len(EXPECTED_FIXTURE_PATHS):
            raise RuntimeError(
                f"{name}: native Core IR fixture paths differ from the exact OGM-31 set"
            )
        fixture_batch_counts = [item.get("native_batch_invocations") for item in fixture_reports]
        fixture_dispatch_counts = [item.get("native_value_dispatches") for item in fixture_reports]
        fixture_reference_counts = [
            item.get("reference_values_resolved_after_forced_gc") for item in fixture_reports
        ]
        if (
            any(
                not isinstance(value, int) or isinstance(value, bool) or value != 1
                for value in fixture_batch_counts
            )
            or any(
                not isinstance(value, int) or isinstance(value, bool) or value <= 0
                for value in fixture_dispatch_counts
            )
            or any(
                not isinstance(value, int) or isinstance(value, bool) or value < 0
                for value in fixture_reference_counts
            )
        ):
            raise RuntimeError(f"{name}: native per-fixture dispatch/root counts are invalid")
        summed_dispatches = sum(fixture_dispatch_counts)
        summed_reference_resolutions = sum(fixture_reference_counts)
        if (
            corpus.get("native_batch_invocations") != sum(fixture_batch_counts)
            or corpus.get("native_batch_invocations") != len(EXPECTED_FIXTURE_PATHS)
            or corpus.get("native_value_dispatches") != summed_dispatches
            or corpus.get("reference_values_resolved_after_forced_gc") != summed_reference_resolutions
            or summed_dispatches <= 0
            or summed_reference_resolutions <= 0
        ):
            raise RuntimeError(f"{name}: native per-fixture dispatch/root counts are inconsistent")

        native_code = candidate.get("native_code")
        strict_equal_code = execution.get("strict_equal_native_code")
        for code_metrics, symbol in (
            (native_code, "ogma_value_loop"),
            (strict_equal_code, "ogma_value_strict_equal"),
        ):
            if (
                not isinstance(code_metrics, dict)
                or code_metrics.get("symbol") != symbol
                or not isinstance(code_metrics.get("numeric_conversion_instructions"), int)
                or code_metrics.get("numeric_conversion_instructions") < 0
                or not isinstance(code_metrics.get("call_instructions"), int)
                or code_metrics.get("call_instructions") < 0
            ):
                raise RuntimeError(f"{name}: measured native code metrics are missing for {symbol}")

        if strict_equal_code.get("call_instructions", 0) < 1:
            raise RuntimeError(
                f"{name}: generated native strict equality lacks its String/BigInt value-comparison callback"
            )

        for modeled_field in (
            "modeled_ir_value_conversion_ops_per_loop_iteration",
            "modeled_ir_value_conversion_ops_per_invocation",
        ):
            modeled_count = candidate.get(modeled_field)
            if not isinstance(modeled_count, int) or modeled_count < 0:
                raise RuntimeError(f"{name}: modeled IR conversion field {modeled_field} is invalid")

        checks.append(
            {
                "candidate": name,
                "native_fixture_observations": compared,
                "symbol_type_error_matches_ogm31": True,
                "native_semantic_matrix_passed": True,
                "native_strict_equality_cases_passed": len(STRICT_EQUALITY_CASES),
                "native_core_ir_fixture_dispatches": summed_dispatches,
                "measured_loop_numeric_conversion_instructions": native_code["numeric_conversion_instructions"],
                "measured_loop_call_sites": native_code["call_instructions"],
                "call_boundary_matches_ogm31": True,
                "host_echo_matches_ogm31": True,
                "ogm27_roots_balanced": True,
                "ogm31_fixture_set_matches": True,
            }
        )

    return {
        "schema": "ogma.ogm38-ogm31-differential-validation",
        "version": "1.0.0",
        "task": "OGM-38",
        "oracle_task": "OGM-31",
        "oracle_revision": dual.get("tested_revision"),
        "native_revision": native.get("tested_revision"),
        "candidate_count": len(checks),
        "fixture_ids": sorted(EXPECTED_FIXTURES),
        "all_candidates_match_portable_ogm31_oracle": True,
        "candidate_checks": checks,
        "limitations": [
            "The branch, loop and numeric dynamic-add paths are executed as generated Cranelift native functions for every Value/SSA candidate.",
            "Call-boundary and host-call cases are covered by the candidate native Value dispatcher and M0 host/ABI adapters; this is not a production ECMAScript Runtime or general Core IR lowering.",
            "The OGM-31 portable Python interpreter and native adapters are an independent M0 oracle, not a production interpreter.",
        ],
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--native-report", type=Path, required=True)
    parser.add_argument("--ogm31-report", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()

    report = validate_reports(read_json(args.native_report), read_json(args.ogm31_report))
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(json.dumps({
        "task": report["task"],
        "oracle_task": report["oracle_task"],
        "candidate_count": report["candidate_count"],
        "all_candidates_match_portable_ogm31_oracle": report["all_candidates_match_portable_ogm31_oracle"],
        "output": str(args.output),
    }, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
