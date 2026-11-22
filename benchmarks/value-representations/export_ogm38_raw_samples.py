#!/usr/bin/env python3
"""Export committed OGM-38 raw evidence projections from authoritative CI JSON."""

from __future__ import annotations

import argparse
import gzip
import hashlib
import json
from pathlib import Path
from typing import Any

from validate_ogm38_ogm31 import validate_semantic_case_evidence

EXPECTED_CANDIDATES = {
    "tagged-16-boxed",
    "tagged-16-representation-aware",
    "nanbox-8-boxed",
    "nanbox-8-representation-aware",
}
EXPECTED_COLLECTORS = {"rc-cycle-removal", "non-moving-mark-sweep"}
EXPECTED_ALLOCATORS = {
    "system-allocator",
    "per-isolate-exact-reuse",
    "per-isolate-size-class-pool",
}
EXPECTED_FIXTURES = 5
EXPECTED_DISPATCHES = 77
EXPECTED_REFERENCES = 20
EXPECTED_SAMPLES = 101


def read_json(path: Path) -> tuple[dict[str, Any], bytes]:
    raw = path.read_bytes()
    value = json.loads(raw.decode("utf-8"))
    if not isinstance(value, dict):
        raise ValueError(f"{path} must contain a JSON object")
    return value, raw


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValueError(message)


def validate_reports(native: dict[str, Any], differential: dict[str, Any]) -> None:
    require(native.get("task") == "OGM-38", "native report task is not OGM-38")
    require(native.get("schema_version") == 1, "unsupported native report schema")
    source_revision = native.get("source_revision")
    require(bool(source_revision), "native report source_revision is missing")
    require(
        source_revision == native.get("tested_revision"),
        "native report source/tested revisions differ",
    )
    require(native.get("measurement_samples") == EXPECTED_SAMPLES, "expected 101 native samples")
    require(native.get("warmup_rounds") == 3, "expected three warmup rounds")
    require(native.get("iterations_per_sample") == 1000, "expected 1,000 iterations/sample")
    require(native.get("target_triple") == "x86_64-unknown-linux-gnu", "unexpected CI target")

    candidates = native.get("candidates")
    require(isinstance(candidates, list), "native candidates must be an array")
    require(
        {item.get("name") for item in candidates} == EXPECTED_CANDIDATES,
        "native candidate matrix differs from the four OGM-38 candidates",
    )
    require(len(candidates) == 4, "native report must contain exactly four candidates")

    expected_pairs = {
        (collector, allocator)
        for collector in EXPECTED_COLLECTORS
        for allocator in EXPECTED_ALLOCATORS
    }
    for candidate in candidates:
        name = candidate["name"]
        for metric in ("compile_ns", "throughput_ns_per_call"):
            distribution = candidate.get(metric)
            require(isinstance(distribution, dict), f"{name}.{metric} is missing")
            samples = distribution.get("raw_samples_ns")
            require(
                isinstance(samples, list) and len(samples) == EXPECTED_SAMPLES,
                f"{name}.{metric} must contain 101 raw values",
            )
            require(
                all(isinstance(value, (int, float)) and value > 0 for value in samples),
                f"{name}.{metric} contains non-positive samples",
            )

        try:
            validate_semantic_case_evidence(
                candidate.get("native_value_semantics"),
                candidate=name,
            )
        except RuntimeError as error:
            raise ValueError(str(error)) from error
        corpus = candidate.get("native_core_ir_corpus", {})
        require(
            corpus.get("canonical_fixture_count") == EXPECTED_FIXTURES
            and corpus.get("native_batch_invocations") == EXPECTED_FIXTURES
            and corpus.get("native_value_dispatches") == EXPECTED_DISPATCHES
            and corpus.get("reference_values_resolved_after_forced_gc") == EXPECTED_REFERENCES
            and corpus.get("all_values_roundtripped_and_dispatched") is True,
            f"{name} native Core IR Value corpus evidence is incomplete",
        )

        comparisons = candidate.get("gc", {}).get("collector_comparisons")
        require(isinstance(comparisons, list), f"{name} GC collector comparisons are missing")
        actual_pairs = set()
        require(len(comparisons) == 6, f"{name} must contain six GC/allocator comparisons")
        for comparison in comparisons:
            pair = (comparison.get("collector"), comparison.get("allocator_strategy"))
            actual_pairs.add(pair)
            raw_samples = comparison.get("raw_samples")
            require(
                isinstance(raw_samples, list) and len(raw_samples) == EXPECTED_SAMPLES,
                f"{name} {pair} must contain 101 GC samples",
            )
            baseline = comparison.get("process_peak_rss_baseline_bytes")
            peak = comparison.get("process_peak_rss_bytes")
            delta = comparison.get("process_peak_rss_delta_bytes")
            require(
                all(isinstance(value, int) and value > 0 for value in (baseline, peak, delta)),
                f"{name} {pair} peak RSS values must be positive integers",
            )
            require(
                peak >= baseline and peak - baseline == delta,
                f"{name} {pair} peak RSS baseline/peak/delta are inconsistent",
            )
            require(
                comparison.get("actual_value_storage_capacity_bytes")
                == candidate["gc"]["value_storage_bytes"],
                f"{name} {pair} Value payload backing bytes do not match the heap workload",
            )
        require(actual_pairs == expected_pairs, f"{name} GC/allocator matrix is incomplete")

        allocator_comparisons = candidate.get("gc", {}).get("allocator_comparisons")
        require(
            isinstance(allocator_comparisons, list)
            and {item.get("strategy") for item in allocator_comparisons} == EXPECTED_ALLOCATORS,
            f"{name} direct allocator microbench matrix is incomplete",
        )
        for comparison in allocator_comparisons:
            samples = comparison.get("raw_samples")
            require(
                isinstance(samples, list) and len(samples) == EXPECTED_SAMPLES,
                f"{name} {comparison.get('strategy')} allocator sample count differs",
            )

    require(
        differential.get("schema") == "ogma.ogm38-ogm31-differential-validation"
        and differential.get("task") == "OGM-38"
        and differential.get("native_revision") == source_revision
        and differential.get("all_candidates_match_portable_ogm31_oracle") is True
        and differential.get("candidate_count") == 4,
        "OGM-31 differential report does not match the native report revision/candidates",
    )


