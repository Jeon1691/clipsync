# Copyright the ClipSync contributors.
# SPDX-License-Identifier: MIT
"""Check report isolation and Docker orchestration with synthetic tools, not build evidence."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


REPO = Path(__file__).resolve().parents[2]
DOCKER_FIXTURE = r'''#!/usr/bin/env python3
import hashlib, json, os, pathlib, subprocess, sys
args = sys.argv[1:]
with open(os.environ["DOCKER_FIXTURE_LOG"], "a") as log:
    log.write(json.dumps(args) + "\n")
mode = os.environ.get("DOCKER_FIXTURE_MODE", "success")
if args[0] == "build":
    tag = args[args.index("--tag") + 1]
    if mode == "fail-first" or (mode == "fail-second" and tag.endswith(":second")):
        raise SystemExit(77)
elif args[0] == "version":
    print("synthetic Docker fixture")
elif args[:2] == ["image", "inspect"]:
    print('[{"Id":"synthetic-fixture"}]')
elif args[0] == "run":
    if mode == "require-caller-ownership":
        expected = f"REPEATABILITY_REPORT_OWNER={os.getuid()}:{os.getgid()}"
        settings = [args[i + 1] for i, arg in enumerate(args) if arg == "--env"]
        if expected not in settings:
            print("exported output directories would remain owned by container root", file=sys.stderr)
            raise SystemExit(78)
    mounts = [args[i + 1] for i, arg in enumerate(args) if arg == "--mount"]
    fields = [dict(item.split("=", 1) for item in mount.split(",") if "=" in item) for mount in mounts]
    report = pathlib.Path(next(item["src"] for item in fields if item["dst"] == "/report"))
    bundle = next(item["src"] for item in fields if item["dst"] == "/input/source.bundle")
    heads = subprocess.check_output(["git", "bundle", "list-heads", bundle], text=True)
    (report / "bundle-heads.txt").write_text(heads)
    second = report.name == "second"
    packages = "different" if mode == "packages-differ" and second else "synthetic-native-package 1.0"
    content = b"different bytes\n" if mode == "outputs-differ" and second else b"synthetic output bytes\n"
    digest = hashlib.sha256(content).hexdigest()
    (report / "outputs").mkdir()
    (report / "outputs/synthetic-artifact").write_bytes(content)
    matching = b"matching output bytes\n"
    (report / "outputs/matching-artifact").write_bytes(matching)
    (report / "artifacts.txt").write_text("matching-artifact\nsynthetic-artifact\n")
    (report / "native-packages.txt").write_text(packages + "\n")
    (report / "first.sha256").write_text(hashlib.sha256(matching).hexdigest() + "  matching-artifact\n" +
                                       digest + "  synthetic-artifact\n")
'''


@unittest.skipIf(os.name == "nt", "Bash/Docker orchestration contracts run on Unix CI platforms")
class BuildReconstructionTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="clipsync-reconstruction-test-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.repo = self.root / "repository"
        (self.repo / "scripts").mkdir(parents=True)
        self.tools = self.root / "tools"
        self.tools.mkdir()
        shutil.copy(REPO / "scripts/reproduce-linux-build.sh", self.repo / "scripts")
        (self.repo / "Dockerfile.reproducible").write_text("FROM committed-synthetic-fixture\n")
        docker = self.tools / "docker"
        docker.write_text(DOCKER_FIXTURE)
        docker.chmod(0o755)
        self.git("init", "-q")
        self.git("add", ".")
        self.git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.test",
                 "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null",
                 "commit", "-qm", "synthetic reconstruction fixture")
        self.commit = self.git("rev-parse", "HEAD").strip()
        self.parent = self.root / "requested report parent"
        self.log = self.root / "docker-arguments.jsonl"

    def git(self, *args):
        return subprocess.check_output(["git", *args], cwd=self.repo, text=True)

    def invoke(self, mode="success", platform=None):
        env = os.environ.copy()
        env.pop("CLIPSYNC_REPRO_PLATFORM", None)
        env.update(PATH=str(self.tools) + os.pathsep + env["PATH"],
                   DOCKER_FIXTURE_LOG=str(self.log), DOCKER_FIXTURE_MODE=mode)
        if platform is not None:
            env["CLIPSYNC_REPRO_PLATFORM"] = platform
        return subprocess.run(["bash", "scripts/reproduce-linux-build.sh", str(self.parent)],
                              cwd=self.repo, env=env, capture_output=True, text=True, timeout=30)

    def runs(self):
        return sorted(path for path in self.parent.glob("run.*") if path.is_dir())

    def test_failed_rerun_preserves_old_source_bound_report_and_has_no_success_markers(self):
        self.parent.mkdir()
        old = {"source-commit.txt": "old-source\n", "comparison.txt": "old success\n",
               "reproduced.sha256": "old hash\n"}
        for name, content in old.items():
            (self.parent / name).write_text(content)
        result = self.invoke(mode="fail-first")
        self.assertEqual(result.returncode, 77, result.stderr)
        self.assertEqual(len(self.runs()), 1)
        current = self.runs()[0]
        self.assertIn(f"Report: {current}", result.stdout)
        self.assertEqual((current / "source-commit.txt").read_text().strip(), self.commit)
        self.assertFalse((current / "comparison.txt").exists())
        self.assertFalse((current / "reproduced.sha256").exists())
        for name, content in old.items():
            self.assertEqual((self.parent / name).read_text(), content)

    def test_each_success_has_its_own_source_recipe_platform_and_report(self):
        # An uncommitted recipe must not alter the recipe bound into either report.
        (self.repo / "Dockerfile.reproducible").write_text("FROM uncommitted-synthetic-fixture\n")
        for _ in range(2):
            result = self.invoke()
            self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(self.runs()), 2)
        for current in self.runs():
            self.assertEqual((current / "source-commit.txt").read_text().strip(), self.commit)
            self.assertEqual((current / "platform.txt").read_text(), "linux/amd64\n")
            self.assertEqual((current / "Dockerfile.reproducible").read_text(), "FROM committed-synthetic-fixture\n")
            self.assertTrue((current / "comparison.txt").is_file())
            self.assertEqual((current / "reproduced.sha256").read_bytes(), (current / "first/first.sha256").read_bytes())
            for build in ["first", "second"]:
                self.assertIn(self.commit, (current / build / "bundle-heads.txt").read_text())
                self.assertFalse((current / build / "outputs").exists())
        self.assert_platform_arguments("linux/amd64", expected_builds=4)

    def assert_platform_arguments(self, platform, expected_builds):
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        builds = [call for call in calls if call[0] == "build"]
        runs = [call for call in calls if call[0] == "run"]
        self.assertEqual(len(builds), expected_builds)
        self.assertEqual(len(runs), expected_builds)
        for call in builds + runs:
            self.assertEqual(call[call.index("--platform") + 1], platform)
        for call in builds:
            self.assertIn("--no-cache", call)

    def test_explicit_arm64_is_used_for_both_environment_builds_and_runs(self):
        result = self.invoke(platform="linux/arm64")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.runs()[0] / "platform.txt").read_text(), "linux/arm64\n")
        self.assert_platform_arguments("linux/arm64", expected_builds=2)

    def test_build_exports_request_the_callers_ownership_before_host_cleanup(self):
        result = self.invoke(mode="require-caller-ownership")
        self.assertEqual(result.returncode, 0, result.stderr)
        current, = self.runs()
        self.assertTrue((current / "comparison.txt").is_file())
        for build in ["first", "second"]:
            self.assertFalse((current / build / "outputs").exists())

    def test_malformed_caller_ids_fail_before_docker_or_report_creation(self):
        identity = self.tools / "id"
        identity.write_text("#!/bin/sh\nprintf '%s\\n' '0 --reference=/other'\n")
        identity.chmod(0o755)
        result = self.invoke()
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertFalse(self.parent.exists())
        self.assertFalse(self.log.exists())

    def test_second_build_and_comparison_failures_do_not_publish_success(self):
        for mode in ["fail-second", "packages-differ", "outputs-differ"]:
            with self.subTest(mode=mode):
                before = set(self.runs())
                result = self.invoke(mode=mode)
                self.assertNotEqual(result.returncode, 0)
                current, = set(self.runs()) - before
                self.assertFalse((current / "comparison.txt").exists())
                self.assertFalse((current / "reproduced.sha256").exists())

    def test_differing_output_bytes_survive_container_and_image_cleanup(self):
        result = self.invoke(mode="outputs-differ")
        self.assertEqual(result.returncode, 1, result.stderr)
        current, = self.runs()
        self.assertEqual((current / "differences/first/synthetic-artifact").read_bytes(),
                         b"synthetic output bytes\n")
        self.assertEqual((current / "differences/second/synthetic-artifact").read_bytes(),
                         b"different bytes\n")
        self.assertIn("DIFFERS: synthetic-artifact",
                      (current / "comparison-failed.txt").read_text())
        self.assertTrue((current / "differences/synthetic-artifact.bytes.txt").read_text())
        for build in ["first", "second"]:
            self.assertFalse((current / "differences" / build / "matching-artifact").exists())
            self.assertFalse((current / build / "outputs").exists())
        self.assertFalse((current / "comparison.txt").exists())
        self.assertFalse((current / "reproduced.sha256").exists())
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertTrue(any(call[:2] == ["image", "rm"] for call in calls))

    def test_unknown_platform_fails_before_docker_or_report_creation(self):
        result = self.invoke(platform="linux/unknown")
        self.assertEqual(result.returncode, 2)
        self.assertFalse(self.parent.exists())
        self.assertFalse(self.log.exists())


if __name__ == "__main__":
    unittest.main()
