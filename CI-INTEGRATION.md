# CI Fast integration for OGM-15

The following two steps are already integrated immediately after the existing `Test repository validator` step in `.github/workflows/ci.yml` and before `Compile CI helper scripts`:

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

The step runs in the existing Python 3.12 Fast job; no new dependency or runtime is needed. It must remain in Fast CI and must not trigger Test262, full E2E, promotion acceptance, Rust builds, or benchmark jobs.

Update the CI summary in `AGENTS.md`, `README.md` and `docs/development-workflow.md` to mention the Core IR schema/verifier/fixture corpus and uploaded report. Add `docs/core-ir-contract.md` plus `contracts/core-ir/v1/schema.json` to the development-workflow source-of-truth table.
