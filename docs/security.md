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
code. Answering "yes" is accepted only at an interactive prompt that has just
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

The CLI and the desktop app show the mode next to the code. The mobile
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
status. Destination checks must include DNS results, IPv4/IPv6 special ranges,
redirect behavior and the address actually used for the connection; see the
assurance case for current evidence and scope.

## Local storage

For identity and room/epoch keys, the current implementation reads private files first and mirrors them to
macOS Keychain / Windows Credential Manager when available. Linux currently
uses private files; a Secret Service backend is not enabled. Secret files have
mode `0600` on Unix. Unix data/inbox directories use `0700`; Windows relies on
the user's profile-directory access controls rather than POSIX modes.
Received filenames are sanitized to basenames.

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
Metadata and traffic timing remain visible to the relay. Denial of service,
delivery by a malicious relay, secure erasure of old copies/backups, and payload
forward secrecy are not guaranteed. Native libraries, OS trust stores and
platform services remain part of the trusted environment.
