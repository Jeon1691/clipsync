# Copyright the ClipSync contributors.
# SPDX-License-Identifier: MIT
"""Exercise export policy with synthetic envelopes; real crypto is verified by gh."""

import base64
import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).resolve().parents[1] / "export-provenance.py"
SPEC = importlib.util.spec_from_file_location("export_provenance", SCRIPT)
PROVENANCE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PROVENANCE)


class ExportProvenanceTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="clipsync-provenance-")
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name).resolve()
        self.artifact = root / "clipsync-fixture.tar.gz"
        self.artifact.write_bytes(b"synthetic archive fixture")
        self.bundle = root / "clipsync-fixture.sigstore.json"
        self.output = root / "clipsync-fixture.intoto.jsonl"
        self.statement = {
            "_type": "https://in-toto.io/Statement/v1",
            "predicateType": "https://slsa.dev/provenance/v1",
            "subject": [{"name": self.artifact.name, "digest": {
                "sha256": hashlib.sha256(self.artifact.read_bytes()).hexdigest()}}],
            "predicate": {"buildDefinition": {"fixture": True}},
        }
        self.envelope = {
            "payloadType": "application/vnd.in-toto+json",
            "payload": self.encode(self.statement),
            "signatures": [{"keyid": "fixture-key", "sig": "c2lnbmF0dXJl"},
                           {"sig": "c2Vjb25kLXNpZ25hdHVyZQ=="}],
        }
        self.write_bundle()
        self.verifier = patch.object(PROVENANCE.subprocess, "run", side_effect=self.verified_result).start()
        self.addCleanup(patch.stopall)

    @staticmethod
    def encode(value):
        return base64.b64encode(json.dumps(value, indent=2).encode()).decode()

    def write_bundle(self):
        self.bundle.write_text(json.dumps({
            "mediaType": "application/vnd.dev.sigstore.bundle.v0.3+json",
            "verificationMaterial": {"fixture": "certificates and transparency proofs"},
            "dsseEnvelope": self.envelope,
        }, indent=2))

    def verified_result(self, *args, **kwargs):
        return subprocess.CompletedProcess(args[0], 0, json.dumps([{
            "attestation": {"bundle": {"dsseEnvelope": self.envelope}},
            "verificationResult": {"statement": self.statement},
        }]), "")

    def export(self, check=False):
        return PROVENANCE.export_provenance(
            self.artifact, self.bundle, self.output, "owner/repository",
            "refs/tags/v1.2.3", "a" * 40, check=check)

    def rejected(self, message, check=False):
        with self.assertRaisesRegex(PROVENANCE.ProvenanceError, message):
            self.export(check=check)
        if not check:
            self.assertFalse(self.output.exists())

    def test_preserves_all_original_signed_fields_and_bundle_bytes(self):
        original = self.bundle.read_bytes()
        self.export()
        lines = self.output.read_text().splitlines()
        self.assertEqual(len(lines), 1)
        self.assertEqual(json.loads(lines[0]), self.envelope)
        self.assertEqual(self.bundle.read_bytes(), original)
        self.assertNotIn("verificationMaterial", json.loads(lines[0]))
        self.export(check=True)
        command = self.verifier.call_args.args[0]
        self.assertEqual(command, [
            "gh", "attestation", "verify", str(self.artifact), "--bundle", str(self.bundle),
            "--repo", "owner/repository", "--signer-workflow", "owner/repository/.github/workflows/release.yml",
            "--source-ref", "refs/tags/v1.2.3", "--source-digest", "a" * 40,
            "--deny-self-hosted-runners", "--format", "json",
        ])

    def test_rejects_tampered_archive(self):
        self.artifact.write_bytes(b"modified archive")
        self.rejected("subject name or SHA-256")
        self.verifier.assert_not_called()

    def test_rejects_wrong_subject_name_or_digest(self):
        for field, value in [("name", "other.tar.gz"), ("digest", {"sha256": "b" * 64})]:
            with self.subTest(field=field):
                statement = copy.deepcopy(self.statement)
                statement["subject"][0][field] = value
                self.envelope["payload"] = self.encode(statement)
                self.write_bundle()
                self.rejected("subject name or SHA-256")

    def test_failed_cryptographic_verifier_never_exports(self):
        self.verifier.side_effect = None
        self.verifier.return_value = subprocess.CompletedProcess([], 1, "", "signature invalid")
        self.rejected("gh attestation verify failed: signature invalid")

    def test_unavailable_or_timed_out_verifier_never_exports(self):
        for error in [FileNotFoundError("gh unavailable"), subprocess.TimeoutExpired("gh", 180)]:
            with self.subTest(error=type(error).__name__):
                self.verifier.side_effect = error
                self.rejected("cannot verify attestation")

    def test_rejects_malformed_or_wrong_type_payloads(self):
        for changes in [
            {"payloadType": "application/json"}, {"payload": "%%%"},
            {"payload": base64.b64encode(b"not JSON").decode()},
            {"payload": self.encode([])}, {"signatures": []},
            {"payload": self.encode(dict(self.statement, _type="wrong"))},
            {"payload": self.encode(dict(self.statement, predicateType="wrong"))},
        ]:
            with self.subTest(changes=changes):
                envelope = self.envelope
                self.envelope = dict(envelope, **changes)
                self.write_bundle()
                self.rejected("expected|invalid")
                self.envelope = envelope
        self.verifier.assert_not_called()

    def test_rejects_successful_verifier_that_returns_another_envelope(self):
        result = self.verified_result([])
        result.stdout = result.stdout.replace("c2lnbmF0dXJl", "dGFtcGVyZWQ=")
        self.verifier.side_effect = None
        self.verifier.return_value = result
        self.rejected("exact original envelope")

    def test_rejects_inputs_changed_during_verification(self):
        def changed(*args, **kwargs):
            self.bundle.write_bytes(self.bundle.read_bytes() + b"\n")
            return self.verified_result(*args, **kwargs)
        self.verifier.side_effect = changed
        self.rejected("changed during verification")

    def test_check_rejects_tampered_envelope_without_rewriting_it(self):
        self.export()
        modified = self.output.read_bytes().replace(b"c2lnbmF0dXJl", b"dGFtcGVyZWQ=")
        self.output.write_bytes(modified)
        self.rejected("export differs", check=True)
        self.assertEqual(self.output.read_bytes(), modified)

    def test_export_never_overwrites_an_existing_output(self):
        self.output.write_bytes(b"existing evidence")
        with self.assertRaises(FileExistsError):
            self.export()
        self.assertEqual(self.output.read_bytes(), b"existing evidence")

    def test_rejects_duplicate_json_keys(self):
        self.bundle.write_text('{"dsseEnvelope":{},"dsseEnvelope":{}}')
        self.rejected("duplicate JSON key")

    def test_requires_nonempty_verification_identity_and_commit(self):
        for repo, ref, digest in [("", "refs/tags/v1", "a" * 40),
                                  ("owner/repo", "", "a" * 40),
                                  ("owner/repo", "refs/tags/v1", "")]:
            with self.subTest(repo=repo, ref=ref, digest=digest):
                with self.assertRaisesRegex(PROVENANCE.ProvenanceError, "source commit SHA"):
                    PROVENANCE.export_provenance(self.artifact, self.bundle, self.output, repo, ref, digest)
        self.verifier.assert_not_called()

    def test_rejects_invalid_verifier_json_or_result_structure(self):
        for contents in ["not JSON", "null", "[null]", '[{"attestation":null}]']:
            with self.subTest(contents=contents):
                self.verifier.side_effect = None
                self.verifier.return_value = subprocess.CompletedProcess([], 0, contents, "")
                self.rejected("invalid JSON|exact original envelope")


if __name__ == "__main__":
    unittest.main()
