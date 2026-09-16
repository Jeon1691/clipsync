# ClipSync protocol v1

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
- `clipsync/v1/epoch/1`
- `clipsync/v1/content/{transferId}`
- `clipsync/v1/nonce/{chunkIndex}`

## Reconnect

Noise pattern `Noise_KK_25519_AESGCM_SHA256` using long-term X25519 device
keys. Payload encryption continues to use the current `roomEpochKey`.
