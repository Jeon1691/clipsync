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

The relay limits online guessing of this password. Every failed
`/v1/pairing/join` counts, whether the code was unknown, expired, busy or
otherwise rejected. By default, each client address (IPv6 by /64 prefix) may
fail 10 times per 15 minutes. The whole relay may fail 20 times per 120-second
pairing TTL.
A code's lifetime overlaps at most two of those relay-wide windows, so an
attacker with any number of addresses can test at most 40 of the 10^6 codes
while a session is open. That bounds the chance of guessing one session at
4 × 10^-5. When either budget is spent, the relay refuses every join, including
one with the correct code, with `429 too_many_attempts`. This response does not
reveal whether a code exists. Successful joins are not counted, but they do not
clear earlier failures. Behind a NAT or proxy, a legitimate user who shares an
address with a guesser is refused until the window ends. Anyone who can spend
the relay-wide budget can keep blocking new pairings. This is a deliberate
trade-off: guessing becomes a denial of service instead of a room compromise.
Per-client limits use the TCP peer address unless trusted proxies are configured;
see [self-hosting](self-hosting.md). The production Compose deployment trusts
its pinned network gateway, so each Cloudflare visitor has its own budget.
Operators can change these budgets, but startup rejects a relay-wide limit above
50 per pairing TTL, which keeps the chance of guessing one session at or below
10^-4. `/metrics` reports refused joins as `clipsync_pairing_throttled`.

Creating a pairing needs no credentials, so creation is limited the same way.
By default each client address (IPv6 by /64 prefix) may create 20 pairings per
15 minutes, each IPv6 /48 three times that, and the whole relay 100 per pairing
TTL, answered with `429 too_many_pairings` once spent. Independently, the relay
holds at most 1,000 open pairing sessions, 0.1% of the code space, so a fresh
code can always be allocated. A full session cap answers `503 relay_full` and
clears within one pairing TTL.

Pairing needs no credentials, so anyone can create rooms by pairing their own
devices, and those rooms hold valid one-year device tokens. The room count
therefore never blocks pairing. The relay holds at most 10,000 rooms by
default, counting open sessions that could still complete. At that cap, a new
pairing evicts the least recently used room that has no device online. A room
is used when one of its devices authenticates, sends anything, or disconnects.
A room with a device online is never evicted. Each online room holds at least
one WebSocket connection, and startup requires the room cap to exceed the
connection cap plus the 1,000 sessions, so an evictable room always exists. An
evicted room's devices get `unknown room` on their next `hello` and must pair
again, the same outcome as a relay restart, which loses every room. Someone
creating rooms as fast as the creation budget allows can therefore evict rooms
whose devices are all offline, oldest first, at the relay-wide creation rate.
As with joins, anyone who can
spend the relay-wide creation budget can block new pairings for a while, but
cannot take over existing rooms. Watch `clipsync_rooms`,
`clipsync_rooms_evicted` and `clipsync_pairing_create_full` on `/metrics`.

The relay generates this password. Pair through a trusted relay: an actively
malicious operator who knows the password is outside the honest-but-curious
model and may impersonate a pairing participant. HTTPS/WSS authenticates the
chosen relay and protects traffic in transit; it does not establish that the
operator is honest.

Device-binding messages share a session wrapping key but use sender-specific
nonce domains (A: index 2, B: index 3). Index 1 is reserved for legacy senders;
receivers decrypt using the nonce carried by the v1 message. This prevents the
two updated pairing participants, or a legacy/updated pair, from encrypting
different binding messages with the same key and nonce.

## Envelope authenticity, expiry, and replay

