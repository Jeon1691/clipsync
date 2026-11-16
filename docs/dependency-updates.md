# Coordinated dependency updates

The remaining Dependabot changes share the workspace manifest and two independent
lockfiles. The `/fuzz` updates also change workspace requirements through the
local protocol and crypto packages. Both committed dependency graphs must be
resolved before running their locked builds; changing only one lockfile leaves
the other build unable to start.

| Original PR | Dependency | Selected version |
| --- | --- | --- |
| #4 | ULID | 3.0.0 |
| #9 | TOML | 1.2.1+spec-1.1.0 |
| #10 | Arboard | 3.7.0 |
| #11 | parking_lot | 0.13.0 |
| #12, #19 | X25519-Dalek | 3.0.0 |
| #13 | AES-GCM | 0.11.1 |
| #17 | Rand | 0.9.5 |
| #18 | HMAC | 0.13.0 |

HMAC's digest update also requires SHA-2 0.11 and HKDF 0.13 for the shared
SHA-256 types. ULID uses Rand 0.10.4 internally; application consumers retain
the requested Rand 0.9.5. The lockfiles retain older AES-GCM/SHA-2 for Snow and
older HKDF/HMAC/SHA-2 for SPAKE2 because those upstream versions still require
them. The frozen vendor test workspace is independent and remains unchanged.

## Compatibility evidence

The migration preserves protocol-v1 nonce lengths, authentication tag placement,
associated data, transcript bytes, KDF labels, static key encodings, fingerprint
format, token JSON, and identifier strings. Fixed wire fixtures run against both
the old and upgraded dependencies:

- OpenSSL AES-256-GCM ciphertext and raw X25519 public-key fixtures.
- Python HMAC/SHA-256 pairing-confirmation, token and fingerprint fixtures.
- Independently calculated ULID byte/Base32/JSON vectors, including the existing
  legacy identifier fallback.
- Real configuration-store TOML roundtrips and non-destructive malformed or
  duplicate-key rejection.

OS randomness still supplies keys. X25519 uses its OS-backed `random()` method;
epoch generation preserves failure on unavailable OS entropy, and relay startup
propagates that failure. Secret erasure, constant-time verification and all
native clipboard features remain enabled. Arboard retains `image-data` and
`wayland-data-control`. The Rust minimum stays at 1.95, matching parking_lot 0.13.

## Pairing persistence under load

Three concurrent copies of the real relay integration suite exposed a failure
also reproduced on the unchanged pre-upgrade source: synchronous keychain and
durable file writes held Tokio workers for 8–10 seconds. The eight-second
watchdog then closed an otherwise authenticated pairing connection.

Room-key and room/config persistence now run on blocking workers. Cloned pairing
keys retain `ZeroizeOnDrop` ownership, storage failures still propagate, and the
watchdog threshold stays unchanged. The same three-process workload passes after
this correction. Its regression evidence and platform limits are recorded in
the [regression audit](regression-audit.md).

Persistence is not a cancellation transaction: a blocking operation already
started can finish after its awaiting task is cancelled. Cancellation does not
roll back credentials already written. Success is reported only after the room
and configuration writes complete.

## Required verification

The protected integration PR must pass the existing 17 required checks, including
the three-platform test matrix, Rust 1.95 compatibility, CodeQL, audits, source
and native dependency checks, coverage and sanitizers. Product coverage remains
at least 90% lines/regions and 85% branches on each measured platform. Both real
fuzz targets and the independently reconstructed Linux build are also checked.

Useful local checks use the installed Rust 1.100 compiler explicitly:

```sh
cargo +1.100.0 metadata --locked --format-version 1
cargo +1.100.0 metadata --locked --manifest-path fuzz/Cargo.toml --format-version 1
cargo +1.100.0 test --workspace --all-targets --all-features --locked -- --test-threads=1
cargo +1.100.0 clippy --workspace --all-targets --locked -- -D warnings
cargo +1.95.0 check --workspace --all-targets --locked
cargo audit --deny unsound --deny yanked --deny unmaintained
cargo audit --file fuzz/Cargo.lock --deny unsound --deny yanked --deny unmaintained
cargo audit --file vendor/test-workspace.Cargo.lock --deny unsound --deny yanked --deny unmaintained
```

Native clipboard opt-ins belong on disposable hosts as described in
[testing](testing.md). Local private-pasteboard and file-based tests do not
establish Linux/Windows runtime or mobile device coverage.
