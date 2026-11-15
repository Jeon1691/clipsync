# Security assurance case

Assessment date: 2026-11-15. Scope: this Rust workspace, its support scripts and
distribution workflows, based on v0.1.16 plus the changes linked from this
document. This is a maintainable claim/evidence argument, not a certification,
proof or independent audit. AI-assisted source review and automated tests alone do not establish human
review. The maintainer review recorded below supplies the separate human
assessment; it is not an independent audit.

## Requirements, assumptions and threats

The intended requirements are listed in [security.md](security.md): authorized
pairing; confidential, authenticated payloads against the relay/network;
rejection of invalid or stale messages; constrained file writes/resource use;
and private, replaceable local credentials. Availability against an arbitrary
hostile relay, endpoint compromise and forward secrecy for stored payloads are
outside the provided guarantees.

Assume an uncompromised authorized endpoint, trustworthy OS/crypto dependencies,
a correctly authenticated out-of-band pairing-code exchange, private local data
directories and correctly configured HTTPS/WSS for public transport. Attackers
may control the network, relay, malformed protocol/file inputs or a webhook
destination. An authorized malicious peer can intentionally send content; the
receiver still treats its filenames, manifests and sizes as untrusted. Relay
operators observe routing, size and timing metadata. Same-user malware and
administrators can access local clipboard contents and keys.

## Trust boundaries

| Boundary | Validation / evidence |
| --- | --- |
| User input → CLI/core | Typed command parsing, configuration validation, clipboard item limits; CLI/core smoke and end-to-end tests |
| Native clipboard/files → encrypted transfers | Clipboard item checks, protocol size limits, manifest construction; clipboard platform and format-matrix tests |
| Untrusted relay/network → transport/core | TLS certificate validation, typed messages, framing/size checks, AEAD authentication, pairing transcript/key confirmation; protocol fuzzing, crypto tests and relay end-to-end tests |
| Client → relay membership/routing | HMAC device token verification, expiry, room/device membership and revocation in relay auth/ws handlers; pairing/revocation/session-bound tests |
| Remote filename/transfer → local inbox | Allowlisted ULID staging IDs and safe basename rules, size/chunk/hash checks and staging confinement; transfer traversal and file round-trip regressions |
| Application → local storage/OS | Private Unix data/inbox directories and secret files, atomic credential writes, separated metadata/secret serialization; storage migration/failure/permission tests. Windows relies on profile ACLs |
| Local command → daemon | Per-user local IPC and OS service ownership; native platform tests. This is not a privilege boundary against same-user malware |
| Relay → optional webhook | HTTP(S) URL checks, public IPv4/IPv6 address policy applied to every DNS result, connection pinning, no redirects/proxies and delivery timeout; webhook DNS, mixed-answer, pinning and redirect regressions |
| Source/dependencies → distributed executable | Cargo checksums/locked builds, dependency audits, SHA/digest-pinned build inputs, CodeQL, required CI, signed source tags and verified artifact attestations |

## Design principles and implementation argument

- **Fail-safe defaults:** missing/invalid credentials fail; pairing confirmation
  failures do not establish room credentials; an absent relay signing secret is
  generated from OsRng, while an explicitly weak secret is rejected. Production
  clients default to HTTPS. Self-hosted relay HTTP must remain behind correctly
  configured TLS termination and firewall rules; the default listener does not
  itself establish a Gold secure-network-default claim.
- **Complete mediation:** token signature/expiry, current membership, sender
  role, frame constraints and destination checks occur on the corresponding
  access/routing paths. Webhook connection addresses are pinned to the validated
  DNS answers so a second lookup cannot bypass the policy.
- **Least privilege:** per-user daemon and private directories, a non-root
  relay container, read-only CI tokens except narrowly scoped release signing
  and publication. These reduce authority; binary hardening is assessed
  separately in [hardening.md](hardening.md).
- **Economy of mechanism / open design:** shared protocol/crypto crates and
  standard SPAKE2, X25519, HKDF/HMAC-SHA256, AES-GCM and Noise libraries avoid a
  bespoke cipher. Source and interfaces are public. Algorithm-suite agility is
  not implemented in v1; changing it requires protocol/compatibility review.
