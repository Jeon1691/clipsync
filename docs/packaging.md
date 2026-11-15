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
   verifies the signatures, and publishes the archives, SHA-256 files, and
   `.sigstore.json` attestation bundles
3. Updates `jeon1691/tap` when `TAP_DEPLOY_KEY` is configured

Manual runs build and verify artifacts without publishing by default. Select a
`v*` tag and enable the `publish` input to publish a manual run. A branch run
never publishes a release or updates the Homebrew tap.

Before creating a release tag, write `docs/releases/vX.Y.Z.md` with a readable
summary of significant changes, security fixes, and upgrade impact. Review the
tag's actual contents and successful CI/security results. The publishing job
requires that file and uses it as the release body; a compare link or generated
commit list alone is insufficient. Rerunning a release uploads assets while
preserving any later edits to its published body.

macOS release jobs use `macos-15`; GitHub retired all `macos-14` runners on
2026-11-02.

To verify a downloaded release archive, download its matching `.sigstore.json`
bundle from the same release and run:

```bash
gh attestation verify clipsync-x86_64-unknown-linux-gnu.tar.gz \
  --bundle clipsync-x86_64-unknown-linux-gnu.sigstore.json \
  --repo Jeon1691/clipsync \
  --signer-workflow Jeon1691/clipsync/.github/workflows/release.yml \
  --source-ref refs/tags/v0.1.15 \
  --deny-self-hosted-runners
```

Replace the example tag and platform with the release you downloaded. Older
releases published before attestation support do not have these bundles.
Verification binds the file digest to this repository, the release workflow,
and the selected tag. A modified archive or a mismatched bundle must fail
verification.

## P1

- Debian/Ubuntu `.deb` + signed APT repo
- Fedora/RHEL `.rpm` + signed DNF repo
- AUR `clipsync-bin`
- Nix flake
- `cargo install clipsync-cli --locked` on crates.io

Installing a package must not start the daemon. launchd / systemd user units
are written only after `room create` or `room join`.
