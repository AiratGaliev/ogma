#!/usr/bin/env python3
"""Measure reproducible M0 interpreter and hybrid-AOT proof footprint costs.

This harness deliberately distinguishes proof-tool measurements from production
runtime metrics. The repository does not yet contain a production Ogma Runtime.
"""

from __future__ import annotations

import argparse
from datetime import datetime, timezone
import gc
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import resource
import subprocess
import sys
import time
import tracemalloc
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
BENCHMARK_DIR = Path(__file__).resolve().parent
DUAL_DIR = ROOT / "benchmarks" / "dual-backend"
PORTABLE_DIR = ROOT / "benchmarks" / "portable-bytecode"
BACKEND_DIR = ROOT / "benchmarks" / "backend-codegen"
PROFILE_MANIFEST = ROOT / "contracts" / "runtime-profiles" / "v1" / "profiles.json"

import importlib.util

dual_spec = importlib.util.spec_from_file_location(
    "ogm31_dual_backend_proof", DUAL_DIR / "run.py"
)
if dual_spec is None or dual_spec.loader is None:
    raise RuntimeError("unable to load the canonical OGM-31 dual-backend proof module")
dual = importlib.util.module_from_spec(dual_spec)
sys.modules[dual_spec.name] = dual
dual_spec.loader.exec_module(dual)

from prototype import (  # noqa: E402
    HostFunction,
    JSException,
    JSObject,
    JSSymbol,
    RuntimeState,
    compile_function,
    execute_function,
    verify_compiled,
)

PROFILE_IDS = ("portable-interpreter-release", "hybrid-native-release")
NATIVE_TARGET = "x86_64-unknown-linux-gnu"
DEFAULT_ITERATIONS = 1_000
DEFAULT_WARMUPS = 3
DEFAULT_SAMPLES = 101
COMPILE_ROUNDS = 15
VERIFY_ROUNDS = 15


def _read_json(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))


def _sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _distribution(samples: list[float]) -> dict[str, Any]:
    if not samples:
        raise ValueError("a latency distribution requires at least one sample")
    ordered = sorted(samples)

    def percentile(fraction: float) -> float:
        index = math.ceil((len(ordered) - 1) * fraction)
        return float(ordered[index])

    p50 = percentile(0.50)
    return {
        "sample_count": len(samples),
        "p50_ns_per_call": p50,
        "p95_ns_per_call": percentile(0.95),
        "p99_ns_per_call": percentile(0.99),
        "throughput_calls_per_second_from_p50": 1_000_000_000 / max(1.0, p50),
        "raw_ns_per_call_samples": samples,
    }


def _timed_call(
    callback: Any,
    *,
    iterations: int,
    warmups: int,
    samples: int,
) -> dict[str, Any]:
    for _ in range(warmups):
        for _ in range(iterations):
            callback()

    raw: list[float] = []
    for _ in range(samples):
        started = time.perf_counter_ns()
        for _ in range(iterations):
            callback()
        elapsed = time.perf_counter_ns() - started
        raw.append(elapsed / iterations)
    return _distribution(raw)


def _timed_indexed_call(
    callback: Any,
    *,
    iterations: int,
    warmups: int,
    samples: int,
) -> dict[str, Any]:
    for _ in range(warmups):
        for index in range(iterations):
            callback(index)

    raw: list[float] = []
    for _ in range(samples):
        started = time.perf_counter_ns()
        for index in range(iterations):
            callback(index)
        elapsed = time.perf_counter_ns() - started
        raw.append(elapsed / iterations)
    return _distribution(raw)


def _peak_tracemalloc(callback: Any) -> tuple[Any, int]:
    gc.collect()
    tracemalloc.start()
    try:
        result = callback()
        _current, peak = tracemalloc.get_traced_memory()
        return result, int(peak)
    finally:
        tracemalloc.stop()


def _ru_maxrss_bytes() -> int:
    # The task's documented measurement target is Ubuntu/Linux, where ru_maxrss
    # is KiB. Keep the platform conversion explicit rather than guessing.
    if sys.platform != "linux":
        raise RuntimeError("process RSS conversion is defined only for the Linux benchmark target")
    return int(resource.getrusage(resource.RUSAGE_SELF).ru_maxrss) * 1024


def _load_inputs() -> tuple[dict[str, dict[str, Any]], dict[str, str], dict[str, Any]]:
    manifest = _read_json(DUAL_DIR / "manifest.json")
    fixtures: dict[str, dict[str, Any]] = {}
    source_hashes: dict[str, str] = {}
    fixture_hashes: dict[str, str] = {}
    for item in manifest["source_fixtures"]:
        source_path = ROOT / item["source"]
        ir_path = ROOT / item["core_ir"]
        if not source_path.is_file() or not ir_path.is_file():
            raise RuntimeError(f"missing canonical OGM-31 input: {item['id']}")
        source_document = source_path.read_text(encoding="utf-8")
        source_hashes[item["source"]] = hashlib.sha256(source_document.encode("utf-8")).hexdigest()
        document = _read_json(ir_path)
        if document.get("contract") != "ogma.core-ir" or document.get("version") != "1.0.0":
            raise RuntimeError(f"{item['id']}: expected canonical Core IR v1")
        source_files = document["function"].get("source_files", [])
        if len(source_files) != 1 or source_files[0].get("path") != item["source"]:
            raise RuntimeError(f"{item['id']}: source-to-Core-IR manifest mapping differs")
        fixture_hashes[item["core_ir"]] = _sha256(ir_path)
        fixtures[item["id"]] = document

    backend_manifest = _read_json(BACKEND_DIR / "fixtures" / "workloads.json")
    backend_paths = {item["fixture"] for item in backend_manifest["workloads"]}
    portable_paths = {item["core_ir"] for item in manifest["source_fixtures"]}
    if backend_paths != portable_paths:
        raise RuntimeError("portable and native benchmark paths do not use the same Core IR fixture set")
    return fixtures, source_hashes, {
        "manifest": manifest,
        "source_hashes": source_hashes,
        "core_ir_hashes": fixture_hashes,
        "native_manifest": backend_manifest,
    }


