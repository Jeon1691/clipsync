# Packaging

Binary name: `clipsync`. Relay binary: `clipsync-relay`.

## P0

- macOS Homebrew tap (`jeon1691/tap/clipsync`) for Apple Silicon and Intel
- Linuxbrew (`jeon1691/tap/clipsync`)
- GitHub Release archives with SHA-256 checksums

```bash
brew install jeon1691/tap/clipsync
```

The tap is https://github.com/Jeon1691/homebrew-tap.

CI (`ci.yml`) runs format, Clippy, workspace tests, platform checks, and relay
end-to-end tests on Linux, macOS, and Windows, plus a minimum-Rust build.

CD (`release.yml`) on `vX.Y.Z` tags:

1. Builds macOS arm64/x86_64, Linux x86_64, and Windows x86_64 (Linux arm is best-effort)
2. Signs build provenance for the archives using GitHub artifact attestations,
   verifies the signatures, and exports the original DSSE envelopes. Each archive
   has a SHA-256 file, the original `.sigstore.json` bundle, and a matching
   `.intoto.jsonl` envelope
3. Updates `jeon1691/tap` when `TAP_DEPLOY_KEY` is configured

Manual runs build and verify artifacts without publishing by default. Select a
`v*` tag and enable the `publish` input to publish a manual run. A branch run
never publishes a release or updates the Homebrew tap.

Before creating a release tag, write `docs/releases/vX.Y.Z.md` with a readable
summary of significant changes, security fixes, and upgrade impact. Review the
tag's actual contents and successful CI/security results. The publishing job
requires that file and uses it as the release body; a compare link or generated
commit list alone is insufficient. The tagged source must be on the default
branch's history. Publication verifies all four required platform archives
(plus Linux arm when available), checksums, bundles, and DSSE exports before
creating a draft. It uploads without overwriting assets, downloads and verifies
the complete uploaded set, rechecks the source tag, and then publishes the draft.
This follows GitHub's [draft-first process for immutable releases](https://docs.github.com/en/code-security/concepts/supply-chain-security/immutable-releases).
Enabling repository release immutability is a separate setting.

A rerun refuses to modify a published release. It can resume an empty draft only
when its tag and explicit source commit match the run. A draft with any existing
assets requires inspection before retrying; the workflow never deletes or
replaces those files automatically.

macOS release jobs use `macos-15`; GitHub retired all `macos-14` runners on
2026-11-02.

To verify a downloaded release archive, download its matching `.sigstore.json`
bundle from the same release and run:

```bash
gh attestation verify clipsync-x86_64-unknown-linux-gnu.tar.gz \
  --bundle clipsync-x86_64-unknown-linux-gnu.sigstore.json \
  --repo Jeon1691/clipsync \
  --signer-workflow Jeon1691/clipsync/.github/workflows/release.yml \
  --source-ref refs/tags/v0.1.17 \
  --source-digest 6f44616a23483c4c881669bb0923823eb24b1b20 \
  --deny-self-hosted-runners
```

Replace the example tag, reviewed source commit, and platform with the release you downloaded. Older
releases published before attestation support do not have these bundles.
Verification binds the file digest to this repository, the release workflow,
and the selected tag. A modified archive or a mismatched bundle must fail
verification.

The `.intoto.jsonl` file contains one standard [DSSE envelope](https://github.com/in-toto/attestation/blob/v1.2.0/spec/v1/envelope.md).
Its `payloadType`, base64 `payload`, and every signature are preserved from the
verified bundle. Certificates and transparency proofs remain in the original
`.sigstore.json`; keep both files. The export alone is not a replacement for
[GitHub CLI's cryptographic verification](https://cli.github.com/manual/gh_attestation_verify).
From a reviewed project checkout, cross-check a downloaded export with:

```bash
python3 scripts/export-provenance.py clipsync-x86_64-unknown-linux-gnu.tar.gz \
  --bundle clipsync-x86_64-unknown-linux-gnu.sigstore.json \
  --output clipsync-x86_64-unknown-linux-gnu.intoto.jsonl \
  --repo Jeon1691/clipsync \
  --source-ref refs/tags/v0.1.17 \
  --source-digest 6f44616a23483c4c881669bb0923823eb24b1b20 \
  --check
```

The helper reruns `gh attestation verify` with the repository, release workflow,
source ref/commit, and hosted-runner policy, validates the subject's filename and
SHA-256, and compares the exact export. Omit `--check` to create a missing export
from existing authentic evidence; an existing output is never overwritten.
Releases with authentic bundles can receive this additional representation of
their original signed statement. Releases without original build attestations
cannot be given retroactive build provenance.

## P1

- Debian/Ubuntu `.deb` + signed APT repo
- Fedora/RHEL `.rpm` + signed DNF repo
- AUR `clipsync-bin`
- Nix flake
- `cargo install clipsync-cli --locked` on crates.io

Installing a package must not start the daemon. launchd / systemd user units
are written only after `room create` or `room join`.
