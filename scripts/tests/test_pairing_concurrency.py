# Copyright the ClipSync contributors.
# SPDX-License-Identifier: MIT
"""Exercise the concurrency runner with real, controlled subprocesses."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "test-pairing-concurrency.py"


@unittest.skipUnless(os.name == "posix", "executable fixtures and process groups require POSIX")
class PairingConcurrencyTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory(prefix="clipsync-concurrency-harness-")
        self.addCleanup(directory.cleanup)
        self.base = Path(directory.name)
        # Keep these tests off the real keychain: a fake security(1) logs each
        # deletion and reports one remaining test entry, then none (exit 44).
        self.security_calls = self.base / "security-calls"
        self.executable(
            "import sys\n"
            f"with open({str(self.security_calls)!r}, 'a') as log:\n"
            "    log.write(' '.join(sys.argv[1:]) + '\\n')\n"
            f"sys.exit(0 if len(open({str(self.security_calls)!r}).readlines()) == 1 else 44)\n",
            name="security",
        )
        self.env = dict(os.environ, PATH=str(self.base) + os.pathsep + os.environ["PATH"])

    def executable(self, body, name="e2e"):
        filename = self.base / name
        filename.write_text("#!" + sys.executable + "\n" + body)
        filename.chmod(0o755)
        return filename

    def cargo_fixture(self, binary, delay=0):
        artifact = {
            "reason": "compiler-artifact",
            "target": {"name": "e2e", "kind": ["test"]},
            "executable": str(binary),
        }
        self.executable(
            "import json, sys, time\n"
            "assert sys.argv[1:] == ['test', '-p', 'clipsync-relay', '--test', 'e2e', "
            "'--locked', '--no-run', '--message-format=json']\n"
            f"time.sleep({delay})\n"
            f"print(json.dumps({artifact!r}))\n",
            name="cargo",
        )
        return self.env

    def invoke(self, executable=None, timeout=5, env=None):
        args = [
            sys.executable, str(SCRIPT), "--output-dir", str(self.base / "logs"),
            "--timeout", str(timeout),
        ]
        if executable is not None:
            args += ["--executable", str(executable)]
        completed = subprocess.run(
            args, capture_output=True, text=True, timeout=15, env=self.env if env is None else env,
        )
        report_file = self.base / "logs/report.json"
        self.assertTrue(report_file.is_file(), completed.stderr)
        return completed, json.loads(report_file.read_text())

    def assert_reaped(self, copies):
        for copy in copies:
            with self.subTest(copy=copy["copy"]), self.assertRaises(ProcessLookupError):
                os.kill(copy["pid"], 0)

    def test_runs_three_complete_suites_with_ten_threads(self):
        binary = self.executable(
            "import os, sys\n"
            "assert sys.argv[1:] == ['--nocapture', '--test-threads=10', '--skip', "
            "'e2e_stores_never_mirror_secrets_into_the_os_keychain']\n"
            "assert os.environ['CLIPSYNC_TEST_NATIVE_KEYCHAIN'] == '1'\n"
            "print('all ten tests ran')\n"
        )
        completed, report = self.invoke(binary)
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertEqual(report["status"], "passed")
        self.assertEqual(len(report["copies"]), 3)
        self.assertEqual([copy["exit_code"] for copy in report["copies"]], [0, 0, 0])
        for copy in report["copies"]:
            self.assertIn("all ten tests ran", Path(copy["log"]).read_text())

    @unittest.skipUnless(sys.platform == "darwin", "only macOS runs use the native keychain")
    def test_removes_every_test_service_entry_and_nothing_else(self):
        completed, report = self.invoke(self.executable("print('suite ran')\n"))
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertEqual(report["test_keychain_entries_removed"], 1)
        self.assertEqual(
            self.security_calls.read_text().splitlines(),
            ["delete-generic-password -s dev.clipsync.test"] * 2,
        )

    @unittest.skipUnless(sys.platform == "darwin", "only macOS runs use the native keychain")
    def test_passing_suites_that_never_reached_the_keychain_fail(self):
        self.security_calls.write_text("already empty\n")
        completed, report = self.invoke(self.executable("print('suite ran')\n"))
        self.assertNotEqual(completed.returncode, 0)
        self.assertEqual(report["status"], "failed")
        self.assertEqual(report["test_keychain_entries_removed"], 0)
        self.assertIn("opt-in had no effect", report["error"])

    @unittest.skipUnless(sys.platform == "darwin", "only macOS runs use the native keychain")
    def test_a_keychain_that_refuses_deletion_fails_the_run(self):
        self.executable("import sys\nsys.exit(51)\n", name="security")
        completed, report = self.invoke(self.executable("print('suite ran')\n"))
        self.assertNotEqual(completed.returncode, 0)
        self.assertEqual(report["status"], "failed")
        self.assertIn("exit 51", report["keychain_cleanup_error"])

    def test_opt_in_names_match_clipsync_storage(self):
        harness = (SCRIPT.parent / "test-pairing-concurrency.py").read_text()
        storage = (SCRIPT.parents[1] / "crates/clipsync-storage/src/secrets.rs").read_text()
        for name, value in [
            ("TEST_NATIVE_KEYCHAIN_ENV", "CLIPSYNC_TEST_NATIVE_KEYCHAIN"),
            ("TEST_NATIVE_KEYCHAIN_OPT_IN", "1"),
            ("TEST_KEYCHAIN_SERVICE", "dev.clipsync.test"),
        ]:
            with self.subTest(name=name):
                self.assertIn(f'const {name}: &str = "{value}";', storage)
                self.assertIn(f'"{value}"', harness)

    def test_timeout_kills_and_reaps_all_three_children(self):
        binary = self.executable("import time\ntime.sleep(60)\n")
        completed, report = self.invoke(binary, timeout=2)
        self.assertEqual(completed.returncode, 1)
        self.assertEqual(report["status"], "timed_out")
        self.assertEqual(len(report["copies"]), 3)
        for copy in report["copies"]:
            self.assertLess(copy["exit_code"], 0)
        self.assert_reaped(report["copies"])

    def test_child_failure_is_nonzero_and_other_children_are_reaped(self):
        binary = self.executable("import sys\nprint('failed suite')\nsys.exit(7)\n")
        completed, report = self.invoke(binary)
        self.assertEqual(completed.returncode, 1)
        self.assertEqual(report["status"], "failed")
        self.assertEqual(len(report["copies"]), 3)
        self.assertIn(7, [copy["exit_code"] for copy in report["copies"]])
        self.assert_reaped(report["copies"])

    def test_default_discovery_uses_cargo_test_artifact(self):
        binary = self.executable("print('discovered e2e')\n")
        completed, report = self.invoke(env=self.cargo_fixture(binary))
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertEqual(report["executable"], str(binary.resolve()))
        self.assertEqual(report["build"]["exit_code"], 0)

    def test_timeout_kills_descendants_in_the_child_process_groups(self):
        binary = self.executable(
            "import subprocess, sys, time\n"
            "child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)'])\n"
            "print(child.pid, flush=True)\n"
            "time.sleep(60)\n"
        )
        completed, report = self.invoke(binary, timeout=2)
        self.assertEqual(completed.returncode, 1)
        self.assertEqual(report["status"], "timed_out")
        self.assertEqual(len(report["copies"]), 3)
        self.assert_reaped(report["copies"])
        for copy in report["copies"]:
            descendant = int(Path(copy["log"]).read_text().strip())
            status = subprocess.run(
                ["ps", "-p", str(descendant), "-o", "stat="], capture_output=True, text=True,
            )
            # Orphaned descendants may briefly await init's reap after being killed.
            self.assertTrue(
                not status.stdout.strip() or status.stdout.strip().startswith("Z"), status.stdout,
            )

    def test_deadline_is_shared_between_cargo_build_and_test_copies(self):
        binary = self.executable("import time\ntime.sleep(3)\n")
        completed, report = self.invoke(timeout=4, env=self.cargo_fixture(binary, delay=2))
        self.assertEqual(completed.returncode, 1)
        self.assertEqual(report["build"]["exit_code"], 0)
        self.assertEqual(report["status"], "timed_out")
        self.assertEqual(len(report["copies"]), 3)
        self.assert_reaped(report["copies"])


if __name__ == "__main__":
    unittest.main()
