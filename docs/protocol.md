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
- `GET /metrics` (operator-only; disabled unless a bearer token is configured)

## Control messages (JSON text frames)

Tagged `type` field: `hello`, `hello_ok`, `peer_joined`, `peer_left`,
`spake2_commit`, `spake2_msg`, `key_confirm`, `device_bind`, `pairing_complete`,
`noise_handshake`, `text_envelope`, `manifest`, `ack`, `epoch_wrap_msg`,
`revoke`, `ping`, `pong`, `error`.

Relay-visible routing header includes room/device IDs, transfer ID, chunk
index, item kind, timestamps. File names, MIME types, hashes, and dimensions
live only in the encrypted manifest. The relay can read the routing header;
under envelope v2 it cannot change it undetected (see below).

## Binary chunk frames

```
magic "CSY1" | version u8 | type u8=2 | transfer_id 16 | chunk_index u32be
| chunk_count u32be | ciphertext_len u32be | nonce||ciphertext
```

The frame layout (and its version byte, 1) is the same for both envelope
versions; only the AEAD associated data differs.

## Envelope versions

`RoutingHeader.version` selects the AEAD associated data (AAD) for the text
envelope, manifest, and chunk frames of one clipboard item. It is separate from
`hello.protocol_version`, which stays `1` so updated clients keep working with
existing relays; relays forward routing headers unchanged.

| `version` | AAD | Header fields authenticated |
| --- | --- | --- |
| 1 (legacy) | text `"{message_id}:text"`; manifest `"{transfer_id}:manifest"`; chunk `transfer_id \|\| u32be(chunk_index)` | message ID (text) or transfer ID only |
| 2 (authenticated) | canonical header encoding below | all fields |

Version 2 AAD, where `lp(x) = u64be(byte_len(x)) || utf8(x)`:

```
"clipsync-envelope-v2" || u16be(version)
|| lp(message_id) || lp(transfer_id) || lp(room_id) || lp(sender_device_id)
|| recipient: 0x00 (broadcast) | 0x01 || lp(device_id)
|| i64be(sent_at Unix seconds) || u32be(sent_at nanoseconds)
|| i64be(expires_at Unix seconds) || u32be(expires_at nanoseconds)
|| item_kind: 0x00 text | 0x01 image | 0x02 files
|| u64be(seq)
|| frame: 0x00 text | 0x01 manifest | 0x02 || u32be(chunk_index) || u32be(chunk_count)
```

Chunk frames carry no routing header. A receiver opens a chunk only for a
transfer whose manifest it accepted, using that manifest's header, so a v2
chunk is bound to the header, its position, and the chunk count.

### Receiver checks

Every text envelope and manifest, of either version, is rejected unless:

1. `version` is 1 or 2;
2. `room_id` is the receiver's current room;
3. `recipient` is `broadcast` or the receiver's device ID;
4. `sender_device_id` is a pinned peer of the room and not the receiver;
5. `item_kind` is `text` for a text envelope, and a decrypted manifest
   describes the same `item_kind` as its header;