- **Separation of privilege / minimized sharing:** possession of relay access
  alone does not reveal the room secret; knowledge of the pairing code and
  successful transcript confirmation are needed. The two binding senders use
  disjoint nonce domains. Persisted device/webhook credentials are separated
  from general configuration and room metadata.
- **Psychological acceptability:** ordinary pairing, clear errors, private
  reporting and documented upgrade/verification reduce pressure to bypass TLS,
  warnings or release checks. User-facing translations and complete
  screen-reader verification remain outstanding.

## Common implementation weaknesses

| Weakness class | Mitigation and concrete evidence |
| --- | --- |
| Broken authentication/authorization (CWE-287/862) | SPAKE2 transcript HMAC confirmations and signed/expiring device tokens; relay membership/revocation checks; wrong-code/revocation end-to-end tests |
| Cryptographic misuse (CWE-323/327) | v0.1.16 separates binding nonces by sender; unit and observed-wire AEAD regressions; crypto fuzz target checks round trips and tamper rejection. Persistent epoch keys mean stored-payload PFS remains unavailable |
| Path traversal (CWE-22) | `safe_filename` and `safe_transfer_id`, platform reserved-name checks and staging-directory confinement; transfer tests and format-file end-to-end tests |
| SSRF (CWE-918) | Resolve and reject nonpublic DNS/literal destinations, pin accepted addresses, preserve URL host/SNI, disable redirects and inherited proxies; webhook synthetic DNS/address and redirect tests |
| Resource exhaustion / malformed input (CWE-400/20) | Typed Serde messages, frame/chunk/file limits, bounded channels, pairing TTL/attempts and transfer/session pruning; protocol fuzzing and stability/large-payload tests. Internet-scale DoS protection still needs deployment controls |
| Secret exposure (CWE-200/522/732) | No checked-in active example secret; separate private credential files, non-secret metadata serialization, atomic migration, no credential value in weak-key error; storage/config regressions. Debug/log output and backups require care |
| Memory corruption (CWE-119/787) | Predominantly safe Rust, native-path sanitizer testing and libFuzzer+ASan; no assertion of complete OS/FFI coverage. Released executables have the documented exploit-mitigation flags |
| Supply-chain compromise (CWE-1104/494) | Audit both lockfiles, monitor dependencies, pin CI inputs, verify release provenance before running/uploading archives; old unmaintained components remain transparently tracked |

## Findings, validation and residual work

The pairing GCM nonce collision and shared relay example key were corrected in
[v0.1.16](releases/v0.1.16.md). This assessment additionally led to DNS/IPv6
webhook destination checks and separated device/webhook credentials with legacy
migration. Regression tests first reproduced the relevant failures, then passed
with the fixes. Consult the corresponding pull request and hosted checks for
the final revision and platform results.

Test counts do not prove coverage. The [coverage workflow](testing.md) measures
the whole workspace, including uncovered desktop/mobile code. The Silver/Gold
coverage thresholds, full reproducible archives, human security review, access
continuity, and independent-review/contributor history remain separately
assessed in [the complete criterion register](best-practices-levels.md).

Before a human reviewer signs off this case, inspect the named authorization,
key lifecycle, input validation, file/native, webhook and release boundaries;
review failing/error paths and the documented assumptions. Record reviewer,
date, exact revision, findings, fixes/tests and accepted residual risks. Understanding secure-design principles alone is not a review record. The
maintainer confirmation below records the actual review of this candidate.

## Human review record

On **2026-11-15**, maintainer **Jeon1691** personally confirmed reviewing the
code and assurance case in [PR #16](https://github.com/Jeon1691/clipsync/pull/16),
source commit `d0501a74e4d1d02160ac113bf1f542731163221a`. The confirmation covered
authentication, key management, file paths, webhook destinations and release
trust boundaries, including the documented remaining risks. Findings and
regressions are recorded above and in the PR. The review uses the stated
security requirements and endpoint/deployment assumptions.

This is an internal human security/design review. It does not create another
maintainer, independent audit or second-person change-review history, and it
does not attest that test-coverage or reproducibility targets are met. Revisit
the case after material protocol, authorization or boundary changes.
