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
        return dict(os.environ, PATH=str(self.base) + os.pathsep + os.environ["PATH"])

    def invoke(self, executable=None, timeout=5, env=None):
        args = [
            sys.executable, str(SCRIPT), "--output-dir", str(self.base / "logs"),
            "--timeout", str(timeout),
        ]
        if executable is not None:
            args += ["--executable", str(executable)]
        completed = subprocess.run(args, capture_output=True, text=True, timeout=15, env=env)
        report_file = self.base / "logs/report.json"
        self.assertTrue(report_file.is_file(), completed.stderr)
        return completed, json.loads(report_file.read_text())

    def assert_reaped(self, copies):
        for copy in copies:
            with self.subTest(copy=copy["copy"]), self.assertRaises(ProcessLookupError):
                os.kill(copy["pid"], 0)

    def test_runs_three_complete_suites_with_ten_threads(self):
        binary = self.executable(
            "import sys\n"
            "assert sys.argv[1:] == ['--nocapture', '--test-threads=10']\n"
            "print('all ten tests ran')\n"
        )
        completed, report = self.invoke(binary)
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertEqual(report["status"], "passed")
        self.assertEqual(len(report["copies"]), 3)
        self.assertEqual([copy["exit_code"] for copy in report["copies"]], [0, 0, 0])
        for copy in report["copies"]:
            self.assertIn("all ten tests ran", Path(copy["log"]).read_text())

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
