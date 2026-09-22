# ClipSync CLI

[![CI](https://github.com/Jeon1691/clipsync/actions/workflows/ci.yml/badge.svg?branch=master)](https://github.com/Jeon1691/clipsync/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/Jeon1691/clipsync?label=release&color=2ea44f)](https://github.com/Jeon1691/clipsync/releases/latest)

End-to-end encrypted clipboard sync for **macOS**, **Linux**, and **Windows**.
Pair two devices with a one-time 6-digit code. After pairing, a background
daemon copies text, images, and files between machines over a zero-knowledge
WebSocket relay.

The relay never sees plaintext, filenames, or encryption keys.

Default relay: `https://clipsync.develicit.dev`  
Override with `CLIPSYNC_RELAY_URL`.

## Install

### macOS and Linux (Homebrew)

```bash
brew install jeon1691/tap/clipsync
```

`clipsync init` runs during `brew install` (and again automatically on first
use if needed). Then:

```bash
clipsync room create
```

Upgrade:

```bash
brew update && brew upgrade jeon1691/tap/clipsync
```

Prebuilt bottles: macOS Apple Silicon and Intel, Linux x86_64 (and arm64 when
the optional Linux arm job succeeds).

### Desktop app

One native window for macOS, Windows, and Linux. It uses the same identity and
login daemon as the CLI.

```bash
cargo run -p clipsync-desktop
```

Create or join a room in the window. After pairing, clipboard sync stays in the
background service (`clipsync daemon`).

### Windows

Download `clipsync-x86_64-pc-windows-msvc.zip` from
[Releases](https://github.com/Jeon1691/clipsync/releases), put `clipsync.exe` on
`PATH`, then:

```powershell
clipsync room create
```

### iOS and Android (native)

The mobile apps are separate repositories. They use the same pairing protocol
as the CLI via UniFFI (`crates/clipsync-mobile`).

- iOS: [Jeon1691/clipsync-ios](https://github.com/Jeon1691/clipsync-ios)
- Android: [Jeon1691/clipsync-android](https://github.com/Jeon1691/clipsync-android)

See [docs/mobile.md](docs/mobile.md).

### From source

```bash
cargo install --path crates/clipsync-cli --locked
```

## Quick start

On device A:

```bash
clipsync room create
```

Prints a 6-digit code and returns immediately. Pairing waits in the background
until the other device joins (about 2 minutes). To wait in this terminal:

```bash
clipsync room create --foreground
```

On device B (another machine, or another `CLIPSYNC_HOME`):

```bash
clipsync room join 482913
```

`clipsync init` is optional. Identity is created on Homebrew install and on
first use.

The pairing code is single-use and expires quickly. If join says the code is
unknown or expired, run `clipsync room create` again on device A.

After pairing, copy on either device. The other clipboard updates
automatically. The user daemon starts at login and reconnects after a reboot.

Already paired before this version? Re-register the login service once:

```bash
clipsync daemon restart
```

Manual fallback (works even without the daemon):

```bash
# macOS
pbpaste | clipsync push
clipsync pull | pbcopy

# any OS
kubectl get pods -A | clipsync push
clipsync pull > pods.txt
clipsync push --file ./report.pdf --file ./diagram.png
clipsync pull --wait --output ./inbox
```

Automatic clipboard sync covers text, images, and files on macOS, Linux, and
Windows. `clipsync push --file` / `clipsync pull --output` still work without
the daemon.

## Login and reboot

After a successful `room create` / `room join` (unless `--no-auto-sync`):

| OS | How the daemon comes back |
| --- | --- |
| macOS | LaunchAgent `dev.clipsync.daemon` (`RunAtLoad`, restart on crash). Uses the Homebrew prefix binary so upgrades keep working. |
| Linux | systemd user unit `clipsync.service` (`Restart=always`) plus `~/.config/autostart/clipsync.desktop`. Needs a graphical login (clipboard). |
| Windows | Startup folder `ClipSync.vbs` runs `clipsync daemon run` hidden at logon. |

`clipsync doctor` reports `login_service` when that job is installed.

```bash
clipsync daemon start|stop|restart
clipsync status
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

Global flags: `--json`, `--yes`, `--non-interactive`.

`room create` waits in the background by default. `--foreground` / `--wait`
blocks until the joiner arrives.

## Environment

| Variable | Purpose |
| --- | --- |
| `CLIPSYNC_RELAY_URL` | Relay base URL (default `https://clipsync.develicit.dev`) |
| `CLIPSYNC_HOME` | Isolated data/config root (useful for two devices on one machine) |
| `CLIPSYNC_CA_FILE` | Extra PEM bundle of private/self-signed CAs |
| `CLIPSYNC_NO_DAEMON` | Set to `1` to skip auto-starting the login daemon |

## Corporate TLS

HTTPS is verified with the **OS trust store** (macOS Keychain, Windows
certificate store, Linux system CAs). If the system browser can open
`https://clipsync.develicit.dev` after the company root is trusted, ClipSync
should too.

If you have the private CA as a PEM file instead:

```bash
export CLIPSYNC_CA_FILE=/path/to/company-root.pem
clipsync doctor
clipsync room create
```

`clipsync doctor` shows `tls.verifier: platform` and any `ca_file`.

## Security

- SPAKE2 balanced PAKE; the 6-digit code is only a one-time password
- Mutual key confirmation bound to protocol version, session, device IDs, and SPAKE2 transcript
- AES-256-GCM payloads, HKDF-SHA-256, X25519 device identity
- Noise `KK` handshake on reconnect
- Text latest-one in-memory queue, 60s TTL; images/files are online-only 256 KiB chunks
- Pairing code TTL 120s, 3 attempts, single joiner
- Identity in a 0600 file, plus macOS Keychain / Windows Credential Manager when available

See [docs/security.md](docs/security.md) and [docs/protocol.md](docs/protocol.md).

## Self-hosted relay

```bash
docker compose up
export CLIPSYNC_RELAY_URL=http://127.0.0.1:7600
clipsync room create
```

See [docs/self-hosting.md](docs/self-hosting.md).

## Layout

```
crates/
  clipsync-cli          # `clipsync` binary
  clipsync-mobile       # UniFFI for iOS / Android
  clipsync-relay        # `clipsync-relay` binary
  clipsync-core         # pairing + sync engine
  clipsync-crypto       # SPAKE2, Noise, AEAD
  clipsync-protocol     # frames, limits, codec
  clipsync-clipboard    # macOS / Linux / Windows adapters
  clipsync-transport    # WSS client (OS TLS roots)
  clipsync-transfer     # chunking + staging
  clipsync-storage      # keychain / 0600 files
  clipsync-daemon       # launchd / systemd / Windows logon + IPC
```

## License

MIT. See [LICENSE-MIT](LICENSE-MIT).
