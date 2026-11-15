# Copyright the ClipSync contributors.
# SPDX-License-Identifier: MIT
"""Exercise the packaging helper's debug/cross-target contracts with isolated tools."""

import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import unittest


REPO = Path(__file__).resolve().parents[2]


@unittest.skipIf(os.name == "nt", "Bash packaging contracts run on the Unix CI platforms")
class PackageReleaseTests(unittest.TestCase):
    def run_package(self, overrides=None, target=None, strip_fails=False):
        with tempfile.TemporaryDirectory(prefix="clipsync-package-") as directory:
            root = Path(directory)
            (root / "scripts").mkdir()
            (root / "tools").mkdir()
            source = Path(os.environ.get("CLIPSYNC_PACKAGING_SCRIPT", REPO / "scripts/package-release.sh"))
            shutil.copy(source, root / "scripts/package-release.sh")
            for name in ["README.md", "LICENSE-MIT"]:
                (root / name).write_text("synthetic packaging fixture\n")
            helpers = {
                "rustc": "#!/bin/sh\necho 'host: x86_64-unknown-linux-gnu'\n",
                "rustup": "#!/bin/sh\nexit 0\n",
                "cargo": """#!/bin/sh
set -eu
target=''
while [ "$#" -gt 0 ]; do
  if [ "$1" = '--target' ]; then shift; target="$1"; fi
  shift
done
directory="target/${target:+$target/}release"
mkdir -p "$directory"
printf 'EXECUTABLE' > "$directory/clipsync"
if [ "${CARGO_PROFILE_RELEASE_STRIP:-symbols}" = 'none' ]; then
  printf ' DEBUG-SYMBOLS' >> "$directory/clipsync"
fi
printf '%s' "${CARGO_PROFILE_RELEASE_STRIP:-symbols}" > profile-strip.txt
""",
                "strip": "#!/bin/sh\n" + (
                    "echo 'unsupported target format' >&2\nexit 1\n" if strip_fails else
                    "for argument do binary=\"$argument\"; done\nprintf 'EXECUTABLE' > \"$binary\"\n"
                ),
            }
            for name, text in helpers.items():
                path = root / "tools" / name
                path.write_text(text)
                path.chmod(0o755)
            env = os.environ.copy()
            for name in ["CLIPSYNC_PRESERVE_DEBUG", "CARGO_PROFILE_RELEASE_DEBUG", "CARGO_PROFILE_RELEASE_STRIP"]:
                env.pop(name, None)
            env["PATH"] = str(root / "tools") + os.pathsep + env["PATH"]
            env.update(overrides or {})
            command = ["bash", "scripts/package-release.sh"] + ([target] if target else [])
            result = subprocess.run(command, cwd=root, env=env, capture_output=True, text=True, timeout=30)
            self.assertEqual(result.returncode, 0, result.stderr)
            chosen = target or "x86_64-unknown-linux-gnu"
            archive = root / "dist" / f"clipsync-{chosen}.tar.gz"
            self.assertTrue(archive.is_file())
            with tarfile.open(archive) as packaged:
                binary = packaged.extractfile(f"clipsync-{chosen}/clipsync").read()
            checksum = (root / "dist" / f"clipsync-{chosen}.sha256").read_text().split()[0]
            self.assertEqual(hashlib.sha256(archive.read_bytes()).hexdigest(), checksum)
            return binary, (root / "profile-strip.txt").read_text(), result.stderr

    def test_requested_debug_information_survives_the_archive(self):
        binary, setting, _ = self.run_package({"CARGO_PROFILE_RELEASE_DEBUG": "2"})
        self.assertIn(b"DEBUG-SYMBOLS", binary)
        self.assertEqual(setting, "none")

    def test_standard_no_strip_setting_survives_the_archive(self):
        binary, setting, _ = self.run_package({"CARGO_PROFILE_RELEASE_STRIP": "none"})
        self.assertIn(b"DEBUG-SYMBOLS", binary)
        self.assertEqual(setting, "none")

    def test_explicit_debug_preservation_survives_the_archive(self):
        binary, _, _ = self.run_package({"CLIPSYNC_PRESERVE_DEBUG": "1"})
        self.assertIn(b"DEBUG-SYMBOLS", binary)

    def test_default_production_archive_can_remain_stripped(self):
        binary, _, _ = self.run_package()
        self.assertEqual(binary, b"EXECUTABLE")

    def test_cross_target_archive_survives_an_unsupported_host_strip(self):
        binary, _, stderr = self.run_package(target="aarch64-unknown-linux-gnu", strip_fails=True)
        self.assertEqual(binary, b"EXECUTABLE")
        self.assertIn("host strip skipped", stderr)


if __name__ == "__main__":
    unittest.main()
