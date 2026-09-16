# Security model

## Threats in scope

- Honest-but-curious relay operator
- Pairing-code guessing
- Replay, truncation, and chunk reordering
- Path traversal on received files
- Oversized payloads and connection floods

## Pairing

The 6-digit code is a SPAKE2 password, not a wrapping key. Both sides bind the
SPAKE2 transcript to the protocol version, pairing session ID, and device IDs,
then exchange HMAC confirmations. Wrong code, mutated transcript, or failed
confirmation aborts without storing credentials.

## Keys

| Key | Purpose |
| --- | --- |
| Device X25519 | Long-term identity, Noise static key, epoch wrap |
| `roomRootKey` | First-epoch derivation only |
| `roomEpochKey` | Payload AEAD; rotated on membership change |
| Per-transfer content key | HKDF from epoch key + transfer ID |

Revoked devices do not receive the next wrapped epoch key.

## Relay

The relay routes opaque ciphertext. It stores at most one latest encrypted text
envelope per recipient in memory for 60 seconds. Image and file frames are
forwarded only while the peer is online and are never assembled or written to
disk.

Webhooks carry metadata only: opaque IDs, item kind, encrypted size, timestamp,
status. Private addresses are blocked.

## Local storage

macOS Keychain / Linux Secret Service first. File fallback is mode `0600`.
Received files land in a `0700` inbox; names are basenames only.
