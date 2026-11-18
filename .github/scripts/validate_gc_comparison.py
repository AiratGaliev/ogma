#!/usr/bin/env python3
"""Validate the OGM-26 GC comparison benchmark evidence report."""

from __future__ import annotations

import json
import math
import sys
from pathlib import Path

EXPECTED_WORKLOADS = {
    "balanced-roots-and-cycles",
    "persistent-host-retention",
    "allocation-churn",
}
EXPECTED_COLLECTORS = {"rc-cycle-removal", "non-moving-mark-sweep"}
PERCENTILES = ("p50", "p95", "p99")


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AssertionError(message)


def finite_non_negative(value: object, label: str) -> float:
    require(isinstance(value, (int, float)), f"{label} must be numeric")
    number = float(value)
    require(math.isfinite(number) and number >= 0, f"{label} must be finite and >= 0")
    return number


def main() -> int:
    if len(sys.argv) != 2:
        raise SystemExit("usage: validate_gc_comparison.py REPORT.json")

    report_path = Path(sys.argv[1])
    with report_path.open("r", encoding="utf-8") as stream:
        report = json.load(stream)

    require(report["schema_version"] == 1, "unsupported report schema")
    require(report["task"] == "OGM-26", "wrong Linear task identifier")
    require(bool(report["source_revision"]), "source_revision is required")
    require(bool(report["tested_revision"]), "tested_revision is required")

    environment = report["environment"]
    for field in ("rustc", "os", "arch", "cpu"):
        require(bool(environment[field]), f"environment.{field} is required")

    configuration = report["configuration"]
    require(configuration["objects"] >= 64, "object workload is too small")
    require(configuration["objects"] % 4 == 0, "object count must be divisible by four")
    require(configuration["warmups"] >= 1, "warmups must be recorded")
    require(configuration["samples"] >= 101, "at least 101 samples are required")
    require(
        configuration["percentile_method"] == "nearest-rank",
        "percentile method must be nearest-rank",
    )
    require(
        configuration["candidate_order"] == "alternating-per-sample",
        "candidate order must alternate to limit systematic order bias",
    )

    results = report["results"]
    require(
        len(results) == len(EXPECTED_WORKLOADS) * len(EXPECTED_COLLECTORS),
        "expected one result per workload and collector",
    )

    indexed = {}
    for result in results:
        key = (result["workload"], result["collector"])
        require(key not in indexed, f"duplicate result: {key}")
        indexed[key] = result

        require(result["workload"] in EXPECTED_WORKLOADS, f"unexpected workload: {key}")
        require(result["collector"] in EXPECTED_COLLECTORS, f"unexpected collector: {key}")

        graph = result["graph"]
        require(graph["objects"] == configuration["objects"], f"wrong object count: {key}")
        require(graph["edges"] >= graph["objects"], f"expected cyclic edge graph: {key}")
        require(len(graph["fingerprint"]) == 16, f"invalid graph fingerprint: {key}")
        roots = graph["root_counts"]
        if result["workload"] != "allocation-churn":
            for field in (
                "interpreter_frames",
                "native_frame_prototype",
                "persistent_host_handles",
                "temporary_scopes",
            ):
                require(roots[field] > 0, f"root inventory missing {field} in {key}")
        else:
            require(roots["temporary_scopes"] == 0, "churn workload must release temp scopes")
            require(
                roots["interpreter_frames"] > 0
                and roots["native_frame_prototype"] > 0
                and roots["persistent_host_handles"] > 0,
                "churn workload must retain all non-temporary root providers",
            )

        metrics = result["metrics"]
        for field in (
            "collection_ns",
            "root_scan_ns",
            "build_ns",
            "end_to_end_ns",
        ):
            values = metrics[field]
            numbers = [
                finite_non_negative(values[p], f"{key}.{field}.{p}") for p in PERCENTILES
            ]
            require(numbers[0] <= numbers[1] <= numbers[2], f"unordered {field} percentiles: {key}")

        for field in (
            "objects_per_second_at_median_build",
            "operations_per_second_at_median_end_to_end",
            "average_edges_examined",
            "average_roots_scanned",
            "average_refcount_operations",
        ):
            finite_non_negative(metrics[field], f"{key}.{field}")

        for field in (
            "live_objects_after",
            "total_reclaimed_objects",
            "peak_heap_bytes",
            "live_heap_bytes_after",
        ):
            require(
                isinstance(metrics[field], int) and metrics[field] >= 0,
                f"invalid {key}.{field}",
            )

        raw_samples = result["raw_samples"]
        require(len(raw_samples) == configuration["samples"], f"wrong raw sample count: {key}")
        for index, sample in enumerate(raw_samples):
            for field in (
                "build_ns",
                "collection_ns",
                "root_scan_ns",
                "roots_scanned",
                "edges_examined",
                "objects_before",
                "objects_after",
                "reclaimed_total",
                "reclaimed_during_collection",
                "peak_heap_bytes",
                "live_heap_bytes_after",
                "refcount_operations",
            ):
                require(
                    isinstance(sample[field], int) and sample[field] >= 0,
                    f"invalid raw sample {index} field {field}: {key}",
                )
            require(
                sample["objects_before"] == graph["objects"],
                f"sample graph count mismatch: {key}",
            )
            require(
                sample["graph"]["fingerprint"] == graph["fingerprint"],
                f"sample graph fingerprint changed: {key}",
            )
            require(
                sample["objects_after"] + sample["reclaimed_total"] == sample["objects_before"],
                f"object lifecycle does not balance: {key}/{index}",
            )

    require(
        set(indexed)
        == {
            (workload, collector)
            for workload in EXPECTED_WORKLOADS
            for collector in EXPECTED_COLLECTORS
        },
        "missing workload/collector pair",
    )

    for workload in EXPECTED_WORKLOADS:
        rc = indexed[(workload, "rc-cycle-removal")]
        tracing = indexed[(workload, "non-moving-mark-sweep")]
        require(rc["graph"] == tracing["graph"], f"candidates used different graphs: {workload}")
        for field in (
            "live_objects_after",
            "total_reclaimed_objects",
            "live_heap_bytes_after",
        ):
            require(
                rc["metrics"][field] == tracing["metrics"][field],
                f"candidate liveness mismatch for {workload}: {field}",
            )
        for index, (rc_sample, tracing_sample) in enumerate(
            zip(rc["raw_samples"], tracing["raw_samples"], strict=True)
        ):
            require(
                rc_sample["graph"] == tracing_sample["graph"],
                f"raw graph mismatch for {workload} sample {index}",
            )
            for field in ("objects_after", "reclaimed_total", "live_heap_bytes_after"):
                require(
                    rc_sample[field] == tracing_sample[field],
                    f"raw liveness mismatch for {workload} sample {index}: {field}",
                )

    print(
        "Validated OGM-26 report: 3 workloads x 2 collectors, "
        f"{configuration['samples']} raw samples per candidate, equivalent graphs and liveness."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
