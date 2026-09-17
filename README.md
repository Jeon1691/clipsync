# ClipSync CLI

End-to-end encrypted clipboard sync for macOS, Linux, and Windows. Pair two
devices with a one-time 6-digit code. After pairing, a background daemon copies
text, images, and files between machines over a zero-knowledge WebSocket relay.

The relay never sees plaintext, filenames, or encryption keys.

## Install

```bash
brew install jeon1691/tap/clipsync
```

macOS (Apple Silicon and Intel) and Linux (Homebrew/Linuxbrew) get a prebuilt
binary. Windows builds are attached to GitHub Releases as `.zip`. Pairing talks
to `https://clipsync.develicit.dev` unless you set `CLIPSYNC_RELAY_URL`.

After pairing, the daemon starts at login on macOS (LaunchAgent), Linux
(systemd user unit + XDG autostart), and Windows (Startup folder).

```bash
# from source (macOS, Linux, Windows)
cargo install --path crates/clipsync-cli --locked
```

Windows: download `clipsync-x86_64-pc-windows-msvc.zip` from
[Releases](https://github.com/Jeon1691/clipsync/releases), put `clipsync.exe` on
`PATH`, then `clipsync room create`.

## Quick start

Default relay: `https://clipsync.develicit.dev`. Override with `CLIPSYNC_RELAY_URL`.

Terminal A:

```bash
clipsync room create                 # prints a 6-digit code; waits in the background
clipsync room create --foreground    # wait in this terminal until the other device joins
```

`clipsync init` runs on Homebrew install and again automatically on first use if needed.

Terminal B (another machine or another `CLIPSYNC_HOME`):

```bash
clipsync init
export CLIPSYNC_RELAY_URL=http://<relay-host>:7600
clipsync room join 482913
```

After pairing, copy on either device. The other clipboard updates automatically.
The user daemon starts at login and reconnects to the relay after a reboot.

Manual fallback:

```bash
pbpaste | clipsync push
clipsync pull | pbcopy
kubectl get pods -A | clipsync push
clipsync pull > pods.txt
clipsync push --file ./report.pdf --file ./diagram.png
```

## Commands

```
clipsync init [--relay-url URL]      # optional; also runs on install and first use
clipsync room create [--ttl 10m] [--no-auto-sync] [--foreground]
clipsync room join <CODE> [--no-auto-sync]
clipsync room leave --yes
clipsync room list
clipsync push [--type auto|text|image|files] [--file <path>...]
clipsync pull [--copy] [--wait] [--output <dir>]
clipsync watch
clipsync sync pause|resume
clipsync daemon start|stop|restart
clipsync status [--json]
clipsync devices
clipsync config get|set
clipsync doctor
clipsync logs [--follow]
```

## Corporate TLS

ClipSync verifies HTTPS with the OS trust store (macOS Keychain). If Safari
opens `https://clipsync.develicit.dev`, ClipSync should too after the company
root is trusted in Keychain.

If you have the private CA as a PEM file instead:

```bash
export CLIPSYNC_CA_FILE=/path/to/company-root.pem
clipsync doctor
clipsync room create
```

## Security

- SPAKE2 balanced PAKE; the 6-digit code is only a one-time password
- Mutual key confirmation bound to protocol version, session, device IDs, and SPAKE2 transcript
- AES-256-GCM payloads, HKDF-SHA-256, X25519 device identity
- Noise `KK` handshake on reconnect
- Text latest-one in-memory queue, 60s TTL; images/files are online-only 256 KiB chunks
- Pairing code TTL 120s, 3 attempts, single joiner

See [docs/security.md](docs/security.md) and [docs/protocol.md](docs/protocol.md).

## Self-hosted relay

```bash
docker compose up
export CLIPSYNC_RELAY_URL=http://127.0.0.1:7600
```

See [docs/self-hosting.md](docs/self-hosting.md).

## Layout

```
crates/
  clipsync-cli          # `clipsync` binary
  clipsync-relay        # `clipsync-relay` binary
  clipsync-core         # pairing + sync engine
  clipsync-crypto       # SPAKE2, Noise, AEAD
  clipsync-protocol     # frames, limits, codec
  clipsync-clipboard    # macOS / X11 / Wayland adapters
  clipsync-transport    # WSS client
  clipsync-transfer     # chunking + staging
  clipsync-storage      # keychain / 0600 files
  clipsync-daemon       # launchd / systemd + IPC
```

## License

MIT OR Apache-2.0
