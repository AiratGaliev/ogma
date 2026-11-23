#!/usr/bin/env python3
"""Reconstruct and verify the Git-tracked OGM-38 raw native measurement archive."""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import lzma
from pathlib import Path
from dataclasses import dataclass
from typing import Any

HERE = Path(__file__).resolve().parent
DEFAULT_PARTS_DIR = HERE / "results" / "ogm38-raw-native-samples.delta.json.xz.b64.parts"
DEFAULT_OUTPUT = HERE / "results" / "ogm38-raw-native-samples.delta.json.xz"
CANDIDATE_NAMES = frozenset({
    "tagged-16-boxed",
    "tagged-16-representation-aware",
    "nanbox-8-boxed",
    "nanbox-8-representation-aware",
})


@dataclass(frozen=True)
class ArchiveExpectation:
    """Immutable provenance and integrity contract for one raw measurement snapshot."""

    part_lengths: tuple[int, ...]
    base64_bytes: int
    xz_bytes: int
    xz_sha256: str
    json_bytes: int
    json_sha256: str
    source_revision: str
    candidate_names: frozenset[str] = CANDIDATE_NAMES
    samples: int = 101
    warmups: int = 3
    metric_series: int = 156
    worker_records: int = 36
    collector_workers: int = 24
    allocator_workers: int = 12
    semantic_case_count: int = 31
    semantic_batch_dispatch_invocations: int = 31
    native_value_dispatches: int = 77

    @property
    def part_count(self) -> int:
        return len(self.part_lengths)


ACTIVE_ARCHIVE = ArchiveExpectation(
    part_lengths=(4000,) * 12 + (3984,),
    base64_bytes=51984,
    xz_bytes=38988,
    xz_sha256="a6f4e0485194cda809af97d5ce84049b0a7ec257adc80d3ee9150dddfe52b5af",
    json_bytes=163793,
    json_sha256="3a165dc0e13c9bb92e7e773b4a39f51dc0fa875f255cd5411db9a364fe7bfe59",
    source_revision="69038fbbd6246f14466ab91821524f7cc0cfb04c",
)

HISTORICAL_ARCHIVE = ArchiveExpectation(
    part_lengths=(4000,) * 13 + (128,),
    base64_bytes=52128,
    xz_bytes=39096,
    xz_sha256="ebe31be1a8af18dda5f22b4dd078518fa7a492fb0a7038798ac572b7ba38d34d",
    json_bytes=163821,
    json_sha256="08294c48b79b574593d3ba6c96e0baf885ae8ab8692a89abf8eb46e4e78d7057",
    source_revision="74991baa54b346389b6193c1f7075954055be181",
)

HISTORICAL_PARTS_DIR = (
    HERE / "results" / "historical"
    / "ogm38-raw-native-samples-74991.delta.json.xz.b64.parts"
)

# Backward-compatible aliases for callers validating the current snapshot.
EXPECTED_PART_COUNT = ACTIVE_ARCHIVE.part_count
EXPECTED_PART_LENGTHS = list(ACTIVE_ARCHIVE.part_lengths)
EXPECTED_BASE64_BYTES = ACTIVE_ARCHIVE.base64_bytes
EXPECTED_XZ_BYTES = ACTIVE_ARCHIVE.xz_bytes
EXPECTED_XZ_SHA256 = ACTIVE_ARCHIVE.xz_sha256
EXPECTED_JSON_BYTES = ACTIVE_ARCHIVE.json_bytes
EXPECTED_JSON_SHA256 = ACTIVE_ARCHIVE.json_sha256
EXPECTED_SOURCE_REVISION = ACTIVE_ARCHIVE.source_revision
EXPECTED_CANDIDATES = set(ACTIVE_ARCHIVE.candidate_names)