def build_primary_projection(
    native: dict[str, Any],
    run_url: str,
    artifact_url: str,
    full_report_sha256: str,
    differential_sha256: str,
) -> dict[str, Any]:
    return {
        "schema": "ogma.ogm38.primary-benchmark-samples",
        "version": 2,
        "task": "OGM-38",
        "source_revision": native["source_revision"],
        "tested_revision": native["tested_revision"],
        "run_url": run_url,
        "full_report_artifact_url": artifact_url,
        "full_report_sha256": full_report_sha256,
        "differential_report_sha256": differential_sha256,
        "target": native["target_triple"],
        "cpu_model": native["cpu_model"],
        "rustc": native["rustc"],
        "operating_system": native["operating_system"],
        "core_ir_contract": native["core_ir_contract"],
        "warmups": native["warmup_rounds"],
        "samples_per_metric": native["measurement_samples"],
        "iterations_per_throughput_sample": native["iterations_per_sample"],
        "semantic_workload": native["semantic_workload"],
        "candidates": [
            {
                "name": candidate["name"],
                "runtime_value_bytes": candidate["runtime_value_bytes"],
                "compile_ns": candidate["compile_ns"]["raw_samples_ns"],
                "native_loop_ns_per_call": candidate["throughput_ns_per_call"]["raw_samples_ns"],
                "calls_per_second_p50": candidate["calls_per_second_p50"],
                "object_bytes": candidate["object_bytes"],
                "shared_library_bytes": candidate["shared_library_bytes"],
                "loop_code_bytes": candidate["native_code"]["function_code_bytes"],
                "semantic_cases_passed": len(candidate["native_value_semantics"]["cases"]),
                "native_semantic_batch_dispatch_invocations": candidate["native_value_semantics"]["native_batch_dispatch_invocations"],
                "native_batch_invocations": candidate["native_core_ir_corpus"]["native_batch_invocations"],
                "native_value_dispatches": candidate["native_core_ir_corpus"]["native_value_dispatches"],
                "reference_values_resolved_after_forced_gc": candidate["native_core_ir_corpus"]["reference_values_resolved_after_forced_gc"],
                "all_semantic_cases_match": candidate["native_value_semantics"]["every_case_matches"],
            }
            for candidate in native["candidates"]
        ],
    }


