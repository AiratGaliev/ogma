#!/usr/bin/env python3
"""Pinned, adapter-driven conformance runner for the Ogma M0 test infrastructure.

The runner deliberately does not implement ECMAScript. It prepares deterministic
requests for an engine adapter and validates the adapter's observable result.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import queue
import shlex
import subprocess
import sys
import threading
import time
from collections import deque
import xml.etree.ElementTree as ET
from pathlib import Path, PurePosixPath
from typing import Any


SCHEMA_VERSION = 1
RESULT_STATUSES = (
    "PASS",
    "FAIL",
    "UNSUPPORTED",
    "EXCLUDED",
    "HARNESS ERROR",
    "UNEXPECTED PASS",
)
FAILURE_STATUSES = {"FAIL", "HARNESS ERROR", "UNEXPECTED PASS"}
FEATURE_STATUSES = {"supported", "partial", "unsupported", "deferred"}
NEGATIVE_PHASES = {"parse", "resolution", "runtime"}


class RunnerError(Exception):
    """Invalid runner input, manifest, corpus or adapter contract."""


def canonical_json(value: Any) -> bytes:
    return json.dumps(
        value, ensure_ascii=False, sort_keys=True, separators=(",", ":")
    ).encode("utf-8")


def sha256_json(value: Any) -> str:
    return hashlib.sha256(canonical_json(value)).hexdigest()


def read_json(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise RunnerError(f"cannot read JSON file {path}: {exc}") from exc
    if not isinstance(value, dict):
        raise RunnerError(f"{path}: top-level JSON value must be an object")
    return value


def write_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
        newline="\n",
    )


def load_baseline(path: Path) -> dict[str, Any]:
    baseline = read_json(path)
    if baseline.get("schemaVersion") != SCHEMA_VERSION:
        raise RunnerError(f"{path}: unsupported baseline schemaVersion")
    repositories = baseline.get("repositories")
    required = {"test262", "test262-parser-tests", "quickjs-ng"}
    if not isinstance(repositories, dict) or not required.issubset(repositories):
        raise RunnerError(f"{path}: missing pinned repository entries")
    for name in required:
        item = repositories[name]
        if not isinstance(item, dict) or not item.get("revision"):
            raise RunnerError(f"{path}: {name} must have an immutable revision")
        revision = item["revision"]
        if len(revision) != 40 or any(c not in "0123456789abcdef" for c in revision):
            raise RunnerError(f"{path}: {name}.revision must be a 40-character commit SHA")
    return baseline


def load_profile(path: Path) -> dict[str, Any]:
    profile = read_json(path)
    if profile.get("schemaVersion") != SCHEMA_VERSION:
        raise RunnerError(f"{path}: unsupported profile schemaVersion")
    if not profile.get("profileId") or not profile.get("version"):
        raise RunnerError(f"{path}: profileId and version are required")
    execution = profile.get("execution")
    if not isinstance(execution, dict) or execution.get("state") not in {
        "not_implemented",
        "ready",
    }:
        raise RunnerError(f"{path}: execution.state must be not_implemented or ready")
    parser_status = profile.get("parserStatus", "not_implemented")
    if parser_status not in {"not_implemented", "ready"}:
        raise RunnerError(f"{path}: parserStatus must be not_implemented or ready")
    default_status = profile.get("defaultFeatureStatus")
    if default_status not in FEATURE_STATUSES:
        raise RunnerError(f"{path}: invalid defaultFeatureStatus")
    feature_status = profile.get("featureStatus", {})
    if not isinstance(feature_status, dict):
        raise RunnerError(f"{path}: featureStatus must be an object")
    for feature, status in feature_status.items():
        if not isinstance(feature, str) or status not in FEATURE_STATUSES:
            raise RunnerError(f"{path}: invalid status for feature {feature!r}")
    exclusions = profile.get("exclusions", [])
    if not isinstance(exclusions, list):
        raise RunnerError(f"{path}: exclusions must be an array")
    seen_exclusions: set[tuple[str, str]] = set()
    for entry in exclusions:
        if not isinstance(entry, dict):
            raise RunnerError(f"{path}: each exclusion must be an object")
        suite = entry.get("suite", "*")
        if suite not in {"*", "test262", "test262-parser-tests"}:
            raise RunnerError(f"{path}: invalid exclusion suite {suite!r}")
        test_id = entry.get("testId")
        required_fields = ("reason", "feature", "owner", "revisitCondition")
        if not isinstance(test_id, str) or not test_id:
            raise RunnerError(f"{path}: every exclusion requires an exact testId")
        if any(not isinstance(entry.get(key), str) or not entry[key].strip() for key in required_fields):
            raise RunnerError(
                f"{path}: exclusion {test_id!r} requires reason, feature, owner and revisitCondition"
            )
        key = (suite, test_id)
        if key in seen_exclusions:
            raise RunnerError(f"{path}: duplicate exclusion {suite}:{test_id}")
        seen_exclusions.add(key)
    return profile


def _strip_yaml_comment(value: str) -> str:
    quote: str | None = None
    depth = 0
    escaped = False
    for index, char in enumerate(value):
        if quote == '"':
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == quote:
                quote = None
            continue
        if quote == "'":
            if char == quote:
                quote = None
            continue
        if char in {"'", '"'}:
            quote = char
        elif char in "[{":
            depth += 1
        elif char in "]}":
            depth -= 1
        elif char == "#" and depth == 0 and (index == 0 or value[index - 1].isspace()):
            return value[:index].rstrip()
    return value.rstrip()


def _split_flow_items(value: str) -> list[str]:
    items: list[str] = []
    start = 0
    quote: str | None = None
    escaped = False
    depth = 0
    for index, char in enumerate(value):
        if quote == '"':
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == quote:
                quote = None
            continue
        if quote == "'":
            if char == quote:
                quote = None
            continue
        if char in {"'", '"'}:
            quote = char
        elif char in "[{":
            depth += 1
        elif char in "]}":
            depth -= 1
        elif char == "," and depth == 0:
            items.append(value[start:index].strip())
            start = index + 1
    tail = value[start:].strip()
    if tail:
        items.append(tail)
    return items


def _yaml_scalar(value: str) -> Any:
    value = _strip_yaml_comment(value).strip()
    if value == "":
        return None
    if value.startswith("[") and value.endswith("]"):
        inner = value[1:-1].strip()
        return [] if not inner else [_yaml_scalar(item) for item in _split_flow_items(inner)]
    if value.startswith("{") and value.endswith("}"):
        inner = value[1:-1].strip()
        result: dict[str, Any] = {}
        for item in _split_flow_items(inner):
            key, separator, raw_value = item.partition(":")
            if not separator:
                raise RunnerError(f"invalid YAML flow mapping entry: {item!r}")
            parsed_key = str(_yaml_scalar(key))
            if parsed_key in result:
                raise RunnerError(f"duplicate YAML key: {parsed_key}")
            result[parsed_key] = _yaml_scalar(raw_value)
        return result
    if value.startswith('"'):
        try:
            return json.loads(value)
        except json.JSONDecodeError as exc:
            raise RunnerError(f"invalid quoted YAML scalar: {value!r}") from exc
    if value.startswith("'") and value.endswith("'"):
        return value[1:-1].replace("''", "'")
    lowered = value.lower()
    if lowered in {"true", "false"}:
        return lowered == "true"
    if lowered in {"null", "~"}:
        return None
    if value.startswith(("!", "&", "*")):
        raise RunnerError("YAML tags, anchors and aliases are not supported in Test262 metadata")
    return value


def _next_content_line(lines: list[str], index: int) -> int:
    while index < len(lines):
        stripped = lines[index].strip()
        if stripped and not stripped.startswith("#"):
            return index
        index += 1
    return len(lines)


def _parse_yaml_block(
    lines: list[str], index: int, indent: int
) -> tuple[Any, int]:
    first = _next_content_line(lines, index)
    if first >= len(lines):
        return {}, first
    first_indent = len(lines[first]) - len(lines[first].lstrip(" "))
    if first_indent != indent:
        raise RunnerError("inconsistent YAML indentation in Test262 frontmatter")
    is_list = lines[first][indent:].startswith("- ")
    if is_list:
        result_list: list[Any] = []
        while True:
            current = _next_content_line(lines, index)
            if current >= len(lines):
                return result_list, current
            raw = lines[current]
            current_indent = len(raw) - len(raw.lstrip(" "))
            if current_indent < indent:
                return result_list, current
            if current_indent != indent or not raw[indent:].startswith("- "):
                return result_list, current
            item = raw[indent + 2 :].strip()
            if not item:
                raise RunnerError("nested YAML list items are not supported in Test262 metadata")
            result_list.append(_yaml_scalar(item))
            index = current + 1

    result: dict[str, Any] = {}
    while True:
        current = _next_content_line(lines, index)
        if current >= len(lines):
            return result, current
        raw = lines[current]
        current_indent = len(raw) - len(raw.lstrip(" "))
        if current_indent < indent:
            return result, current
        if current_indent != indent:
            raise RunnerError("inconsistent YAML indentation in Test262 frontmatter")
        text = _strip_yaml_comment(raw[indent:]).strip()
        if text.startswith("- "):
            return result, current
        key, separator, raw_value = text.partition(":")
        if not separator or not key.strip():
            raise RunnerError(f"invalid Test262 metadata line: {text!r}")
        key = key.strip()
        if key in result:
            raise RunnerError(f"duplicate Test262 metadata key: {key}")
        raw_value = raw_value.strip()
        index = current + 1
        if raw_value in {"|", "|-", "|+", ">", ">-", ">+"}:
            block_lines: list[str] = []
            while index < len(lines):
                candidate = lines[index]
                if candidate.strip() and len(candidate) - len(candidate.lstrip(" ")) <= indent:
                    break
                if candidate.strip():
                    block_indent = len(candidate) - len(candidate.lstrip(" "))
                    block_lines.append(candidate[min(block_indent, indent + 2) :])
                else:
                    block_lines.append("")
                index += 1
            result[key] = "\n".join(block_lines)
        elif raw_value:
            result[key] = _yaml_scalar(raw_value)
        else:
            child = _next_content_line(lines, index)
            if child < len(lines):
                child_indent = len(lines[child]) - len(lines[child].lstrip(" "))
                if child_indent > indent:
                    result[key], index = _parse_yaml_block(lines, child, child_indent)
                else:
                    result[key] = None
            else:
                result[key] = None


def parse_test262_metadata(source: str) -> dict[str, Any]:
    lines = source.splitlines()
    start = next(
        (index for index, line in enumerate(lines) if line.strip() == "/*---"),
        None,
    )
    if start is None:
        # Test262 metadata is optional. Unannotated tests use the default
        # non-raw, script, sloppy+strict execution contract.
        return {}
    end = next(
        (
            index
            for index in range(start + 1, len(lines))
            if lines[index].strip() == "---*/"
        ),
        None,
    )
    if end is None:
        raise RunnerError("unterminated Test262 YAML frontmatter")
    metadata, consumed = _parse_yaml_block(lines[start + 1 : end], 0, 0)
    if consumed < len(lines[start + 1 : end]):
        raise RunnerError("unparsed content in Test262 YAML frontmatter")
    if not isinstance(metadata, dict):
        raise RunnerError("Test262 frontmatter must be a YAML mapping")
    return metadata


def _string_list(metadata: dict[str, Any], key: str) -> list[str]:
    value = metadata.get(key, [])
    if value is None:
        return []
    if not isinstance(value, list) or any(not isinstance(item, str) for item in value):
        raise RunnerError(f"Test262 metadata {key!r} must be a list of strings")
    if len(value) != len(set(value)):
        raise RunnerError(f"Test262 metadata {key!r} contains duplicates")
    return value


def test262_modes(flags: list[str]) -> list[str]:
    if "onlyStrict" in flags and "noStrict" in flags:
        raise RunnerError("Test262 flags onlyStrict and noStrict are mutually exclusive")
    if "raw" in flags:
        if "onlyStrict" in flags or "module" in flags or "async" in flags:
            raise RunnerError("raw tests cannot also specify onlyStrict, module or async")
        return ["sloppy"]
    if "module" in flags:
        if "onlyStrict" in flags or "noStrict" in flags:
            raise RunnerError("module tests cannot also specify onlyStrict or noStrict")
        return ["module"]
    if "onlyStrict" in flags:
        return ["strict"]
    if "noStrict" in flags:
        return ["sloppy"]
    return ["sloppy", "strict"]


def _safe_relative_path(value: str) -> PurePosixPath:
    if (
        not isinstance(value, str)
        or not value
        or "\\" in value
        or "\x00" in value
        or ":" in value
    ):
        raise RunnerError(f"unsafe relative path: {value!r}")
    path = PurePosixPath(value)
    if path.is_absolute() or not path.parts or ".." in path.parts or "." in path.parts:
        raise RunnerError(f"unsafe relative path: {value!r}")
    return path


def _load_harness_includes(
    harness_root: Path,
    names: list[str],
    raw: bool,
    async_test: bool,
) -> list[dict[str, str]]:
    if raw:
        if names:
            raise RunnerError("raw Test262 tests must not declare harness includes")
        return []
    ordered_names = ["assert.js", "sta.js"]
    if async_test:
        ordered_names.append("doneprintHandle.js")
    ordered_names.extend(names)
    result: list[dict[str, str]] = []
    seen: set[str] = set()
    for name in ordered_names:
        relative = _safe_relative_path(name)
        if not relative.suffix:
            relative = relative.with_suffix(".js")
        canonical = relative.as_posix()
        if canonical in seen:
            continue
        seen.add(canonical)
        root_path = harness_root.resolve()
        file_path = root_path.joinpath(*relative.parts)
        try:
            resolved_path = file_path.resolve(strict=True)
            resolved_path.relative_to(root_path)
            if not resolved_path.is_file():
                raise RunnerError(f"Test262 harness include is not a file: {canonical}")
            source = resolved_path.read_text(encoding="utf-8")
        except RunnerError:
            raise
        except (OSError, UnicodeError, ValueError) as exc:
            raise RunnerError(f"cannot safely load Test262 harness include {canonical}: {exc}") from exc
        result.append({"name": canonical, "source": source})
    return result


def _pinned_source(baseline: dict[str, Any], suite: str) -> dict[str, Any]:
    key = "test262" if suite == "test262" else "test262-parser-tests"
    item = baseline["repositories"][key]
    return {
        "repository": item["url"],
        "revision": item["revision"],
    }


def _parser_equivalence_key(test_id: str) -> str | None:
    relative = _safe_relative_path(test_id)
    if relative.parts[0] not in {"pass", "pass-explicit"}:
        return None
    return PurePosixPath(*relative.parts[1:]).as_posix()


def _validate_parser_equivalence_pairs(test_ids: list[str]) -> None:
    passed = {
        key
        for test_id in test_ids
        if test_id.startswith("pass/")
        and (key := _parser_equivalence_key(test_id)) is not None
    }
    explicit = {
        key
        for test_id in test_ids
        if test_id.startswith("pass-explicit/")
        and (key := _parser_equivalence_key(test_id)) is not None
    }
    if passed != explicit:
        missing_explicit = sorted(passed - explicit)
        missing_original = sorted(explicit - passed)
        raise RunnerError(
            "parser pass/pass-explicit corpus pairs do not match; "
            f"missing pass-explicit={missing_explicit[:10]}, "
            f"missing pass={missing_original[:10]}"
        )


def _shard_assignment_key(suite: str, test_id: str) -> str:
    if suite == "test262-parser-tests":
        equivalence_key = _parser_equivalence_key(test_id)
        if equivalence_key is not None:
            return f"parser-pass-equivalence:{equivalence_key}"
    return test_id


def discover_test_ids(suite_root: Path, suite: str) -> list[str]:
    if suite == "test262":
        test_root = suite_root / "test"
        if not test_root.is_dir():
            raise RunnerError(f"Test262 test directory does not exist: {test_root}")
        return sorted(
            path.relative_to(test_root).as_posix()
            for path in test_root.rglob("*.js")
            if path.is_file() and "_FIXTURE" not in path.name
        )
    if suite == "test262-parser-tests":
        ids: list[str] = []
        for category in ("pass", "pass-explicit", "early", "fail"):
            category_root = suite_root / category
            if not category_root.is_dir():
                raise RunnerError(f"parser corpus directory does not exist: {category_root}")
            ids.extend(
                path.relative_to(suite_root).as_posix()
                for path in category_root.rglob("*.js")
                if path.is_file()
            )
        normalized_ids = sorted(ids)
        _validate_parser_equivalence_pairs(normalized_ids)
        return normalized_ids
    raise RunnerError(f"unsupported suite: {suite}")


def _manifest_profile(profile: dict[str, Any]) -> dict[str, Any]:
    execution = profile.get("execution")
    if not isinstance(execution, dict) or execution.get("state") not in {
        "not_implemented",
        "ready",
    }:
        raise RunnerError("invalid execution state in active feature profile")
    parser_status = profile.get("parserStatus", "not_implemented")
    if parser_status not in {"not_implemented", "ready"}:
        raise RunnerError("invalid parserStatus in active feature profile")
    return {
        "id": profile["profileId"],
        "version": profile["version"],
        "sha256": sha256_json(profile),
        "executionState": execution["state"],
        "parserStatus": parser_status,
    }


def build_manifest(
    suite: str,
    test_ids: list[str],
    baseline: dict[str, Any],
    profile: dict[str, Any],
    shard_count: int,
) -> dict[str, Any]:
    if suite not in {"test262", "test262-parser-tests"}:
        raise RunnerError(f"unsupported suite: {suite}")
    if shard_count < 1:
        raise RunnerError("shard-count must be >= 1")
    normalized_ids = sorted(test_ids)
    if len(normalized_ids) != len(set(normalized_ids)):
        raise RunnerError("test corpus contains duplicate test IDs")
    if suite == "test262-parser-tests":
        _validate_parser_equivalence_pairs(normalized_ids)
    source = _pinned_source(baseline, suite)
    assignments: dict[int, list[str]] = {index: [] for index in range(shard_count)}
    tests: list[dict[str, Any]] = []
    for test_id in normalized_ids:
        assignment_key = _shard_assignment_key(suite, test_id)
        shard = int(hashlib.sha256(assignment_key.encode("utf-8")).hexdigest()[:16], 16) % shard_count
        assignments[shard].append(test_id)
        tests.append({"id": test_id, "shardIndex": shard})
    manifest: dict[str, Any] = {
        "schemaVersion": SCHEMA_VERSION,
        "suite": suite,
        "source": source,
        "profile": _manifest_profile(profile),
        "shardCount": shard_count,
        "tests": tests,
        "shards": [
            {"index": index, "testIds": assignments[index]}
            for index in range(shard_count)
        ],
    }
    manifest["manifestSha256"] = sha256_json(manifest)
    return manifest


def validate_manifest(manifest: dict[str, Any]) -> None:
    if manifest.get("schemaVersion") != SCHEMA_VERSION:
        raise RunnerError("unsupported shard manifest schemaVersion")
    if manifest.get("suite") not in {"test262", "test262-parser-tests"}:
        raise RunnerError("invalid suite in shard manifest")
    count = manifest.get("shardCount")
    if not isinstance(count, int) or count < 1:
        raise RunnerError("invalid shardCount in shard manifest")
    profile = manifest.get("profile")
    if (
        not isinstance(profile, dict)
        or not isinstance(profile.get("id"), str)
        or not isinstance(profile.get("version"), str)
        or not isinstance(profile.get("sha256"), str)
        or profile.get("executionState") not in {"not_implemented", "ready"}
        or profile.get("parserStatus") not in {"not_implemented", "ready"}
    ):
        raise RunnerError("shard manifest has an invalid feature profile descriptor")
    source = manifest.get("source")
    if (
        not isinstance(source, dict)
        or not isinstance(source.get("repository"), str)
        or not isinstance(source.get("revision"), str)
        or len(source["revision"]) != 40
        or any(char not in "0123456789abcdef" for char in source["revision"])
    ):
        raise RunnerError("shard manifest has an invalid pinned source descriptor")
    if len(profile["sha256"]) != 64 or any(
        char not in "0123456789abcdef" for char in profile["sha256"]
    ):
        raise RunnerError("shard manifest has an invalid feature profile SHA-256")
    tests = manifest.get("tests")
    shards = manifest.get("shards")
    if not isinstance(tests, list) or not isinstance(shards, list) or len(shards) != count:
        raise RunnerError("shard manifest has an invalid tests/shards collection")
    ids: set[str] = set()
    expected: dict[int, list[str]] = {index: [] for index in range(count)}
    for entry in tests:
        if not isinstance(entry, dict):
            raise RunnerError("invalid test entry in shard manifest")
        test_id = entry.get("id")
        shard = entry.get("shardIndex")
        if not isinstance(test_id, str) or not test_id:
            raise RunnerError("invalid test ID in shard manifest")
        if test_id in ids:
            raise RunnerError(f"duplicate test ID in shard manifest: {test_id}")
        ids.add(test_id)
        if not isinstance(shard, int) or not 0 <= shard < count:
            raise RunnerError(f"invalid shard assignment for {test_id}")
        assignment_key = _shard_assignment_key(manifest["suite"], test_id)
        deterministic = int(hashlib.sha256(assignment_key.encode("utf-8")).hexdigest()[:16], 16) % count
        if deterministic != shard:
            raise RunnerError(f"non-deterministic shard assignment for {test_id}")
        expected[shard].append(test_id)
    if manifest["suite"] == "test262-parser-tests":
        _validate_parser_equivalence_pairs(sorted(ids))
    actual_indices: set[int] = set()
    for shard in shards:
        if not isinstance(shard, dict):
            raise RunnerError("invalid shard record")
        index = shard.get("index")
        if not isinstance(index, int) or index in actual_indices or not 0 <= index < count:
            raise RunnerError("duplicate or invalid shard index")
        actual_indices.add(index)
        if shard.get("testIds") != expected[index]:
            raise RunnerError(f"shard {index} does not match deterministic assignments")
    if actual_indices != set(range(count)):
        raise RunnerError("shard manifest is missing shard definitions")
    digest = manifest.get("manifestSha256")
    unhashed = dict(manifest)
    unhashed.pop("manifestSha256", None)
    if digest != sha256_json(unhashed):
        raise RunnerError("shard manifest SHA-256 does not match its contents")


def _read_test262_descriptor(suite_root: Path, test_id: str) -> dict[str, Any]:
    test_root = suite_root / "test"
    path = test_root.joinpath(*_safe_relative_path(test_id).parts)
    try:
        source = path.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as exc:
        raise RunnerError(f"cannot read Test262 test {test_id}: {exc}") from exc
    metadata = parse_test262_metadata(source)
    features = _string_list(metadata, "features")
    flags = _string_list(metadata, "flags")
    includes = _string_list(metadata, "includes")
    if "_FIXTURE" in PurePosixPath(test_id).name:
        raise RunnerError("Test262 module fixture files must not be run as standalone tests")
    negative = metadata.get("negative")
    if negative is not None:
        if not isinstance(negative, dict):
            raise RunnerError("Test262 metadata negative must be an object")
        phase = negative.get("phase")
        error_type = negative.get("type")
        if phase not in NEGATIVE_PHASES or not isinstance(error_type, str) or not error_type:
            raise RunnerError("negative tests require a supported phase and error type")
        negative = {"phase": phase, "type": error_type}
    raw = "raw" in flags
    async_test = "async" in flags
    harness = _load_harness_includes(
        suite_root / "harness", includes, raw, async_test
    )
    return {
        "id": test_id,
        "suite": "test262",
        "source": source,
        "metadata": metadata,
        "features": features,
        "flags": flags,
        "includes": harness,
        "modes": test262_modes(flags),
        "sourceType": "module" if "module" in flags else "script",
        "raw": raw,
        "async": async_test,
        "negative": negative,
    }


def _read_parser_descriptor(suite_root: Path, test_id: str) -> dict[str, Any]:
    relative = _safe_relative_path(test_id)
    category = relative.parts[0]
    if category not in {"pass", "pass-explicit", "early", "fail"}:
        raise RunnerError(f"unknown parser-tests category in {test_id}")
    path = suite_root.joinpath(*relative.parts)
    try:
        source = path.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as exc:
        raise RunnerError(f"cannot read parser test {test_id}: {exc}") from exc
    return {
        "id": test_id,
        "suite": "test262-parser-tests",
        "source": source,
        "metadata": {},
        "features": [],
        "flags": [],
        "includes": [],
        "modes": ["script"],
        "sourceType": "script",
        "raw": True,
        "async": False,
        "negative": None,
        "parseExpectation": {
            "outcome": "parsed" if category in {"pass", "pass-explicit"} else "rejected",
            "phase": "early" if category == "early" else "parse",
        },
        "equivalenceGroup": (
            _parser_equivalence_key(test_id)
            if category in {"pass", "pass-explicit"}
            else None
        ),
        "equivalenceRole": category if category in {"pass", "pass-explicit"} else None,
    }


def _exclusion_for(profile: dict[str, Any], suite: str, test_id: str) -> dict[str, Any] | None:
    for exclusion in profile.get("exclusions", []):
        if exclusion.get("testId") == test_id and exclusion.get("suite", "*") in {"*", suite}:
            return exclusion
    return None


def _unsupported_reason(profile: dict[str, Any], descriptor: dict[str, Any]) -> str | None:
    if descriptor["suite"] == "test262-parser-tests":
        if profile.get("parserStatus", "not_implemented") != "ready":
            return profile.get("parserReason", "the parser is not available in the active build")
    else:
        execution = profile["execution"]
        if execution["state"] != "ready":
            return execution.get("reason", "the engine execution adapter is not implemented")
    feature_status = profile.get("featureStatus", {})
    default_status = profile["defaultFeatureStatus"]
    deferred = [
        feature
        for feature in descriptor["features"]
        if feature_status.get(feature, default_status) in {"unsupported", "deferred"}
    ]
    if deferred:
        return "required features are outside the active execution profile: " + ", ".join(sorted(deferred))
    return None


class AdapterSession:
    """One persistent line-delimited adapter worker for a whole shard."""

    def __init__(self, command: list[str]) -> None:
        self.command = command
        self.process: subprocess.Popen[str] | None = None
        self.responses: queue.Queue[str | BaseException | None] | None = None
        self.stderr_lines: deque[str] = deque(maxlen=16)
        self.request_id = 0
        self.start_error: str | None = None

    def __enter__(self) -> AdapterSession:
        return self

    def __exit__(self, exc_type: Any, exc: Any, traceback: Any) -> None:
        self.close()

    def _stderr_text(self) -> str:
        return "".join(self.stderr_lines)[-4000:]

    def _start(self) -> bool:
        if self.start_error is not None:
            return False
        if not self.command:
            self.start_error = "no engine adapter command was supplied"
            return False
        try:
            process = subprocess.Popen(
                self.command,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                encoding="utf-8",
                errors="replace",
                bufsize=1,
            )
        except OSError as exc:
            self.start_error = f"cannot start engine adapter: {exc}"
            return False

        responses: queue.Queue[str | BaseException | None] = queue.Queue()
        self.process = process
        self.responses = responses
        self.stderr_lines.clear()

        def read_stdout() -> None:
            try:
                assert process.stdout is not None
                for line in process.stdout:
                    responses.put(line)
            except BaseException as exc:
                responses.put(exc)
            finally:
                responses.put(None)

        def read_stderr() -> None:
            try:
                assert process.stderr is not None
                for line in process.stderr:
                    self.stderr_lines.append(line[-512:])
            except (OSError, ValueError):
                pass

        threading.Thread(target=read_stdout, daemon=True, name="ogma-adapter-stdout").start()
        threading.Thread(target=read_stderr, daemon=True, name="ogma-adapter-stderr").start()
        return True

    def _stop(self) -> None:
        process = self.process
        self.process = None
        self.responses = None
        if process is None:
            return
        try:
            if process.stdin is not None:
                process.stdin.close()
        except (OSError, ValueError):
            pass
        try:
            process.wait(timeout=0.5)
        except subprocess.TimeoutExpired:
            process.terminate()
            try:
                process.wait(timeout=0.5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
        for stream in (process.stdout, process.stderr):
            if stream is not None:
                try:
                    stream.close()
                except (OSError, ValueError):
                    pass

    def close(self) -> None:
        self._stop()

    def run(
        self, request: dict[str, Any], timeout_ms: int
    ) -> tuple[dict[str, Any] | None, dict[str, Any] | None]:
        if self.process is None and not self._start():
            return None, {
                "status": "HARNESS ERROR",
                "reason": self.start_error or "cannot start engine adapter",
            }
        process = self.process
        responses = self.responses
        if process is None or responses is None:
            return None, {
                "status": "HARNESS ERROR",
                "reason": "engine adapter worker is unavailable",
            }
        if process.poll() is not None:
            stderr = self._stderr_text()
            self._stop()
            return None, {
                "status": "HARNESS ERROR",
                "reason": f"engine adapter worker exited before the shard completed (code {process.returncode})",
                "stderr": stderr,
            }

        self.request_id += 1
        request_id = self.request_id
        request["requestId"] = request_id
        try:
            assert process.stdin is not None
            process.stdin.write(json.dumps(request, ensure_ascii=False) + "\n")
            process.stdin.flush()
        except (BrokenPipeError, OSError, ValueError) as exc:
            stderr = self._stderr_text()
            self._stop()
            return None, {
                "status": "HARNESS ERROR",
                "reason": f"cannot send request to engine adapter: {exc}",
                "stderr": stderr,
            }

        try:
            line = responses.get(timeout=timeout_ms / 1000)
        except queue.Empty:
            self._stop()
            return None, {
                "status": "FAIL",
                "failureKind": "TIMEOUT",
                "reason": f"adapter exceeded the {timeout_ms} ms test timeout",
            }

        if line is None or isinstance(line, BaseException):
            stderr = self._stderr_text()
            self._stop()
            reason = (
                f"engine adapter stdout reader failed: {line}"
                if isinstance(line, BaseException)
                else "engine adapter exited without a response"
            )
            return None, {"status": "HARNESS ERROR", "reason": reason, "stderr": stderr}

        try:
            response = json.loads(line)
        except json.JSONDecodeError as exc:
            stderr = self._stderr_text()
            self._stop()
            return None, {
                "status": "HARNESS ERROR",
                "reason": f"engine adapter emitted invalid JSON: {exc}",
                "stderr": stderr,
                "stdout": line[-4000:],
            }
        if (
            not isinstance(response, dict)
            or response.get("protocol") != "ogma-test-runner-adapter"
            or type(response.get("version")) is not int
            or response.get("version") != 1
            or type(response.get("requestId")) is not int
            or response.get("requestId") != request_id
        ):
            stderr = self._stderr_text()
            self._stop()
            return None, {
                "status": "HARNESS ERROR",
                "reason": "engine adapter response has an invalid protocol envelope or requestId",
                "stderr": stderr,
            }
        return response, None

def _evaluate_test262_response(
    descriptor: dict[str, Any], response: dict[str, Any]
) -> dict[str, Any]:
    outcome = response.get("outcome")
    if outcome == "unsupported":
        return {"status": "UNSUPPORTED", "reason": response.get("reason", "adapter reported unsupported")}
    if outcome == "harness_error":
        return {"status": "HARNESS ERROR", "reason": response.get("reason", "adapter reported a harness error")}
    if outcome not in {"completed", "threw"}:
        return {"status": "HARNESS ERROR", "reason": "adapter returned an invalid Test262 outcome"}
    negative = descriptor["negative"]
    if negative:
        if (
            outcome == "threw"
            and response.get("phase") == negative["phase"]
            and response.get("errorName") == negative["type"]
        ):
            return {"status": "PASS"}
        if outcome == "completed":
            return {
                "status": "FAIL",
                "reason": f"expected {negative['phase']} {negative['type']}, but the test completed",
            }
        return {
            "status": "FAIL",
            "reason": (
                "negative completion mismatch: expected "
                f"{negative['phase']} {negative['type']}, got "
                f"{response.get('phase')} {response.get('errorName')}"
            ),
        }
    if outcome == "threw":
        return {
            "status": "FAIL",
            "reason": f"unexpected {response.get('errorName', 'exception')} during {response.get('phase', 'execution')}",
        }
    if descriptor["async"]:
        async_signal = response.get("asyncSignal")
        if async_signal == "Test262:AsyncTestComplete":
            return {"status": "PASS"}
        if isinstance(async_signal, str) and async_signal.startswith(
            "Test262:AsyncTestFailure:"
        ):
            return {
                "status": "FAIL",
                "failureKind": "ASYNC_FAILURE",
                "reason": async_signal,
            }
        return {
            "status": "FAIL",
            "failureKind": "ASYNC_NOT_COMPLETED",
            "reason": "async Test262 test did not emit the required print completion signal",
        }
    return {"status": "PASS"}


def _evaluate_parser_response(
    descriptor: dict[str, Any], response: dict[str, Any]
) -> dict[str, Any]:
    outcome = response.get("outcome")
    if outcome == "unsupported":
        return {"status": "UNSUPPORTED", "reason": response.get("reason", "parser feature unsupported")}
    if outcome == "harness_error":
        return {"status": "HARNESS ERROR", "reason": response.get("reason", "parser adapter error")}
    expected = descriptor["parseExpectation"]
    if outcome == expected["outcome"] and (
        outcome == "parsed" or response.get("phase") == expected["phase"]
    ):
        result = {"status": "PASS"}
        if descriptor.get("equivalenceGroup") is not None:
            fingerprint = response.get("normalizedAstSha256")
            if (
                not isinstance(fingerprint, str)
                or len(fingerprint) != 64
                or any(char not in "0123456789abcdef" for char in fingerprint)
            ):
                return {
                    "status": "HARNESS ERROR",
                    "reason": (
                        "parser adapter must return normalizedAstSha256 for "
                        "pass/pass-explicit equivalence fixtures"
                    ),
                }
            result["normalizedAstSha256"] = fingerprint
        return result
    return {
        "status": "FAIL",
        "reason": (
            f"expected parser outcome {expected['outcome']} at {expected['phase']}; "
            f"got {outcome} at {response.get('phase')}"
        ),
    }


def _run_variant(
    descriptor: dict[str, Any],
    mode: str,
    adapter_session: AdapterSession,
    suite_root: Path,
    timeout_ms: int,
    async_timeout_ms: int,
) -> dict[str, Any]:
    suite_root = suite_root.resolve()
    if descriptor["suite"] == "test262":
        test_path = PurePosixPath("test", descriptor["id"])
    else:
        test_path = PurePosixPath(descriptor["id"])
    test_directory = suite_root.joinpath(*test_path.parts[:-1]).resolve()
    request = {
        "protocol": "ogma-test-runner-adapter",
        "version": 1,
        "test": {
            "id": descriptor["id"],
            "suite": descriptor["suite"],
            "source": descriptor["source"],
            "mode": mode,
            "sourceType": descriptor["sourceType"],
            "raw": descriptor["raw"],
            "async": descriptor["async"],
            "flags": descriptor["flags"],
            "features": descriptor["features"],
            "includes": descriptor["includes"],
            "negative": descriptor["negative"],
            "parseExpectation": descriptor.get("parseExpectation"),
            "equivalenceGroup": descriptor.get("equivalenceGroup"),
            "equivalenceRole": descriptor.get("equivalenceRole"),
            "corpusRoot": str(suite_root),
            "testPath": test_path.as_posix(),
            "testDirectory": str(test_directory),
        },
        "timeoutMs": async_timeout_ms if descriptor["async"] else timeout_ms,
    }
    effective_timeout = async_timeout_ms if descriptor["async"] else timeout_ms
    response, error = adapter_session.run(request, effective_timeout)
    if error:
        return {"mode": mode, **error}
    assert response is not None
    evaluated = (
        _evaluate_test262_response(descriptor, response)
        if descriptor["suite"] == "test262"
        else _evaluate_parser_response(descriptor, response)
    )
    if "durationMs" in response:
        evaluated["adapterDurationMs"] = response["durationMs"]
    return {"mode": mode, **evaluated}


def _combined_status(variants: list[dict[str, Any]]) -> str:
    statuses = {variant["status"] for variant in variants}
    if "HARNESS ERROR" in statuses:
        return "HARNESS ERROR"
    if "UNEXPECTED PASS" in statuses:
        return "UNEXPECTED PASS"
    if "FAIL" in statuses:
        return "FAIL"
    if statuses == {"UNSUPPORTED"}:
        return "UNSUPPORTED"
    if statuses == {"EXCLUDED"}:
        return "EXCLUDED"
    if "UNSUPPORTED" in statuses:
        return "UNSUPPORTED"
    if "EXCLUDED" in statuses:
        return "EXCLUDED"
    return "PASS"


def run_one_test(
    suite_root: Path,
    suite: str,
    test_id: str,
    profile: dict[str, Any],
    adapter_command: list[str],
    timeout_ms: int,
    async_timeout_ms: int,
    probe_unsupported: bool = False,
    probe_excluded: bool = False,
    adapter_session: AdapterSession | None = None,
) -> dict[str, Any]:
    exclusion = _exclusion_for(profile, suite, test_id)
    if exclusion and not probe_excluded:
        return {
            "id": test_id,
            "status": "EXCLUDED",
            "reason": exclusion["reason"],
            "exclusion": exclusion,
            "variants": [],
        }

    if suite == "test262" and profile["execution"]["state"] != "ready" and not probe_unsupported:
        return {
            "id": test_id,
            "status": "UNSUPPORTED",
            "reason": profile["execution"].get(
                "reason", "the engine execution adapter is not implemented"
            ),
            "variants": [],
        }
    if (
        suite == "test262-parser-tests"
        and profile.get("parserStatus", "not_implemented") != "ready"
        and not probe_unsupported
    ):
        return {
            "id": test_id,
            "status": "UNSUPPORTED",
            "reason": profile.get(
                "parserReason", "the parser adapter is not implemented"
            ),
            "variants": [],
        }

    try:
        descriptor = (
            _read_test262_descriptor(suite_root, test_id)
            if suite == "test262"
            else _read_parser_descriptor(suite_root, test_id)
        )
    except RunnerError as exc:
        return {
            "id": test_id,
            "status": "HARNESS ERROR",
            "reason": str(exc),
            "variants": [],
        }

    unsupported_reason = _unsupported_reason(profile, descriptor)
    if unsupported_reason and not probe_unsupported:
        return {
            "id": test_id,
            "status": "UNSUPPORTED",
            "reason": unsupported_reason,
            "variants": [],
        }

    if adapter_session is None:
        with AdapterSession(adapter_command) as local_session:
            variants = [
                _run_variant(
                    descriptor,
                    mode,
                    local_session,
                    suite_root,
                    timeout_ms,
                    async_timeout_ms,
                )
                for mode in descriptor["modes"]
            ]
    else:
        variants = [
            _run_variant(
                descriptor,
                mode,
                adapter_session,
                suite_root,
                timeout_ms,
                async_timeout_ms,
            )
            for mode in descriptor["modes"]
        ]
    status = _combined_status(variants)
    if exclusion and probe_excluded and status == "PASS":
        status = "UNEXPECTED PASS"
    if unsupported_reason and probe_unsupported and status == "PASS":
        status = "UNEXPECTED PASS"
    reason = (
        "previously excluded test now passes and requires profile review"
        if exclusion and status == "UNEXPECTED PASS"
        else "previously unsupported test now passes and requires profile review"
        if unsupported_reason and status == "UNEXPECTED PASS"
        else next(
            (
                variant.get("reason")
                for variant in variants
                if variant["status"] in FAILURE_STATUSES and variant.get("reason")
            ),
            None,
        )
    )
    result = {
        "id": test_id,
        "status": status,
        "reason": reason,
        "features": descriptor["features"],
        "flags": descriptor["flags"],
        "modes": descriptor["modes"],
        "variants": variants,
    }
    if descriptor.get("equivalenceGroup") is not None:
        result["equivalenceGroup"] = descriptor["equivalenceGroup"]
        result["equivalenceRole"] = descriptor["equivalenceRole"]
    failure_kind = next(
        (variant.get("failureKind") for variant in variants if variant.get("failureKind")),
        None,
    )
    if failure_kind:
        result["failureKind"] = failure_kind
    return result


def summarize(results: list[dict[str, Any]]) -> dict[str, int]:
    summary = {status: 0 for status in RESULT_STATUSES}
    for result in results:
        status = result.get("status")
        if status in summary:
            summary[status] += 1
    summary["total"] = len(results)
    return summary


def write_junit(
    path: Path,
    suite_name: str,
    results: list[dict[str, Any]],
    aggregation_errors: list[str] | None = None,
) -> None:
    summary = summarize(results)
    aggregation_errors = aggregation_errors or []
    root = ET.Element(
        "testsuite",
        {
            "name": suite_name,
            "tests": str(summary["total"] + len(aggregation_errors)),
            "failures": str(summary["FAIL"] + summary["UNEXPECTED PASS"]),
            "errors": str(summary["HARNESS ERROR"] + len(aggregation_errors)),
            "skipped": str(summary["UNSUPPORTED"] + summary["EXCLUDED"]),
        },
    )
    for result in results:
        case = ET.SubElement(root, "testcase", {"name": result["id"], "classname": suite_name})
        status = result["status"]
        reason = result.get("reason") or status
        if status in {"FAIL", "UNEXPECTED PASS"}:
            node = ET.SubElement(case, "failure", {"message": reason, "type": result.get("failureKind", status)})
            node.text = json.dumps(result.get("variants", []), ensure_ascii=False)
        elif status == "HARNESS ERROR":
            node = ET.SubElement(case, "error", {"message": reason, "type": "HARNESS ERROR"})
            node.text = json.dumps(result.get("variants", []), ensure_ascii=False)
        elif status in {"UNSUPPORTED", "EXCLUDED"}:
            ET.SubElement(case, "skipped", {"message": reason, "type": status})
    for index, error in enumerate(aggregation_errors, start=1):
        case = ET.SubElement(
            root,
            "testcase",
            {"name": f"__aggregation_error_{index}__", "classname": suite_name},
        )
        node = ET.SubElement(
            case,
            "error",
            {"message": error, "type": "AGGREGATION ERROR"},
        )
        node.text = error
    path.parent.mkdir(parents=True, exist_ok=True)
    ET.ElementTree(root).write(path, encoding="utf-8", xml_declaration=True)


def _apply_parser_equivalence(results: list[dict[str, Any]]) -> None:
    groups: dict[str, dict[str, dict[str, Any]]] = {}
    for result in results:
        group = result.get("equivalenceGroup")
        role = result.get("equivalenceRole")
        if isinstance(group, str) and role in {"pass", "pass-explicit"}:
            groups.setdefault(group, {})[role] = result

    for group, members in groups.items():
        if set(members) != {"pass", "pass-explicit"}:
            raise RunnerError(f"parser equivalence group is incomplete in shard: {group}")
        original = members["pass"]
        explicit = members["pass-explicit"]
        statuses = {original["status"], explicit["status"]}
        if statuses == {"PASS"}:
            original_hash = original["variants"][0].get("normalizedAstSha256")
            explicit_hash = explicit["variants"][0].get("normalizedAstSha256")
            if original_hash != explicit_hash:
                for result in (original, explicit):
                    result["status"] = "FAIL"
                    result["failureKind"] = "PARSER_AST_MISMATCH"
                    result["reason"] = (
                        "pass/ and pass-explicit/ variants produced different "
                        f"normalized AST fingerprints for {group}"
                    )
            continue
        if len(statuses) == 1:
            continue

        # A passing half of an equivalence pair is not a complete pass when
        # its matching explicit-parentheses variant was not comparable.
        non_pass = [
            result for result in (original, explicit)
            if result["status"] != "PASS"
        ]
        priority = (
            "HARNESS ERROR",
            "FAIL",
            "UNEXPECTED PASS",
            "UNSUPPORTED",
            "EXCLUDED",
        )
        target_status = next(
            (status for status in priority if status in statuses),
            "FAIL",
        )
        target_reason = next(
            (
                result.get("reason")
                for result in non_pass
                if result.get("reason")
            ),
            f"parser equivalence pair {group} did not produce two comparable passes",
        )
        for result in (original, explicit):
            if result["status"] == "PASS":
                result["status"] = target_status
                result["reason"] = target_reason
                result["failureKind"] = "PARSER_EQUIVALENCE_INCOMPLETE"


def run_manifest(
    manifest: dict[str, Any],
    suite_root: Path,
    profile: dict[str, Any],
    adapter_command: list[str],
    shard_index: int,
    timeout_ms: int = 10_000,
    async_timeout_ms: int = 10_000,
    probe_unsupported: bool = False,
    probe_excluded: bool = False,
    verify_corpus: bool = True,
) -> dict[str, Any]:
    validate_manifest(manifest)
    if shard_index < 0 or shard_index >= manifest["shardCount"]:
        raise RunnerError("shard index is outside the manifest range")
    if profile["profileId"] != manifest["profile"]["id"] or profile["version"] != manifest["profile"]["version"]:
        raise RunnerError("active feature profile does not match the shard manifest")
    if sha256_json(profile) != manifest["profile"]["sha256"]:
        raise RunnerError("active feature profile content differs from the shard manifest")
    if verify_corpus:
        actual_ids = discover_test_ids(suite_root, manifest["suite"])
        expected_ids = [entry["id"] for entry in manifest["tests"]]
        if actual_ids != expected_ids:
            raise RunnerError("checked-out corpus files differ from the deterministic shard manifest")

    shard = next(item for item in manifest["shards"] if item["index"] == shard_index)
    started = time.monotonic()
    results: list[dict[str, Any]] = []
    with AdapterSession(adapter_command) as adapter_session:
        for test_id in shard["testIds"]:
            results.append(
                run_one_test(
                    suite_root,
                    manifest["suite"],
                    test_id,
                    profile,
                    adapter_command,
                    timeout_ms,
                    async_timeout_ms,
                    probe_unsupported,
                    probe_excluded,
                    adapter_session,
                )
            )
    if manifest["suite"] == "test262-parser-tests":
        _apply_parser_equivalence(results)
    return {
        "schemaVersion": SCHEMA_VERSION,
        "suite": manifest["suite"],
        "source": manifest["source"],
        "profile": manifest["profile"],
        "manifestSha256": manifest["manifestSha256"],
        "shardIndex": shard_index,
        "shardCount": manifest["shardCount"],
        "results": results,
        "summary": summarize(results),
        "durationMs": round((time.monotonic() - started) * 1000, 3),
        "probeUnsupported": probe_unsupported,
        "probeExcluded": probe_excluded,
    }


def aggregate_reports(
    manifest: dict[str, Any],
    reports: list[dict[str, Any]],
    baseline: dict[str, Any],
    profile: dict[str, Any],
) -> dict[str, Any]:
    validate_manifest(manifest)
    expected_source = _pinned_source(baseline, manifest["suite"])
    if manifest["source"] != expected_source:
        raise RunnerError("shard manifest source revision does not match the pinned baseline")
    if manifest["profile"] != _manifest_profile(profile):
        raise RunnerError("shard manifest feature profile does not match the active profile")
    count = manifest["shardCount"]
    assigned = {
        index: set(shard["testIds"]) for index, shard in
        ((item["index"], item) for item in manifest["shards"])
    }
    expected_ids = {entry["id"] for entry in manifest["tests"]}
    shard_seen: set[int] = set()
    shard_duplicates: list[int] = []
    result_occurrences: dict[str, list[dict[str, Any]]] = {}
    errors: list[str] = []
    if manifest["suite"] == "test262" and manifest["profile"]["executionState"] != "ready":
        errors.append("Test262 aggregation requires an implemented execution adapter")
    if (
        manifest["suite"] == "test262-parser-tests"
        and manifest["profile"]["parserStatus"] != "ready"
    ):
        errors.append("parser-corpus aggregation requires an implemented parser adapter")

    for report in reports:
        if report.get("schemaVersion") != SCHEMA_VERSION:
            errors.append("shard report has an unsupported schemaVersion")
            continue
        if report.get("manifestSha256") != manifest["manifestSha256"]:
            errors.append("shard report references a different manifest")
            continue
        if (
            report.get("suite") != manifest["suite"]
            or report.get("shardCount") != count
            or report.get("source") != manifest["source"]
            or report.get("profile") != manifest["profile"]
        ):
            errors.append("shard report suite, source, profile or shardCount does not match the manifest")
            continue
        index = report.get("shardIndex")
        if not isinstance(index, int) or not 0 <= index < count:
            errors.append(f"invalid shardIndex in report: {index!r}")
            continue
        if index in shard_seen:
            shard_duplicates.append(index)
            errors.append(f"duplicate shard result for index {index}")
            continue
        shard_seen.add(index)
        shard_results = report.get("results")
        if not isinstance(shard_results, list):
            errors.append(f"shard {index} results must be an array")
            continue
        shard_ids: set[str] = set()
        for result in shard_results:
            if not isinstance(result, dict):
                errors.append(f"shard {index} contains a non-object test result")
                continue
            test_id = result.get("id")
            if not isinstance(test_id, str):
                errors.append(f"shard {index} contains a result without a test ID")
                continue
            if result.get("status") not in RESULT_STATUSES:
                errors.append(f"test {test_id} has an invalid result classification")
            if test_id in shard_ids:
                errors.append(f"duplicate test result within shard {index}: {test_id}")
            shard_ids.add(test_id)
            if test_id not in expected_ids:
                errors.append(f"unexpected test result: {test_id}")
            elif test_id not in assigned[index]:
                errors.append(f"test {test_id} was reported by the wrong shard {index}")
            result_occurrences.setdefault(test_id, []).append(result)
        missing_in_shard = assigned[index] - shard_ids
        if missing_in_shard:
            errors.append(f"shard {index} is missing tests: {', '.join(sorted(missing_in_shard))}")

    missing_shards = sorted(set(range(count)) - shard_seen)
    if missing_shards:
        errors.append("missing shard results: " + ", ".join(map(str, missing_shards)))
    duplicate_tests = sorted(
        test_id for test_id, occurrences in result_occurrences.items() if len(occurrences) > 1
    )
    if duplicate_tests:
        errors.append("duplicate test results: " + ", ".join(duplicate_tests))
    missing_tests = sorted(expected_ids - set(result_occurrences))
    if missing_tests:
        errors.append("missing test results: " + ", ".join(missing_tests))

    results = [
        result_occurrences[test_id][0]
        for test_id in sorted(result_occurrences)
        if test_id in expected_ids and result_occurrences[test_id]
    ]
    if not any(result.get("status") == "PASS" for result in results):
        errors.append("aggregate has no PASS results; no executed conformance evidence is available")
    return {
        "schemaVersion": SCHEMA_VERSION,
        "suite": manifest["suite"],
        "source": manifest["source"],
        "profile": manifest["profile"],
        "manifestSha256": manifest["manifestSha256"],
        "shardCount": count,
        "results": results,
        "summary": summarize(results),
        "aggregation": {
            "complete": not errors,
            "missingShards": missing_shards,
            "duplicateShards": sorted(shard_duplicates),
            "missingTests": missing_tests,
            "duplicateTests": duplicate_tests,
            "errors": errors,
        },
    }


def _parse_adapter_command(value: str | None) -> list[str]:
    if not value:
        return []
    command = shlex.split(value, posix=(sys.platform != "win32"))
    if not command:
        raise RunnerError("adapter command is empty")
    return command


def _load_and_check_revision(suite_root: Path, expected: str) -> None:
    if not (suite_root / ".git").exists():
        raise RunnerError(f"source corpus is not a Git checkout: {suite_root}")
    try:
        process = subprocess.run(
            ["git", "-C", str(suite_root), "rev-parse", "HEAD"],
            text=True,
            capture_output=True,
            check=True,
            timeout=10,
        )
        status = subprocess.run(
            ["git", "-C", str(suite_root), "status", "--porcelain", "--untracked-files=all"],
            text=True,
            capture_output=True,
            check=True,
            timeout=10,
        )
    except (OSError, subprocess.SubprocessError) as exc:
        raise RunnerError(f"cannot inspect source corpus Git state: {exc}") from exc
    actual = process.stdout.strip()
    if actual != expected:
        raise RunnerError(
            f"source corpus revision mismatch: expected {expected}, checked out {actual}"
        )
    if status.stdout.strip():
        raise RunnerError(
            "source corpus working tree is not clean; modified or untracked files "
            "would invalidate the pinned test/harness snapshot"
        )


def _add_baseline_profile_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--baseline", type=Path, default=Path("conformance/baseline.json"))
    parser.add_argument("--profile", type=Path, default=Path("conformance/profile.json"))


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="action", required=True)

    plan = subparsers.add_parser("plan", help="create a deterministic corpus/shard manifest")
    plan.add_argument("--suite", choices=["test262", "test262-parser-tests"], required=True)
    plan.add_argument("--suite-root", type=Path, required=True)
    plan.add_argument("--shard-count", type=int, default=1)
    plan.add_argument("--output", type=Path, required=True)
    _add_baseline_profile_arguments(plan)

    run = subparsers.add_parser("run", help="run one deterministic shard through an engine adapter")
    run.add_argument("--manifest", type=Path, required=True)
    run.add_argument("--suite-root", type=Path, required=True)
    run.add_argument("--shard-index", type=int, required=True)
    run.add_argument("--adapter", help="adapter command; receives one JSON request on stdin")
    run.add_argument("--timeout-ms", type=int, default=10_000)
    run.add_argument("--async-timeout-ms", type=int, default=10_000)
    run.add_argument("--probe-unsupported", action="store_true")
    run.add_argument("--probe-excluded", action="store_true")
    run.add_argument("--report", type=Path, required=True)
    run.add_argument("--junit", type=Path)
    _add_baseline_profile_arguments(run)

    aggregate = subparsers.add_parser("aggregate", help="validate and aggregate every shard result")
    aggregate.add_argument("--manifest", type=Path, required=True)
    aggregate.add_argument("--reports", type=Path, nargs="*", default=[])
    aggregate.add_argument("--report", type=Path, required=True)
    aggregate.add_argument("--junit", type=Path)
    _add_baseline_profile_arguments(aggregate)
    return parser


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    try:
        if args.action == "plan":
            baseline = load_baseline(args.baseline)
            profile = load_profile(args.profile)
            source = _pinned_source(baseline, args.suite)
            _load_and_check_revision(args.suite_root, source["revision"])
            manifest = build_manifest(
                args.suite,
                discover_test_ids(args.suite_root, args.suite),
                baseline,
                profile,
                args.shard_count,
            )
            write_json(args.output, manifest)
            print(f"wrote {len(manifest['tests'])} test IDs across {args.shard_count} shards")
            return 0

        if args.action == "run":
            if args.timeout_ms < 1 or args.async_timeout_ms < 1:
                raise RunnerError("timeouts must be positive integers")
            baseline = load_baseline(args.baseline)
            profile = load_profile(args.profile)
            manifest = read_json(args.manifest)
            validate_manifest(manifest)
            pinned = _pinned_source(baseline, manifest["suite"])
            if manifest["source"] != pinned:
                raise RunnerError("shard manifest source revision does not match the pinned baseline")
            _load_and_check_revision(args.suite_root, pinned["revision"])
            report = run_manifest(
                manifest,
                args.suite_root,
                profile,
                _parse_adapter_command(args.adapter),
                args.shard_index,
                args.timeout_ms,
                args.async_timeout_ms,
                args.probe_unsupported,
                args.probe_excluded,
            )
            write_json(args.report, report)
            if args.junit:
                write_junit(args.junit, manifest["suite"], report["results"])
            print(json.dumps(report["summary"], sort_keys=True))
            return 1 if any(item["status"] in FAILURE_STATUSES for item in report["results"]) else 0

        baseline = load_baseline(args.baseline)
        profile = load_profile(args.profile)
        manifest = read_json(args.manifest)
        reports = [read_json(path) for path in args.reports]
        report = aggregate_reports(manifest, reports, baseline, profile)
        write_json(args.report, report)
        if args.junit:
            write_junit(
                args.junit,
                manifest["suite"],
                report["results"],
                report["aggregation"]["errors"],
            )
        print(json.dumps(report["summary"], sort_keys=True))
        if not report["aggregation"]["complete"]:
            return 1
        return 1 if any(item["status"] in FAILURE_STATUSES for item in report["results"]) else 0
    except RunnerError as exc:
        print(f"ogma-test-runner: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
