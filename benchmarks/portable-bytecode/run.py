#!/usr/bin/env python3
"""Run the deterministic OGM-16 stack/register benchmark and emit raw JSON evidence."""

from __future__ import annotations

import argparse
from datetime import datetime, timezone
import json
import statistics
import subprocess
import sys
import time
import tracemalloc
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
BENCHMARK_DIR = Path(__file__).resolve().parent
RESULTS_DIR = BENCHMARK_DIR / "results"
sys.path.insert(0, str(BENCHMARK_DIR))

from corpus import write_corpus  # noqa: E402
from prototype import (  # noqa: E402
    canonical_json,
    compile_workload,
    environment_snapshot,
    execute_case,
    run_workload_correctness,
    verifier_complexity,
    verify_compiled,
)


def median_or_zero(values: list[int]) -> int:
    return int(statistics.median(values)) if values else 0


def verify_core_ir_corpus() -> dict[str, Any]:
    report_path = RESULTS_DIR / "core-ir-verifier-report.json"
    command = [
        sys.executable,
        str(ROOT / ".github" / "scripts" / "verify_core_ir.py"),
        "--manifest",
        str(BENCHMARK_DIR / "fixtures" / "manifest.json"),
        "--report",
        str(report_path),
    ]
    completed = subprocess.run(command, cwd=ROOT, text=True, capture_output=True, check=False)
    if completed.returncode != 0:
        raise RuntimeError(
            "Core IR v1 fixture verification failed.\n"
            f"Command: {' '.join(command)}\n"
            f"stdout:\n{completed.stdout}\nstderr:\n{completed.stderr}"
        )
    if not report_path.is_file():
        raise RuntimeError("Core IR verifier did not produce its requested machine-readable report")
    return json.loads(report_path.read_text(encoding="utf-8"))


