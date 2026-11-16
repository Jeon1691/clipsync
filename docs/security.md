# Security model

To disclose a vulnerability privately, follow the [security policy](../SECURITY.md).

## Threats in scope

- Honest-but-curious relay operator
- Pairing-code guessing
- A relay that runs one SPAKE2 session with each device during pairing, within
  the limit described under [Verification code](#verification-code-sas)
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
             info = "clipsync/v1/sas")[0..8] as u64be mod 10^6
```

Both devices show the code and the other device's relay-reported name. The
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

Users must actually compare the codes. Typing the code shown on the other
device is the most reliable comparison, and the only one the CLI accepts
without a terminal: `clipsync room confirm CODE` and a piped `room join`
answer must be the other device's digits, which are checked against the local
code. Answering "yes" is accepted only at an interactive prompt that has just
displayed the local code, and relies on the user having looked.

**Limit: offline search by a relay.** There is no commitment step: a device
sends its SPAKE2 message before it sees the peer's. A relay in the middle sees
both honest messages before it picks its own message toward each device. It can
therefore search offline for a pair of its own messages that make the two
devices derive the same 6-digit code. This is a birthday search of roughly
2^10 cheap trials per side, so a relay that is built to do this can defeat the
comparison. The code fully stops a guessed-code joiner and any relay that does
not run this search, and it makes an attack an explicit, targeted change to
the relay. Closing the gap needs a commitment to the SPAKE2 messages, which is
a protocol change; relay SAS grinding is addressed in a follow-up PR. Until
that ships, pair through a relay you trust.

**Older peers.** The code is computed locally, so the wire format is unchanged
and an updated device still pairs with an older one. The older device shows no
code, so the comparison cannot be made. Treat such a pairing as unverified, or
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
