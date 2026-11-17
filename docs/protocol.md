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
`spake2_msg`, `key_confirm`, `device_bind`, `pairing_complete`,
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
- `clipsync/v1/sas`
- `clipsync/v1/epoch/1`
- `clipsync/v1/content/{transferId}`
- `clipsync/v1/nonce/{chunkIndex}`

## Pairing verification code (SAS)

Computed locally on each device from its SPAKE2 secret; nothing is added to
the wire:

```
sas_key = HKDF-SHA-256(ikm = spake2_secret, salt = SHA-256(transcript),
                       info = "clipsync/v1/sas")
sas     = decimal(u64be(sas_key[0..8]) mod 10^6), zero-padded to 6 digits
```

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
