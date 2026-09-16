# ClipSync CLI

End-to-end encrypted clipboard sync for macOS and Linux. Pair two devices with a
one-time 6-digit code. After pairing, a background daemon copies text, images,
and files between machines over a zero-knowledge WebSocket relay.

The relay never sees plaintext, filenames, or encryption keys.

## Install

```bash
brew install jeon1691/tap/clipsync
```

macOS (Apple Silicon and Intel) and Linux (Homebrew/Linuxbrew) get a prebuilt
binary. Pairing talks to `https://clipsync.develicit.dev` unless you set
`CLIPSYNC_RELAY_URL`.

```bash
# from source
cargo install --path crates/clipsync-cli --locked
```

## Quick start

Default relay: `https://clipsync.develicit.dev`. Override with `CLIPSYNC_RELAY_URL`.

Terminal A:

```bash
clipsync init
clipsync room create    # prints a 6-digit code and waits
```

Terminal B (another machine or another `CLIPSYNC_HOME`):

```bash
clipsync init
export CLIPSYNC_RELAY_URL=http://<relay-host>:7600
clipsync room join 482913
```

After pairing, copy on either device. The other clipboard updates automatically.

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
clipsync init
clipsync room create [--ttl 10m] [--no-auto-sync]
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
