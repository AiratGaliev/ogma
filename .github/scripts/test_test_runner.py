from __future__ import annotations

import hashlib
import subprocess
import sys
import tempfile
import time
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))

from tools import test_runner


FIXTURES = ROOT / "tests" / "test-runner" / "fixtures"
TEST262_ROOT = FIXTURES / "test262"
PARSER_ROOT = FIXTURES / "parser-tests"
BASELINE = test_runner.load_baseline(ROOT / "conformance" / "baseline.json")
FAKE_ADAPTER = [sys.executable, str(ROOT / "tests" / "test-runner" / "fake_adapter.py")]


def ready_profile() -> dict:
    return {
        "schemaVersion": 1,
        "profileId": "self-test",
        "version": "1.0.0",
        "execution": {"state": "ready"},
        "parserStatus": "ready",
        "defaultFeatureStatus": "supported",
        "featureStatus": {"Array": "supported"},
        "exclusions": [],
    }


def manifest_for(suite: str, root: Path, profile: dict, shard_count: int = 1):
    return test_runner.build_manifest(
        suite,
        test_runner.discover_test_ids(root, suite),
        BASELINE,
        profile,
        shard_count,
    )


def report_for_shard(manifest: dict, shard_index: int, status: str = "PASS") -> dict:
    shard = manifest["shards"][shard_index]
    return {
        "schemaVersion": 1,
        "suite": manifest["suite"],
        "source": manifest["source"],
        "profile": manifest["profile"],
        "manifestSha256": manifest["manifestSha256"],
        "shardIndex": shard_index,
        "shardCount": manifest["shardCount"],
        "results": [
            {"id": test_id, "status": status, "variants": []}
            for test_id in shard["testIds"]
        ],
    }


class AdapterWorkerTests(unittest.TestCase):
    @staticmethod
    def request(source: str, suite: str = "test262-parser-tests", timeout_ms: int = 1000):
        return {
            "protocol": "ogma-test-runner-adapter",
            "version": 1,
            "test": {"suite": suite, "source": source},
            "timeoutMs": timeout_ms,
        }

    def test_worker_is_reused_and_responses_are_correlated(self) -> None:
        with test_runner.AdapterSession(FAKE_ADAPTER) as session:
            first, first_error = session.run(
                self.request("// parser: parsed"), 1000
            )
            second, second_error = session.run(
                self.request("// parser: parsed"), 1000
            )
        self.assertIsNone(first_error)
        self.assertIsNone(second_error)
        self.assertEqual(first["requestId"], 1)
        self.assertEqual(second["requestId"], 2)
        self.assertEqual(first["workerPid"], second["workerPid"])

    def test_blocked_stdin_write_is_bounded_by_test_timeout(self) -> None:
        command = [
            sys.executable,
            "-c",
            "import time; time.sleep(5)",
        ]
        request = self.request("// parser: parsed", timeout_ms=50)
        request["test"]["source"] = "x" * (2 * 1024 * 1024)
        started = time.monotonic()
        with test_runner.AdapterSession(command) as session:
            response, error = session.run(request, 50)
        elapsed = time.monotonic() - started
        self.assertIsNone(response)
        self.assertEqual(error["status"], "FAIL")
        self.assertEqual(error["failureKind"], "TIMEOUT")
        self.assertLess(elapsed, 2.0)

    def test_nonzero_worker_exit_after_last_response_is_not_ignored(self) -> None:
        response_code = (
            "import json,sys; request=json.loads(sys.stdin.readline()); "
            "print(json.dumps({'protocol':'ogma-test-runner-adapter',"
            "'version':1,'requestId':request['requestId'],'outcome':'parsed',"
            "'phase':'parse'}), flush=True); sys.exit(9)"
        )
        command = [sys.executable, "-c", response_code]
        with self.assertRaisesRegex(test_runner.RunnerError, "exited unsuccessfully"):
            with test_runner.AdapterSession(command) as session:
                response, error = session.run(
                    self.request("// parser: parsed"), 1000
                )
                self.assertIsNone(error)
                self.assertEqual(response["outcome"], "parsed")

    def test_unsolicited_stdout_after_last_response_is_a_harness_error(self) -> None:
        response_code = (
            "import json,sys; request=json.loads(sys.stdin.readline()); "
            "response={'protocol':'ogma-test-runner-adapter','version':1,"
            "'requestId':request['requestId'],'outcome':'parsed','phase':'parse'}; "
            "print(json.dumps(response), flush=True); "
            "print(json.dumps(response), flush=True); sys.stdin.read()"
        )
        command = [sys.executable, "-c", response_code]
        with self.assertRaisesRegex(test_runner.RunnerError, "unsolicited stdout"):
            with test_runner.AdapterSession(command) as session:
                response, error = session.run(
                    self.request("// parser: parsed"), 1000
                )
                self.assertIsNone(error)
                self.assertEqual(response["outcome"], "parsed")

    def test_timed_out_worker_is_killed_and_restarted_for_next_test(self) -> None:
        with test_runner.AdapterSession(FAKE_ADAPTER) as session:
            self.assertTrue(session._start())
            first_worker_pid = session.process.pid
            timed_out, timeout_error = session.run(
                self.request("// result: timeout", suite="test262", timeout_ms=50),
                50,
            )
            recovered, recovery_error = session.run(
                self.request("// parser: parsed"), 1000
            )
            restarted_worker_pid = session.process.pid
        self.assertIsNone(timed_out)
        self.assertEqual(timeout_error["status"], "FAIL")
        self.assertEqual(timeout_error["failureKind"], "TIMEOUT")
        self.assertIsNone(recovery_error)
        self.assertEqual(recovered["requestId"], 2)
        self.assertNotEqual(first_worker_pid, restarted_worker_pid)


