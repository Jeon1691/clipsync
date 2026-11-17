# Security model

To disclose a vulnerability privately, follow the [security policy](../SECURITY.md).

## Threats in scope

- Honest-but-curious relay operator
- Pairing-code guessing
- A relay that runs one SPAKE2 session with each device during pairing, within
  the limits described under [Verification code](#verification-code-sas) and
  [Commitment round](#commitment-round)
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
otherwise rejected. A join whose body does not decode (`400 invalid_request`,
for example an invalid device ID) or that names an unsupported protocol version
is refused before any code is looked up, with a response that does not depend
on the code, so it tests no code and is not counted. By default, each client address (IPv6 by /64 prefix) may
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

The relay generates this password and sees it in the create response and the
join request. HTTPS/WSS authenticates the chosen relay and protects traffic in
transit; it does not establish that the operator is honest. A relay that knows
the password can run one SPAKE2 session with each device. Key confirmation
then succeeds on both devices, because each device really did agree a key with
the relay. The verification code below is the check that catches this.

### Verification code (SAS)

After key confirmation, each device derives a 6-digit short authentication
string from its own SPAKE2 result:

```
HKDF-SHA-256(ikm = SPAKE2 secret, salt = transcript hash,
             info = "clipsync/v1/sas-committed" after the commitment round,
                    "clipsync/v1/sas" without it)[0..8] as u64be mod 10^6
```

Both devices show the code, the other device's relay-reported name, and
whether the [commitment round](#commitment-round) ran (`protection: full` or
`protection: REDUCED`). The
user confirms only if the two screens show the same code. Two devices show the
same code only when they ran SPAKE2 with each other:

- **Relay in the middle.** Each device ran SPAKE2 with the relay, so each
  derived a different secret and transcript, and the codes differ.
- **Guessed code.** A joiner who guessed the pairing code completed SPAKE2
  with the creator. The device the user meant to pair never joined, so it
  shows no matching code, and the creator's user rejects.

Until the user confirms, the device sends no `device_bind` and stores no room
keys, room record, or device token. A rejection, an unanswered prompt, or the
pairing deadline aborts with nothing stored. The deadline runs from when the
code was created, not from when the prompt appears: the relay keeps a pairing
session for 120 seconds after `room create`, so joining and comparing must both
fit in that window. On rejection the device first sends a `key_confirm` with
an empty MAC as an abort notice, then an `error` frame (`sas_rejected`), and
the relay drops the pairing session. The relay forwards the notice but not the
`error`, so the other device stops at once with nothing stored, whether its
user is still comparing or already confirmed. A device that predates the code
fails key confirmation on the notice instead. If the notice is lost, the other
device's later `device_bind` is refused, or it fails at its pairing deadline.
The notice is unauthenticated, but a relay that forges it gains nothing it
could not get by dropping the session. The peer name comes from the relay and
is not authenticated; only the code is.

**The peer name is fixed before SPAKE2.** The creator takes the joiner's
device ID and name from the relay's first `peer_joined`, and SPAKE2 starts
only after it; the joiner takes the creator's from the join response. Any
later `peer_joined` with a different device ID or name aborts the pairing
with nothing stored; an identical repeat changes nothing. The honest relay
announces the joiner to the creator again when a pairing connection
re-registers, always with the same device, so those repeats are ignored.
Pinning the name does not keep it from naming a code: a relay running both
SPAKE2 sessions can finish the one with the joiner before it announces the
joiner to the creator, so it already knows the joiner's code and can pick a
name such as `verification code: 123 456`. What keeps such a name from
passing for the code line is how it is shown (below): sanitized, quoted, on
its own line labelled `other device:` and at most 40 columns wide. A name
that only looks like a code line inside its quotes is still possible, so
users compare the unquoted `verification code:` line, which carries no
untrusted text.

**Untrusted text next to the code.** The relay chooses the peer name it
reports and the text of its error messages. Printed raw, terminal escape
sequences, carriage returns or backspaces in that text could overwrite the
displayed code with matching digits, Unicode bidirectional controls could
reorder digits in a text view, and blank-looking padding could wrap a name
into a row that looks like the code line, so a user could approve a split
pairing. Peer names, relay error messages and the SAS prompt's peer name in
every front end go through one sanitizer
(`clipsync_core::sanitize_untrusted`), which shows as visible `\u{..}`
escapes:

- C0 and C1 controls (including ESC, CR and backspace);
- bidi and other format controls (U+061C, U+200B-U+200F, U+2028-U+202E,
  U+2060-U+206F, U+FEFF, U+FFF9-U+FFFB);
- Unicode `White_Space` other than the ASCII space (no-break, ideographic,
  en/em and other spaces), the remaining default-ignorable code points
  (soft hyphen, combining grapheme joiner, Hangul fillers U+115F, U+1160,
  U+3164 and U+FFA0, Khmer inherent vowels, Mongolian selectors, variation
  selectors, tags), and the braille blank U+2800;
- combining marks past two on one base character, counted in the combining
  diacritical marks blocks (U+0300-U+036F, U+1AB0-U+1AFF, U+1DC0-U+1DFF),
  the marks for symbols U+20D0-U+20FF, the half marks U+FE20-U+FE2F and the
  Cyrillic marks U+0483-U+0489, so a stack cannot draw over the code line.
  Marks of a particular script are not counted.

It also collapses runs of spaces and bounds the rendered width in terminal
columns, counting two for every non-ASCII character: names to 40 columns,
relay messages to 256. The CLI prints the quoted peer name on an
`other device:` line before the code, which sits alone on its own line with
nothing untrusted printed after it; at 40 columns the name line cannot wrap on
a terminal 56 columns or wider, and on a terminal at least 30 columns wide it
cannot wrap into a row holding a whole forged code. On a terminal 26 to 29
columns wide the name can wrap into such a row; the real code line still
follows the name line. Below 26 columns the real code line wraps too. The
desktop app and the mobile `SasVerifier` receive the sanitized name and show
it apart from the code.

**Relay-chosen codes and IDs.** The pairing code, room ID and peer device ID
that the CLI prints come from the relay. The client accepts a pairing code
only as exactly six ASCII digits, and room and device IDs only as ASCII
letters, digits, `-`, `_` and `.` (at most 64), and requires the completion
to name the room the pairing announced; anything else aborts with nothing
stored. The CLI prints IDs read back from local files (`room list`,
`clipsync devices`, the background pairing file) through the same check,
escaping any that fail it, and passes peer names, OS strings and fingerprints
in `clipsync devices` through the sanitizer. Other output, such as
clipboard text printed by `clipsync pull`, is not sanitized: it is the
content being synced.

**A pairing that ends during the comparison.** If the other device rejects,
the pairing expires, or the request is removed or rejected while `clipsync
room confirm` is prompting, the prompt ends at once with the reason; it does
not wait for another line. Foreground `room create` and `room join` already
stop their prompt when the pairing ends.

Users must actually compare the codes. Typing the code shown on the other
device is the most reliable comparison, and the only one the CLI accepts
without a terminal: `clipsync room confirm CODE` and a piped `room join`
answer must be the other device's digits, which are checked against the local
code. Every one of these paths prints this device's protection line (fixed
local text, apart from the peer's name) before the answer counts; `room
confirm CODE`, which shows no code, prints it once `CODE` has been read. Under
`protection: REDUCED` a match is trustworthy only on a relay the user trusts,
so an answer that no person gave right after seeing the REDUCED line (`room
confirm CODE`, a prompt answer read from a pipe or file, or a prompt whose
output is redirected or in `--json`) is refused before comparing unless the command was given `--accept-reduced-protection`.
Otherwise a relay that downgraded the creator could have a scripted
confirmation approve a code it searched for without the warning reaching
anyone. With `pairing.require_commitment` set, the pairing is refused before
any code is shown, and the flag does not change that. Answering "yes" is
accepted only at an interactive prompt that has just
displayed the local code, and relies on the user having looked. Before that
prompt the CLI discards everything already typed at the terminal, so a "yes"
typed while `room confirm` or `room join` was still waiting cannot approve a
code that appears later. If the terminal's pending input cannot be discarded
(for example an MSYS terminal on Windows, whose stdin is a pipe rather than a
console), only the other device's digits are accepted.

#### Commitment round

The relay knows the pairing code, so it can run one SPAKE2 session with each
device. If a device sent its SPAKE2 message before anything bound the other
side, a relay in the middle would see both honest messages before it picked
its own message toward each device. It could then search offline for a pair
of messages that make the two devices derive the same 6-digit code, a
birthday search of roughly 2^10 cheap trials per side.

The commitment round removes that freedom. The joiner first sends a SHA-256
commitment to its SPAKE2 message, bound to the protocol version, pairing
session ID, both device IDs and its role. The creator sends its own message
only after it holds that commitment, and checks that the joiner's message,
revealed afterwards, opens it; a mismatch aborts before key confirmation with
nothing stored. The joiner reveals its message only after the creator's
arrived. When both devices run the round this way, a relay in the middle has to fix its message toward the
creator (by committing to it) before it learns the creator's message, and its
message toward the joiner before it learns the joiner's. When it learns each
device's message it can no longer change anything that device's code depends
on, so the two codes are independent: they match with probability 10^-6
(about 2^-20) per pairing attempt. A failed attempt shows the users two
different codes and stores nothing. This is the commit-then-reveal order of
ZRTP (RFC 6189) and of the Bluetooth LE Secure Connections numeric
comparison. Binding rests on SHA-256 collision resistance; the committed
message is a fresh random group element, so it hides itself without a
separate nonce. See [protocol](protocol.md#commitment-round) for the frames.

**Reduced protection.** A device that predates the round, and a relay that
predates it (it drops the unknown frame), still pair. The creator waits 5
seconds for a commitment that never comes, or answers an older joiner's
SPAKE2 message at once, and both devices then derive the code without the
round. The screen says `protection: REDUCED`. In that mode the creator may
send its message before anything binds the joiner's side, and the offline
search above is possible again, so pair only through a relay you trust.

**What a matching code proves.** Each half of the exchange is protected on
its own. The creator's half is protected only if the creator held the
joiner's commitment before it sent its message, which the creator checks
itself; then it shows `protection: full`. The joiner's half is protected
whenever the joiner runs the round, because it always commits first and
reveals last. The joiner's mode line repeats the creator's acknowledgement,
which the relay can forge, so it is not proof about the creator. A relay that
leaves either half unprotected can search for messages toward that device
until its code equals the other device's code, whatever either screen says.
Therefore:

- **Trust a match only when both screens show `protection: full`.** Then the
  creator held a commitment and the joiner runs the round, so the relay had
  one try per pairing. A device that predates the round shows no protection
  line, and a match with it is not protected against the relay.
- The mode is also an HKDF input to the code. A relay that only strips the
  acknowledgement, or only withholds the commitment, without that search
  leaves the devices in different modes, and their codes differ. This is
  defense in depth, not the protection itself.
- A relay can make the creator fall back by dropping the commitment, exactly
  as an older relay does; the two cases cannot be told apart. The creator
  then shows `protection: REDUCED`, even if the relay makes the joiner show
  `full`. With `clipsync config set pairing.require_commitment true` on the
  creating device, the creator refuses instead: it sends `error`
  (`commitment_required`) before it shows a code and stores nothing. Set it
  on every device you pair from, because the creator's setting is the one
  that counts against a malicious relay. A joiner with the setting refuses a
  creator that does not acknowledge the commitment, but it cannot detect a
  forged acknowledgement. The setting is off by default so that devices and
  relays without the round still pair.

The CLI and the desktop app show the mode next to the code, and every CLI
confirmation path states it (see [the comparison](#verification-code-sas) above
for when the CLI also requires `--accept-reduced-protection`). The mobile
`SasVerifier.confirm_sas` callback receives it as `full_protection`, and the
mobile library also logs a warning for a reduced-protection pairing.

**Older peers.** An updated device still pairs with an older one. With a
device that shows a code but predates the commitment round, both show the
same code and the updated device states `protection: REDUCED` when its peer
is honest. Through a malicious relay it may show `full` (see above), but the
older device shows no protection line, so such a match is never protected.
A device that predates the code shows none, so the comparison cannot be made. Treat such a pairing as unverified, or
update both devices first. An older device writes its room key to local
storage right after key confirmation, before the comparison. If the updated
device rejects, the older one stores no room record or device token, but
keeps that room-key secret. No room refers to it, so it is never used; removing
it would need a change on the older device.

**Skipping the comparison.** `clipsync room create|join --insecure-skip-sas`
and the mobile `create_room()` / `join_room()` calls, kept for existing apps,
accept without comparing. They provide none of the protection above.

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

The canonical copy of the device identity (`identity.json`) and of each room
and epoch key (`room-<id>.secret`, `epoch-<id>.secret`) is a private file in
the data directory, and loads read that file first. On macOS and Windows every
save also mirrors the value to Keychain / Credential Manager on a best-effort
basis (a failure is logged and the file remains authoritative). The mirror is
consulted only when the file is missing; a recovered identity is re-cached as
the private file, while recovered room/epoch keys are not. Linux and other Unix
platforms use the private files only; there is no Secret Service backend.

Every secret file, and `pairing.json` (which holds the live pairing code), is
written through one helper: a temporary file in the destination directory is
restricted to mode `0600` on Unix before any bytes are written, synced, and
atomically renamed over the destination, followed by a parent-directory sync on
Unix. A secret is therefore never readable under the process umask while being
written, a crash leaves the previous file rather than a truncated one, and a
link planted at the destination is replaced rather than followed. Unix
data/inbox/log directories are set to `0700` (best effort) when storage opens,
and so is the runtime directory when ClipSync creates it (an existing one, such
as `$XDG_RUNTIME_DIR`, is left as is). Windows relies on the user's
profile-directory access controls rather than POSIX modes. Received filenames
are sanitized to basenames. A file transfer whose names
differ only by letter case is rejected whole on every platform, because
case-insensitive filesystems (macOS, Windows) would store them as one file.
Names the receiving filesystem otherwise resolves to one entry, such as
Unicode normalization variants on APFS, are rejected when staging writes the
second one; filesystems that keep them apart (ext4, NTFS) accept both.
On macOS, every file ClipSync writes from peer-received bytes carries a
`com.apple.quarantine` attribute naming `ClipSync` as the agent, so opening a
received app, script or installer goes through Gatekeeper's first-launch check.
This covers files committed to the inbox (marked through the open file in the
private partial directory, before any bytes are written and before the
directory is published), the files, image or text that
`clipsync pull --output DIR` writes (marked through the open file before any
bytes are written), and the pasteboard image file staged for Finder paste
(marked before it is renamed into place). When a commit reuses an identical
published directory, or the staging sweep restores an interrupted replacement,
any of its files that lacks the attribute (for example one written by an older
version) gets it under the publish lock, and an existing value is never
replaced. If the volume rejects the attribute, the failure is logged and the
validated file is still delivered unquarantined.
Wire IDs are checked when decoded (see [interfaces](interfaces.md)), so a
relay- or peer-supplied room ID cannot add path separators, `..` or control
characters to `room-<room-id>.secret` or `epoch-<room-id>.secret`; those
paths are built only after checking the ID again, and the relay answers
`400` to create/join requests carrying such IDs.

`clipsync.log` in the log directory captures the background pairing worker's
stdout/stderr (and, on macOS, the launchd daemon's). The pairing worker opens it
as a `0600` file on Unix and narrows an existing log with a wider mode; a log
that launchd creates relies on the `0700` log directory. The worker writes only
status lines, never the pairing code: the foreground command reads the code
from `pairing.json` and shows it on the terminal.

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
channel, and users must compare the verification code on both devices. Use HTTPS/WSS with trusted certificates for public deployments; the
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
