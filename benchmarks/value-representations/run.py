"""Run the canonical Core IR Value/SSA proof and emit JSON evidence."""
from __future__ import annotations

import argparse
import json
import platform
import sys
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(Path(__file__).resolve().parent))

from corpus import load_corpus  # noqa: E402
from model import analyze_corpus  # noqa: E402


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--manifest",
        default="tests/core-ir/manifest.json",
        help="canonical repository-relative Core IR manifest",
    )
    parser.add_argument(
        "--output",
        type=Path,
        default=Path(__file__).resolve().parent / "results" / "value-ssa-report.json",
    )
    args = parser.parse_args()

    payload = analyze_corpus(load_corpus(ROOT, args.manifest))
    payload["environment"] = {
        "python": sys.version,
        "implementation": platform.python_implementation(),
        "platform": platform.platform(),
        "machine": platform.machine(),
        "generated_at_utc": datetime.now(timezone.utc).isoformat(),
    }

    output_path = args.output if args.output.is_absolute() else ROOT / args.output
    output_path.parent.mkdir(parents=True, exist_ok=True)
    output_path.write_text(
        json.dumps(payload, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    print(json.dumps(payload, indent=2, sort_keys=True))
    try:
        shown_path = output_path.relative_to(ROOT)
    except ValueError:
        shown_path = output_path
    print(f"wrote {shown_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
