# Packaging

Binary name: `clipsync`. Relay binary: `clipsync-relay`.

## P0

- macOS Homebrew tap (`jeon1691/tap/clipsync`) for Apple Silicon and Intel
- Linuxbrew (`jeon1691/tap/clipsync`)
- GitHub Release archives with SHA-256 checksums

```bash
brew install jeon1691/tap/clipsync
```

The tap is https://github.com/Jeon1691/homebrew-tap. Tagging `vX.Y.Z` on
https://github.com/Jeon1691/clipsync builds binaries and, when the
`HOMEBREW_TAP_TOKEN` secret is set, updates `Formula/clipsync.rb`.

## P1

- Debian/Ubuntu `.deb` + signed APT repo
- Fedora/RHEL `.rpm` + signed DNF repo
- AUR `clipsync-bin`
- Nix flake
- `cargo install clipsync-cli --locked` on crates.io

Installing a package must not start the daemon. launchd / systemd user units
are written only after `room create` or `room join`.
