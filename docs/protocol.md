# ClipSync protocol v1

See the [public interface reference](interfaces.md) for CLI inputs/outputs,
HTTP request/response and error schemas, WebSocket fields, and mobile bindings.

## Limits

| Item | Limit |
| --- | --- |
| Pairing TTL | 120s, 3 attempts, 1 joiner |
| Message TTL | default 60s, hard max 300s |
| Text | 256 KiB |
| Image | 16 MiB |
| File | 64 MiB each, 16 files, 128 MiB total |
| Chunk | 256 KiB plaintext |
| WebSocket frame | 384 KiB |

## HTTP

- `POST /v1/pairing/create`
- `POST /v1/pairing/join`
- `GET /v1/ws`
- `GET /healthz`
- `GET /metrics`

## Control messages (JSON text frames)

Tagged `type` field: `hello`, `hello_ok`, `peer_joined`, `peer_left`,
`spake2_commit`, `spake2_msg`, `key_confirm`, `device_bind`, `pairing_complete`,
`noise_handshake`, `text_envelope`, `manifest`, `ack`, `epoch_wrap_msg`,
`revoke`, `ping`, `pong`, `error`.

Relay-visible routing header includes room/device IDs, transfer ID, chunk
index, item kind, timestamps. File names, MIME types, hashes, and dimensions
live only in the encrypted manifest.

## Binary chunk frames

```
magic "CSY1" | version u8 | type u8=2 | transfer_id 16 | chunk_index u32be
| chunk_count u32be | ciphertext_len u32be | nonce||ciphertext
```

## Pairing transcript

```
"clipsync-pairing-v1" || u16be(version) || session_id || device_a || device_b
|| len(msg_a) || msg_a || len(msg_b) || msg_b
```

HKDF-SHA-256 labels:

- `clipsync/v1/confirm-a`
- `clipsync/v1/confirm-b`
- `clipsync/v1/pairing-wrap`
- `clipsync/v1/room-root`
- `clipsync/v1/sas` (SAS without the commitment round)
- `clipsync/v1/sas-committed` (SAS after the commitment round)
- `clipsync/v1/epoch/1`
- `clipsync/v1/content/{transferId}`
- `clipsync/v1/nonce/{chunkIndex}`

## Commitment round

The joiner (role B) commits to its SPAKE2 message before the creator (role A)
sends one, so a relay in the middle cannot pick its messages after seeing the
devices' messages (see [security](security.md#commitment-round)). This is the
commit-then-reveal order of ZRTP (RFC 6189, `hvi`) and of the Bluetooth LE
Secure Connections numeric comparison.

```
commitment = SHA-256("clipsync/v1/spake2-commit" || u16be(version)
                     || lp(session_id) || lp(device_a) || lp(device_b)
                     || lp("B") || lp(msg_b))
lp(x)      = u32be(len(x)) || x
```

`msg_b` is a fresh random group element, so it is its own hiding randomness;
there is no separate nonce.

1. The joiner sends `spake2_commit {pairing_session_id, commitment}` (Base64)
   right after its `hello`, and keeps its `spake2_msg` back.
2. The creator waits for that commitment after the joiner is announced. When
   it arrives, the creator sends its `spake2_msg` with `after_commitment:
   true`.
3. Only after the creator's `spake2_msg` arrived does the joiner send its own
   `spake2_msg` (the reveal). The creator checks that it opens the commitment;
   if not, it sends `error` (`commitment_mismatch`) and stops with nothing
   stored, before key confirmation.

Fallback, in which both devices derive the SAS without the round:

- A creator that receives a plain `spake2_msg` before any commitment (an older
  joiner sends it at once) answers without `after_commitment`.
- A creator that gets no commitment within 5 seconds (`COMMITMENT_WAIT`)
  sends its `spake2_msg` without `after_commitment`. An older relay drops
  `spake2_commit` as an unknown frame, so this keeps pairing through it from
  hanging.
- A joiner whose received `spake2_msg` lacks `after_commitment` (an older
  creator ignores the unknown frame; an older relay drops the frame and
  forwards `spake2_msg` without the new field) derives the SAS without the
  round. It still reveals only after the creator's message.

`after_commitment` is omitted from the JSON when false, so older peers see an
unchanged `spake2_msg`. With `pairing.require_commitment = true` a device
that would fall back sends `error` (`commitment_required`) instead and stops
with nothing stored.

The relay forwards `spake2_commit` only within an authenticated pairing
session, only from the joiner's side, only to the creator's side, and only up
to 64 bytes (a Base64 SHA-256 digest is 44). It holds the first commitment
while the creator is not connected, like the first SPAKE2 message, and
delivers it before that message. It never fans `spake2_commit` out to room
members.

## Pairing verification code (SAS)

Computed locally on each device from its SPAKE2 secret; nothing is added to
the wire:

```
label   = "clipsync/v1/sas-committed" after the commitment round,
          "clipsync/v1/sas" without it
sas_key = HKDF-SHA-256(ikm = spake2_secret, salt = SHA-256(transcript),
                       info = label)
sas     = decimal(u64be(sas_key[0..8]) mod 10^6), zero-padded to 6 digits
```

Each device shows which of the two it used. Without the round the code is the
one older (pre-commitment) SAS clients show. Because the label differs, two
devices that disagree about whether the round ran show different codes unless
a relay also searches for messages toward the downgraded device; a match is
trustworthy only when both devices show full protection (see
[security](security.md#commitment-round)).

After verifying the peer's `key_confirm`, a client shows the SAS and waits for
the local user. It sends its `device_bind` only after the user confirms. It
stores room keys, the room record, and the device token only after its own
`device_bind` went out and the peer's `device_bind` and `pairing_complete`
arrived. On rejection it sends a `key_confirm` whose `mac` is empty, then
`error` with code `sas_rejected`; the relay forwards the `key_confirm` to the
peer and aborts the pairing session on any client `error`. A SAS client that
receives a second `key_confirm` after the peer's first one stops with nothing
stored; a pre-SAS client fails key confirmation on it. Pre-SAS clients send
`device_bind` right after key confirmation; a SAS client buffers it until its
user confirms. A pre-SAS client also stores its room keys at that point, so
when the SAS side rejects, the pre-SAS client keeps an unused room-key secret,
though no room record or device token. See [security](security.md#verification-code-sas) for what the
comparison covers.

The peer device ID and name shown with the SAS are fixed before SPAKE2
starts: the creator takes them from the first `peer_joined`, the joiner from
the join response. A client that receives a later `peer_joined` with a
different device ID or name sends `error` (`pairing_failed`) and stops with
nothing stored. Clients also refuse a pairing code that is not six ASCII
digits, room or device IDs outside `[A-Za-z0-9._-]` (at most 64 bytes), and a
`pairing_complete` naming a room other than the one the pairing announced.

## Reconnect

Noise pattern `Noise_KK_25519_AESGCM_SHA256` using long-term X25519 device
keys. Payload encryption continues to use the current `roomEpochKey`.
