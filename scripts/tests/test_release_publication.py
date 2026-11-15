# Copyright the ClipSync contributors.
# SPDX-License-Identifier: MIT
"""Run the actual publication shell with isolated GitHub and source-policy stubs."""

import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import textwrap
import unittest


WORKFLOW = Path(__file__).resolve().parents[2] / ".github/workflows/release.yml"


@unittest.skipIf(os.name == "nt" or any(shutil.which(tool) is None for tool in ["bash", "jq", "sha256sum"]),
                 "publication requires the Bash, jq, and sha256sum tools available on Ubuntu")
class ReleasePublicationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        # Read only this named step; the fixtures exercise its actual shell body.
        section = WORKFLOW.read_text().split("      - name: Verify assets and publish draft release\n", 1)[1]
        cls.script = textwrap.dedent(section.split("        run: |\n", 1)[1].split("      - name:", 1)[0])

    def run_publication(self, state="absent", missing=None, corrupt_download=False,
                        ancestor=True, tag_matches=True, draft_matches=True, wrong_checksum_name=False):
        with tempfile.TemporaryDirectory(prefix="clipsync-publish-") as temporary:
            root = Path(temporary)
            for directory in ["artifacts", "tools", "scripts", "docs/releases", "runner"]:
                (root / directory).mkdir(parents=True)
            (root / "docs/releases/v1.2.3.md").write_text("Release fixture with upgrade notes.\n")
            for target in ["aarch64-apple-darwin", "x86_64-apple-darwin",
                           "x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"]:
                prefix = f"clipsync-{target}"
                name = prefix + (".zip" if "windows" in target else ".tar.gz")
                content = b"synthetic archive"
                (root / "artifacts" / name).write_bytes(content)
                (root / "artifacts" / f"{prefix}.sha256").write_text(
                    f"{hashlib.sha256(content).hexdigest()}  {name}\n")
                for suffix in ["sigstore.json", "intoto.jsonl"]:
                    (root / "artifacts" / f"{prefix}.{suffix}").write_text("synthetic evidence\n")
            if missing:
                (root / "artifacts" / missing).unlink()
            if wrong_checksum_name:
                checksum = root / "artifacts/clipsync-aarch64-apple-darwin.sha256"
                checksum.write_text(checksum.read_text().replace("aarch64-apple-darwin", "x86_64-apple-darwin"))
            # The exporter has separate cryptographic-policy tests. Here its
            # successful check isolates publication ordering and refusal paths.
            (root / "scripts/export-provenance.py").write_text(
                "import os\nfrom pathlib import Path\n"
                "with Path(os.environ['CALLS']).open('a') as log: log.write('verify-evidence\\n')\n")
            stub = f"#!{sys.executable}\n" + textwrap.dedent('''\
                import json, os
                from pathlib import Path
                import shutil, sys
                command = Path(sys.argv[0]).name
                args = sys.argv[1:]
                with Path(os.environ['CALLS']).open('a') as log:
                    log.write(json.dumps([command] + args) + '\\n')
                if command == 'git':
                    if args[0] == 'rev-parse':
                        print(os.environ['GITHUB_SHA'] if os.environ['TAG_MATCHES'] == '1' else 'b' * 40)
                    if args[0] == 'merge-base' and os.environ['ANCESTOR'] == '0':
                        sys.exit(1)
                elif args[:2] == ['repo', 'view']:
                    print('master')
                elif args[:2] == ['release', 'view']:
                    state = os.environ['RELEASE_STATE']
                    if state == 'absent': sys.exit(1)
                    print(json.dumps({'isDraft': state != 'published', 'tagName': 'v1.2.3',
                        'targetCommitish': os.environ['GITHUB_SHA'] if os.environ['DRAFT_MATCHES'] == '1' else 'b' * 40,
                        'assets': [{}] if state == 'nonempty' else []}))
                elif args[:2] == ['release', 'download']:
                    destination = Path(args[args.index('--dir') + 1])
                    for source in Path('artifacts').iterdir():
                        shutil.copyfile(source, destination / source.name)
                    if os.environ['CORRUPT_DOWNLOAD'] == '1':
                        next(destination.glob('*.intoto.jsonl')).write_text('corrupted uploaded evidence')
                ''')
            for tool in ["gh", "git"]:
                path = root / "tools" / tool
                path.write_text(stub)
                path.chmod(0o755)
            # Use the active interpreter for the workflow's Python calls on hosts
            # where its executable is named differently.
            (root / "tools/python3").symlink_to(sys.executable)
            env = dict(os.environ, PATH=str(root / "tools") + os.pathsep + os.environ["PATH"],
                       GITHUB_REF_NAME="v1.2.3", GITHUB_REF="refs/tags/v1.2.3",
                       GITHUB_REPOSITORY="owner/repository", GITHUB_SHA="a" * 40,
                       RUNNER_TEMP=str(root / "runner"), CALLS=str(root / "calls"),
                       RELEASE_STATE=state, CORRUPT_DOWNLOAD=str(int(corrupt_download)),
                       ANCESTOR=str(int(ancestor)), TAG_MATCHES=str(int(tag_matches)),
                       DRAFT_MATCHES=str(int(draft_matches)))
            result = subprocess.run(["bash", "-c", self.script], cwd=root, env=env,
                                    text=True, capture_output=True, timeout=30)
            lines = (root / "calls").read_text().splitlines()
            commands = [json.loads(line) for line in lines if line.startswith("[")]
            mutations = [command for command in commands
                         if command[:2] == ["gh", "release"] and command[2] in {"create", "upload", "edit"}]
            return result, lines, mutations

    def test_new_release_is_drafted_uploaded_downloaded_verified_then_published(self):
        result, lines, mutations = self.run_publication()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual([command[2] for command in mutations], ["create", "upload", "edit"])
        self.assertIn("--draft", mutations[0])
        self.assertIn("--target", mutations[0])
        self.assertIn("--draft=false", mutations[-1])
        self.assertNotIn("--clobber", " ".join(lines))
        self.assertEqual(lines.count("verify-evidence"), 8)
        self.assertEqual(lines[-1], json.dumps(mutations[-1]))

    def test_existing_published_release_or_nonempty_draft_is_never_modified(self):
        for state in ["published", "nonempty"]:
            with self.subTest(state=state):
                result, _, mutations = self.run_publication(state=state)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(mutations, [])

    def test_empty_draft_can_resume_only_for_the_same_source(self):
        result, _, mutations = self.run_publication(state="empty")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual([command[2] for command in mutations], ["upload", "edit"])
        result, _, mutations = self.run_publication(state="empty", draft_matches=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(mutations, [])

    def test_missing_platform_or_sidecar_blocks_all_release_mutations(self):
        for missing in ["clipsync-x86_64-pc-windows-msvc.zip", "clipsync-x86_64-apple-darwin.intoto.jsonl"]:
            with self.subTest(missing=missing):
                result, _, mutations = self.run_publication(missing=missing)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(mutations, [])

    def test_ineligible_source_blocks_all_release_mutations(self):
        for options in [{"ancestor": False}, {"tag_matches": False}]:
            with self.subTest(options=options):
                result, _, mutations = self.run_publication(**options)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(mutations, [])

    def test_checksum_for_a_different_valid_archive_blocks_all_release_mutations(self):
        result, _, mutations = self.run_publication(wrong_checksum_name=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(mutations, [])

    def test_changed_uploaded_evidence_never_publishes_the_draft(self):
        result, _, mutations = self.run_publication(corrupt_download=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual([command[2] for command in mutations], ["create", "upload"])


if __name__ == "__main__":
    unittest.main()