class ArchiveValidationError(ValueError):
    """Raised when the committed OGM-38 raw archive is incomplete or corrupt."""


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def load_committed_archive(
    parts_dir: Path = DEFAULT_PARTS_DIR,
    *,
    expectation: ArchiveExpectation = ACTIVE_ARCHIVE,
) -> tuple[bytes, dict[str, Any]]:
    """Join Base64 fragments and validate them against the selected immutable snapshot contract."""
    names = [f"part-{index:02d}.b64" for index in range(expectation.part_count)]
    actual_names = sorted(path.name for path in parts_dir.glob("part-*.b64"))
    if actual_names != names:
        raise ArchiveValidationError(f"expected {names}, found {actual_names} in {parts_dir}")

    chunks = [(parts_dir / name).read_bytes() for name in names]
    lengths = [len(chunk) for chunk in chunks]
    if lengths != list(expectation.part_lengths):
        raise ArchiveValidationError(
            f"unexpected Base64 fragment sizes: {lengths}; "
            f"expected {list(expectation.part_lengths)}"
        )
    encoded = b"".join(chunks)
    if len(encoded) != expectation.base64_bytes:
        raise ArchiveValidationError(
            f"Base64 archive length mismatch: {len(encoded)}; expected {expectation.base64_bytes}"
        )
    try:
        compressed = base64.b64decode(encoded, validate=True)
    except ValueError as exc:
        raise ArchiveValidationError("Base64 fragments are malformed") from exc
    if len(compressed) != expectation.xz_bytes or sha256(compressed) != expectation.xz_sha256:
        raise ArchiveValidationError("reconstructed XZ length or SHA-256 mismatch")

    try:
        raw_json = lzma.decompress(compressed)
    except lzma.LZMAError as exc:
        raise ArchiveValidationError("reconstructed XZ archive cannot be decompressed") from exc
    if len(raw_json) != expectation.json_bytes or sha256(raw_json) != expectation.json_sha256:
        raise ArchiveValidationError("decompressed JSON length or SHA-256 mismatch")
    try:
        report = json.loads(raw_json)
    except json.JSONDecodeError as exc:
        raise ArchiveValidationError("decompressed raw report is invalid JSON") from exc

    if report.get("schema") != "ogma.ogm38.raw-measurements-delta" or report.get("version") != 1:
        raise ArchiveValidationError("raw-report schema/version mismatch")
    if report.get("source_revision") != expectation.source_revision:
        raise ArchiveValidationError("raw-report source revision mismatch")
    if report.get("tested_revision") != expectation.source_revision:
        raise ArchiveValidationError("raw-report tested revision mismatch")
    if report.get("samples") != expectation.samples or report.get("warmups") != expectation.warmups:
        raise ArchiveValidationError("raw-report sampling configuration mismatch")

    series = report.get("metric_arrays")
    if not isinstance(series, list) or len(series) != expectation.metric_series:
        raise ArchiveValidationError(
            f"expected {expectation.metric_series} measurement series"
        )
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
        if not isinstance(deltas, list) or len(deltas) != expectation.samples - 1:
            raise ArchiveValidationError(
                f"{name} does not encode {expectation.samples} samples"
            )
        if any(isinstance(value, bool) or not isinstance(value, int) for value in deltas):
            raise ArchiveValidationError(f"invalid delta in {name}")

    workers = report.get("workers")
    if not isinstance(workers, list) or len(workers) != expectation.worker_records:
        raise ArchiveValidationError(
            f"expected {expectation.worker_records} collector/allocator workers"
        )
    if sum(worker.get("type") == "collector" for worker in workers) != expectation.collector_workers:
        raise ArchiveValidationError(f"expected {expectation.collector_workers} collector workers")
    if sum(worker.get("type") == "allocator" for worker in workers) != expectation.allocator_workers:
        raise ArchiveValidationError(f"expected {expectation.allocator_workers} allocator workers")

    candidates = report.get("candidates")
    if not isinstance(candidates, list) or {
        candidate.get("name") for candidate in candidates
    } != set(expectation.candidate_names):
        raise ArchiveValidationError("candidate inventory mismatch")
    for candidate in candidates:
        if candidate.get("native_semantic_case_count") != expectation.semantic_case_count:
            raise ArchiveValidationError("candidate semantic-case count mismatch")
        if (
            candidate.get("native_semantic_batch_dispatch_invocations")
            != expectation.semantic_batch_dispatch_invocations
        ):
            raise ArchiveValidationError("candidate native batch count mismatch")
        if candidate.get("native_value_dispatches") != expectation.native_value_dispatches:
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
