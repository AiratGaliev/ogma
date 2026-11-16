#!/usr/bin/env python3
"""Fail the OGM-14 proof when pinned fixture expectations regress."""

from __future__ import annotations

import json
from pathlib import Path

ROOT = Path(__file__).resolve().parent
RESULTS = ROOT / "results"

oxc = json.loads((RESULTS / "oxc-diagnostics.json").read_text())
swc = json.loads((RESULTS / "swc-diagnostics.json").read_text())

failures: list[str] = []
if oxc["parseMismatches"]:
    failures.append(f"Oxc parse fixture mismatches: {oxc['parseMismatches']}")
if swc["parseMismatches"]:
    failures.append(f"SWC parse fixture mismatches: {swc['parseMismatches']}")
if oxc["semanticMismatches"]:
    failures.append(f"Oxc semantic fixture mismatches: {oxc['semanticMismatches']}")
if oxc["semanticChecksPerformed"] == 0:
    failures.append("Oxc did not execute any semantic fixture checks")
if oxc["semanticErrorsObserved"] == 0:
    failures.append("Oxc did not report the pinned duplicate lexical binding fixture")

if failures:
    for failure in failures:
        print(f"::error::{failure}")
    raise SystemExit(1)

print("OGM-14 fixtures passed: Oxc parse/semantic and SWC parse.")
