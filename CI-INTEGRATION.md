# CI Fast integration for OGM-15

The following steps are integrated in the existing Fast CI workflow immediately after `Test repository validator`:

```yaml
      - name: Verify Core IR v1 contract and fixture corpus
        run: python .github/scripts/verify_core_ir.py --manifest tests/core-ir/manifest.json --report "$RUNNER_TEMP/core-ir-verifier-report.json"

      - name: Upload Core IR verifier report
        if: always()
        uses: actions/upload-artifact@v4
        with:
          name: core-ir-v1-verifier-report
          path: ${{ runner.temp }}/core-ir-verifier-report.json
          if-no-files-found: warn
          retention-days: 30
```

The verifier runs in the existing Python 3.12 Fast job with no new runtime or third-party dependency. The fixture manifest contains 25 cases (4 accepted and 21 rejected), and the combined `test_*.py` discovery runs 52 unit tests across the Core IR verifier, semantic-hardening regressions, malformed-attribute regressions and repository validator.

Fast CI keeps this check bounded. It must not trigger Test262, full E2E, browser/promotion acceptance, Rust builds or benchmarks. Rust fmt/Clippy/workspace tests remain not applicable until a root Cargo workspace exists.

The uploaded deterministic JSON report includes contract/verifier versions, per-fixture expected/actual results and structured diagnostics. This package proves the shared backend-neutral contract and its verifier; it does not claim an interpreter or native AOT implementation. Actual dual-backend consumption and differential execution evidence belong to OGM-31 after its runtime, bytecode, native-backend and GC/root prerequisites exist.

Source of truth:
- `docs/core-ir-contract.md`: semantic contract and backend invariants.
- `contracts/core-ir/v1/schema.json`: machine-readable schema.
- `tests/core-ir/manifest.json` and `tests/core-ir/fixtures/`: executable positive/negative contract cases.
- `.github/scripts/verify_core_ir.py`: structural and semantic verifier.
