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
2. Publishes GitHub Release archives + SHA-256
3. Updates `jeon1691/tap` when `TAP_DEPLOY_KEY` is configured

## P1

- Debian/Ubuntu `.deb` + signed APT repo
- Fedora/RHEL `.rpm` + signed DNF repo
- AUR `clipsync-bin`
- Nix flake
- `cargo install clipsync-cli --locked` on crates.io

Installing a package must not start the daemon. launchd / systemd user units
are written only after `room create` or `room join`.
