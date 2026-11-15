# GitHub security alert remediation

Reviewed on 2026-11-11 against `master` commit
`02da013d3ef357357eeb42e6d4762b3dce3b36e7`.

## Changes

| GitHub alerts | Remediation |
| --- | --- |
| Dependabot #1–4 | Require rustls 0.23.46 and update the lockfile. |
| Code Scanning #1, #3, #4 | Default workflow tokens to read-only; grant release publishing write access only in the publishing job. |
| Code Scanning #5 | Configure weekly Dependabot updates for Cargo, the fuzz workspace, Actions, and Docker. |
| Code Scanning #6–18 | Pin Actions to upstream commit SHAs and both Docker images to multi-platform manifest digests. Build the container with `--locked`. |
| Code Scanning #20 | Add a security reporting policy and link it from the security model. Private vulnerability reporting is enabled on GitHub. |
| Code Scanning #24 | Add a cargo-fuzz target for binary chunk and JSON control-message decoding, with seed inputs and scheduled/PR CI runs. |
| Code Scanning #25, vulnerability findings | Remove the vulnerable quick-xml dependency through eframe 0.32.3; update rustls, rustls-webpki, webpki-roots, and webpki-root-certs. Update Tokio and rustix for memory-safety advisories, replace rustls-pemfile with rustls's PEM API, and replace yanked lockfile entries. |

The 2026-11-15 follow-up also patches RUSTSEC-2026-0523 and RUSTSEC-2026-0524
with quinn-proto 0.11.19, and RUSTSEC-2026-0538 with quinn-udp 0.5.16.
The v0.2.0 workspace requires Rust 1.95, checked in CI, and fuzz jobs validate their
lockfile with `cargo metadata --locked` before building.

The security workflow audits both committed lockfiles. Vulnerabilities, unsound
code advisories, and yanked packages fail the audit job. Unmaintained-package
warnings remain visible; they are not suppressed with an advisory ignore list.

## Pairing and deployment follow-up

The 2026-11-15 Best Practices review found two additional issues in the defaults:

- Both pairing participants encrypted their device-binding messages with the
  same SPAKE2 wrapping key and nonce. Sender A now derives nonce index 2 and
  sender B index 3; both avoid legacy index 1. Tests reproduce the original
  collision, observe real pairing traffic, and check both directions of a
  legacy/new v1 wire exchange. Key derivation and receiver framing are unchanged.
- Root Docker Compose supplied a public example token-signing key. Compose now
  forwards an optional configured value; absent, empty, and whitespace-only
  values use an OS-generated 32-byte key. Explicit nonblank environment keys
  shorter than 32 bytes fail without printing the value. Examples no longer
  provide a shared key.

These fixes were published in v0.1.16. Upgrade both clients and re-pair to replace
keys from an earlier handshake. Self-hosted relay operators should rebuild,
remove or rotate any example key, restart, and re-pair their clients. A binary
update does not retroactively protect an old handshake. See the
[release notes](releases/v0.1.16.md) and [key rotation](self-hosting.md#token-keys-and-rotation).

## Maintenance-advisory migration

The v0.2.0 migration removes all six former informational maintenance findings:
`basic-toml`, `paste`, `ttf-parser`, `zbus_names`, `zvariant`, and
`zvariant_derive`. It upgrades eframe to 0.37 and UniFFI to 0.32.2, selects
winit's existing Crossfont Wayland title renderer, backports Askama's upstream
TOML-parser fix, and migrates the AccessKit/AT-SPI source stack to maintained
zbus 6.3 APIs. Accessibility, native decorations/title text, both graphics
backends, clipboard formats, and mobile bindings remain enabled.

Both real lockfile audits now report zero vulnerabilities and no warnings,
without advisory ignores. This is not solely a released-dependency update:
seven explicit compatibility source forks are maintained by ClipSync. Their
pinned original sources, complete local diffs, upstream licenses, ownership,
weekly review policy, and retirement conditions are recorded in
[vendor/README.md](../vendor/README.md). Local source patches require manual
security review because registry advisories do not fully assess modified code.

See [dependency migration](dependency-migration.md) for each replacement path,
actual Rust 1.95 minimum, upstream-source evidence, behavior tests, and remaining
platform verification limits. The first patch-retirement review is due
**2026-12-11**. Weekly dependency checks and both lockfile audits remain in place.

## Compiler-warning review and strict checks

The earlier Rust 1.100 review found 113 distinct warning locations across the
three supported platforms. The follow-up fixes practical style, platform
selection, dependency and metadata warnings and makes Clippy warnings errors in
CI. Deliberate public-error and queue layouts retain narrow, reasoned lint
expectations instead of broad suppressions or compatibility-breaking boxing.
See [compiler warnings](compiler-warnings.md) for the current policy and
historical inventory, and the final hosted CI results for platform verification.

## Silver/Gold review follow-up

The higher-tier review additionally fixes webhook DNS/IPv6 destination bypasses
and separates device/webhook credentials from metadata, with private atomic
migration and startup error preservation. See the [v0.1.17 release notes](releases/v0.1.17.md),
[assurance case](assurance-case.md), and [complete criterion register](best-practices-levels.md).
The current public Best Practices entry has earned Passing; Silver/Gold are not
claimed until every required criterion has evidence.
