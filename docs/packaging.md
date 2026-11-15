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

CI (`ci.yml`) runs format, clippy, unit tests, and an in-process pairing e2e
on Ubuntu and macOS.

CD (`release.yml`) on `vX.Y.Z` tags:

1. Builds macOS arm64/x86_64 and Linux x86_64 (Linux arm is best-effort)
2. Signs build provenance for the archives using GitHub artifact attestations,
   verifies the signatures, and publishes the archives, SHA-256 files, and
   `.sigstore` attestation bundles
3. Updates `jeon1691/tap` when `TAP_DEPLOY_KEY` is configured

Manual runs build and verify artifacts without publishing by default. Select a
`v*` tag and enable the `publish` input to publish a manual run. A branch run
never publishes a release or updates the Homebrew tap.

To verify a downloaded release archive, download its matching `.sigstore`
bundle from the same release and run:

```bash
gh attestation verify clipsync-x86_64-unknown-linux-gnu.tar.gz \
  --bundle clipsync-x86_64-unknown-linux-gnu.sigstore \
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