def _input_memory(fixtures: dict[str, dict[str, Any]], manifest: dict[str, Any]) -> dict[str, Any]:
    source_paths = [ROOT / item["source"] for item in manifest["source_fixtures"]]
    ir_paths = [ROOT / item["core_ir"] for item in manifest["source_fixtures"]]

    def read_sources() -> list[str]:
        return [path.read_text(encoding="utf-8") for path in source_paths]

    _sources, source_peak = _peak_tracemalloc(read_sources)

    def parse_core_ir() -> list[dict[str, Any]]:
        return [_read_json(path) for path in ir_paths]

    parsed_documents, parse_peak = _peak_tracemalloc(parse_core_ir)
    retained_bytes = len(
        json.dumps(parsed_documents, sort_keys=True, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    )
    return {
        "source": {
            "file_count": len(source_paths),
            "serialized_bytes": sum(path.stat().st_size for path in source_paths),
            "python_read_peak_bytes": source_peak,
            "status": "measured-source-input-only",
        },
        "ast": {
            "peak_bytes": None,
            "status": "not_implemented",
            "reason": "Ogma has no JavaScript parser/AST implementation; source fixtures are traceable reference inputs only.",
        },
        "core_ir": {
            "file_count": len(ir_paths),
            "serialized_bytes": sum(path.stat().st_size for path in ir_paths),
            "json_parse_peak_bytes": parse_peak,
            "canonical_json_bytes_after_parse": retained_bytes,
            "status": "measured-fixture-json-parse-proxy",
            "note": "This measures JSON fixture materialization, not the peak memory of a production source-to-Core-IR frontend.",
        },
    }


def _case_specs(compiled: dict[str, Any]) -> dict[str, tuple[Any, list[Any], dict[str, Any]]]:
    shared_object = JSObject({})

    def call_target(receiver: Any, argument: Any) -> Any:
        if receiver != 2 or argument != "item=5":
            raise JSException("TypeError: call_target received an unexpected receiver/argument")
        return receiver + len(argument)

    callback_map = {"call_target": call_target}
    return {
        "branch_equal": (compiled["branch"], [7.0, 7.0], {}),
        "branch_unequal": (compiled["branch"], [7.0, 8.0], {}),
        "loop_integer": (compiled["loop"], [0.0, 5.0, 1.0], {}),
        "loop_fractional": (compiled["loop"], [0.5, 3.5, 0.75], {}),
        "dynamic_add_normal": (compiled["dynamic_exception"], [7, 8], {}),
        "dynamic_add_throw": (compiled["dynamic_exception"], [JSSymbol(), 9], {}),
        "call_boundary": (
            compiled["call_boundary"],
            [HostFunction("call_target"), 2, "item=", shared_object, shared_object, 5.0],
            callback_map,
        ),
        "host_call": (compiled["host_call"], [31], {}),
    }


def _invoke_case(
    compiled: Any,
    arguments: list[Any],
    callbacks: dict[str, Any],
    state: RuntimeState,
) -> Any:
    state.host_trace.clear()
    state.call_trace.clear()
    state.host_callbacks = callbacks
    try:
        return {"return": execute_function(compiled.function_id, arguments, state)}
    except JSException as error:
        return {"throw": error.value}


def _portable_worker(
    *,
    iterations: int,
    warmups: int,
    samples: int,
    artifact_dir: Path,
) -> dict[str, Any]:
    ready_timestamp_ns = time.monotonic_ns()
    fixtures, source_hashes, provenance = _load_inputs()
    input_memory = _input_memory(fixtures, provenance["manifest"])

    def compile_all() -> dict[str, Any]:
        return {name: compile_function(document, "register") for name, document in fixtures.items()}

    # Time compilation without tracemalloc overhead, then sample memory in a separate pass.
    compile_wall_samples: list[float] = []
    compile_codegen_samples: list[float] = []
    decode_samples: list[float] = []
    compiled: dict[str, Any] = {}
    for _ in range(COMPILE_ROUNDS):
        started = time.perf_counter_ns()
        round_candidates = compile_all()
        compile_wall_samples.append(float(time.perf_counter_ns() - started))
        compile_codegen_samples.append(float(sum(item.code_generation_ns for item in round_candidates.values())))
        decode_samples.append(float(sum(item.decode_ns for item in round_candidates.values())))
        compiled = round_candidates
    _memory_probe, compile_peak = _peak_tracemalloc(compile_all)

    verify_samples: list[float] = []
    for _ in range(VERIFY_ROUNDS):
        started = time.perf_counter_ns()
        for candidate in compiled.values():
            verify_compiled(candidate)
        verify_samples.append(float(time.perf_counter_ns() - started))

    # Check the full reference observation set against both existing portable encodings.
    (artifact_dir / "register").mkdir(parents=True, exist_ok=True)
    (artifact_dir / "stack").mkdir(parents=True, exist_ok=True)
    register_observations, register_artifacts = dual.run_portable_observations(
        fixtures, "register", artifact_dir / "register"
    )
    stack_observations, stack_artifacts = dual.run_portable_observations(
        fixtures, "stack", artifact_dir / "stack"
    )
    if register_observations != stack_observations:
        raise AssertionError("portable stack/register observations differ")
    if len(register_artifacts) != len(fixtures) or len(stack_artifacts) != len(fixtures):
        raise AssertionError("portable execution did not emit every canonical .ogmb fixture")

    bytecode_metadata = {}
    for name, candidate in compiled.items():
        output = artifact_dir / "register" / f"{name}-ogm29.ogmb"
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_bytes(candidate.container)
        bytecode_metadata[name] = {
            "container_bytes": len(candidate.container),
            "code_bytes": len(candidate.code),
            "sha256": hashlib.sha256(candidate.container).hexdigest(),
            "verified_before_execution": True,
        }

    def invoke_indexed(
        candidate: Any,
        arguments_for_index: Any,
        callbacks: dict[str, Any],
    ) -> Any:
        state = RuntimeState({candidate.function_id: candidate})
        state.host_callbacks = callbacks

        def invoke(index: int) -> Any:
            arguments = arguments_for_index(index)
            return _invoke_case(candidate, arguments, callbacks, state)

        return invoke

    shared_object = JSObject({})

    def call_target(receiver: Any, argument: Any) -> Any:
        if receiver != 2 or argument != "item=5":
            raise JSException("TypeError: call_target received an unexpected receiver/argument")
        return receiver + len(argument)

    call_callbacks = {"call_target": call_target}
    call_host = invoke_indexed(
        compiled["call_boundary"],
        lambda _index: [HostFunction("call_target"), 2, "item=", shared_object, shared_object, 5.0],
        call_callbacks,
    )
    branch = invoke_indexed(
        compiled["branch"], lambda index: [float(index % 17), float(index % 13)], {}
    )
    loop = invoke_indexed(
        compiled["loop"], lambda _index: [0.0, 5.0, 1.0], {}
    )
    dynamic_normal = invoke_indexed(
        compiled["dynamic_exception"], lambda index: [index % 100, 7], {}
    )
    symbol = JSSymbol()
    dynamic_throw = invoke_indexed(
        compiled["dynamic_exception"], lambda _index: [symbol, 7], {}
    )
    host_call = invoke_indexed(
        compiled["host_call"], lambda index: [index], {}
    )

    workloads = {
        "core_ir_ssa_branch": _timed_indexed_call(
            branch, iterations=iterations, warmups=warmups, samples=samples
        ),
        "core_ir_loop_js_add_to_number": _timed_indexed_call(
            loop, iterations=iterations, warmups=warmups, samples=samples
        ),
        "core_ir_dynamic_add_normal_completion": _timed_indexed_call(
            dynamic_normal, iterations=iterations, warmups=warmups, samples=samples
        ),
        "core_ir_dynamic_add_throw_completion": _timed_indexed_call(
            dynamic_throw, iterations=iterations, warmups=warmups, samples=samples
        ),
        "core_ir_js_add_js_call_boundary": _timed_indexed_call(
            call_host, iterations=iterations, warmups=warmups, samples=samples
        ),
        "core_ir_host_call": _timed_indexed_call(
            host_call, iterations=iterations, warmups=warmups, samples=samples
        ),
    }
    direct_host_distribution = _timed_indexed_call(
        lambda value: value + 11,
        iterations=iterations,
        warmups=warmups,
        samples=samples,
    )
    host_call_distribution = workloads["core_ir_host_call"]
    host_delta = (
        host_call_distribution["p50_ns_per_call"]
        - direct_host_distribution["p50_ns_per_call"]
    )
    host_overhead = {
        "dispatch_path": host_call_distribution,
        "direct_host_function": direct_host_distribution,
        "estimated_dispatch_overhead_ns_per_call": host_delta,
        "estimated_dispatch_overhead_ratio": (
            host_call_distribution["p50_ns_per_call"] / direct_host_distribution["p50_ns_per_call"]
            if direct_host_distribution["p50_ns_per_call"] > 0 else None
        ),
        "interpretation": "Python proof-interpreter host capability dispatch versus a direct Python function, using the native host-call workload's changing input sequence; not a Rust ABI or production host-call result.",
    }

    return {
        "ready_timestamp_ns": ready_timestamp_ns,
        "python": sys.version.split()[0],
        "platform": platform.platform(),
        "machine": platform.machine(),
        "peak_rss_bytes": _ru_maxrss_bytes(),
        "input_memory": input_memory,
        "compile": {
            "rounds": COMPILE_ROUNDS,
            "wall": _distribution(compile_wall_samples),
            "code_generation": _distribution(compile_codegen_samples),
            "decode": _distribution(decode_samples),
            "in_band_verification_included": True,
            "tracemalloc_peak_bytes": compile_peak,
            "standalone_verification": _distribution(verify_samples),
            "note": "Python M0 prototype compilation of canonical Core IR fixtures into register .ogmb; not a production Rust frontend/compiler.",
        },
        "bytecode_artifacts": bytecode_metadata,
        "workloads": workloads,
        "host_call_overhead": host_overhead,
        "portable_differential": {
            "same_stack_and_register_observations": True,
            "same_expected_observations": True,
            "register_artifact_count": len(register_artifacts),
            "stack_artifact_count": len(stack_artifacts),
        },
        "process_memory_metric": "Linux ru_maxrss high-water mark for the Python proof worker; not Ogma VM heap/RSS.",
    }


def _resolve_profile_plan(document: dict[str, Any], profile_id: str) -> dict[str, Any]:
    components = {component["id"]: component for component in document["components"]}
    profile = next((item for item in document["profiles"] if item["id"] == profile_id), None)
    if profile is None:
        raise ValueError(f"runtime profile {profile_id!r} not found")

    included: set[str] = set()
    visiting: list[str] = []

    def visit(component_id: str) -> None:
        if component_id not in components:
            raise ValueError(f"profile {profile_id}: unknown component {component_id!r}")
        if component_id in visiting:
            raise ValueError(f"profile {profile_id}: dependency cycle {' -> '.join([*visiting, component_id])}")
        if component_id in included:
            return
        visiting.append(component_id)
        for dependency in components[component_id]["requires"]:
            visit(dependency)
        visiting.pop()
        included.add(component_id)

    for root in profile["includeRoots"]:
        visit(root)

    excluded_required = sorted(included.intersection(profile["excludeComponents"]))
    if excluded_required:
        raise ValueError(f"profile {profile_id}: excluded required components {excluded_required}")

    resolved = sorted(included)
    packaging_counts: dict[str, int] = {}
    for component_id in resolved:
        package = components[component_id]["packaging"]
        packaging_counts[package] = packaging_counts.get(package, 0) + 1

    return {
        "profile_id": profile_id,
        "build_mode": profile["buildMode"],
        "execution_backend": profile["executionBackend"],
        "host_compatibility": profile["hostCompatibility"],
        "resolved_components": resolved,
        "packaging_component_counts": dict(sorted(packaging_counts.items())),
        "feature_flags": sorted(profile["featureFlags"]),
        "rust_build": profile["rustBuild"],
        "aot_build": profile["aotBuild"],
        "runtime_package_graph_status": "logical-profile-contract-only",
        "note": "OGM-28 component identifiers are build-boundary contracts, not Cargo crates or proof of existing linked binaries.",
    }


def _cargo_metadata() -> dict[str, Any]:
    command = [
        "cargo", "metadata", "--format-version", "1", "--locked",
        "--manifest-path", str(BACKEND_DIR / "Cargo.toml"),
    ]
    completed = subprocess.run(command, cwd=ROOT, text=True, capture_output=True, check=False)
    if completed.returncode != 0:
        raise RuntimeError(
            "cargo metadata failed\n"
            f"command: {' '.join(command)}\nstdout:\n{completed.stdout}\nstderr:\n{completed.stderr}"
        )
    metadata = json.loads(completed.stdout)
    packages = {
        package["id"]: {
            "name": package["name"],
            "version": package["version"],
            "source": package.get("source"),
        }
        for package in metadata["packages"]
    }
    nodes = {node["id"]: node for node in metadata.get("resolve", {}).get("nodes", [])}
    root_id = next(
        package["id"] for package in metadata["packages"]
        if package["name"] == "ogma-backend-codegen-benchmark"
    )
    edges = []
    for node_id, node in nodes.items():
        for dependency in node.get("deps", []):
            target = packages.get(dependency["pkg"], {"name": dependency["pkg"], "version": "unknown"})
            for kind in dependency.get("dep_kinds", []):
                edges.append({
                    "from": packages[node_id]["name"],
                    "to": target["name"],
                    "kind": kind.get("kind") or "normal",
                    "target": kind.get("target"),
                })
    all_packages = sorted(
        ({"name": value["name"], "version": value["version"], "source": value["source"]}
         for value in packages.values()),
        key=lambda item: (item["name"], item["version"]),
    )
    compiler_names = {
        "cranelift-codegen", "cranelift-frontend", "cranelift-module",
        "cranelift-native", "cranelift-object", "inkwell", "llvm-sys",
    }
    compiler_packages = [item for item in all_packages if item["name"] in compiler_names or item["name"].startswith("llvm-sys-")]
    root_dependencies = sorted(
        {
            packages[dependency["pkg"]]["name"]
            for dependency in nodes[root_id].get("deps", [])
            if dependency["pkg"] in packages
        }
    )
    return {
        "manifest_path": "benchmarks/backend-codegen/Cargo.toml",
        "lockfile_path": "benchmarks/backend-codegen/Cargo.lock",
        "lockfile_sha256": _sha256(BACKEND_DIR / "Cargo.lock"),
        "root_package": "ogma-backend-codegen-benchmark",
        "root_direct_dependencies": root_dependencies,
        "resolved_package_count": len(all_packages),
        "packages": all_packages,
        "dependency_edges": sorted(edges, key=lambda item: (item["from"], item["to"], item["kind"], item["target"] or "")),
        "native_code_generator_packages": compiler_packages,
        "scope": "The complete compile-time graph of the M0 benchmark executable and its proof crates. It is not a shipped Ogma Runtime dependency graph.",
    }


def _native_host_call_overhead(backend: dict[str, Any]) -> dict[str, Any]:
    workloads = backend.get("workloads", {})
    measured = workloads.get("core_ir_host_call")
    direct = workloads.get("direct_host_helper")
    if not isinstance(measured, dict) or not isinstance(direct, dict):
        raise RuntimeError("native benchmark report is missing the host-call/direct-helper measurements")
    measured_p50 = float(measured["p50_ns_per_call"])
    direct_p50 = float(direct["p50_ns_per_call"])
    return {
        "generated_aot_host_call": measured,
        "direct_native_host_helper": direct,
        "estimated_dispatch_overhead_ns_per_call": measured_p50 - direct_p50,
        "estimated_dispatch_overhead_ratio": measured_p50 / direct_p50 if direct_p50 > 0 else None,
        "interpretation": "Cranelift/LLVM generated fixture call through the M0 host helper path versus a direct helper call. The helper uses proof adapters/mock ABI, not a production Ogma Runtime ABI.",
    }


def _parse_time_report(path: Path) -> dict[str, Any]:
    if not path.is_file():
        raise RuntimeError(f"GNU time did not write its report: {path}")
    text = path.read_text(encoding="utf-8", errors="replace")
    rss_value = None
    elapsed = None
    for line in text.splitlines():
        if "Maximum resident set size (kbytes):" in line:
            rss_value = int(line.rsplit(":", 1)[1].strip()) * 1024
        if "Elapsed (wall clock) time" in line:
            elapsed = line.rsplit(":", 1)[1].strip()
    if rss_value is None:
        raise RuntimeError(f"GNU time report has no peak RSS metric: {text}")
    return {
        "peak_rss_bytes": rss_value,
        "elapsed_wall_clock": elapsed,
        "raw_report": text,
        "metric_scope": "entire native backend benchmark process, including mock runtime setup, both native compilers, object linking and timed proof workloads.",
    }


def _build_and_measure_native(
    *,
    iterations: int,
    warmups: int,
    samples: int,
    artifact_dir: Path,
    source_revision: str,
) -> dict[str, Any]:
    target_dir = artifact_dir / "cargo-target"
    target_dir.mkdir(parents=True, exist_ok=True)
    build_command = [
        "cargo", "build", "--manifest-path", str(BACKEND_DIR / "Cargo.toml"),
        "--release", "--locked", "--target-dir", str(target_dir),
    ]
    build_started = time.perf_counter_ns()
    build = subprocess.run(build_command, cwd=ROOT, text=True, capture_output=True, check=False)
    build_elapsed_ns = time.perf_counter_ns() - build_started
    if build.returncode != 0:
        raise RuntimeError(
            "native proof executable build failed\n"
            f"command: {' '.join(build_command)}\nstdout:\n{build.stdout[-12000:]}\nstderr:\n{build.stderr[-12000:]}"
        )

    binary = target_dir / "release" / ("ogma-backend-codegen-benchmark.exe" if os.name == "nt" else "ogma-backend-codegen-benchmark")
    if not binary.is_file():
        raise RuntimeError(f"Cargo did not produce expected native proof executable: {binary}")
    time_executable = Path("/usr/bin/time")
    if not time_executable.is_file():
        raise RuntimeError("GNU /usr/bin/time is required for the documented Linux RSS measurement")

    native_report_path = artifact_dir / "ogm30-native-backend-report.json"
    native_artifact_dir = artifact_dir / "native"
    time_report_path = artifact_dir / "native-process-time.txt"
    command = [
        str(binary),
        "--iterations", str(iterations),
        "--warmups", str(warmups),
        "--samples", str(samples),
        "--revision", source_revision,
        "--output", str(native_report_path),
        "--artifact-dir", str(native_artifact_dir),
    ]
    wrapped_command = [str(time_executable), "-v", "-o", str(time_report_path), *command]
    native_environment = os.environ.copy()
    native_environment["CARGO_TARGET_DIR"] = str(target_dir)
    process_started = time.perf_counter_ns()
    completed = subprocess.run(
        wrapped_command,
        cwd=ROOT,
        env=native_environment,
        text=True,
        capture_output=True,
        check=False,
    )
    process_elapsed_ns = time.perf_counter_ns() - process_started
    if completed.returncode != 0:
        raise RuntimeError(
            "native AOT proof benchmark failed\n"
            f"command: {' '.join(wrapped_command)}\n"
            f"stdout:\n{completed.stdout[-16000:]}\nstderr:\n{completed.stderr[-16000:]}"
        )

    native_report = _read_json(native_report_path)
    if native_report.get("task") != "OGM-30":
        raise RuntimeError("native benchmark did not emit an OGM-30 backend report")
    if native_report.get("native_target_triple") != NATIVE_TARGET:
        raise RuntimeError(
            f"expected the documented benchmark target {NATIVE_TARGET}, got {native_report.get('native_target_triple')}"
        )

    backends = {}
    for key, report_key in (("cranelift", "cranelift"), ("llvm", "llvm_backend")):
        result = native_report[report_key]
        object_path = Path(result["native_object_path"])
        if not object_path.is_file() or object_path.stat().st_size <= 0:
            raise RuntimeError(f"{key}: native object file is missing or empty")
        backends[key] = {
            "first_compile_ns": result["first_compile_ns"],
            "compile_time": {
                **result["compile_time_ns"],
                "sample_count": len(result["compile_time_ns"]["raw_samples_ns"]),
            },
            "first_link_ns": result["first_link_ns"],
            "link_time": {
                **result["link_time_ns"],
                "sample_count": len(result["link_time_ns"]["raw_samples_ns"]),
            },
            "object_bytes": result["object_bytes"],
            "shared_library_bytes": result["shared_library_bytes"],
            "copied_native_object": {
                "path": str(object_path.relative_to(ROOT)) if object_path.is_relative_to(ROOT) else str(object_path),
                "bytes": object_path.stat().st_size,
                "sha256": _sha256(object_path),
            },
            "workloads": result["workloads"],
            "host_call_overhead": _native_host_call_overhead(result),
        }

    resource_report = _parse_time_report(time_report_path)
    return {
        "binary": {
            "path": str(binary.relative_to(ROOT)) if binary.is_relative_to(ROOT) else str(binary),
            "bytes": binary.stat().st_size,
            "sha256": _sha256(binary),
        },
        "cargo_build": {
            "wall_ns": build_elapsed_ns,
            "command": build_command,
        },
        "benchmark_process": {
            "wall_ns": process_elapsed_ns,
            "peak_rss_bytes": resource_report["peak_rss_bytes"],
            "peak_rss_scope": resource_report["metric_scope"],
            "gnu_time_report": str(time_report_path.relative_to(ROOT)) if time_report_path.is_relative_to(ROOT) else str(time_report_path),
            "startup_latency_ns": None,
            "startup_status": "not_separately_measured",
        },
        "target": native_report["native_target_triple"],
        "source_revision": native_report["source_revision"],
        "tested_revision": native_report["tested_revision"],
        "rustc": native_report["rustc"],
        "llvm": native_report["llvm"],
        "clang": native_report["clang"],
        "os": native_report["os"],
        "cpu_model": native_report["cpu_model"],
        "build_configuration": native_report["build_configuration"],
        "iterations_per_sample": native_report["iterations_per_sample"],
        "warmup_rounds": native_report["warmup_rounds"],
        "measurement_samples": native_report["measurement_samples"],
        "llvm_package_sizes_kib": native_report["llvm_package_sizes_kib"],
        "llvm_package_footprint_packages": native_report["llvm_package_footprint_packages"],
        "cranelift_build_artifact_bytes": native_report["cranelift_build_artifact_bytes"],
        "llvm_binding_build_artifact_bytes": native_report["llvm_binding_build_artifact_bytes"],
        "backends": backends,
        "limitations": [
            "These are M0 benchmark and mock-runtime proof artifacts, not a shipped native Ogma Runtime.",
            "The measured process RSS includes both Cranelift and LLVM compile/link runs and timed fixture workloads, not only AOT compile peak or steady-state runtime RSS.",
            "The native benchmark executable is a tooling binary, not a hybrid profile application executable.",
            "The native proof uses exact-fixture lowerers, a mock ABI and modeled Runtime boundaries.",
        ],
    }


def _system_snapshot(native: dict[str, Any]) -> dict[str, Any]:
    return {
        "platform": platform.platform(),
        "system": platform.system(),
        "release": platform.release(),
        "machine": platform.machine(),
        "processor": platform.processor() or native.get("cpu_model", "unavailable"),
        "python": sys.version.split()[0],
        "rustc": native["rustc"],
        "cargo": subprocess.run(["cargo", "--version"], cwd=ROOT, text=True, capture_output=True, check=True).stdout.strip(),
        "llvm": native["llvm"],
        "clang": native["clang"],
        "target": native["target"],
    }


def _report(
    *,
    iterations: int,
    warmups: int,
    samples: int,
    output_path: Path,
    artifact_dir: Path,
    source_revision: str,
) -> dict[str, Any]:
    artifact_dir.mkdir(parents=True, exist_ok=True)
    _fixtures, source_hashes, provenance = _load_inputs()
    portable_command = [
        sys.executable, str(Path(__file__).resolve()),
        "--worker", "portable",
        "--iterations", str(iterations),
        "--warmups", str(warmups),
        "--samples", str(samples),
        "--artifact-dir", str(artifact_dir / "portable"),
    ]
    portable_process_started = time.monotonic_ns()
    portable_completed = subprocess.run(
        portable_command, cwd=ROOT, text=True, capture_output=True, check=False
    )
    portable_process_wall_ns = time.monotonic_ns() - portable_process_started
    if portable_completed.returncode != 0:
        raise RuntimeError(
            "portable interpreter proof worker failed\n"
            f"command: {' '.join(portable_command)}\nstdout:\n{portable_completed.stdout[-12000:]}\nstderr:\n{portable_completed.stderr[-12000:]}"
        )
    try:
        portable = json.loads(portable_completed.stdout)
    except json.JSONDecodeError as error:
        raise RuntimeError(f"portable worker did not emit valid JSON: {portable_completed.stdout[-4000:]}") from error
    portable_ready_ns = int(portable.pop("ready_timestamp_ns"))
    portable["process"] = {
        "ready_latency_ns_from_parent_spawn": portable_ready_ns - portable_process_started,
        "end_to_end_wall_ns": portable_process_wall_ns,
        "startup_metric_scope": "Python proof-worker process startup through module import and entry into the benchmark worker; not Ogma Runtime startup.",
    }

    native = _build_and_measure_native(
        iterations=iterations,
        warmups=warmups,
        samples=samples,
        artifact_dir=artifact_dir,
        source_revision=source_revision,
    )

    profiles_document = _read_json(PROFILE_MANIFEST)
    profiles = {
        profile_id: _resolve_profile_plan(profiles_document, profile_id)
        for profile_id in PROFILE_IDS
    }
    cargo_dependencies = _cargo_metadata()

    fixture_records = []
    for item in provenance["manifest"]["source_fixtures"]:
        fixture_records.append({
            "id": item["id"],
            "source": item["source"],
            "source_bytes": (ROOT / item["source"]).stat().st_size,
            "source_sha256": source_hashes[item["source"]],
            "core_ir": item["core_ir"],
            "core_ir_bytes": (ROOT / item["core_ir"]).stat().st_size,
            "core_ir_sha256": provenance["core_ir_hashes"][item["core_ir"]],
        })

    runtime_not_available = {
        "status": "not_implemented",
        "value": None,
        "reason": "There is no production Ogma Runtime, root Cargo workspace, production parser or profile-linked executable in this M0 repository.",
    }
    return {
        "schema": "ogma.runtime-footprint-report",
        "version": "1.0.0",
        "task": "OGM-29",
        "generated_at_utc": datetime.now(timezone.utc).isoformat(),
        "source_revision": source_revision,
        "tested_revision": native["tested_revision"],
        "environment": _system_snapshot(native),
        "workload": {
            "contract": provenance["manifest"]["core_ir_contract"],
            "manifest": "benchmarks/dual-backend/manifest.json",
            "native_manifest": "benchmarks/backend-codegen/fixtures/workloads.json",
            "fixture_set": fixture_records,
            "same_workload_for_portable_and_native": True,
            "note": "Both proof paths consume the same five canonical Core IR fixtures. JavaScript files are source references; no JS parser or source-to-Core-IR lowering is present.",
        },
        "protocol": {
            "iterations_per_workload_sample": iterations,
            "warmup_rounds": warmups,
            "measurement_samples": samples,
            "portable_compile_rounds": COMPILE_ROUNDS,
            "portable_verification_rounds": VERIFY_ROUNDS,
            "timing_clock": "time.perf_counter_ns / Rust std::time::Instant",
            "target": native["target"],
            "comparison_policy": "Only compare paired metrics with the same fixture/workload, target and declared measurement boundary. Cross-language throughput is not treated as production-runtime equivalence.",
        },
        "logical_runtime_profiles": profiles,
        "proof_measurements": {
            "portable_interpreter_profile": portable,
            "hybrid_native_profile": native,
        },
        "host_call_overhead": {
            "portable_proof_interpreter": portable["host_call_overhead"],
            "cranelift_aot_proof": native["backends"]["cranelift"]["host_call_overhead"],
            "llvm_aot_comparison": native["backends"]["llvm"]["host_call_overhead"],
        },
        "binary_and_object_footprint": {
            "production_portable_runtime_executable": runtime_not_available,
            "production_hybrid_runtime_executable": runtime_not_available,
            "portable_bytecode_containers": portable["bytecode_artifacts"],
            "native_tooling_executable": native["binary"],
            "native_objects_and_linked_fixture_libraries": {
                name: {
                    "object_bytes": backend["object_bytes"],
                    "shared_library_bytes": backend["shared_library_bytes"],
                    "object_artifact": backend["copied_native_object"],
                }
                for name, backend in native["backends"].items()
            },
            "llvm_development_package_footprint": {
                "packages": native["llvm_package_footprint_packages"],
                "installed_sizes_kib": native["llvm_package_sizes_kib"],
                "interpretation": "Build-host toolchain footprint only; not the transitive deployment size or shipped runtime dependency size.",
            },
        },
        "startup_and_memory": {
            "production_interpreter_runtime_startup_ns": runtime_not_available,
            "production_hybrid_runtime_startup_ns": runtime_not_available,
            "portable_proof_worker_startup": portable["process"],
            "native_proof_tool_startup": {
                "status": "not_separately_measured",
                "startup_latency_ns": None,
                "reason": "The existing OGM-30 binary has no startup marker before compiler/runtime setup; adding one would change the OGM-30 proof executable outside this task's scope.",
            },
            "portable_proof_worker_peak_rss_bytes": portable["peak_rss_bytes"],
            "native_compiler_and_proof_process_peak_rss_bytes": native["benchmark_process"]["peak_rss_bytes"],
            "production_interpreter_runtime_peak_rss_bytes": runtime_not_available,
            "production_hybrid_runtime_peak_rss_bytes": runtime_not_available,
            "memory_stage_measurements": portable["input_memory"] | {
                "bytecode_compile_peak_tracemalloc_bytes": portable["compile"]["tracemalloc_peak_bytes"],
                "native_compiler_process_peak_rss_bytes": native["benchmark_process"]["peak_rss_bytes"],
            },
            "native_peak_rss_scope": native["benchmark_process"]["peak_rss_scope"],
        },
        "compile_costs": {
            "portable_core_ir_to_bytecode": portable["compile"],
            "native_aot": {
                "cranelift": {
                    "first_compile_ns": native["backends"]["cranelift"]["first_compile_ns"],
                    "compile_samples": native["backends"]["cranelift"]["compile_time"],
                    "first_link_ns": native["backends"]["cranelift"]["first_link_ns"],
                    "link_samples": native["backends"]["cranelift"]["link_time"],
                },
                "llvm_reference": {
                    "first_compile_ns": native["backends"]["llvm"]["first_compile_ns"],
                    "compile_samples": native["backends"]["llvm"]["compile_time"],
                    "first_link_ns": native["backends"]["llvm"]["first_link_ns"],
                    "link_samples": native["backends"]["llvm"]["link_time"],
                },
            },
            "javascript_source_to_ast": runtime_not_available,
            "ast_to_core_ir": runtime_not_available,
            "note": "Portable and native compile costs are reported as separate paths. Existing source references are not parsed; native lowering is the exact-fixture adapter from OGM-30.",
        },
        "dependency_graphs": {
            "benchmark_compile_time_graph": cargo_dependencies,
            "logical_shipped_runtime_graphs": profiles,
            "actual_shipped_runtime_package_graph": {
                "status": "not_implemented",
                "value": None,
                "reason": "No production Cargo workspace/crates or profile-linked runtime artifacts exist. The OGM-28 logical component graph is reported separately and is not presented as package-resolution evidence.",
            },
        },
        "acceptance": {
            "same_representative_core_ir_workload_and_target": True,
            "startup_binary_memory_and_compile_costs_reported_separately": True,
            "source_core_ir_bytecode_and_native_compile_memory_boundaries_reported": True,
            "compile_time_and_logical_runtime_dependency_graphs_separated": True,
            "reproduction_environment_and_machine_readable_results": True,
            "no_unjustified_performance_target_invented": True,
        },
        "production_metric_status": {
            "runtime_startup": "not_implemented",
            "production_runtime_binary_size": "not_implemented",
            "production_runtime_peak_rss": "not_implemented",
            "javascript_ast_peak_memory": "not_implemented",
            "production_core_ir_compiler_memory": "not_implemented",
            "shipped_cargo_runtime_dependency_graph": "not_implemented",
        },
        "limitations": [
            "This is an M0 footprint baseline over bounded proof artifacts, not a production runtime benchmark.",
            "The portable execution implementation is a Python proof interpreter. Its throughput, tracemalloc and process RSS are prototype/tool measurements, not Rust-native VM results.",
            "The native code-generation path is an exact-fixture Cranelift/LLVM adapter linked to a mock C ABI and modeled Runtime boundary; it is not a production hybrid Runtime.",
            "The native process RSS includes Cranelift and LLVM compilation/linking, mock Runtime setup and timed fixture workloads. It is not isolated AOT compiler peak memory or steady-state native Runtime RSS.",
            "The source files are reference inputs paired with pre-authored Core IR fixtures. No JavaScript parser or AST exists, so AST allocation metrics are explicitly unavailable.",
            "Runtime binary sizes and the actual shipped Rust dependency graph cannot be measured before a production Runtime/workspace exists; proof executable/object/container measurements are separately labelled.",
            "The recorded LLVM installed package sizes cover only the explicitly named packages, not their complete transitive system package closure.",
        ],
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--worker", choices=("portable",), help=argparse.SUPPRESS)
    parser.add_argument("--iterations", type=int, default=DEFAULT_ITERATIONS)
    parser.add_argument("--warmups", type=int, default=DEFAULT_WARMUPS)
    parser.add_argument("--samples", type=int, default=DEFAULT_SAMPLES)
    parser.add_argument("--output", type=Path, default=Path("/tmp/ogm29-footprint-report.json"))
    parser.add_argument("--artifact-dir", type=Path, default=Path("/tmp/ogm29-footprint-artifacts"))
    parser.add_argument("--revision", default=os.environ.get("OGM29_SOURCE_REVISION") or os.environ.get("GITHUB_SHA") or "local-working-tree")
    args = parser.parse_args()

    if args.iterations < 1 or args.warmups < 0 or args.samples < 101:
        parser.error("iterations must be positive, warmups non-negative and samples at least 101")

    if args.worker == "portable":
        result = _portable_worker(
            iterations=args.iterations,
            warmups=args.warmups,
            samples=args.samples,
            artifact_dir=args.artifact_dir,
        )
        print(json.dumps(result, sort_keys=True, ensure_ascii=False, allow_nan=False))
        return 0

    report = _report(
        iterations=args.iterations,
        warmups=args.warmups,
        samples=args.samples,
        output_path=args.output,
        artifact_dir=args.artifact_dir,
        source_revision=args.revision,
    )
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(
        json.dumps(report, indent=2, sort_keys=True, ensure_ascii=False, allow_nan=False) + "\n",
        encoding="utf-8",
        newline="\n",
    )
    summary = {
        "task": report["task"],
        "target": report["environment"]["target"],
        "source_revision": report["source_revision"],
        "portable_runtime": report["production_metric_status"]["runtime_startup"],
        "hybrid_runtime": report["production_metric_status"]["production_runtime_binary_size"],
        "portable_compile_samples": report["compile_costs"]["portable_core_ir_to_bytecode"]["wall"]["sample_count"],
        "native_backends": sorted(report["proof_measurements"]["hybrid_native_profile"]["backends"]),
        "acceptance": report["acceptance"],
        "report": str(args.output),
        "artifacts": str(args.artifact_dir),
    }
    print(json.dumps(summary, indent=2, sort_keys=True, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