def build_runtime_projection(
    native: dict[str, Any],
    run_url: str,
    artifact_url: str,
    full_report_sha256: str,
) -> dict[str, Any]:
    candidates = []
    for candidate in native["candidates"]:
        gc = candidate["gc"]
        comparisons = []
        for item in gc["collector_comparisons"]:
            comparisons.append(
                {
                    "collector": item["collector"],
                    "allocator_strategy": item["allocator_strategy"],
                    "object_count": item["object_count"],
                    "edge_count": item["edge_count"],
                    "interpreter_frame_roots": item["interpreter_frame_roots"],
                    "native_frame_roots": item["native_frame_roots"],
                    "persistent_host_handles": item["persistent_host_handles"],
                    "temporary_scope_roots": item["temporary_scope_roots"],
                    "peak_logical_heap_bytes_max": item["peak_logical_heap_bytes_max"],
                    "actual_value_storage_capacity_bytes": item["actual_value_storage_capacity_bytes"],
                    "allocator_peak_reserved_bytes_max": item["allocator_peak_reserved_bytes_max"],
                    "allocator_peak_internal_fragmentation_bytes_max": item["allocator_peak_internal_fragmentation_bytes_max"],
                    "process_peak_rss_baseline_bytes": item["process_peak_rss_baseline_bytes"],
                    "process_peak_rss_bytes": item["process_peak_rss_bytes"],
                    "process_peak_rss_delta_bytes": item["process_peak_rss_delta_bytes"],
                    "encoded_object_values_resolved": item["encoded_object_values_resolved"],
                    "build_ns": item["build_ns"]["raw_samples_ns"],
                    "collection_ns": item["collection_ns"]["raw_samples_ns"],
                    "root_scan_ns": item["root_scan_ns"]["raw_samples_ns"],
                    "cleanup_collection_ns": item["cleanup_collection_ns"]["raw_samples_ns"],
                    "raw_samples": item["raw_samples"],
                }
            )
        allocator_comparisons = [
            {
                "strategy": item["strategy"],
                "object_count": item["object_count"],
                "payload_bytes_per_object": item["payload_bytes_per_object"],
                "value_storage_bytes": item["value_storage_bytes"],
                "allocation_rounds_per_sample": item["allocation_rounds_per_sample"],
                "peak_reserved_bytes_max": item["peak_reserved_bytes_max"],
                "peak_live_requested_bytes_max": item["peak_live_requested_bytes_max"],
                "peak_internal_fragmentation_bytes_max": item["peak_internal_fragmentation_bytes_max"],
                "reused_allocations_total": item["reused_allocations_total"],
                "cache_evictions_total": item["cache_evictions_total"],
                "allocation_failures_total": item["allocation_failures_total"],
                "allocation_ns_per_sample": item["allocation_ns_per_sample"]["samples"],
                "deallocation_ns_per_sample": item["deallocation_ns_per_sample"]["samples"],
                "raw_samples": item["raw_samples"],
            }
            for item in gc["allocator_comparisons"]
        ]
        candidates.append(
            {
                "name": candidate["name"],
                "runtime_value_bytes": candidate["runtime_value_bytes"],
                "value_storage_bytes": gc["value_storage_bytes"],
                "object_count": gc["object_count"],
                "graph_edges": gc["graph_edges"],
                "collector_comparisons": comparisons,
                "allocator_comparisons": allocator_comparisons,
                "native_shadow_roots": candidate["native_shadow_roots"],
            }
        )

    return {
        "schema": "ogma.ogm38.runtime-raw-samples",
        "version": 2,
        "task": "OGM-38",
        "source_revision": native["source_revision"],
        "tested_revision": native["tested_revision"],
        "run_url": run_url,
        "full_report_artifact_url": artifact_url,
        "full_report_sha256": full_report_sha256,
        "target": native["target_triple"],
        "cpu_model": native["cpu_model"],
        "rustc": native["rustc"],
        "operating_system": native["operating_system"],
        "sample_count": native["measurement_samples"],
        "warmups": native["warmup_rounds"],
        "rss_method": "Linux /proc/self/status VmHWM in one isolated child per candidate x collector x allocator; baseline before warmups, peak after warmups and all samples",
        "candidates": candidates,
    }


def write_json(path: Path, value: dict[str, Any]) -> None:
    path.write_text(
        json.dumps(value, indent=2, sort_keys=True, ensure_ascii=False) + "\n",
        encoding="utf-8",
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--native-report", type=Path, required=True)
    parser.add_argument("--differential-report", type=Path, required=True)
    parser.add_argument("--run-url", required=True)
    parser.add_argument("--artifact-url", required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    arguments = parser.parse_args()

    native, native_bytes = read_json(arguments.native_report)
    differential, differential_bytes = read_json(arguments.differential_report)
    validate_reports(native, differential)

    full_sha = hashlib.sha256(native_bytes).hexdigest()
    differential_sha = hashlib.sha256(differential_bytes).hexdigest()
    output_dir: Path = arguments.output_dir
    output_dir.mkdir(parents=True, exist_ok=True)

    # Keep the complete authoritative report, not merely the selected projections.
    full_compressed = gzip.compress(native_bytes, compresslevel=9, mtime=0)
    (output_dir / "ogm38-native-value-ssa-report.json.gz").write_bytes(full_compressed)

    primary = build_primary_projection(
        native,
        arguments.run_url,
        arguments.artifact_url,
        full_sha,
        differential_sha,
    )
    write_json(output_dir / "ogm38-primary-benchmark-samples.json", primary)

    runtime = build_runtime_projection(
        native,
        arguments.run_url,
        arguments.artifact_url,
        full_sha,
    )
    runtime_bytes = json.dumps(
        runtime, separators=(",", ":"), sort_keys=True, ensure_ascii=False
    ).encode("utf-8")
    (output_dir / "ogm38-runtime-raw-samples.json.gz").write_bytes(
        gzip.compress(runtime_bytes, compresslevel=9, mtime=0)
    )

    (output_dir / "ogm38-ogm31-differential-report.json").write_bytes(
        differential_bytes
    )

    print(
        json.dumps(
            {
                "source_revision": native["source_revision"],
                "full_report_sha256": full_sha,
                "differential_report_sha256": differential_sha,
                "candidates": len(native["candidates"]),
                "gc_worker_comparisons": sum(
                    len(candidate["gc"]["collector_comparisons"])
                    for candidate in native["candidates"]
                ),
                "gc_samples_per_worker": EXPECTED_SAMPLES,
                "outputs": [
                    "ogm38-native-value-ssa-report.json.gz",
                    "ogm38-primary-benchmark-samples.json",
                    "ogm38-runtime-raw-samples.json.gz",
                    "ogm38-ogm31-differential-report.json",
                ],
            },
            indent=2,
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
