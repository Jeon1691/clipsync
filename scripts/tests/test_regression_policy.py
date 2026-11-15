# Copyright the ClipSync contributors.
# SPDX-License-Identifier: MIT
"""Check the regression gate's bookkeeping and evidence validation, without building Rust."""

import copy
import importlib.util
import json
import io
import os
from contextlib import redirect_stdout
from contextlib import redirect_stderr
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).resolve().parents[1] / "check-regressions.py"
SPEC = importlib.util.spec_from_file_location("regression_policy", SCRIPT)
POLICY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(POLICY)


class RegressionPolicyTests(unittest.TestCase):
    def test_compiled_discovery_cannot_be_labeled_as_another_platform(self):
        with patch.object(POLICY, "current_platform", return_value="macos"), \
                patch.object(POLICY, "discover_rust_tests", side_effect=AssertionError("must not build")), \
                redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()):
            self.assertEqual(POLICY.main(["--verify-tests", "--platform", "windows"]), 1)

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="clipsync-policy-")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        source = self.root / "crates/clipsync-demo/src/lib.rs"
        source.parent.mkdir(parents=True)
        source.write_text("#[test]\nfn protects_behavior() {}\nfn ordinary_helper() {}\n")
        self.check = {
            "kind": "rust", "source": "crates/clipsync-demo/src/lib.rs",
            "package": "clipsync-demo", "target": "lib:clipsync_demo",
            "test": "tests::protects_behavior", "platforms": ["linux", "macos", "windows"],
        }
        self.inventory = {("clipsync-demo", "lib:clipsync_demo"): {"tests::protects_behavior"}}

    def manifest(self, covered):
        return {
            "schema_version": 1, "minimum_percent": 50,
            "defects": [{
                "id": f"F{number:02}", "kind": "Runtime", "fixes": ["fixture"],
                "description": f"Independent historical defect {number}",
                "checks": [copy.deepcopy(self.check)] if number <= covered else [],
            } for number in range(1, 85)],
        }

    def evaluate(self, document, **kwargs):
        return POLICY.evaluate(document, self.root, "linux", rust_tests=self.inventory, **kwargs)

    def run_with_legacy_encoding(self, document, mode):
        script = self.root / "scripts/check-regressions.py"
        script.parent.mkdir(exist_ok=True)
        script.write_bytes(SCRIPT.read_bytes())
        manifest = self.root / "docs/regressions.json"
        manifest.parent.mkdir(exist_ok=True)
        manifest.write_text(json.dumps(document, ensure_ascii=False), encoding="utf-8")
        # POSIX's C locale supplies a real ASCII default. The isolated cp1252
        # mode also reproduces Windows' undefined 0x90 byte on UTF-8 systems.
        bootstrap = '''
import io, locale, runpy, sys
mode, script, *arguments = sys.argv[1:]
assert sys.flags.utf8_mode == 0
if mode == "native":
    assert locale.getpreferredencoding(False).lower().replace("-", "") != "utf8"
else:
    original_open = io.open
    def windows_default_open(file, mode="r", buffering=-1, encoding=None,
                             errors=None, newline=None, closefd=True, opener=None):
        if "b" not in mode and encoding in (None, "locale"):
            encoding = "cp1252"
        return original_open(file, mode, buffering, encoding, errors, newline, closefd, opener)
    io.open = windows_default_open
sys.argv = [script, *arguments]
runpy.run_path(script, run_name="__main__")
'''
        env = dict(os.environ, PYTHONUTF8="0", LC_ALL="C", PYTHONCOERCECLOCALE="0")
        result = subprocess.run(
            [sys.executable, "-c", bootstrap, mode, str(script), "--platform", "linux",
             "--report", "report.json"], cwd=self.root, env=env,
            capture_output=True, encoding="utf-8", timeout=15,
        )
        report = json.loads((self.root / "report.json").read_text(encoding="utf-8"))
        return result, report

    def test_utf8_repository_sources_and_manifest_ignore_the_process_default_encoding(self):
        rust = self.root / self.check["source"]
        rust.write_text("// UTF-8 fixture: А – 한글\n#[test]\nfn protects_behavior() {}\n",
                        encoding="utf-8")
        python = self.root / "scripts/tests/test_unicode.py"
        python.parent.mkdir(parents=True)
        python.write_text("# UTF-8 fixture: А – 한글\nclass Checks:\n    def test_behavior(self): pass\n",
                          encoding="utf-8")
        document = self.manifest(42)
        document["defects"][0]["description"] = "UTF-8 ledger text: А – 한글"
        document["defects"][0]["checks"] = [{
            "kind": "python", "source": "scripts/tests/test_unicode.py",
            "test": "test_unicode.Checks.test_behavior", "platforms": ["linux"],
        }]
        original_rust, original_python = rust.read_bytes(), python.read_bytes()
        for mode in (["native", "cp1252"] if os.name != "nt" else ["cp1252"]):
            with self.subTest(encoding=mode):
                result, report = self.run_with_legacy_encoding(document, mode)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertTrue(report["passed"], report["errors"])
                self.assertEqual((report["covered"], report["total"]), (42, 84))
                self.assertEqual(rust.read_bytes(), original_rust)
                self.assertEqual(python.read_bytes(), original_python)

    def test_invalid_utf8_source_is_rejected_without_replacement_or_repair(self):
        rust = self.root / self.check["source"]
        malformed = b"// invalid UTF-8: \xff\n#[test]\nfn protects_behavior() {}\n"
        rust.write_bytes(malformed)
        result, report = self.run_with_legacy_encoding(self.manifest(42), "cp1252")
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertFalse(report["passed"])
        self.assertTrue(any("utf-8" in error and "decode" in error for error in report["errors"]))
        self.assertEqual(rust.read_bytes(), malformed)

    def test_41_of_84_is_below_the_required_half(self):
        report = self.evaluate(self.manifest(41))
        self.assertFalse(report["passed"])
        self.assertEqual((report["covered"], report["total"]), (41, 84))

    def test_42_of_84_reaches_the_required_half(self):
        report = self.evaluate(self.manifest(42))
        self.assertTrue(report["passed"], report["errors"])

    def test_removing_an_uncovered_baseline_defect_cannot_raise_the_ratio(self):
        document = self.manifest(60)
        document["defects"].pop()
        report = self.evaluate(document)
        self.assertFalse(report["passed"])
        self.assertTrue(any("F84" in error for error in report["errors"]))

    def test_new_defects_expand_the_complete_denominator(self):
        document = self.manifest(42)
        document["defects"].append({
            "id": "F85", "kind": "Runtime", "fixes": ["new-fix"],
            "description": "New independently corrected failure", "checks": [],
        })
        report = self.evaluate(document)
        self.assertFalse(report["passed"])
        self.assertEqual((report["covered"], report["total"]), (42, 85))

    def test_missing_source_invalidates_a_claim_even_above_half(self):
        document = self.manifest(60)
        document["defects"][0]["checks"][0]["source"] = "crates/clipsync-demo/src/missing.rs"
        report = self.evaluate(document)
        self.assertFalse(report["passed"])
        self.assertEqual(report["covered"], 59)

    def test_an_ordinary_function_cannot_be_claimed_as_a_discovered_test(self):
        document = self.manifest(60)
        document["defects"][0]["checks"][0]["test"] = "ordinary_helper"
        report = self.evaluate(document)
        self.assertFalse(report["passed"])
        self.assertTrue(any("not discovered" in error for error in report["errors"]))

    def test_the_same_name_in_a_different_target_does_not_satisfy_a_claim(self):
        document = self.manifest(60)
        document["defects"][0]["checks"][0]["target"] = "test:different_target"
        self.assertFalse(self.evaluate(document)["passed"])

    def test_manual_covered_booleans_are_rejected(self):
        document = self.manifest(42)
        document["defects"][-1]["covered"] = True
        self.assertFalse(self.evaluate(document)["passed"])

    def test_source_references_cannot_escape_the_repository(self):
        document = self.manifest(60)
        document["defects"][0]["checks"][0]["source"] = "../outside.rs"
        self.assertFalse(self.evaluate(document)["passed"])

    def test_off_platform_cases_are_registered_without_claiming_local_execution(self):
        document = self.manifest(42)
        document["defects"][0]["checks"][0]["platforms"] = ["macos"]
        report = self.evaluate(document)
        self.assertTrue(report["passed"], report["errors"])
        self.assertEqual(report["defects"][0]["checks"][0]["status"], "other-platform")

    def test_empty_platform_scope_cannot_bypass_discovery(self):
        document = self.manifest(60)
        document["defects"][0]["checks"][0]["platforms"] = []
        self.assertFalse(self.evaluate(document)["passed"])

    def test_a_lower_manifest_threshold_cannot_disable_the_half_requirement(self):
        document = self.manifest(1)
        document["minimum_percent"] = 1
        self.assertFalse(self.evaluate(document)["passed"])

    def test_echoing_a_workflow_command_is_not_an_executable_check(self):
        document = self.manifest(42)
        source = self.root / ".github/workflows/security.yml"
        source.parent.mkdir(parents=True)
        source.write_text("fixture workflow\n")
        document["defects"][0]["checks"] = [{
            "kind": "workflow", "source": ".github/workflows/security.yml",
            "job": "fuzz", "step": "Check lock", "platforms": ["linux"],
            "commands": [{"prefix": ["cargo", "metadata"], "required": ["--locked"]}],
        }]
        def workflow(_path):
            return {"jobs": {"fuzz": {"steps": [{
                "name": "Check lock", "run": "echo 'cargo metadata --locked'",
            }]}}}
        self.assertFalse(self.evaluate(document, workflow_loader=workflow)["passed"])

    def test_workflow_checks_accept_real_commands_with_additional_arguments(self):
        commands = POLICY.shell_commands("# cargo metadata --locked\ncargo metadata --format-version 1 --locked > /dev/null\n")
        self.assertTrue(POLICY.matches_command(commands, {
            "prefix": ["cargo", "metadata"], "required": ["--locked"],
        }))
        self.assertFalse(POLICY.matches_command(commands, {
            "prefix": ["cargo", "metadata"], "required": ["--offline"],
        }))

    def test_test_listing_does_not_count_benchmarks_or_summary_text(self):
        listing = "a::case: test\nthroughput: benchmark\n\n1 test, 1 benchmark\n"
        self.assertEqual(POLICY.parse_test_list(listing), {"a::case"})

    def test_report_is_machine_readable(self):
        report = self.evaluate(self.manifest(42))
        self.assertEqual(json.loads(json.dumps(report))["covered"], 42)

    def test_duplicate_ids_cannot_inflate_covered_defect_counts(self):
        document = self.manifest(41)
        document["defects"].append(copy.deepcopy(document["defects"][0]))
        report = self.evaluate(document)
        self.assertFalse(report["passed"])
        self.assertEqual((report["covered"], report["total"]), (41, 84))

    def test_release_only_check_must_precede_the_publish_job(self):
        check = {
            "job": "smoke", "step": "Run portable binary", "execution": "release-before-publish",
            "commands": [{"prefix": ["docker", "run"], "required": ["--read-only"]}],
        }
        workflow = {"jobs": {
            "smoke": {"steps": [{"name": "Run portable binary", "run": "docker run --read-only image"}]},
            "publish": {"steps": []},
        }}
        with self.assertRaises(POLICY.PolicyError):
            POLICY.verify_workflow(check, workflow)
        workflow["jobs"]["publish"]["needs"] = ["smoke"]
        POLICY.verify_workflow(check, workflow)

    def test_off_platform_workflow_registration_does_not_require_ruby(self):
        document = self.manifest(42)
        source = self.root / ".github/workflows/release.yml"
        source.parent.mkdir(parents=True)
        source.write_text("fixture workflow\n")
        document["defects"][0]["checks"] = [{
            "kind": "workflow", "source": ".github/workflows/release.yml", "job": "smoke",
            "step": "Run portable binary", "platforms": ["linux"],
            "commands": [{"prefix": ["docker", "run"]}],
        }]

        def unavailable_ruby(_path):
            self.fail("Windows validation must not invoke Ruby for a Linux-only workflow")

        report = POLICY.evaluate(document, self.root, "windows", rust_tests=self.inventory,
                                 workflow_loader=unavailable_ruby)
        self.assertTrue(report["passed"], report["errors"])

    def test_malformed_workflow_reports_a_policy_error(self):
        with self.assertRaises(POLICY.PolicyError):
            POLICY.verify_workflow({}, None)

    def test_mobile_library_artifact_types_share_the_lib_target_namespace(self):
        metadata = {"workspace_members": ["mobile-id"], "packages": [{"id": "mobile-id", "name": "clipsync-mobile"}]}
        artifact = {
            "reason": "compiler-artifact", "package_id": "mobile-id", "executable": "/fixture/mobile-tests",
            "profile": {"test": True}, "target": {"name": "clipsync_mobile", "kind": ["staticlib", "cdylib", "rlib"]},
        }

        def cargo_output(command, _root):
            if command[:2] == ["cargo", "metadata"]:
                return json.dumps(metadata)
            if command[:2] == ["cargo", "test"]:
                return json.dumps(artifact)
            if command[0] == "/fixture/mobile-tests":
                return "" if "--ignored" in command else "bridge_regression: test\n"
            self.fail(f"unexpected discovery command: {command}")

        with patch.object(POLICY, "run", side_effect=cargo_output):
            try:
                inventory = POLICY.discover_rust_tests(self.root)
            except POLICY.PolicyError as error:
                self.fail(str(error))
        self.assertEqual(inventory, {("clipsync-mobile", "lib:clipsync_mobile"): {"bridge_regression"}})


if __name__ == "__main__":
    unittest.main()