class ConfigurationValidationTests(unittest.TestCase):
    def test_profile_rejects_unhashable_or_invalid_state_values(self) -> None:
        cases = [
            ({"execution": {"state": []}}, "execution.state"),
            ({"parserStatus": []}, "parserStatus"),
            ({"defaultFeatureStatus": []}, "defaultFeatureStatus"),
            ({"featureStatus": {"Array": []}}, "status for feature"),
            ({"exclusions": [{"suite": [], "testId": "x.js", "reason": "r", "feature": "f", "owner": "OGM", "revisitCondition": "c"}]}, "exclusion suite"),
        ]
        for patch, message in cases:
            with self.subTest(patch=patch), tempfile.TemporaryDirectory() as directory:
                profile = ready_profile()
                for key, value in patch.items():
                    if key == "execution":
                        profile[key] = value
                    else:
                        profile[key] = value
                path = Path(directory) / "profile.json"
                test_runner.write_json(path, profile)
                with self.assertRaisesRegex(test_runner.RunnerError, message):
                    test_runner.load_profile(path)

    def test_baseline_requires_string_repository_url_and_revision(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            baseline = {
                **BASELINE,
                "repositories": {
                    **BASELINE["repositories"],
                    "test262": {
                        **BASELINE["repositories"]["test262"],
                        "url": [],
                    },
                },
            }
            path = Path(directory) / "baseline.json"
            test_runner.write_json(path, baseline)
            with self.assertRaisesRegex(test_runner.RunnerError, "repository URL"):
                test_runner.load_baseline(path)


class Test262MetadataTests(unittest.TestCase):
    def test_missing_frontmatter_uses_default_test262_metadata(self) -> None:
        descriptor = test_runner._read_test262_descriptor(
            TEST262_ROOT, "runner/no-metadata.js"
        )
        self.assertEqual(descriptor["metadata"], {})
        self.assertEqual(descriptor["modes"], ["sloppy", "strict"])
        self.assertEqual(descriptor["includes"][0]["name"], "assert.js")

    def test_frontmatter_after_license_comments_and_block_yaml_is_parsed(self) -> None:
        descriptor = test_runner._read_test262_descriptor(
            TEST262_ROOT, "runner/license-before-metadata.js"
        )
        self.assertEqual(descriptor["features"], ["Array"])
        self.assertEqual(descriptor["flags"], ["noStrict"])
        self.assertEqual(descriptor["modes"], ["sloppy"])
        self.assertIn("Example algorithm", descriptor["metadata"]["info"])

    def test_complete_metadata_is_forwarded_to_the_engine_adapter(self) -> None:
        descriptor = test_runner._read_test262_descriptor(
            TEST262_ROOT, "runner/locale-metadata.js"
        )
        self.assertEqual(descriptor["metadata"]["locale"], ["en", "fr"])
        self.assertEqual(descriptor["featureRequirements"], ["Array"])
        result = test_runner.run_one_test(
            TEST262_ROOT,
            "test262",
            "runner/locale-metadata.js",
            ready_profile(),
            FAKE_ADAPTER,
            1000,
            1000,
        )
        self.assertEqual(result["status"], "PASS")

    def test_metadata_includes_features_flags_and_both_default_modes(self) -> None:
        test_id = "runner/strict-default.js"
        descriptor = test_runner._read_test262_descriptor(TEST262_ROOT, test_id)
        self.assertEqual(descriptor["features"], ["Array"])
        self.assertEqual(descriptor["modes"], ["sloppy", "strict"])
        self.assertEqual(descriptor["sourceType"], "script")

    def test_harness_includes_are_loaded_in_test262_order(self) -> None:
        descriptor = test_runner._read_test262_descriptor(
            TEST262_ROOT, "runner/basic-async.js"
        )
        self.assertEqual(
            [item["name"] for item in descriptor["includes"]],
            ["assert.js", "sta.js", "doneprintHandle.js", "helper.js"],
        )
        self.assertTrue(descriptor["async"])

    def test_async_completion_signal_is_required_and_failure_signal_fails(self) -> None:
        complete = test_runner.run_one_test(
            TEST262_ROOT,
            "test262",
            "runner/basic-async.js",
            ready_profile(),
            FAKE_ADAPTER,
            1000,
            1000,
        )
        self.assertEqual(complete["status"], "PASS")

        failure = test_runner.run_one_test(
            TEST262_ROOT,
            "test262",
            "runner/async-failure.js",
            ready_profile(),
            FAKE_ADAPTER,
            1000,
            1000,
        )
        self.assertEqual(failure["status"], "FAIL")
        self.assertEqual(failure["variants"][0]["failureKind"], "ASYNC_FAILURE")

    def test_module_fixture_is_not_discovered_as_a_standalone_test(self) -> None:
        test_ids = test_runner.discover_test_ids(TEST262_ROOT, "test262")
        self.assertIn("runner/module.js", test_ids)
        self.assertNotIn("runner/import_FIXTURE.js", test_ids)

    def test_raw_tests_do_not_receive_harness_includes(self) -> None:
        descriptor = test_runner._read_test262_descriptor(TEST262_ROOT, "runner/raw.js")
        self.assertTrue(descriptor["raw"])
        self.assertEqual(descriptor["includes"], [])
        self.assertEqual(descriptor["modes"], ["sloppy"])

    def test_windows_path_separators_and_drive_paths_are_rejected(self) -> None:
        for value in (r"..\\outside.js", r"C:\\outside.js", "/outside.js"):
            with self.subTest(value=value):
                with self.assertRaises(test_runner.RunnerError):
                    test_runner._safe_relative_path(value)

    def test_conflicting_strictness_flags_are_a_harness_error(self) -> None:
        result = test_runner.run_one_test(
            TEST262_ROOT,
            "test262",
            "runner/invalid-flags.js",
            ready_profile(),
            FAKE_ADAPTER,
            1000,
            1000,
        )
        self.assertEqual(result["status"], "HARNESS ERROR")
        self.assertIn("mutually exclusive", result["reason"])

    def test_invalid_negative_phase_type_is_a_harness_error(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "test").mkdir()
            (root / "harness").mkdir()
            for name in ("assert.js", "sta.js"):
                (root / "harness" / name).write_text("// fixture\n", encoding="utf-8")
            (root / "test" / "invalid-negative.js").write_text(
                "/*---\nnegative:\n  phase: []\n  type: SyntaxError\n---*/\n0;\n",
                encoding="utf-8",
            )
            result = test_runner.run_one_test(
                root,
                "test262",
                "invalid-negative.js",
                ready_profile(),
                FAKE_ADAPTER,
                1000,
                1000,
            )
            self.assertEqual(result["status"], "HARNESS ERROR")
            self.assertIn("negative tests require", result["reason"])

    def test_negative_parse_phase_and_error_type_are_checked(self) -> None:
        result = test_runner.run_one_test(
            TEST262_ROOT,
            "test262",
            "runner/negative-parse.js",
            ready_profile(),
            FAKE_ADAPTER,
            1000,
            1000,
        )
        self.assertEqual(result["status"], "PASS")
        self.assertEqual(result["modes"], ["strict"])
        self.assertEqual(result["variants"][0]["status"], "PASS")

    def test_module_tests_use_one_module_variant_and_resolve_fixture_context(self) -> None:
        result = test_runner.run_one_test(
            TEST262_ROOT,
            "test262",
            "runner/module.js",
            ready_profile(),
            FAKE_ADAPTER,
            1000,
            1000,
        )
        self.assertEqual(result["status"], "PASS")
        self.assertEqual(result["modes"], ["module"])

    def test_async_completion_is_required(self) -> None:
        result = test_runner.run_one_test(
            TEST262_ROOT,
            "test262",
            "runner/async-incomplete.js",
            ready_profile(),
            FAKE_ADAPTER,
            1000,
            1000,
        )
        self.assertEqual(result["status"], "FAIL")
        self.assertEqual(result["variants"][0]["failureKind"], "ASYNC_NOT_COMPLETED")

    def test_timeout_is_a_failure_not_a_pass(self) -> None:
        result = test_runner.run_one_test(
            TEST262_ROOT,
            "test262",
            "runner/timeout.js",
            ready_profile(),
            FAKE_ADAPTER,
            50,
            50,
        )
        self.assertEqual(result["status"], "FAIL")
        self.assertEqual(result["variants"][0]["failureKind"], "TIMEOUT")

    def test_untagged_intl_suite_is_inferred_as_deferred_profile_requirement(self) -> None:
        profile = ready_profile()
        profile["featureStatus"]["Intl"] = "deferred"
        descriptor = test_runner._read_test262_descriptor(
            TEST262_ROOT, "intl402/Array/no-feature-tag.js"
        )
        self.assertEqual(descriptor["features"], [])
        self.assertEqual(descriptor["featureRequirements"], ["Intl"])
        deferred = test_runner.run_one_test(
            TEST262_ROOT,
            "test262",
            "intl402/Array/no-feature-tag.js",
            profile,
            FAKE_ADAPTER,
            1000,
            1000,
        )
        self.assertEqual(deferred["status"], "UNSUPPORTED")
        self.assertIn("Intl", deferred["reason"])

        profile["featureStatus"]["Intl"] = "supported"
        supported = test_runner.run_one_test(
            TEST262_ROOT,
            "test262",
            "intl402/Array/no-feature-tag.js",
            profile,
            FAKE_ADAPTER,
            1000,
            1000,
        )
        self.assertEqual(supported["status"], "PASS")

        staging = test_runner._read_test262_descriptor(
            TEST262_ROOT, "staging/intl402/Array/no-feature-tag.js"
        )
        self.assertEqual(staging["featureRequirements"], ["Intl"])
        profile["featureStatus"]["Intl"] = "deferred"
        staging_result = test_runner.run_one_test(
            TEST262_ROOT,
            "test262",
            "staging/intl402/Array/no-feature-tag.js",
            profile,
            FAKE_ADAPTER,
            1000,
            1000,
        )
        self.assertEqual(staging_result["status"], "UNSUPPORTED")

    def test_unsupported_feature_is_not_counted_as_pass(self) -> None:
        profile = ready_profile()
        profile["featureStatus"]["Array"] = "deferred"
        result = test_runner.run_one_test(
            TEST262_ROOT,
            "test262",
            "runner/strict-default.js",
            profile,
            FAKE_ADAPTER,
            1000,
            1000,
        )
        self.assertEqual(result["status"], "UNSUPPORTED")
        self.assertEqual(result["variants"], [])

    def test_partial_feature_runs_only_exact_allowlisted_test_ids(self) -> None:
        profile = ready_profile()
        profile["featureStatus"]["Array"] = "partial"
        profile["supportedTests"] = {
            "Array": ["runner/strict-default.js"],
        }
        supported = test_runner.run_one_test(
            TEST262_ROOT,
            "test262",
            "runner/strict-default.js",
            profile,
            FAKE_ADAPTER,
            1000,
            1000,
        )
        outside = test_runner.run_one_test(
            TEST262_ROOT,
            "test262",
            "runner/basic-async.js",
            profile,
            FAKE_ADAPTER,
            1000,
            1000,
        )
        self.assertEqual(supported["status"], "PASS")
        self.assertEqual(outside["status"], "UNSUPPORTED")
        self.assertIn("exact partial-support allowlist", outside["reason"])

    def test_unimplemented_runtime_is_unsupported_without_reading_test_metadata(self) -> None:
        profile = ready_profile()
        profile["execution"] = {
            "state": "not_implemented",
            "reason": "Ogma Runtime is not implemented",
        }
        result = test_runner.run_one_test(
            TEST262_ROOT,
            "test262",
            "missing/metadata-is-not-read.js",
            profile,
            FAKE_ADAPTER,
            1000,
            1000,
        )
        self.assertEqual(result["status"], "UNSUPPORTED")
        self.assertIn("Runtime is not implemented", result["reason"])

    def test_unsupported_probe_does_parse_metadata_and_reports_bad_test(self) -> None:
        profile = ready_profile()
        profile["execution"] = {"state": "not_implemented"}
        result = test_runner.run_one_test(
            TEST262_ROOT,
            "test262",
            "missing/metadata-is-not-read.js",
            profile,
            FAKE_ADAPTER,
            1000,
            1000,
            probe_unsupported=True,
        )
        self.assertEqual(result["status"], "HARNESS ERROR")

    def test_probe_detects_unexpected_pass_for_unsupported_feature(self) -> None:
        profile = ready_profile()
        profile["featureStatus"]["Array"] = "deferred"
        result = test_runner.run_one_test(
            TEST262_ROOT,
            "test262",
            "runner/strict-default.js",
            profile,
            FAKE_ADAPTER,
            1000,
            1000,
            probe_unsupported=True,
        )
        self.assertEqual(result["status"], "UNEXPECTED PASS")

    def test_exact_exclusions_are_visible_and_probeable(self) -> None:
        profile = ready_profile()
        profile["exclusions"] = [{
            "suite": "test262",
            "testId": "runner/strict-default.js",
            "reason": "Known unsupported host behaviour",
            "feature": "Array",
            "owner": "OGM",
            "revisitCondition": "Remove after the host hook is implemented",
        }]
        excluded = test_runner.run_one_test(
            TEST262_ROOT,
            "test262",
            "runner/strict-default.js",
            profile,
            FAKE_ADAPTER,
            1000,
            1000,
        )
        self.assertEqual(excluded["status"], "EXCLUDED")
        self.assertEqual(excluded["exclusion"]["feature"], "Array")
        probed = test_runner.run_one_test(
            TEST262_ROOT,
            "test262",
            "runner/strict-default.js",
            profile,
            FAKE_ADAPTER,
            1000,
            1000,
            probe_excluded=True,
        )
        self.assertEqual(probed["status"], "UNEXPECTED PASS")

    def test_invalid_adapter_protocol_is_a_harness_error(self) -> None:
        invalid_adapter = [sys.executable, "-c", "print('{}')"]
        result = test_runner.run_one_test(
            TEST262_ROOT,
            "test262",
            "runner/strict-default.js",
            ready_profile(),
            invalid_adapter,
            1000,
            1000,
        )
        self.assertEqual(result["status"], "HARNESS ERROR")
        self.assertIn("invalid protocol envelope", result["variants"][0]["reason"])

    def test_json_and_junit_reports_preserve_classifications(self) -> None:
        results = [
            {"id": "pass.js", "status": "PASS", "variants": []},
            {"id": "unsupported.js", "status": "UNSUPPORTED", "reason": "deferred", "variants": []},
            {"id": "broken.js", "status": "HARNESS ERROR", "reason": "bad metadata", "variants": []},
            {"id": "newly-supported.js", "status": "UNEXPECTED PASS", "reason": "review", "variants": []},
        ]
        summary = test_runner.summarize(results)
        self.assertEqual(summary["PASS"], 1)
        self.assertEqual(summary["UNSUPPORTED"], 1)
        self.assertEqual(summary["HARNESS ERROR"], 1)
        self.assertEqual(summary["UNEXPECTED PASS"], 1)
        report_path = Path(self.id().replace(".", "_") + ".xml")
        json_path = report_path.with_suffix(".json")
        report = {"results": results, "summary": summary}
        try:
            test_runner.write_json(json_path, report)
            self.assertEqual(test_runner.read_json(json_path), report)
            test_runner.write_junit(report_path, "test262", results)
            root = ET.parse(report_path).getroot()
            self.assertEqual(root.attrib["tests"], "4")
            self.assertEqual(root.attrib["errors"], "1")
            self.assertEqual(root.attrib["failures"], "1")
            self.assertEqual(root.attrib["skipped"], "1")
        finally:
            report_path.unlink(missing_ok=True)
            json_path.unlink(missing_ok=True)


class ParserAndShardingTests(unittest.TestCase):
    def test_parser_can_run_before_runtime_execution_adapter_exists(self) -> None:
        profile = ready_profile()
        profile["execution"] = {"state": "not_implemented"}
        manifest = manifest_for("test262-parser-tests", PARSER_ROOT, profile)
        report = test_runner.run_manifest(
            manifest,
            PARSER_ROOT,
            profile,
            FAKE_ADAPTER,
            0,
            timeout_ms=1000,
            async_timeout_ms=1000,
            verify_corpus=True,
        )
        self.assertEqual(report["summary"]["PASS"], 6)
        shard = manifest["shards"][0]
        aggregate = test_runner.aggregate_reports(
            manifest,
            [{
                "schemaVersion": 1,
                "suite": "test262-parser-tests",
                "source": manifest["source"],
                "profile": manifest["profile"],
                "manifestSha256": manifest["manifestSha256"],
                "shardIndex": shard["index"],
                "shardCount": manifest["shardCount"],
                "results": report["results"],
            }],
            BASELINE,
            profile,
            PARSER_ROOT,
        )
        self.assertTrue(aggregate["aggregation"]["complete"])

    def test_aggregate_fails_closed_until_test262_execution_is_ready(self) -> None:
        profile = ready_profile()
        profile["execution"] = {"state": "not_implemented"}
        manifest = manifest_for("test262", TEST262_ROOT, profile)
        reports = [
            report_for_shard(manifest, shard["index"], "UNSUPPORTED")
            for shard in manifest["shards"]
        ]
        aggregate = test_runner.aggregate_reports(
            manifest, reports, BASELINE, profile, TEST262_ROOT
        )
        self.assertFalse(aggregate["aggregation"]["complete"])
        self.assertIn(
            "Test262 aggregation requires an implemented execution adapter",
            aggregate["aggregation"]["errors"],
        )

    def test_aggregate_fails_when_every_test_is_unsupported(self) -> None:
        profile = ready_profile()
        profile["featureStatus"]["Array"] = "deferred"
        manifest = manifest_for("test262", TEST262_ROOT, profile)
        reports = [
            report_for_shard(manifest, shard["index"], "UNSUPPORTED")
            for shard in manifest["shards"]
        ]
        aggregate = test_runner.aggregate_reports(
            manifest, reports, BASELINE, profile, TEST262_ROOT
        )
        self.assertFalse(aggregate["aggregation"]["complete"])
        self.assertIn(
            "aggregate has no PASS results; no executed conformance evidence is available",
            aggregate["aggregation"]["errors"],
        )

    def test_parser_module_fixtures_use_module_grammar(self) -> None:
        descriptor = test_runner._read_parser_descriptor(
            PARSER_ROOT, "pass/valid.module.js"
        )
        self.assertEqual(descriptor["sourceType"], "module")
        self.assertEqual(descriptor["modes"], ["module"])

    def test_early_errors_can_use_parse_phase_when_parser_has_no_separate_mode(self) -> None:
        result = test_runner.run_one_test(
            PARSER_ROOT,
            "test262-parser-tests",
            "early/early-error.js",
            ready_profile(),
            FAKE_ADAPTER + ["--no-early-phase"],
            1000,
            1000,
        )
        self.assertEqual(result["status"], "PASS")

    def test_test_root_symlink_cannot_escape_the_pinned_checkout(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "corpus"
            outside = Path(directory) / "outside"
            root.mkdir()
            outside.mkdir()
            (outside / "test.js").write_text("// outside\n", encoding="utf-8")
            (root / "test").symlink_to(outside, target_is_directory=True)
            with self.assertRaisesRegex(test_runner.RunnerError, "test root must remain inside"):
                test_runner._read_test262_descriptor(root, "test.js")

    def test_test_and_harness_symlinks_cannot_escape_their_roots(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            test_root = root / "test"
            harness_root = root / "harness"
            test_root.mkdir()
            harness_root.mkdir()
            outside = root / "outside.js"
            outside.write_text("// outside\n", encoding="utf-8")
            test_link = test_root / "escape.js"
            harness_link = harness_root / "escape.js"
            try:
                test_link.symlink_to(outside)
                harness_link.symlink_to(outside)
            except OSError as exc:
                self.skipTest(f"symlink creation is unavailable: {exc}")
            with self.assertRaisesRegex(test_runner.RunnerError, "safely read Test262 test"):
                test_runner._read_test262_descriptor(root, "escape.js")
            with self.assertRaisesRegex(test_runner.RunnerError, "safely read Test262 harness include"):
                test_runner._load_harness_includes(harness_root, ["escape.js"], False, False)

    def test_parser_corpus_pass_fail_and_early_error_phases(self) -> None:
        manifest = manifest_for("test262-parser-tests", PARSER_ROOT, ready_profile())
        report = test_runner.run_manifest(
            manifest,
            PARSER_ROOT,
            ready_profile(),
            FAKE_ADAPTER,
            0,
            timeout_ms=1000,
            async_timeout_ms=1000,
            verify_corpus=True,
        )
        self.assertEqual(report["summary"]["PASS"], 6)
        self.assertEqual(report["summary"]["FAIL"], 0)

    def test_aggregate_rejects_manifest_from_different_pinned_baseline(self) -> None:
        profile = ready_profile()
        manifest = manifest_for("test262", TEST262_ROOT, profile)
        report = report_for_shard(manifest, 0)
        other_baseline = {
            **BASELINE,
            "repositories": {
                **BASELINE["repositories"],
                "test262": {
                    **BASELINE["repositories"]["test262"],
                    "revision": "a" * 40,
                },
            },
        }
        with self.assertRaisesRegex(test_runner.RunnerError, "pinned baseline"):
            test_runner.aggregate_reports(
                manifest, [report], other_baseline, profile, TEST262_ROOT
            )

    def test_aggregate_rejects_manifest_from_different_active_profile(self) -> None:
        profile = ready_profile()
        manifest = manifest_for("test262", TEST262_ROOT, profile)
        report = report_for_shard(manifest, 0)
        stale_profile = ready_profile()
        stale_profile["featureStatus"]["Array"] = "deferred"
        with self.assertRaisesRegex(test_runner.RunnerError, "active profile"):
            test_runner.aggregate_reports(
                manifest, [report], BASELINE, stale_profile, TEST262_ROOT
            )

    def test_upstream_checkout_must_match_commit_and_be_clean(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(["git", "init", "-q", str(root)], check=True)
            subprocess.run(["git", "-C", str(root), "config", "user.name", "Ogma Tests"], check=True)
            subprocess.run(["git", "-C", str(root), "config", "user.email", "ogma-tests@example.invalid"], check=True)
            test_file = root / "test.js"
            test_file.write_text("let value = 1;\n", encoding="utf-8")
            subprocess.run(["git", "-C", str(root), "add", "test.js"], check=True)
            subprocess.run(
                ["git", "-C", str(root), "commit", "-q", "-m", "fixture"],
                check=True,
            )
            revision = subprocess.run(
                ["git", "-C", str(root), "rev-parse", "HEAD"],
                check=True,
                capture_output=True,
                text=True,
            ).stdout.strip()
            self.assertIsNone(test_runner._load_and_check_revision(root, revision))

            test_file.write_text("let value = 2;\n", encoding="utf-8")
            with self.assertRaisesRegex(test_runner.RunnerError, "working tree is not clean"):
                test_runner._load_and_check_revision(root, revision)

            subprocess.run(
                ["git", "-C", str(root), "checkout", "--", "test.js"],
                check=True,
            )
            (root / "untracked.js").write_text("let untracked = true;\n", encoding="utf-8")
            with self.assertRaisesRegex(test_runner.RunnerError, "working tree is not clean"):
                test_runner._load_and_check_revision(root, revision)

    def test_ignored_untracked_javascript_cannot_extend_pinned_corpus(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for category in ("pass", "pass-explicit", "early", "fail"):
                (root / category).mkdir()
            (root / "pass" / "valid.js").write_text("let value = 1;\n", encoding="utf-8")
            (root / "pass-explicit" / "valid.js").write_text(
                "let value = (1);\n", encoding="utf-8"
            )
            (root / "early" / "early.js").write_text("let value = 1;\n", encoding="utf-8")
            (root / "fail" / "invalid.js").write_text("let = ;\n", encoding="utf-8")
            (root / ".gitignore").write_text(
                "pass/ignored.js\npass-explicit/ignored.js\n", encoding="utf-8"
            )
            subprocess.run(["git", "init", "-q", str(root)], check=True)
            subprocess.run(
                ["git", "-C", str(root), "config", "user.name", "Ogma Tests"],
                check=True,
            )
            subprocess.run(
                ["git", "-C", str(root), "config", "user.email", "ogma-tests@example.invalid"],
                check=True,
            )
            subprocess.run(["git", "-C", str(root), "add", "."], check=True)
            subprocess.run(
                ["git", "-C", str(root), "commit", "-q", "-m", "pinned fixture"],
                check=True,
            )
            revision = subprocess.run(
                ["git", "-C", str(root), "rev-parse", "HEAD"],
                check=True,
                capture_output=True,
                text=True,
            ).stdout.strip()
            (root / "pass" / "ignored.js").write_text(
                "let injected = true;\n", encoding="utf-8"
            )
            (root / "pass-explicit" / "ignored.js").write_text(
                "let injected = (true);\n", encoding="utf-8"
            )
            status = subprocess.run(
                ["git", "-C", str(root), "status", "--porcelain"],
                check=True,
                capture_output=True,
                text=True,
            ).stdout
            self.assertEqual(status, "")
            with self.assertRaisesRegex(test_runner.RunnerError, "Git-tracked source"):
                test_runner._load_and_check_revision(
                    root, revision, "test262-parser-tests"
                )

    def test_shard_manifest_is_deterministic_and_disjoint(self) -> None:
        profile = ready_profile()
        test_ids = test_runner.discover_test_ids(TEST262_ROOT, "test262")
        first = test_runner.build_manifest("test262", test_ids, BASELINE, profile, 3)
        second = test_runner.build_manifest("test262", list(reversed(test_ids)), BASELINE, profile, 3)
        self.assertEqual(first["manifestSha256"], second["manifestSha256"])
        flattened = [test_id for shard in first["shards"] for test_id in shard["testIds"]]
        self.assertEqual(sorted(flattened), sorted(test_ids))
        self.assertEqual(len(flattened), len(set(flattened)))
        test_runner.validate_manifest(first)

    def test_aggregate_success_requires_exactly_one_result_per_test(self) -> None:
        manifest = manifest_for("test262", TEST262_ROOT, ready_profile(), 2)
        reports = [
            report_for_shard(manifest, shard["index"])
            for shard in manifest["shards"]
        ]
        aggregate = test_runner.aggregate_reports(
            manifest, reports, BASELINE, ready_profile(), TEST262_ROOT
        )
        self.assertTrue(aggregate["aggregation"]["complete"])
        self.assertEqual(aggregate["summary"]["PASS"], len(manifest["tests"]))

    def test_aggregate_detects_missing_shard_and_missing_tests(self) -> None:
        manifest = manifest_for("test262", TEST262_ROOT, ready_profile(), 2)
        first_report = report_for_shard(manifest, 0)
        first_report["results"] = []
        aggregate = test_runner.aggregate_reports(
            manifest, [first_report], BASELINE, ready_profile(), TEST262_ROOT
        )
        self.assertFalse(aggregate["aggregation"]["complete"])
        self.assertEqual(len(aggregate["aggregation"]["missingShards"]), 1)
        self.assertGreaterEqual(len(aggregate["aggregation"]["missingTests"]), 1)
        report_path = Path(self.id().replace(".", "_") + ".xml")
        try:
            test_runner.write_junit(
                report_path,
                "test262",
                aggregate["results"],
                aggregate["aggregation"]["errors"],
            )
            root = ET.parse(report_path).getroot()
            self.assertGreater(int(root.attrib["errors"]), 0)
            self.assertTrue(
                any(
                    node.attrib.get("type") == "AGGREGATION ERROR"
                    for node in root.findall(".//error")
                )
            )
        finally:
            report_path.unlink(missing_ok=True)

    def test_aggregate_rejects_manifest_that_omits_pinned_corpus_tests(self) -> None:
        profile = ready_profile()
        all_ids = test_runner.discover_test_ids(TEST262_ROOT, "test262")
        omitted = next(test_id for test_id in all_ids if test_id.startswith("runner/timeout"))
        incomplete = test_runner.build_manifest(
            "test262", [test_id for test_id in all_ids if test_id != omitted],
            BASELINE, profile, 1
        )
        with self.assertRaisesRegex(test_runner.RunnerError, "complete pinned corpus"):
            test_runner.aggregate_reports(
                incomplete,
                [],
                BASELINE,
                profile,
                TEST262_ROOT,
            )

    def test_aggregate_complete_is_false_for_every_blocking_result_class(self) -> None:
        manifest = manifest_for("test262", TEST262_ROOT, ready_profile())
        for status in ("FAIL", "HARNESS ERROR", "UNEXPECTED PASS"):
            with self.subTest(status=status):
                reports = [report_for_shard(manifest, 0)]
                reports[0]["results"][0]["status"] = status
                aggregate = test_runner.aggregate_reports(
                    manifest, reports, BASELINE, ready_profile(), TEST262_ROOT
                )
                self.assertFalse(aggregate["aggregation"]["complete"])
                self.assertTrue(
                    any(
                        f"blocking aggregate status {status}" in error
                        for error in aggregate["aggregation"]["errors"]
                    )
                )

    def test_aggregate_handles_malformed_status_without_type_error(self) -> None:
        manifest = manifest_for("test262", TEST262_ROOT, ready_profile())
        report = report_for_shard(manifest, 0)
        report["results"][0]["status"] = []
        aggregate = test_runner.aggregate_reports(
            manifest, [report], BASELINE, ready_profile(), TEST262_ROOT
        )
        self.assertFalse(aggregate["aggregation"]["complete"])
        self.assertTrue(
            any("invalid result classification" in error for error in aggregate["aggregation"]["errors"])
        )
        report_path = Path(self.id().replace(".", "_") + ".xml")
        try:
            test_runner.write_junit(
                report_path,
                "test262",
                aggregate["results"],
                aggregate["aggregation"]["errors"],
            )
            root = ET.parse(report_path).getroot()
            self.assertGreater(int(root.attrib["errors"]), 0)
            self.assertTrue(
                any(
                    node.attrib.get("type") == "HARNESS ERROR"
                    for node in root.findall(".//error")
                )
            )
        finally:
            report_path.unlink(missing_ok=True)

    def test_aggregate_detects_duplicate_shards_and_duplicate_test_results(self) -> None:
        manifest = manifest_for("test262", TEST262_ROOT, ready_profile())
        report = report_for_shard(manifest, 0)
        duplicate = dict(report)
        duplicate["results"] = [dict(result) for result in report["results"]]
        duplicate["results"][0]["status"] = "BOGUS"
        duplicate["results"].append(dict(duplicate["results"][0]))
        aggregate = test_runner.aggregate_reports(
            manifest, [report, duplicate], BASELINE, ready_profile(), TEST262_ROOT
        )
        self.assertFalse(aggregate["aggregation"]["complete"])
        self.assertEqual(aggregate["aggregation"]["duplicateShards"], [0])
        self.assertTrue(any("duplicate test result" in error for error in aggregate["aggregation"]["errors"]))
        self.assertTrue(any("invalid result classification" in error for error in aggregate["aggregation"]["errors"]))

    def test_parser_explicit_variants_are_sharded_together_and_ast_compared(self) -> None:
        profile = ready_profile()
        manifest = manifest_for("test262-parser-tests", PARSER_ROOT, profile, 4)
        assignments = {
            entry["id"]: entry["shardIndex"] for entry in manifest["tests"]
        }
        self.assertEqual(
            assignments["pass/valid.js"],
            assignments["pass-explicit/valid.js"],
        )
        report = test_runner.run_manifest(
            manifest,
            PARSER_ROOT,
            profile,
            FAKE_ADAPTER,
            assignments["pass/valid.js"],
            timeout_ms=1000,
            async_timeout_ms=1000,
        )
        pair_results = [
            result
            for result in report["results"]
            if result.get("equivalenceGroup") == "valid.js"
        ]
        self.assertEqual(len(pair_results), 2)
        self.assertEqual({result["status"] for result in pair_results}, {"PASS"})
        self.assertEqual(
            {
                result["variants"][0]["normalizedAstSha256"]
                for result in pair_results
            },
            {hashlib.sha256(b"normalized-equivalent-parser-fixture").hexdigest()},
        )

    def test_aggregate_independently_rejects_different_parser_ast_fingerprints(self) -> None:
        profile = ready_profile()
        manifest = manifest_for("test262-parser-tests", PARSER_ROOT, profile, 1)
        report = test_runner.run_manifest(
            manifest,
            PARSER_ROOT,
            profile,
            FAKE_ADAPTER,
            0,
            timeout_ms=1000,
            async_timeout_ms=1000,
        )
        pair = [
            result
            for result in report["results"]
            if result.get("equivalenceGroup") == "valid.js"
        ]
        pair[1]["variants"][0]["normalizedAstSha256"] = "b" * 64
        shard_report = {
            "schemaVersion": 1,
            "suite": manifest["suite"],
            "source": manifest["source"],
            "profile": manifest["profile"],
            "manifestSha256": manifest["manifestSha256"],
            "shardIndex": 0,
            "shardCount": 1,
            "results": report["results"],
        }
        aggregate = test_runner.aggregate_reports(
            manifest, [shard_report], BASELINE, profile, PARSER_ROOT
        )
        self.assertFalse(aggregate["aggregation"]["complete"])
        self.assertTrue(
            any(
                "different normalized AST fingerprints" in error
                for error in aggregate["aggregation"]["errors"]
            )
        )

    def test_parser_explicit_ast_mismatch_fails_both_members_of_pair(self) -> None:
        profile = ready_profile()
        manifest = manifest_for("test262-parser-tests", PARSER_ROOT, profile, 2)
        assignments = {
            entry["id"]: entry["shardIndex"] for entry in manifest["tests"]
        }
        report = test_runner.run_manifest(
            manifest,
            PARSER_ROOT,
            profile,
            FAKE_ADAPTER + ["--ast-mismatch-explicit"],
            assignments["pass/valid.js"],
            timeout_ms=1000,
            async_timeout_ms=1000,
        )
        pair_results = [
            result
            for result in report["results"]
            if result.get("equivalenceGroup") == "valid.js"
        ]
        self.assertEqual(len(pair_results), 2)
        self.assertEqual({result["status"] for result in pair_results}, {"FAIL"})
        self.assertEqual(
            {result["failureKind"] for result in pair_results},
            {"PARSER_AST_MISMATCH"},
        )


if __name__ == "__main__":
    unittest.main()
