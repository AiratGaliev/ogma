#!/usr/bin/env python3
"""Run OGM-17's deterministic structural comparison and emit JSON evidence."""
from __future__ import annotations
import argparse, json, platform, sys
from pathlib import Path
from datetime import datetime, timezone
ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(Path(__file__).resolve().parent))
from model import report  # noqa: E402
def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=Path(__file__).resolve().parent / "results" / "value-ssa-report.json")
    args = parser.parse_args()
    payload = report()
    payload["environment"] = {"python": sys.version, "implementation": platform.python_implementation(), "platform": platform.platform(), "machine": platform.machine(), "generated_at_utc": datetime.now(timezone.utc).isoformat()}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(json.dumps(payload, indent=2, sort_keys=True))
    print(f"wrote {args.output.relative_to(ROOT)}")
    return 0
if __name__ == "__main__":
    raise SystemExit(main())
