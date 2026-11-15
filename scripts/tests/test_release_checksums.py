# Copyright the ClipSync contributors.
# SPDX-License-Identifier: MIT
"""Accept GNU Windows binary markers without weakening release asset binding."""

import hashlib
import importlib.util
from pathlib import Path
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "verify-release-checksum.py"
SPEC = importlib.util.spec_from_file_location("verify_release_checksum", SCRIPT)
CHECKSUM = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHECKSUM)


class ReleaseChecksumTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory(prefix="clipsync-release-checksum-")
        self.addCleanup(directory.cleanup)
        root = Path(directory.name)
        self.archive = root / "clipsync-x86_64-pc-windows-msvc.zip"
        self.archive.write_bytes(b"synthetic Windows release archive")
        self.checksum = root / "clipsync-x86_64-pc-windows-msvc.sha256"
        self.digest = hashlib.sha256(self.archive.read_bytes()).hexdigest()

    def verify(self, record):
        self.checksum.write_bytes(record.encode("ascii"))
        return CHECKSUM.verify_checksum(self.archive, self.checksum)

    def test_windows_gnu_binary_marker_is_accepted(self):
        self.assertEqual(self.verify(f"{self.digest} *{self.archive.name}\n"), self.digest)
        self.assertEqual(self.verify(f"{self.digest} *{self.archive.name}\r\n"), self.digest)

    def test_gnu_text_marker_and_complete_record_without_newline_are_accepted(self):
        self.assertEqual(self.verify(f"{self.digest}  {self.archive.name}\n"), self.digest)
        self.assertEqual(self.verify(f"{self.digest.upper()}  {self.archive.name}"), self.digest)

    def test_wrong_basename_and_paths_are_rejected_even_with_the_correct_digest(self):
        for name in ["other.zip", "../" + self.archive.name, "./" + self.archive.name,
                     str(self.archive), self.archive.name.upper()]:
            with self.subTest(name=name), self.assertRaisesRegex(ValueError, "expected archive basename"):
                self.verify(f"{self.digest} *{name}\n")

    def test_multiple_records_malformed_markers_and_wrong_hashes_are_rejected(self):
        record = f"{self.digest} *{self.archive.name}\n"
        for value in [record * 2, record + "\n", f"{self.digest} ?{self.archive.name}\n",
                      f"{self.digest} {self.archive.name}\n", f"{'0' * 64} *{self.archive.name}\n"]:
            with self.subTest(value=value), self.assertRaises(ValueError):
                self.verify(value)

    def test_archive_mutation_is_rejected(self):
        self.archive.write_bytes(b"changed archive bytes")
        with self.assertRaisesRegex(ValueError, "digest does not match"):
            self.verify(f"{self.digest} *{self.archive.name}\n")


if __name__ == "__main__":
    unittest.main()
