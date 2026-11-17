"""Canonical Core IR corpus loader for OGM-17."""
from __future__ import annotations

from dataclasses import dataclass
import hashlib
import json
from pathlib import Path, PurePosixPath
from typing import Any

CONTRACT = "ogma.core-ir"
VERSION = "1.0.0"
DEFAULT_MANIFEST = "tests/core-ir/manifest.json"


@dataclass(frozen=True)
class Fixture:
    path: str
    sha256: str
    document: dict[str, Any]


@dataclass(frozen=True)
class Corpus:
    manifest: str
    manifest_sha256: str
    corpus_sha256: str
    fixtures: tuple[Fixture, ...]


def repository_root() -> Path:
    return Path(__file__).resolve().parents[2]


def load_corpus(root: Path | None = None, manifest: str = DEFAULT_MANIFEST) -> Corpus:
    root = (root or repository_root()).resolve()
    relative = PurePosixPath(manifest.replace("\\", "/"))
    if relative.is_absolute() or ".." in relative.parts or "\\" in manifest:
        raise ValueError("manifest must be a normalized repository-relative path")

    manifest_path = (root / Path(*relative.parts)).resolve()
    if root not in manifest_path.parents:
        raise ValueError("manifest path escapes repository root")
    manifest_bytes = manifest_path.read_bytes()
    manifest_doc = json.loads(manifest_bytes)
    if manifest_doc.get("contract") != CONTRACT or manifest_doc.get("version") != VERSION:
        raise ValueError("Core IR manifest contract/version mismatch")

    entries = manifest_doc.get("fixtures")
    if not isinstance(entries, list):
        raise ValueError("Core IR manifest fixtures must be an array")

    seen: set[str] = set()
    fixtures: list[Fixture] = []
    digest = hashlib.sha256()
    digest.update(CONTRACT.encode("ascii") + b"\0" + VERSION.encode("ascii") + b"\0")

    for entry in entries:
        if not isinstance(entry, dict) or entry.get("expect") not in {"accepted", "rejected"}:
            raise ValueError("invalid Core IR manifest fixture entry")
        raw_path = entry.get("path")
        if not isinstance(raw_path, str) or not raw_path:
            raise ValueError("fixture path must be a non-empty string")
        rel = PurePosixPath(raw_path)
        if rel.is_absolute() or ".." in rel.parts or "\\" in raw_path or rel.as_posix() != raw_path:
            raise ValueError(f"unsafe or non-normalized fixture path: {raw_path!r}")
        if raw_path in seen:
            raise ValueError(f"duplicate Core IR fixture path: {raw_path}")
        seen.add(raw_path)

        # Rejected fixtures are verifier regressions, not compiler workload inputs.
        if entry["expect"] != "accepted":
            continue

        fixture_path = (root / Path(*rel.parts)).resolve()
        if root not in fixture_path.parents:
            raise ValueError(f"fixture escapes repository root: {raw_path}")
        fixture_bytes = fixture_path.read_bytes()
        document = json.loads(fixture_bytes)
        if document.get("contract") != CONTRACT or document.get("version") != VERSION:
            raise ValueError(f"unexpected Core IR envelope in {raw_path}")
        if not isinstance(document.get("function"), dict):
            raise ValueError(f"missing function document in {raw_path}")

        fixture_hash = hashlib.sha256(fixture_bytes).hexdigest()
        fixtures.append(Fixture(raw_path, fixture_hash, document))
        digest.update(raw_path.encode("utf-8") + b"\0")
        digest.update(fixture_bytes + b"\0")

    if not fixtures:
        raise ValueError("canonical manifest has no accepted Core IR fixtures")

    return Corpus(
        manifest=relative.as_posix(),
        manifest_sha256=hashlib.sha256(manifest_bytes).hexdigest(),
        corpus_sha256=digest.hexdigest(),
        fixtures=tuple(fixtures),
    )