Envelope v2 (see [protocol](protocol.md#envelope-versions)) authenticates the
whole routing header: version, message and transfer IDs, room, sender,
recipient, `sent_at`, `expires_at`, item kind, and `seq`. It binds each chunk to
that header, its index, and the chunk count. A relay that rewrites any of these
fields makes AEAD fail. Receivers reject envelopes for another room or
recipient, from a sender that is not a pinned room peer, or past
`expires_at` (with a 30-second clock-skew tolerance). They also reject a `seq`
that is not newer than the last accepted one from the same sender. That
high-water mark is stored in the private data directory, so a replay is
rejected after the in-memory cache expires and after a daemon restart.

Limits:

- Sender identity is not cryptographically bound to a device. Every room
  member holds the same epoch key, so any member can produce a valid envelope
  that names any `sender_device_id`. What v2 adds is that the relay, which
  lacks the key, cannot change the claimed sender. Receivers accept only
  pinned peers other than themselves; in a two-device room that is the other
  device. No per-device signature is added.
- Legacy v1 envelopes authenticate only the message or transfer ID. Updated
  receivers accept them from a sender until that sender's first v2 envelope;
  later v1 envelopes from it are rejected as a downgrade. Until then, a relay
  can replay a recorded v1 envelope after the 120-second in-memory window or a
  restart, and extend its expiry. v1 checks run on unauthenticated values.
  `sync.strict_envelopes` refuses all v1, at the cost of not syncing with
  devices that are not updated.
- Updated devices send v1 to a peer until it proves it opens v2 (an accepted
  v2 envelope from it, or a v2 capability in its Noise handshake payload), then
  v2 for good; a broadcast uses v1 while any pinned peer of the room lacks that
  evidence. A malicious relay can delay the switch by dropping handshake
  messages, which keeps a pair on v1 (with the v1 limits above) for as long as
  it keeps dropping them, indefinitely if it wants. Two devices that are never
  online at the same time never run a handshake and also stay on v1. Once
  every device of a room is updated, set `sync.strict_envelopes` to remove v1
  entirely. The relay cannot forge either signal, because both need the epoch key or the
  peer's pinned static key, and it cannot move a peer already marked v2 back
  to v1, because the mark is stored and never cleared. Unauthenticated signals
  are not used for the decision. A room member, who holds the epoch key, can
  make a device switch to v2 early by sending a v2 envelope under another
  member's ID; if the impersonated peer is not updated, it then stops syncing
  with that device in both directions (v2 it cannot open, its v1 refused as a
  downgrade). Room members are trusted not to do this.
- An older item that arrives after a newer one from the same sender is dropped.
  Delivery and ordering by a malicious relay are not guaranteed: it can still
  drop or delay envelopes within their lifetime.
- Deleting `replay-state.json` resets the high-water marks. Replay protection
  then falls back to the expiry window until each sender's next accepted
  envelope.

## Keys

| Key | Purpose |
| --- | --- |
| Device X25519 | Long-term identity, Noise static key (handshake and capability payload only), epoch wrap |
| `roomRootKey` | First-epoch derivation only |
| `roomEpochKey` | Payload AEAD; current clients retain the initial key until re-pairing |
| Per-transfer content key | HKDF from epoch key + transfer ID |

The relay rejects revoked device tokens. The crypto library provides epoch-key
wrapping, but the current client does not automatically rotate room keys on
membership changes. Re-pair to establish a new room key.

Full forward secrecy for stored payloads is not provided. When both devices are
online they run a Noise `KK` handshake with their long-term keys. Its only use
is the small capability payload each side sends in it (which envelope versions
it accepts): a payload that opens is authenticated to the peer's pinned static
key, so the relay can drop it but not forge or alter it. The resulting session
is otherwise unused: clipboard payloads are not encrypted under it, sync does
not wait for it, and a missing or failed handshake does not stop sync. Noise
therefore provides no payload confidentiality, integrity, sender
authentication, or forward secrecy here. Payloads use persistent room epoch
keys; compromise of an epoch secret exposes the payload keys derived from it. The
library's epoch-wrapping mechanism also allows a recorded wrap to be decrypted
after compromise of the recipient's long-term identity. Treat
device and room secrets as sensitive, and re-pair to replace old room keys.
Changing this property requires a different room-key lifecycle and compatible
protocol changes; the current v1 format is retained for existing clients.

## Relay

The relay routes opaque ciphertext. It stores at most one latest encrypted text
envelope per recipient device in each room, in memory until the sender's
expiry and never longer than 60 seconds. It holds about 256 recipients'
envelopes at once, at most 16 of them sent from one client address (IPv6 by /64
prefix), so one user with many rooms cannot fill the queue for everyone.
Concurrent senders can pass the checks together, so either cap can briefly be
exceeded by the number of senders. When the queue, or the sender's share of
it, is full, a text for an offline recipient is acknowledged as
`peer_offline`. Image and file frames are forwarded only while the peer is
online and are never assembled or written to disk.

Room traffic and presence reach only sockets authenticated with a device token
for a current, non-revoked room member, and routes are keyed by room and device
ID. Device IDs are not secret, and the pairing endpoints issue pairing tokens
for any device ID without authentication. A pairing socket therefore exchanges
frames only within its own pairing session; it cannot send room traffic, or
receive, displace or take offline a room member's route, and a device in one
room cannot shadow the same device ID in another room. A member may hold up to
four routed sockets at once, such as its sync daemon beside a CLI `push` or
`pull`, and the relay delivers the member's room traffic to each of them. A
fifth socket evicts the member's oldest, and an older socket whose outbound
queue stays full for 30 seconds is evicted instead of stalling delivery. The
relay sends to all of a member's sockets at once, so stalled sockets delay a
delivery by at most 30 seconds in total. A socket that sends nothing for 60
seconds, three missed client heartbeats, is closed and loses its route. A
half-open socket left behind by a reconnect therefore stops counting the member
as online within that time: text is queued for it again and its peers see it
leave. Only a routed socket may send room traffic, and closing one socket
removes only its own registration. Revocation removes all of the member's routes
and closes every one of its sockets. A socket checks that it is still routed
before it forwards each frame, and the check and forward are not atomic, so a
frame that passed the check just before the revocation is still forwarded: at
most one frame per socket of the revoked member, because each socket handles its
frames one at a time. After that none of its sockets can send room traffic.
Pairing requests that name a device ID already connected to a room are not
rejected: such a check would reveal presence to unauthenticated callers and stop
a device from pairing into another room, while routing no longer depends on it.

All relay state is in memory and bounded. Pairing sessions are removed from
every index when they are aborted, and within seconds of completing or
expiring. A room is removed once every device in it is revoked, or once its
device tokens have expired (one year after pairing; tokens are not renewed)
and none of its devices still has a routed socket. At the room cap, the least
recently used room with no device online (no routed member socket) is also
evicted to make way for a new pairing (see [Pairing](#pairing)).

By default the relay accepts at most 512 WebSocket connections at once, 32 from
one client address (IPv6 by /64 prefix) and 96 from one IPv6 /48. It refuses
further upgrades with `503` or `429`. Sockets that are not room members may
hold at most 75% of the relay-wide slots: sockets that have not sent a valid
`hello` yet, and pairing-token sockets, because pairing tokens are issued
without authentication. Past that share, or at the relay-wide cap while any
such socket is open, a new socket evicts the oldest of them instead of being
refused, so a flood of such sockets cannot lock out clients that send `hello`
right away. An evicted socket keeps its slot for the fraction of a second it
takes to close, so sockets closing after an eviction at the cap may exceed it
by 5% (at least one). Every socket holds its own slot, so each of a member's
up to four routed sockets counts against these caps. A closing pairing socket
sends its room no leave notice, so it never waits for a peer that stopped
reading. A pairing-token socket is also closed when its pairing session
expires. A socket that has not sent a valid `hello` within 10 seconds of the
upgrade is closed, even while it keeps sending other messages or stops reading
the relay's replies; no message extends that deadline. A room member's socket
is closed after 60 seconds of silence, as above. The caps still count
sockets during those deadlines, so a client that keeps reopening idle sockets can deny
connections to others sharing its address. Clients with enough addresses can
hold the authenticated share by authenticating into rooms of their own. The
IPv6 /48 caps assume one site per /48; where an ISP puts many customers in one
/48, they share these caps, and one of them can spend them for the rest.

Webhooks carry metadata only: opaque IDs, item kind, encrypted size, timestamp,
status. Destination checks must include DNS results, IPv4/IPv6 special ranges,
redirect behavior and the address actually used for the connection; see the
assurance case for current evidence and scope.

## Local storage

For identity and room/epoch keys, the current implementation reads private files first and mirrors them to
macOS Keychain / Windows Credential Manager when available. Linux currently
uses private files; a Secret Service backend is not enabled. Secret files have
mode `0600` on Unix. Unix data/inbox directories use `0700`; Windows relies on
the user's profile-directory access controls rather than POSIX modes.
Received filenames are sanitized to basenames. A file transfer whose names
differ only by letter case is rejected whole on every platform, because
case-insensitive filesystems (macOS, Windows) would store them as one file.
Names the receiving filesystem otherwise resolves to one entry, such as
Unicode normalization variants on APFS, are rejected when staging writes the
second one; filesystems that keep them apart (ext4, NTFS) accept both.

`replay-state.json` in the data directory holds envelope sequence numbers, not
secrets. It is written atomically as a private file. Its integrity matters
for replay protection (see above). An unreadable file stops sync with an error
instead of being reset.

Device tokens are stored separately as `device-token-<hex-room-id>.secret`;
webhook credentials use `webhook.secret`. These credentials use canonical private files without OS-keychain fallback; missing or empty files are errors. New `state.json` and `config.toml`
serialization omits these values. Loading a legacy inline credential writes
the separate private secret successfully before atomically replacing metadata.
A failed migration retains the original file. Missing secrets fail the load
rather than silently inventing a credential. Back up the complete private data
directory before migration; old binaries cannot read the new token layout.
Update clients together, and re-pair rather than downgrading migrated state.
The writes are individually atomic and ordered, rather than a general
multi-file transaction. Unix parent-directory syncing improves rename
durability; Windows power-loss durability is not established by these tests.

To replace client credentials, stop the daemon and re-pair to replace device and
room keys/tokens; no recompilation is needed. The webhook secret can be replaced
in its separate private file while stopped. Restart consumers after rotation.
Relay signing and webhook secrets are supplied separately through runtime
environment variables; rotate them and restart the relay, then re-pair affected
clients. Do not put production values in the checked-in example or logs.

## Security expectations and limits

The intended requirements are confidentiality and integrity of clipboard
payloads against the relay/network, pairing authorization, rejection of stale
or invalid transfers, bounded resource use, confined received paths, and private
local credentials. The [assurance case](assurance-case.md) connects these claims
to implementations and tests and records unresolved limits.

A compromised or unlocked authorized endpoint can read clipboard contents and
keys. Same-user local malware and administrators are outside the confidentiality
guarantee. Pairing codes must be shared through an authenticated separate
channel. Use HTTPS/WSS with trusted certificates for public deployments; the
relay requires a TLS certificate/key pair by default and permits only TLS
1.2/1.3. Its explicit HTTP opt-in requires TLS termination and network
restrictions for public deployments; see [self-hosting](self-hosting.md).
Clients refuse plaintext `http://`/`ws://` relay URLs, and URLs without a
scheme, when saving configuration and again before each relay request, because
pairing codes (the PAKE password), pairing tokens and device tokens would
otherwise cross the network in clear. Loopback hosts (`127.0.0.0/8`, `::1`,
`localhost`) are exempt; `CLIPSYNC_ALLOW_INSECURE_RELAY=1` overrides the check
for a trusted network and logs a warning on every use. Relay HTTP requests never
follow redirects, so a relay or TLS front cannot forward the pairing code to an
unchecked host or scheme. Plaintext relay requests never use a proxy
(`HTTP_PROXY`, `ALL_PROXY` or system settings), so loopback traffic stays on
this machine; `https://` relays still honor proxies.
Metadata and traffic timing remain visible to the relay. Denial of service,
delivery by a malicious relay, secure erasure of old copies/backups, and payload
forward secrecy are not guaranteed. Native libraries, OS trust stores and
platform services remain part of the trusted environment.
