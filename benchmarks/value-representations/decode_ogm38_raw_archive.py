#!/usr/bin/env python3
"""Reconstruct and verify the Git-tracked OGM-38 raw native measurement archive."""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import lzma
from pathlib import Path
from typing import Any

HERE = Path(__file__).resolve().parent
DEFAULT_PARTS_DIR = HERE / "results" / "ogm38-raw-native-samples.delta.json.xz.b64.parts"
DEFAULT_OUTPUT = HERE / "results" / "ogm38-raw-native-samples.delta.json.xz"
EXPECTED_PART_COUNT = 14
EXPECTED_PART_LENGTHS = [4000] * 13 + [128]
EXPECTED_BASE64_BYTES = 52128
EXPECTED_XZ_BYTES = 39096
EXPECTED_XZ_SHA256 = "ebe31be1a8af18dda5f22b4dd078518fa7a492fb0a7038798ac572b7ba38d34d"
EXPECTED_JSON_BYTES = 163821
EXPECTED_JSON_SHA256 = "08294c48b79b574593d3ba6c96e0baf885ae8ab8692a89abf8eb46e4e78d7057"
EXPECTED_SOURCE_REVISION = "74991baa54b346389b6193c1f7075954055be181"
EXPECTED_CANDIDATES = {
    "tagged-16-boxed", "tagged-16-representation-aware",
    "nanbox-8-boxed", "nanbox-8-representation-aware",
}


class ArchiveValidationError(ValueError):
    """Raised when the committed OGM-38 raw archive is incomplete or corrupt."""


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def load_committed_archive(
    parts_dir: Path = DEFAULT_PARTS_DIR,
) -> tuple[bytes, dict[str, Any]]:
    """Join Base64 fragments, validate archive checksums and check its measurement inventory."""
    names = [f"part-{index:02d}.b64" for index in range(EXPECTED_PART_COUNT)]
    actual_names = sorted(path.name for path in parts_dir.glob("part-*.b64"))
    if actual_names != names:
        raise ArchiveValidationError(f"expected {names}, found {actual_names} in {parts_dir}")

    chunks = [(parts_dir / name).read_bytes() for name in names]
    lengths = [len(chunk) for chunk in chunks]
    if lengths != EXPECTED_PART_LENGTHS:
        raise ArchiveValidationError(f"unexpected Base64 fragment sizes: {lengths}")
    encoded = b"".join(chunks)
    if len(encoded) != EXPECTED_BASE64_BYTES:
        raise ArchiveValidationError(f"Base64 archive length mismatch: {len(encoded)}")
    try:
        compressed = base64.b64decode(encoded, validate=True)
    except ValueError as exc:
        raise ArchiveValidationError("Base64 fragments are malformed") from exc
    if len(compressed) != EXPECTED_XZ_BYTES or sha256(compressed) != EXPECTED_XZ_SHA256:
        raise ArchiveValidationError("reconstructed XZ length or SHA-256 mismatch")

    try:
        raw_json = lzma.decompress(compressed)
    except lzma.LZMAError as exc:
        raise ArchiveValidationError("reconstructed XZ archive cannot be decompressed") from exc
    if len(raw_json) != EXPECTED_JSON_BYTES or sha256(raw_json) != EXPECTED_JSON_SHA256:
        raise ArchiveValidationError("decompressed JSON length or SHA-256 mismatch")
    try:
        report = json.loads(raw_json)
    except json.JSONDecodeError as exc:
        raise ArchiveValidationError("decompressed raw report is invalid JSON") from exc

    if report.get("schema") != "ogma.ogm38.raw-measurements-delta" or report.get("version") != 1:
        raise ArchiveValidationError("raw-report schema/version mismatch")
    if report.get("source_revision") != EXPECTED_SOURCE_REVISION:
        raise ArchiveValidationError("raw-report source revision mismatch")
    if report.get("tested_revision") != EXPECTED_SOURCE_REVISION:
        raise ArchiveValidationError("raw-report tested revision mismatch")
    if report.get("samples") != 101 or report.get("warmups") != 3:
        raise ArchiveValidationError("raw-report sampling configuration mismatch")

    series = report.get("metric_arrays")
    if not isinstance(series, list) or len(series) != 156:
        raise ArchiveValidationError("expected 156 measurement series")
    seen: set[str] = set()
    for item in series:
        if not isinstance(item, list) or len(item) != 3:
            raise ArchiveValidationError("malformed delta series")
        name, first, deltas = item
        if not isinstance(name, str) or name in seen:
            raise ArchiveValidationError("missing or duplicate metric name")
        seen.add(name)
        if isinstance(first, bool) or not isinstance(first, int):
            raise ArchiveValidationError(f"invalid initial sample in {name}")
        if not isinstance(deltas, list) or len(deltas) != 100:
            raise ArchiveValidationError(f"{name} does not encode 101 samples")
        if any(isinstance(value, bool) or not isinstance(value, int) for value in deltas):
            raise ArchiveValidationError(f"invalid delta in {name}")

    workers = report.get("workers")
    if not isinstance(workers, list) or len(workers) != 36:
        raise ArchiveValidationError("expected 36 collector/allocator workers")
    if sum(worker.get("type") == "collector" for worker in workers) != 24:
        raise ArchiveValidationError("expected 24 collector workers")
    if sum(worker.get("type") == "allocator" for worker in workers) != 12:
        raise ArchiveValidationError("expected 12 allocator workers")

    candidates = report.get("candidates")
    if not isinstance(candidates, list) or {
        candidate.get("name") for candidate in candidates
    } != EXPECTED_CANDIDATES:
        raise ArchiveValidationError("candidate inventory mismatch")
    for candidate in candidates:
        if candidate.get("native_semantic_case_count") != 31:
            raise ArchiveValidationError("candidate semantic-case count mismatch")
        if candidate.get("native_semantic_batch_dispatch_invocations") != 31:
            raise ArchiveValidationError("candidate native batch count mismatch")
        if candidate.get("native_value_dispatches") != 77:
            raise ArchiveValidationError("candidate Value-dispatch count mismatch")
    return compressed, report


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--parts-dir", type=Path, default=DEFAULT_PARTS_DIR)
    parser.add_argument("--output", type=Path, default=DEFAULT_OUTPUT)
    parser.add_argument("--verify-only", action="store_true")
    args = parser.parse_args()

    compressed, report = load_committed_archive(args.parts_dir)
    if not args.verify_only:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_bytes(compressed)
    print(f"source_revision={report['source_revision']}")
    print(f"metric_series={len(report['metric_arrays'])}")
    print(f"worker_records={len(report['workers'])}")
    print(f"xz_bytes={len(compressed)}")
    print(f"xz_sha256={sha256(compressed)}")
    print("mode=verify-only" if args.verify_only else f"output={args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
