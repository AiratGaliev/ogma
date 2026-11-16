#!/usr/bin/env python3
"""Dependency-free repository checks for the Ogma fast CI lane."""

from __future__ import annotations

import os
import re
import sys
from pathlib import Path
from urllib.parse import unquote, urlsplit

LINK_RE = re.compile(r"!?\[[^\]]*\]\((<[^>]+>|[^)\s]+)(?:\s+[^)]*)?\)")
FENCE_OPEN_RE = re.compile(r"^ {0,3}(`{3,}|~{3,})")
FENCE_CLOSE_RE = re.compile(r"^ {0,3}(`{3,}|~{3,})[ \t]*$")
INLINE_CODE_RE = re.compile(
    r"(?<!\x60)(?P<ticks>\x60+)(?!\x60)(?P<body>.*?)(?<!\x60)(?P=ticks)(?!\x60)",
    re.DOTALL,
)
IGNORED_DIRS = {
    ".git",
    ".idea",
    ".venv",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    "ENV",
    "node_modules",
    "target",
    "venv",
}


def without_markdown_code(content: str) -> str:
    """Mask fenced and inline code so examples are not parsed as Markdown links."""
    visible_lines: list[str] = []
    active_fence: tuple[str, int] | None = None

    for line in content.splitlines():
        closing = FENCE_CLOSE_RE.match(line)
        if active_fence is not None:
            char, width = active_fence
            if closing and closing.group(1)[0] == char and len(closing.group(1)) >= width:
                active_fence = None
            visible_lines.append("")
            continue

        opening = FENCE_OPEN_RE.match(line)
        if opening:
            marker = opening.group(1)
            active_fence = (marker[0], len(marker))
            visible_lines.append("")
            continue

        visible_lines.append(line)

    visible_content = "\n".join(visible_lines)

    def blank_code_span(match: re.Match[str]) -> str:
        return "".join("\n" if char == "\n" else " " for char in match.group(0))

    return INLINE_CODE_RE.sub(blank_code_span, visible_content)


def markdown_files(root: Path) -> list[Path]:
    """Return Markdown files while ignoring generated and IDE directories."""
    return sorted(
        path
        for path in root.rglob("*.md")
        if not any(part in IGNORED_DIRS for part in path.relative_to(root).parts)
    )


def validate_markdown(path: Path, root: Path, enforce_lf: bool | None = None) -> list[str]:
    """Validate encoding, line endings, whitespace, fences and local links."""
    if enforce_lf is None:
        enforce_lf = os.name != "nt" or os.environ.get("GITHUB_ACTIONS") == "true"
    errors: list[str] = []
    relative = path.relative_to(root).as_posix()
    data = path.read_bytes()

    if enforce_lf and b"\r" in data:
        errors.append(f"{relative}: contains CR characters; use LF line endings")
    if data and not data.endswith(b"\n"):
        errors.append(f"{relative}: missing final newline")

    try:
        content = data.decode("utf-8")
    except UnicodeDecodeError as exc:
        return errors + [f"{relative}: is not valid UTF-8 ({exc})"]

    lines = content.splitlines()
    for number, line in enumerate(lines, start=1):
        if line.rstrip(" \t") != line:
            errors.append(f"{relative}:{number}: trailing whitespace")

    active_fence: tuple[str, int, int] | None = None
    for number, line in enumerate(lines, start=1):
        closing = FENCE_CLOSE_RE.match(line)
        if active_fence is not None:
            char, width, _ = active_fence
            if closing and closing.group(1)[0] == char and len(closing.group(1)) >= width:
                active_fence = None
            continue

        opening = FENCE_OPEN_RE.match(line)
        if opening:
            marker = opening.group(1)
            active_fence = (marker[0], len(marker), number)

    if active_fence is not None:
        _, _, start_line = active_fence
        errors.append(f"{relative}:{start_line}: unclosed fenced code block")

    root_resolved = root.resolve()
    link_content = without_markdown_code(content)
    for match in LINK_RE.finditer(link_content):
        destination = match.group(1)
        if destination.startswith("<") and destination.endswith(">"):
            destination = destination[1:-1]
        if not destination or destination.startswith("#"):
            continue

        parsed = urlsplit(destination)
        if parsed.scheme or parsed.netloc or not parsed.path:
            continue

        local_path = unquote(parsed.path)
        target = (path.parent / local_path).resolve()
        try:
            target.relative_to(root_resolved)
        except ValueError:
            errors.append(f"{relative}: local link escapes repository: {destination}")
            continue

        if not target.exists():
            errors.append(f"{relative}: broken local link: {destination}")

    return errors


def validate_repository(root: Path) -> list[str]:
    files = markdown_files(root)
    errors: list[str] = []
    for path in files:
        errors.extend(validate_markdown(path, root))
    if not files:
        errors.append("repository: no Markdown files found")
    return errors


def main() -> int:
    root = Path(__file__).resolve().parents[2]
    errors = validate_repository(root)
    if errors:
        for error in errors:
            print(f"ERROR: {error}", file=sys.stderr)
        print(f"Repository validation failed with {len(errors)} error(s).", file=sys.stderr)
        return 1

    count = len(markdown_files(root))
    print(f"Repository validation passed: {count} Markdown files checked.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
