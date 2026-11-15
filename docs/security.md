# Security model

To disclose a vulnerability privately, follow the [security policy](../SECURITY.md).

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

Device-binding messages share a session wrapping key but use sender-specific
nonce domains (A: index 2, B: index 3). Index 1 is reserved for legacy senders;
receivers decrypt using the nonce carried by the v1 message. This prevents the
two updated pairing participants, or a legacy/updated pair, from encrypting
different binding messages with the same key and nonce.

## Keys

| Key | Purpose |
| --- | --- |
| Device X25519 | Long-term identity, Noise static key, epoch wrap |
| `roomRootKey` | First-epoch derivation only |
| `roomEpochKey` | Payload AEAD; current clients retain the initial key until re-pairing |
| Per-transfer content key | HKDF from epoch key + transfer ID |

The relay rejects revoked device tokens. The crypto library provides epoch-key
wrapping, but the current client does not automatically rotate room keys on
membership changes. Re-pair to establish a new room key.

Full forward secrecy for stored payloads is not provided. Noise handshakes use
ephemeral keys, but payloads continue to use persistent room epoch keys;
compromise of an epoch secret exposes the payload keys derived from it. The
library's epoch-wrapping mechanism also allows a recorded wrap to be decrypted
after compromise of the recipient's long-term identity. Treat
device and room secrets as sensitive, and re-pair to replace old room keys.
Changing this property requires a different room-key lifecycle and compatible
protocol changes; the current v1 format is retained for existing clients.

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
