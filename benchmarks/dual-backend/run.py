#!/usr/bin/env python3
"""Reproducible M0 differential proof for one Core IR subset across portable and native paths."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import struct
import subprocess
import sys
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
PROOF_DIR = Path(__file__).resolve().parent
PORTABLE_DIR = ROOT / "benchmarks" / "portable-bytecode"
BACKEND_DIR = ROOT / "benchmarks" / "backend-codegen"
sys.path.insert(0, str(PORTABLE_DIR))

from corpus import build_workloads  # noqa: E402
from prototype import (  # noqa: E402
    ContainerReader,
    HostFunction,
    JSException,
    JSObject,
    JSSymbol,
    RuntimeState,
    compile_function,
    compile_workload,
    execute_case,
    execute_function,
    run_workload_correctness,
    verify_compiled,
)


def read_json(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def load_canonical_fixtures() -> tuple[dict[str, dict[str, Any]], dict[str, dict[str, Any]]]:
    manifest = read_json(PROOF_DIR / "manifest.json")
    fixtures: dict[str, dict[str, Any]] = {}
    source_hashes: dict[str, str] = {}
    for item in manifest["source_fixtures"]:
        source_path = ROOT / item["source"]
        core_ir_path = ROOT / item["core_ir"]
        if not source_path.is_file() or not core_ir_path.is_file():
            raise RuntimeError(f"missing source/IR proof input for {item['id']}")
        document = read_json(core_ir_path)
        if document.get("contract") != "ogma.core-ir" or document.get("version") != "1.0.0":
            raise RuntimeError(f"{item['id']}: expected canonical Core IR v1")
        source_files = document["function"].get("source_files", [])
        if len(source_files) != 1 or source_files[0].get("path") != item["source"]:
            raise RuntimeError(f"{item['id']}: Core IR source mapping does not match manifest")
        source_hashes[item["source"]] = sha256(source_path.read_bytes())
        fixtures[item["id"]] = document

    native_manifest = read_json(ROOT / manifest["native_backend_manifest"])
    if (
        native_manifest.get("contract") != "ogma.backend-codegen-workloads"
        or native_manifest.get("version") != "1.0.0"
        or native_manifest.get("core_ir_contract") != "ogma.core-ir/1.0.0"
    ):
        raise RuntimeError("native backend manifest is not the expected version")
    native_paths = {item["fixture"] for item in native_manifest["workloads"]}
    proof_paths = {item["core_ir"] for item in manifest["source_fixtures"]}
    if native_paths != proof_paths:
        raise RuntimeError("portable and native paths do not consume the same Core IR fixture set")
    return fixtures, source_hashes


def run_python_case(
    compiled: Any,
    arguments: list[Any],
    *,
    host_callbacks: dict[str, Any] | None = None,
) -> dict[str, Any]:
    state = RuntimeState({compiled.function_id: compiled})
    state.host_callbacks = host_callbacks or {}
    try:
        value = execute_function(compiled.function_id, arguments, state)
        outcome: dict[str, Any] = {"return": value, "host_trace": list(state.host_trace)}
    except JSException as error:
        outcome = {"throw": error.value, "host_trace": list(state.host_trace)}
    for call in state.call_trace:
        outcome["host_trace"].append({
            "symbol": "benchmark.host." + call["symbol"],
            "args": call["args"],
        })
    return outcome


def run_portable_observations(
    fixtures: dict[str, dict[str, Any]], model: str, artifact_dir: Path
) -> tuple[dict[str, Any], dict[str, Any]]:
    compiled: dict[str, Any] = {}
    bytecode_metadata: dict[str, Any] = {}
    for name, document in fixtures.items():
        candidate = compile_function(document, model)
        # compile_function verifies before returning; repeat the check at the execution
        # boundary so the proof explicitly demonstrates verifier-before-execute.
        verify_compiled(candidate)
        compiled[name] = candidate
        container_path = artifact_dir / f"{name}-{model}.ogmb"
        container_path.write_bytes(candidate.container)
        bytecode_metadata[name] = {
            "path": str(container_path),
            "sha256": sha256(candidate.container),
            "bytes": len(candidate.container),
            "encoding_id": candidate.artifact["manifest"]["encoding_id"],
            "execution_model": candidate.artifact["manifest"]["metadata"]["execution_model"],
            "verified_before_execution": True,
        }

    def call_target(receiver: Any, argument: Any) -> Any:
        if receiver != 2 or argument != "item=5":
            raise JSException("TypeError: call_target received an unexpected receiver/argument")
        return receiver + len(argument)

    shared_object = JSObject({})
    observations = {
        "branch_equal": run_python_case(compiled["branch"], [7.0, 7.0]),
        "branch_unequal": run_python_case(compiled["branch"], [7.0, 8.0]),
        "loop_integer": run_python_case(compiled["loop"], [0.0, 5.0, 1.0]),
        "loop_fractional": run_python_case(compiled["loop"], [0.5, 3.5, 0.75]),
        "dynamic_add_normal": run_python_case(compiled["dynamic_exception"], [7, 8]),
        "dynamic_add_throw": run_python_case(compiled["dynamic_exception"], [JSSymbol(), 9]),
        "call_boundary": run_python_case(
            compiled["call_boundary"],
            [HostFunction("call_target"), 2, "item=", shared_object, shared_object, 5.0],
            host_callbacks={"call_target": call_target},
        ),
        "host_call": run_python_case(compiled["host_call"], [31]),
    }
    return observations, bytecode_metadata


def unsupported_aot_operations(
    function: dict[str, Any], supported_operations: set[str]
) -> list[str]:
    """Return runtime operations not covered by the native fixture profiles.

    Core IR js.constant nodes materialize literal-pool values at compile time;
    they are not runtime operations and intentionally do not appear in the
    OGM-30 native workload operation lists.
    """
    operations: set[str] = set()
    for block in function["blocks"]:
        for instruction in block["instructions"]:
            operation = instruction["op"]
            if operation == "js.constant":
                attributes = instruction.get("attributes", {})
                effects = instruction.get("effects", {})
                if (
                    instruction.get("operands") != []
                    or not isinstance(instruction.get("result"), dict)
                    or instruction["result"].get("kind") != "value"
                    or not isinstance(attributes, dict)
                    or set(attributes) != {"literal", "value"}
                    or any(bool(value) for value in effects.values())
                ):
                    raise AssertionError("js.constant must remain a side-effect-free literal-pool value")
                continue
            operations.add(operation)
    return sorted(operations - supported_operations)


def run_property_fallback() -> dict[str, Any]:
    workload = next(item for item in build_workloads() if item["id"] == "property-access")
    candidates = {
        model: compile_workload(workload, model)
        for model in ("stack", "register")
    }
    for model, functions in candidates.items():
        for candidate in functions:
            verify_compiled(candidate)
    observations = run_workload_correctness(
        workload, candidates["stack"], candidates["register"]
    )
    actual, dispatches, allocations = execute_case(
        workload, candidates["register"], workload["cases"][0]["args"]
    )
    if actual != workload["cases"][0]["expected"]:
        raise AssertionError("portable fallback did not preserve property-access result/host trace")
    return {
        "aot_outcome": "unsupported",
        "unsupported_operations": ["js.get_property", "js.set_property"],
        "selected_fallback": "portable-register-bytecode-interpreter",
        "expected": workload["cases"][0]["expected"],
        "observed": actual,
        "dispatches": dispatches,
        "allocations": allocations,
        "stack_register_equivalence": all(
            item["stack"] == item["register"] == item["expected"] for item in observations
        ),
    }


def verify_invalid_bytecode(compiled: Any) -> dict[str, Any]:
    tampered = bytearray(compiled.container)
    code_offset = struct.unpack_from("<Q", tampered, 48 + 1 * 32 + 8)[0]
    tampered[code_offset] ^= 0x01
    reader = ContainerReader()
    artifact = reader.read(bytes(tampered))
    if artifact is not None:
        raise AssertionError("tampered .ogmb code unexpectedly passed container validation")
    return {
        "tampered_code_offset": code_offset,
        "rejected": True,
        "diagnostics": [str(item) for item in reader.errors],
    }


def validate_runtime_integration(
    backend_name: str, integration: dict[str, Any]
) -> dict[str, Any]:
    abi_version = integration.get("ogm19_version", {})
    required_features = (1 << 0) | (1 << 1) | (1 << 2) | (1 << 3) | (1 << 4) | (1 << 63)
    if abi_version.get("major") != 1 or (
        int(abi_version.get("features", 0)) & required_features
    ) != required_features:
        raise RuntimeError(f"{backend_name}: OGM-19 ABI v1 feature negotiation was incomplete")

    completions = integration.get("ogm19_completions", {})
    expected_completions = {
        "dynamic_add_normal": (0, 15),
        "dynamic_add_throw": (1, -(1 << 63)),
        "host_echo_normal": (0, 42),
        "host_echo_throw": (1, -7),
    }
    for name, (kind, payload) in expected_completions.items():
        actual = completions.get(name, {})
        if actual.get("kind") != kind or actual.get("payload") != payload:
            raise RuntimeError(
                f"{backend_name}: OGM-19 completion mismatch for {name}: {actual!r}"
            )
    if integration.get("ogm19_last_status") != 0:
        raise RuntimeError(f"{backend_name}: OGM-19 reported a failed ABI status")

    safepoints = integration.get("ogm27_native_safepoints", {})
    required_sites = {
        "function_entry", "loop_back_edge", "allocation_boundary",
        "call_boundary", "host_call_boundary",
    }
    if set(safepoints) != required_sites or any(
        int(safepoints[site]) <= 0 for site in required_sites
    ):
        raise RuntimeError(f"{backend_name}: generated code did not execute every OGM-27 safepoint")

    model = integration.get("ogm27_model", {})
    if int(model.get("callback_invocations", 0)) < 5:
        raise RuntimeError(f"{backend_name}: OGM-19 callbacks were not exercised")
    expected_callback_trace = [
        {
            "symbol": "benchmark.core_ir.dynamic_add",
            "args": [7.0, 8.0],
            "completion": {"kind": 0, "value": 15.0},
        },
        {
            "symbol": "benchmark.core_ir.dynamic_add",
            "args": [-(1 << 63), 9.0],
            "completion": {"kind": 1, "value": -(1 << 63)},
        },
        {
            "symbol": "benchmark.host.echo",
            "args": [31.0],
            "completion": {"kind": 0, "value": 42.0},
        },
        {
            "symbol": "benchmark.host.echo",
            "args": [32.0],
            "completion": {"kind": 0, "value": 43.0},
        },
        {
            "symbol": "benchmark.host.echo",
            "args": [-7.0],
            "completion": {"kind": 1, "value": -7.0},
        },
    ]
    if model.get("callback_trace") != expected_callback_trace:
        raise RuntimeError(f"{backend_name}: actual OGM-19 callback argument/completion trace differed")
    if not model.get("argument_handles_survived_collection_inside_callback"):
        raise RuntimeError(f"{backend_name}: callback argument roots did not survive collection")
    if int(model.get("normal_completions", 0)) <= 0 or int(model.get("throw_completions", 0)) <= 0:
        raise RuntimeError(f"{backend_name}: OGM-27 transition model missed a completion kind")
    expected_path = ["Native", "Interpreter", "HostCallback"]
    if expected_path not in model.get("completion_transition_paths", []):
        raise RuntimeError(f"{backend_name}: missing nested OGM-27 transition path")
    native_frame = model.get("native_shadow_frame", {})
    if (
        not native_frame.get("active_during_execution")
        or not native_frame.get("live_managed_root_survived_every_collection")
    ):
        raise RuntimeError(f"{backend_name}: OGM-27 native shadow-frame root proof failed")
    root_maps = native_frame.get("root_maps", {})
    if set(root_maps) != {"1", "2", "3", "4", "5"} or any(
        int(value) <= 0 for value in root_maps.values()
    ):
        raise RuntimeError(f"{backend_name}: not all root-map safepoints were observed")
    budget = model.get("execution_budget", {})
    if (
        int(budget.get("work_units", 0)) <= 0
        or int(budget.get("polls", 0)) < 5
        or int(budget.get("max_unpolled_work", 0)) != 64
    ):
        raise RuntimeError(f"{backend_name}: OGM-27 budget polling was not exercised")

    return {
        "ogm19_abi_v1_negotiated": True,
        "ogm19_version": abi_version,
        "ogm19_completions": completions,
        "ogm19_callback_trace": model["callback_trace"],
        "ogm19_callbacks": int(model["callback_invocations"]),
        "ogm27_native_safepoints": safepoints,
        "ogm27_model": model,
    }
def run_native_backends(
    *,
    output_path: Path,
    artifact_dir: Path,
    source_revision: str,
    iterations: int,
) -> dict[str, Any]:
    native_report_path = output_path.with_name(output_path.stem + "-native-backends.json")
    native_object_dir = artifact_dir / "native"
    command = [
        "cargo", "run",
        "--manifest-path", str(BACKEND_DIR / "Cargo.toml"),
        "--locked", "--release", "--", 
        "--iterations", str(iterations),
        "--warmups", "3",
        "--samples", "101",
        "--revision", source_revision,
        "--output", str(native_report_path),
        "--artifact-dir", str(native_object_dir),
    ]
    completed = subprocess.run(
        command, cwd=ROOT, text=True, capture_output=True, check=False
    )
    if completed.returncode != 0:
        raise RuntimeError(
            "native backend proof failed\n"
            f"command: {' '.join(command)}\n"
            f"stdout:\n{completed.stdout[-12000:]}\n"
            f"stderr:\n{completed.stderr[-12000:]}"
        )
    native_report = read_json(native_report_path)
    if native_report.get("task") != "OGM-30":
        raise RuntimeError("unexpected native backend report task")
    expected_target = read_json(PROOF_DIR / "manifest.json")["target"]["triple"]
    if native_report.get("native_target_triple") != expected_target:
        raise RuntimeError(
            f"expected target {expected_target}, got {native_report.get('native_target_triple')}"
        )

    backends: dict[str, Any] = {}
    for key in ("cranelift", "llvm_backend"):
        item = native_report[key]
        object_path = item.get("native_object_path")
        if not object_path or not Path(object_path).is_file() or Path(object_path).stat().st_size == 0:
            raise RuntimeError(f"{key}: missing target-native object artifact")
        if Path(object_path).read_bytes()[:4] != bytes.fromhex("7f454c46"):
            raise RuntimeError(f"{key}: target object is not an ELF relocatable object")
        helper_calls = item["runtime_helper_calls"]
        if not helper_calls or any(int(value) <= 0 for value in helper_calls.values()):
            raise RuntimeError(f"{key}: one or more expected runtime helper calls were not observed")
        runtime_integration = validate_runtime_integration(
            key, item.get("runtime_integration", {})
        )
        backends["llvm" if key == "llvm_backend" else key] = {
            "native_object": {
                "path": object_path,
                "bytes": Path(object_path).stat().st_size,
                "format": "ELF relocatable object (.o)",
            },
            "compile": item["compile_time_ns"],
            "link": item["link_time_ns"],
            "semantic_observations": item["semantic_observations"],
            "runtime_helper_calls": helper_calls,
            "runtime_integration": runtime_integration,
            "workloads": item["workloads"],
        }

    return {
        "target_triple": native_report["native_target_triple"],
        "source_revision": native_report["source_revision"],
        "tested_revision": native_report["tested_revision"],
        "backends": backends,
    }


def build_report(
    *,
    output_path: Path,
    artifact_dir: Path,
    iterations: int,
    source_revision: str,
) -> dict[str, Any]:
    manifest = read_json(PROOF_DIR / "manifest.json")
    fixtures, source_hashes = load_canonical_fixtures()
    portable_dir = artifact_dir / "portable"
    portable_dir.mkdir(parents=True, exist_ok=True)

    register_observations, bytecode_artifacts = run_portable_observations(
        fixtures, "register", portable_dir
    )
    stack_observations, stack_artifacts = run_portable_observations(
        fixtures, "stack", portable_dir
    )
    if register_observations != stack_observations:
        raise AssertionError("portable stack/register interpreters disagree")

    native = run_native_backends(
        output_path=output_path,
        artifact_dir=artifact_dir,
        source_revision=source_revision,
        iterations=iterations,
    )
    for backend_name, backend in native["backends"].items():
        if backend["semantic_observations"] != register_observations:
            raise AssertionError(
                f"{backend_name} native AOT observations differ from portable register interpreter"
            )

    property_workload = next(item for item in build_workloads() if item["id"] == "property-access")
    aot_manifest = read_json(ROOT / manifest["native_backend_manifest"])
    supported_operations = {
        operation
        for workload in aot_manifest["workloads"]
        for operation in workload["operations"]
    }
    unsupported_operations = unsupported_aot_operations(
        property_workload["functions"][0]["function"],
        supported_operations,
    )
    if unsupported_operations != ["js.get_property", "js.set_property"]:
        raise AssertionError(f"unexpected AOT unsupported-operation set: {unsupported_operations}")
    fallback = run_property_fallback()
    if fallback["unsupported_operations"] != unsupported_operations:
        raise AssertionError("documented AOT fallback does not match backend support manifest")

    branch_compiled = compile_function(fixtures["branch"], "register")
    invalid_bytecode = verify_invalid_bytecode(branch_compiled)
    helper_evidence = {
        name: backend["runtime_helper_calls"]
        for name, backend in native["backends"].items()
    }
    if register_observations["dynamic_add_throw"] != {
        "throw": "TypeError: Cannot convert a Symbol value to a number",
        "host_trace": [],
    }:
        raise AssertionError("portable exception completion differs from the M0 expected TypeError")

    acceptance = {
        "same_core_ir_inputs_for_both_execution_paths": True,
        "versioned_bytecode_verifier_before_execution": True,
        "target_specific_native_objects_separate_from_ogmb": True,
        "differential_values_exceptions_side_effects_and_host_calls": True,
        "invalid_bytecode_unsupported_aot_and_runtime_helper_fallback": True,
        "ogm19_versioned_abi_callbacks_and_explicit_completions": all(
            backend["runtime_integration"]["ogm19_abi_v1_negotiated"]
            for backend in native["backends"].values()
        ),
        "ogm27_generated_safepoints_roots_transitions_and_budget": all(
            backend["runtime_integration"]["ogm27_native_safepoints"]
            and backend["runtime_integration"]["ogm27_model"]["native_shadow_frame"]["live_managed_root_survived_every_collection"]
            for backend in native["backends"].values()
        ),
        "reproducible_documented_linux_x86_64_target": True,
        "unsupported_subset_and_m0_non_production_scope_documented": True,
    }
    return {
        "schema": "ogma.dual-backend-proof-report",
        "version": "1.0.0",
        "task": "OGM-31",
        "source_revision": source_revision,
        "tested_revision": native["tested_revision"],
        "generated_at_utc": __import__("datetime").datetime.now(
            __import__("datetime").timezone.utc
        ).isoformat(),
        "environment": {
            "python": sys.version.split()[0],
            "platform": platform.platform(),
            "machine": platform.machine(),
        },
        "target": native["target_triple"],
        "core_ir_contract": manifest["core_ir_contract"],
        "portable_execution_model": manifest["portable_execution_model"],
        "source_fixtures": [
            {
                **item,
                "source_sha256": source_hashes[item["source"]],
                "core_ir_sha256": sha256((ROOT / item["core_ir"]).read_bytes()),
            }
            for item in manifest["source_fixtures"]
        ],
        "portable_bytecode": {
            "register_artifacts": bytecode_artifacts,
            "stack_artifacts": stack_artifacts,
            "verifier_invoked_before_each_execution": True,
            "stack_register_observations_equal": register_observations == stack_observations,
        },
        "native_backends": native["backends"],
        "differential_observations": {
            "portable_register": register_observations,
            "portable_stack": stack_observations,
            "cranelift": native["backends"]["cranelift"]["semantic_observations"],
            "llvm": native["backends"]["llvm"]["semantic_observations"],
            "all_equal": all(
                backend["semantic_observations"] == register_observations
                for backend in native["backends"].values()
            ),
        },
        "runtime_helper_fallback": helper_evidence,
        "native_runtime_integration": {
            name: backend["runtime_integration"]
            for name, backend in native["backends"].items()
        },
        "unsupported_aot_fallback": fallback,
        "negative_cases": {
            "tampered_ogmb": invalid_bytecode,
            "unsupported_aot_operations_fail_closed": {
                "operations": unsupported_operations,
                "fallback_selected": "portable-register-bytecode-interpreter",
                "verified": True,
            },
        },
        "acceptance": acceptance,
        "limitations": [
            "This is an M0 architecture proof, not a production JavaScript/TypeScript engine or Runtime.",
            "The source files are traceable reference inputs paired with canonical pre-authored Core IR v1 fixtures; no source parser or source-to-Core-IR compiler is implemented here.",
            "The native AOT adapters are exact-fixture profile lowerers. Dynamic-add and host calls route through the existing OGM-19 mock C ABI function table, opaque handles, root scopes, registered callbacks and explicit completions; this is still a mock ABI/Runtime proof, not a production Runtime.",
            "The OGM-19 mock value store supports Number, Boolean and Function only. The native Symbol-conversion failure is represented at this boundary by the reserved i64::MIN numeric sentinel, not by a production JS Symbol Value. The string/object call-boundary fixture still uses a dedicated exact-fixture callback shim.",
            "Generated Cranelift and LLVM objects call modeled OGM-27 safepoints for function entry, loop back-edge, allocation, call and host-call boundaries. The proof exercises the OGM-27 shadow-frame root map, activation/completion, persistent-promotion and budget model; it is not a production stack-map encoder or production collector.",
            "The exception fixture's handler only rethrows the same value. Portable lowering maps that narrow case to direct propagation and rejects handlers with additional effects; general catch/finally lowering is not implemented.",
            "Property access, proxies, accessors, prototypes, suspension, general calls, arbitrary source programs and broad ECMAScript conformance are outside the supported AOT subset.",
        ],
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--artifact-dir", type=Path, required=True)
    parser.add_argument("--iterations", type=int, default=1000)
    args = parser.parse_args()
    if args.iterations < 1:
        parser.error("--iterations must be positive")
    source_revision = (
        os.environ.get("OGM31_SOURCE_SHA")
        or os.environ.get("GITHUB_SHA")
        or "local-working-tree"
    )
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.artifact_dir.mkdir(parents=True, exist_ok=True)
    report = build_report(
        output_path=args.output,
        artifact_dir=args.artifact_dir,
        iterations=args.iterations,
        source_revision=source_revision,
    )
    args.output.write_text(
        json.dumps(report, indent=2, sort_keys=True, ensure_ascii=False, allow_nan=False) + "\n",
        encoding="utf-8",
    )
    summary = {
        "task": report["task"],
        "target": report["target"],
        "all_differential_observations_equal": report["differential_observations"]["all_equal"],
        "native_backends": sorted(report["native_backends"]),
        "portable_ogmb_artifacts": len(report["portable_bytecode"]["register_artifacts"]),
        "unsupported_operations_fallback": report["unsupported_aot_fallback"]["unsupported_operations"],
        "acceptance": report["acceptance"],
        "report": str(args.output),
    }
    print(json.dumps(summary, indent=2, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
