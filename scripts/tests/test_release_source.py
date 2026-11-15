# Copyright the ClipSync contributors.
# SPDX-License-Identifier: MIT
"""Exercise publication guards against real, isolated local Git repositories."""

import os
import shutil
from pathlib import Path
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "verify-release-source.sh"


@unittest.skipIf(os.name == "nt" or any(shutil.which(tool) is None for tool in ["bash", "git"]),
                 "source publication uses Bash and Git on the Ubuntu publisher")
class ReleaseSourceTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory(prefix="clipsync-release-source-")
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.env = {key: value for key, value in os.environ.items()
                    if not key.startswith("GIT_CONFIG_") and key not in {"GH_TOKEN", "GITHUB_TOKEN"}}
        self.env.update({
            "HOME": str(self.root), "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": os.devnull,
            "GIT_AUTHOR_NAME": "Synthetic release fixture", "GIT_COMMITTER_NAME": "Synthetic release fixture",
            "GIT_AUTHOR_EMAIL": "fixture@example.invalid", "GIT_COMMITTER_EMAIL": "fixture@example.invalid",
            "GIT_TERMINAL_PROMPT": "0", "GITHUB_REPOSITORY": "fixture/repository",
            "GITHUB_REF": "refs/tags/v1.2.3",
        })
        author = self.root / "author"
        self.git(self.root, "init", "--initial-branch=master", str(author))
        self.first = self.commit(author, "first")
        self.git(author, "-c", "tag.gpgsign=false", "tag", "-a", "v1.2.3", "-m", "fixture")
        self.second = self.commit(author, "second")
        self.git(author, "checkout", "--orphan", "disconnected")
        self.disconnected = self.commit(author, "disconnected")
        self.origin = self.root / "origin.git"
        self.git(self.root, "clone", "--bare", str(author), str(self.origin))
        self.git(self.root, "--git-dir", str(self.origin), "symbolic-ref", "HEAD", "refs/heads/master")
        self.checkout = self.root / "checkout"
        self.git(self.root, "clone", str(self.origin), str(self.checkout))
        self.git(self.checkout, "checkout", "--detach", "refs/tags/v1.2.3")
        self.env["GITHUB_SHA"] = self.first
        tools = self.root / "bin"
        tools.mkdir()
        gh = tools / "gh"
        gh.write_text('#!/bin/sh\nprintf "%s\\n" "${CLIPSYNC_TEST_DEFAULT_BRANCH:-master}"\n')
        gh.chmod(0o755)
        self.env["PATH"] = str(tools) + os.pathsep + self.env["PATH"]

    def git(self, directory, *args):
        return subprocess.run(["git", *args], cwd=directory, env=self.env,
                              capture_output=True, text=True, check=True).stdout.strip()

    def commit(self, directory, contents):
        (directory / "fixture").write_text(contents)
        self.git(directory, "add", "fixture")
        self.git(directory, "-c", "commit.gpgsign=false", "commit", "-m", contents)
        return self.git(directory, "rev-parse", "HEAD")

    def verify(self, **changes):
        env = self.env | changes
        return subprocess.run(["bash", str(SCRIPT)], cwd=self.checkout, env=env,
                              capture_output=True, text=True, timeout=10)

    def test_annotated_tag_on_default_branch_is_accepted(self):
        result = self.verify()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.git(self.checkout, "rev-parse", "HEAD"), self.first)

    def test_tag_retargeted_after_initial_check_is_rejected_before_promotion(self):
        self.assertEqual(self.verify().returncode, 0)
        self.git(self.root, "--git-dir", str(self.origin), "update-ref", "refs/tags/v1.2.3", self.second)
        result = self.verify()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("does not identify this workflow source commit", result.stderr)
        # Only local tracking refs may change; neither checkout nor origin is rewritten.
        self.assertEqual(self.git(self.checkout, "rev-parse", "HEAD"), self.first)
        self.assertEqual(self.git(self.root, "--git-dir", str(self.origin), "rev-parse", "refs/tags/v1.2.3"), self.second)

    def test_current_default_branch_must_contain_the_tagged_commit(self):
        result = self.verify(CLIPSYNC_TEST_DEFAULT_BRANCH="disconnected")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("ancestor of the current default branch", result.stderr)

    def test_deleted_remote_tag_is_rejected_despite_stale_local_tag(self):
        self.assertEqual(self.verify().returncode, 0)
        self.git(self.root, "--git-dir", str(self.origin), "update-ref", "-d", "refs/tags/v1.2.3")
        self.assertNotEqual(self.verify().returncode, 0)

    def test_nonrelease_ref_and_malformed_source_digest_are_rejected(self):
        self.assertNotEqual(self.verify(GITHUB_REF="refs/heads/master").returncode, 0)
        self.assertNotEqual(self.verify(GITHUB_SHA="not-a-commit").returncode, 0)


if __name__ == "__main__":
    unittest.main()