6. `expires_at` is at most `CLOCK_SKEW_SECS` (30 s) in the past and
   `expires_at - sent_at` is at most 300 s; for v2, `sent_at` is also at most
   30 s in the future (not checked for v1, see [Clocks](#clocks));
7. a v1 envelope does not come from a sender that has already delivered a v2
   envelope to this device (downgrade), and the receiver does not run with
   `sync.strict_envelopes`;
8. AEAD opens;
9. for v2, `seq` is strictly greater than the highest `seq` previously accepted
   from that sender in that room. The new value is stored durably before the
   item is applied. It is stored only after AEAD succeeds, so a forged header
   cannot raise it.

For v2, AEAD authenticates every value checks 1–6 read. For v1 the same checks
run on values a relay can rewrite, and step 9 does not apply.

Rejections are logged as warnings; the expiry and future-`sent_at` messages
name the clock tolerance and say to set both devices to automatic time.

### Clocks

v2 requires the sender's and receiver's wall clocks to agree within
`CLOCK_SKEW_SECS` (30 s). A v2 envelope whose `sent_at` is more than 30 s ahead
of the receiver's clock is rejected, and so is one received more than 30 s
after `expires_at`, so a receiver clock more than the TTL plus 30 s ahead of
the sender's (90 s with the default `sync.ttl_secs` of 60) also drops fresh
envelopes. Devices that use automatic (NTP) time are far inside this margin.

For v1 the future-`sent_at` check is skipped. The relay can rewrite a v1
`sent_at`, so the check would add no protection, and it would stop a released
peer whose clock runs ahead, which synced before v2. The expiry check still
applies to v1: a receiver whose clock is more than the TTL plus 30 s ahead of
a released sender's drops that sender's envelopes. Released clients did not
check expiry, so this is new for such pairs; it bounds how long a relay can
hold a v1 envelope as long as it leaves `expires_at` unchanged.

### Sequence numbers and replay state

Senders use `seq = max(last_sent + 1, current Unix time in microseconds)` per
room and store it before sending. Numbers therefore keep increasing across
restarts, a clock stepping back, a deleted state file, and a daemon and one-shot
`push` sending at once. Receivers store, per room and sender, the highest
accepted v2 `seq` and whether the sender has used v2. The same per-peer entry records
whether the peer advertised v2 in its Noise handshake (send side only, see
below).

Both live in `replay-state.json` in the private data directory (mode `0600` on
Unix). Missing fields default, so older or partial files load. An unreadable
file stops sending and receiving with an error instead of being reset; deleting
it limits replay protection to the expiry window until each sender's next
envelope is accepted. It also forgets which peers accept v2, so the device
sends v1 again until the next handshake; a peer that already locked this
device to v2 drops those envelopes as a downgrade meanwhile.

The daemon and one-shot processes use the same file: `push` of an image or
files, `push` of text when the daemon is not reachable, and `pull` when the
daemon is not reachable or has nothing to return. Every change is a
load-modify-save under an exclusive advisory lock on `replay-state.lock` in the
same directory (`flock` on Unix including Android, `LockFileEx` on Windows;
created `0600`). One process therefore cannot overwrite another's update;
without the lock, a concurrent save could roll back a `highest_seq` and let a
replay through, or clear a sender's v2 flag and re-open the v1 downgrade. The OS
releases the lock if a process dies. A sender decides the envelope version in
the same locked update that reserves the `seq`, so once one process has sent v2
into a room, every envelope with a later `seq` from this device is v2 too.
Readers take no lock, because every save atomically replaces the whole file.
If the lock cannot be taken, the change fails with an error, as for an
unreadable file; there is no unlocked fallback.

Because acceptance is strictly increasing, an older item that arrives after a
newer one from the same sender is dropped. One relay connection delivers in
order; this can affect two processes of one device sending concurrently, whose
`seq` reservations and relay sends can interleave.

### Transition from v1

The send version is negotiated per peer; no configuration is needed during a
staggered upgrade.

- **Receive.** Updated receivers always accept v2. They accept v1 from a sender
  only until they accept a v2 envelope from that sender. That state is stored,
  so later v1 envelopes from the sender are rejected as a downgrade, including
  after a restart. v1 envelopes still get checks 1–6 (without the
  future-`sent_at` check) on unauthenticated values and only the in-memory
  message-ID cache (120 s) against replay: legacy senders restart `seq` at 1
  every session.
- **Send.** An updated device sends v1 to a peer until it has positive evidence
  that the peer opens v2, then v2 to that peer for good. Evidence is either
  1. a v2 envelope from that peer that this device accepted, or
  2. the peer's capability payload in a completed Noise handshake (see
     [Reconnect](#reconnect)) listing envelope version 2.

  Both are authenticated: a v2 envelope needs the room epoch key, and the
  handshake payload is bound to the peer's pinned static key. The evidence is
  stored in `replay-state.json` and never cleared, so it cannot be rolled
  back. Unauthenticated signals, such as `ack.detail` or presence, are not
  used. A handshake advertisement alone does not make the peer's v1 envelopes
  a downgrade (the peer may run with `sync.legacy_envelopes`).
- **Room broadcast.** Envelopes are broadcast, so one version serves every
  peer: v2 only when every pinned peer of the room has given evidence,
  otherwise v1 for the least capable peer. Once this device has sent v2 into
  a room (`sent_authenticated` in `replay-state.json`), it keeps sending v2
  there: peers that accepted it would refuse its v1. A peer pinned later that
  cannot open v2 then cannot read this device (pairing creates a new room with
  one peer, so this does not arise today). Sending both a v1 and a v2 copy was
  rejected: an updated receiver would apply the item twice or, once the sender
  is locked to v2, log the v1 copy as a downgrade, and every transfer would
  cost twice the bandwidth.
- **Clocks.** Updated and released devices need no configuration, but the
  expiry check is new for v1: a pair keeps syncing only while the receiver's
  clock is no more than the TTL plus 30 s (90 s by default) ahead of the
  sender's (see [Clocks](#clocks)). Once on v2, both clocks must agree within 30 s.
- **Released clients** cannot decrypt v2 and never advertise it: they send an
  empty handshake payload and ignore the payload they receive (they read it
  into a 1024-byte buffer, `LEGACY_HANDSHAKE_PAYLOAD_MAX_BYTES`; the
  capability payload is far smaller). They therefore keep receiving v1, and
  updated devices keep accepting their v1.
- Two updated devices switch to v2 the first time both are online: the
  device with the smaller device ID starts the handshake on `hello_ok` and on
  `peer_joined`. If they are never online together, or a relay drops the
  handshake, they stay on v1 (see `docs/security.md`). One-shot `push` does
  not run a handshake; it uses the stored evidence.

Overrides (restart the daemon after changing them):

| Key | Send | Receive |
| --- | --- | --- |
| default | negotiated as above | v2; v1 until the sender's first v2 |
| `sync.legacy_envelopes = true` | always v1 | as default |
| `sync.strict_envelopes = true` | always v2 | v2 only; every v1 is rejected |

`strict_envelopes` wins if both are set. `legacy_envelopes` is a manual escape
hatch, for example a peer that advertises v2 but cannot open it. Enable it only
before the peer has received v2 from this device: an updated peer that has
already accepted v2 from it rejects its later v1 envelopes as a downgrade.
`strict_envelopes` stops syncing with peers that are not updated.

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
  hanging. A commitment the creator's connection had already received when
  the wait ran out still counts, so a creator that was briefly stalled (for
  example, a suspended process) does not fall back. Only one still in transit
  loses to the wait.
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
digits, a create or join response whose room or device ID breaks the ID rule
in [interfaces.md](interfaces.md) (it fails to decode), and a
`pairing_complete` naming a room other than the one the pairing announced.

## Reconnect

When a pinned peer is online, clients run `Noise_KK_25519_AESGCM_SHA256` with
their long-term X25519 device keys. The device with the lexicographically
smaller device ID initiates, on `hello_ok` and on `peer_joined`. A responder
finds the sender by trying the pinned static key of each peer that would
initiate towards it; a message that opens under none of them is ignored.

Each handshake message from an updated client carries a capability payload,
the JSON object

```
{"envelope_versions":[1,2]}
```

listing the envelope versions the device accepts (`[2]` in strict mode).
Unknown fields are ignored; an empty or unparseable payload advertises
nothing. Payloads must not exceed `LEGACY_HANDSHAKE_PAYLOAD_MAX_BYTES`
(1024): released clients read handshake payloads into a buffer of that size
and fail the handshake on anything larger. Released clients send an empty
payload.

Apart from that payload, the handshake result is unused: no clipboard payload
is encrypted under the Noise session, and sync neither waits for nor depends on
a completed handshake. Payload encryption uses the current `roomEpochKey` and
the envelope AAD above.
