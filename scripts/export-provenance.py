# Copyright the ClipSync contributors.
# SPDX-License-Identifier: MIT
"""Export the original DSSE envelope only after GitHub verifies its Sigstore bundle.

The companion bundle remains the source of certificates and transparency proofs.
This helper neither signs a new statement nor independently implements Sigstore
verification. --check verifies the bundle again and cross-checks an existing export.
"""

import argparse
import base64
import binascii
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys


class ProvenanceError(Exception):
    """Invalid provenance or unsuccessful cryptographic verification."""


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ProvenanceError(f"duplicate JSON key: {key}")
        result[key] = value
    return result


def load_json(contents):
    try:
        return json.loads(contents, object_pairs_hook=unique_object)
    except (ValueError, UnicodeError) as error:
        raise ProvenanceError(f"invalid JSON: {error}") from error


def decode_base64(value):
    try:
        if not isinstance(value, str) or not value:
            raise ValueError("expected a nonempty base64 string")
        return base64.b64decode(value, validate=True)
    except (ValueError, binascii.Error) as error:
        raise ProvenanceError(f"invalid base64: {error}") from error


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def read_envelope(bundle_bytes, artifact, digest):
    bundle = load_json(bundle_bytes)
    envelope = bundle.get("dsseEnvelope") if isinstance(bundle, dict) else None
    if not isinstance(envelope, dict):
        raise ProvenanceError("expected a Sigstore bundle containing a DSSE envelope")
    if envelope.get("payloadType") != "application/vnd.in-toto+json":
        raise ProvenanceError("expected an in-toto JSON payload type")
    signatures = envelope.get("signatures")
    if not isinstance(signatures, list) or not signatures:
        raise ProvenanceError("expected at least one DSSE signature")
    for signature in signatures:
        if not isinstance(signature, dict):
            raise ProvenanceError("invalid DSSE signature")
        decode_base64(signature.get("sig"))
    statement = load_json(decode_base64(envelope.get("payload")))
    if not isinstance(statement, dict) or statement.get("_type") != "https://in-toto.io/Statement/v1":
        raise ProvenanceError("expected an in-toto Statement v1")
    if statement.get("predicateType") != "https://slsa.dev/provenance/v1":
        raise ProvenanceError("expected SLSA provenance v1")
    subjects = statement.get("subject")
    if not isinstance(subjects, list):
        raise ProvenanceError("expected an artifact subject list")
    matches = [subject for subject in subjects
               if isinstance(subject, dict) and subject.get("name") == artifact.name]
    digests = matches[0].get("digest") if len(matches) == 1 else None
    if not isinstance(digests, dict) or digests.get("sha256") != digest:
        raise ProvenanceError("subject name or SHA-256 digest does not match the artifact")
    return envelope, statement


def matches_verified_result(verified, envelope, statement):
    if not isinstance(verified, list):
        return False
    for item in verified:
        try:
            if (item["attestation"]["bundle"]["dsseEnvelope"] == envelope
                    and item["verificationResult"]["statement"] == statement):
                return True
        except (KeyError, TypeError):
            continue
    return False


def export_provenance(artifact, bundle, output, repo, source_ref, source_digest, check=False):
    if (not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repo)
            or not re.fullmatch(r"refs/(?:heads|tags)/\S+", source_ref)
            or not re.fullmatch(r"(?:[0-9a-f]{40}|[0-9a-f]{64})", source_digest)):
        raise ProvenanceError("expected a repository, full branch/tag ref, and source commit SHA")
    artifact, bundle, output = (Path(path).resolve() for path in (artifact, bundle, output))
    if output in {artifact, bundle} or not output.name.endswith(".intoto.jsonl"):
        raise ProvenanceError("output must be a separate .intoto.jsonl file")
    original_bundle = bundle.read_bytes()
    digest = sha256(artifact)
    envelope, statement = read_envelope(original_bundle, artifact, digest)
    command = [
        "gh", "attestation", "verify", str(artifact), "--bundle", str(bundle),
        "--repo", repo, "--signer-workflow", f"{repo}/.github/workflows/release.yml",
        "--source-ref", source_ref, "--source-digest", source_digest,
        "--deny-self-hosted-runners", "--format", "json",
    ]
    try:
        result = subprocess.run(command, capture_output=True, text=True, timeout=180)
    except (OSError, subprocess.TimeoutExpired) as error:
        raise ProvenanceError(f"cannot verify attestation: {error}") from error
    if result.returncode:
        raise ProvenanceError(f"gh attestation verify failed: {result.stderr.strip()}")
    verified = load_json(result.stdout)
    if not matches_verified_result(verified, envelope, statement):
        raise ProvenanceError("verifier did not return the exact original envelope and statement")
    if bundle.read_bytes() != original_bundle or sha256(artifact) != digest:
        raise ProvenanceError("artifact or bundle changed during verification")
    # Re-serialize only the envelope wrapper; all signed fields remain untouched.
    encoded = (json.dumps(envelope, separators=(",", ":"), ensure_ascii=True) + "\n").encode("utf-8")
    if check:
        if output.read_bytes() != encoded:
            raise ProvenanceError("export differs from the verified original DSSE envelope")
    else:
        with output.open("xb") as stream:
            stream.write(encoded)
    return digest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("artifact", type=Path)
    parser.add_argument("--bundle", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--repo", required=True, help="expected GitHub owner/repository")
    parser.add_argument("--source-ref", required=True, help="expected full source ref, e.g. refs/tags/vX.Y.Z")
    parser.add_argument("--source-digest", required=True, help="expected source commit SHA")
    parser.add_argument("--check", action="store_true", help="verify an existing export without writing")
    args = parser.parse_args()
    try:
        digest = export_provenance(**vars(args))
    except (ProvenanceError, OSError) as error:
        parser.exit(1, f"provenance: {error}\n")
    print(f"Verified {args.artifact.name} (sha256:{digest}); {'checked' if args.check else 'exported'} {args.output}")


if __name__ == "__main__":
    main()