def benchmark_candidate(
    workload: dict[str, Any],
    model: str,
    iterations: int,
    warmups: int,
    compile_rounds: int,
    verifier_rounds: int,
    measurement_rounds: int,
) -> tuple[dict[str, Any], list[Any]]:
    compile_samples: list[int] = []
    compile_wall_samples: list[int] = []
    compiled = None

    for _ in range(compile_rounds):
        started = time.perf_counter_ns()
        candidate = compile_workload(workload, model)
        compile_wall_samples.append(time.perf_counter_ns() - started)
        compile_samples.append(sum(item.code_generation_ns for item in candidate))
        compiled = candidate

    assert compiled is not None

    verifier_samples: list[int] = []
    for _ in range(verifier_rounds):
        started = time.perf_counter_ns()
        for item in compiled:
            verify_compiled(item)
        verifier_samples.append(time.perf_counter_ns() - started)

    # One untimed execution establishes deterministic dispatch counts for each test case.
    case_dispatches: list[int] = []
    for case in workload["cases"]:
        _, count = execute_case(workload, compiled, case["args"])
        case_dispatches.append(count)

    for _ in range(warmups):
        for case in workload["cases"]:
            execute_case(workload, compiled, case["args"])

    execution_elapsed_samples: list[int] = []
    execution_rate_samples: list[float] = []
    dispatch_rate_samples: list[float] = []
    dispatch_samples: list[int] = []
    execution_count = len(workload["cases"]) * iterations
    expected_dispatches = iterations * sum(case_dispatches)
    for _ in range(measurement_rounds):
        execution_started = time.perf_counter_ns()
        measured_dispatches = 0
        measured_executions = 0
        for case in workload["cases"]:
            for _ in range(iterations):
                _, count = execute_case(workload, compiled, case["args"])
                measured_dispatches += count
                measured_executions += 1
        execution_elapsed = time.perf_counter_ns() - execution_started
        if measured_executions != execution_count:
            raise AssertionError("benchmark round executed an unexpected number of cases")
        if measured_dispatches != expected_dispatches:
            raise AssertionError("interpreter dispatch count changed between measurement rounds")
        execution_elapsed_samples.append(execution_elapsed)
        dispatch_samples.append(measured_dispatches)
        execution_rate_samples.append(execution_count * 1_000_000_000 / max(1, execution_elapsed))
        dispatch_rate_samples.append(measured_dispatches * 1_000_000_000 / max(1, execution_elapsed))
    execution_elapsed = median_or_zero(execution_elapsed_samples)
    measured_dispatches = median_or_zero(dispatch_samples)
    executions_per_second = statistics.median(execution_rate_samples)
    dispatches_per_second = statistics.median(dispatch_rate_samples)

    memory_samples: list[int] = []
    for case in workload["cases"]:
        tracemalloc.start()
        tracemalloc.reset_peak()
        execute_case(workload, compiled, case["args"])
        _current, peak = tracemalloc.get_traced_memory()
        tracemalloc.stop()
        memory_samples.append(int(peak))

    all_instructions = [item for function in compiled for item in function.instructions]
    complexity_items = [verifier_complexity(item) for item in compiled]
    complexity = {
        key: sum(int(item[key] or 0) for item in complexity_items)
        for key in (
            "instructions", "control_flow_edges", "immediate_and_constant_operands",
            "stack_effect_checks", "register_reads", "register_writes",
            "state_analysis_operations", "structural_work_units", "verification_input_bytes",
        )
    }
    complexity["verifier_median_ns"] = median_or_zero(verifier_samples)

    metrics = {
        "model": model,
        "function_count": len(compiled),
        "case_count": len(workload["cases"]),
        "instruction_count": len(all_instructions),
        "dispatches_per_case": case_dispatches,
        "mean_dispatches_per_execution": statistics.mean(case_dispatches) if case_dispatches else 0,
        "code_bytes": sum(len(item.code) for item in compiled),
        "container_bytes": sum(len(item.container) for item in compiled),
        "constant_pool_bytes": sum(len(canonical_json(item.constants)) for item in compiled),
        "debug_map_bytes": sum(len(canonical_json(item.debug_map)) for item in compiled),
        "local_slots": sum(item.local_count for item in compiled),
        "virtual_registers": sum(item.register_count for item in compiled),
        "compile_cost_median_ns": median_or_zero(compile_samples),
        "compile_pipeline_wall_median_ns": median_or_zero(compile_wall_samples),
        "verifier_complexity": complexity,
        "interpreter": {
            "iterations_per_case": iterations,
            "measurement_rounds": measurement_rounds,
            "executions_per_round": execution_count,
            "elapsed_ns_median": execution_elapsed,
            "executions_per_second_median": executions_per_second,
            "measured_dispatches_per_round": measured_dispatches,
            "dispatches_per_second_median": dispatches_per_second,
        },
        "memory": {
            "tracemalloc_peak_bytes_per_case": memory_samples,
            "tracemalloc_peak_bytes_max": max(memory_samples, default=0),
        },
        "raw_samples": {
            "compile_codegen_ns": compile_samples,
            "compile_pipeline_wall_ns": compile_wall_samples,
            "verifier_ns": verifier_samples,
            "memory_peak_bytes": memory_samples,
            "interpreter_elapsed_ns": execution_elapsed_samples,
            "interpreter_executions_per_second": execution_rate_samples,
            "interpreter_dispatches_per_round": dispatch_samples,
            "interpreter_dispatches_per_second": dispatch_rate_samples,
        },
    }
    return metrics, compiled


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--iterations", type=int, default=1_000)
    parser.add_argument("--warmups", type=int, default=3)
    parser.add_argument("--compile-rounds", type=int, default=15)
    parser.add_argument("--verifier-rounds", type=int, default=15)
    parser.add_argument("--measurement-rounds", type=int, default=9)
    parser.add_argument("--output", type=Path, default=RESULTS_DIR / "benchmark-results.json")
    args = parser.parse_args()
    for name, value in (
        ("iterations", args.iterations),
        ("warmups", args.warmups),
        ("compile-rounds", args.compile_rounds),
        ("verifier-rounds", args.verifier_rounds),
        ("measurement-rounds", args.measurement_rounds),
    ):
        if value < (0 if name == "warmups" else 1):
            parser.error(f"{name} is outside the allowed range")

    RESULTS_DIR.mkdir(parents=True, exist_ok=True)
    workload_document = write_corpus()
    core_ir_report = verify_core_ir_corpus()

    all_results: list[dict[str, Any]] = []
    for workload in workload_document["workloads"]:
        stack_metrics, stack_compiled = benchmark_candidate(
            workload, "stack", args.iterations, args.warmups,
            args.compile_rounds, args.verifier_rounds, args.measurement_rounds,
        )
        register_metrics, register_compiled = benchmark_candidate(
            workload, "register", args.iterations, args.warmups,
            args.compile_rounds, args.verifier_rounds, args.measurement_rounds,
        )
        observations = run_workload_correctness(workload, stack_compiled, register_compiled)
        all_results.append({
            "workload": workload["id"],
            "description": workload["description"],
            "observable_equivalence": True,
            "observations": observations,
            "candidates": {
                "stack": stack_metrics,
                "register": register_metrics,
            },
        })

    stack_total = sum(item["candidates"]["stack"]["instruction_count"] for item in all_results)
    register_total = sum(item["candidates"]["register"]["instruction_count"] for item in all_results)
    stack_code = sum(item["candidates"]["stack"]["code_bytes"] for item in all_results)
    register_code = sum(item["candidates"]["register"]["code_bytes"] for item in all_results)
    report = {
        "schema": "ogma.stack-register-benchmark",
        "version": "1.0.0",
        "task": "OGM-16",
        "generated_at_utc": datetime.now(timezone.utc).isoformat(),
        "source_revision": __import__("os").environ.get("GITHUB_SHA", "local-working-tree"),
        "environment": environment_snapshot(),
        "protocol": {
            "iterations_per_case": args.iterations,
            "warmup_iterations_per_case": args.warmups,
            "compile_rounds": args.compile_rounds,
            "verifier_rounds": args.verifier_rounds,
            "measurement_rounds": args.measurement_rounds,
            "timing_clock": "time.perf_counter_ns",
            "memory_metric": "Python tracemalloc peak; interpreter/prototype allocation proxy, not native VM RSS",
            "correctness": "both decoded bytecode interpreters must match the expected observation and each other",
        },
        "core_ir_verification": core_ir_report,
        "summary": {
            "workload_count": len(all_results),
            "all_observations_equivalent": all(item["observable_equivalence"] for item in all_results),
            "stack_instruction_count": stack_total,
            "register_instruction_count": register_total,
            "stack_code_bytes": stack_code,
            "register_code_bytes": register_code,
            "register_instruction_reduction_percent": (
                (stack_total - register_total) * 100 / stack_total if stack_total else 0
            ),
            "register_code_size_delta_percent": (
                (register_code - stack_code) * 100 / stack_code if stack_code else 0
            ),
        },
        "workloads": all_results,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(
        json.dumps(report, indent=2, sort_keys=True, ensure_ascii=False, allow_nan=False) + "\n",
        encoding="utf-8",
        newline="\n",
    )
    print(json.dumps(report["summary"], indent=2, sort_keys=True))
    print(f"Raw benchmark results: {args.output.relative_to(ROOT) if args.output.is_relative_to(ROOT) else args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
