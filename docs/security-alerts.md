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
The workspace requires Rust 1.88, checked in CI, and fuzz jobs validate their
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

## Remaining maintenance advisories

These are informational unmaintained-package findings, not patched-version
vulnerability findings. They still contribute to Scorecard alert #25. The
2026-11-15 review found no released, compatible upgrade that removes all six while
preserving the current accessibility, window decorations, and mobile bindings.
Review again by **2026-12-11** and check weekly Dependabot PRs.

| Advisory | Dependency | Current dependency path |
| --- | --- | --- |
| [RUSTSEC-2026-0374](https://rustsec.org/advisories/RUSTSEC-2026-0374.html) | basic-toml | UniFFI → Askama's code generator |
| [RUSTSEC-2024-0436](https://rustsec.org/advisories/RUSTSEC-2024-0436.html) | paste | eframe's optional wgpu backend → wgpu-hal → metal; compile-time macro |
| [RUSTSEC-2026-0192](https://rustsec.org/advisories/RUSTSEC-2026-0192.html) | ttf-parser | eframe/winit → native font rendering |
| [RUSTSEC-2026-0371](https://rustsec.org/advisories/RUSTSEC-2026-0371.html) | zbus_names | eframe → AccessKit/AT-SPI on Linux |
| [RUSTSEC-2026-0370](https://rustsec.org/advisories/RUSTSEC-2026-0370.html) | zvariant | eframe → AccessKit/AT-SPI on Linux |
| [RUSTSEC-2026-0372](https://rustsec.org/advisories/RUSTSEC-2026-0372.html) | zvariant_derive | Linux accessibility stack's compile-time macros |

The follow-up checked these replacement paths against current upstream releases:

- **Binding generation:** [UniFFI 0.32.2](https://github.com/mozilla/uniffi-rs/blob/v0.32.2/uniffi_bindgen/Cargo.toml)
  still enables Askama's `config` feature. Released Askama 0.16.1 still uses
  `basic-toml` for that feature. Upstream
  [commit 3863808](https://github.com/askama-rs/askama/commit/3863808cf594054f7644979ed92caaafc878f378)
  replaces the parser with maintained `toml`, but the fix is not yet published.
  Recheck when a release containing this commit is compatible with UniFFI.
- **Window titles and fonts:** eframe 0.37 uses Skrifa for its own font parsing,
  but its winit 0.30 dependency still selects `sctk-adwaita` 0.10.1 with
  `ab_glyph`, which retains `ttf-parser`. The
  [winit 0.30.13 manifest](https://github.com/rust-windowing/winit/blob/v0.30.13/Cargo.toml)
  cannot select the newer `sctk-adwaita` Skrifa backend. Track
  [sctk-adwaita #99](https://github.com/PolyMeilex/sctk-adwaita/issues/99) and a
  compatible winit release that preserves title text.
- **Linux accessibility:** even the latest AccessKit Unix adapter, 0.24.1,
  [requires zbus 5.19](https://docs.rs/crate/accesskit_unix/0.24.1/source/Cargo.toml).
  The maintained replacement is zbus 6, which
  [incorporates the former standalone wire-format crates](https://rustsec.org/advisories/RUSTSEC-2026-0370.html).
  This needs an AccessKit/AT-SPI migration before ClipSync can adopt it.
- **Graphics macros:** resolving eframe 0.33.3 retained `metal` 0.32 and `paste`;
  an actual `cargo audit --json` still reported all six maintenance advisories,
  so that ineffective upgrade was reverted. eframe 0.34.3 and later move to a
  newer graphics stack and raise the compiler requirement; the latest
  [eframe 0.37 requires Rust 1.95](https://docs.rs/crate/eframe/0.37.0/source/Cargo.toml).
  A larger GUI migration solely to remove this one warning is deferred while
  the other upstream blockers remain. Rust 1.88 remains the tested minimum.

Both lockfiles remain audited without advisory ignores, package renames, or
locally maintained dependency forks. Native accessibility, title bars, image
formats, and mobile bindings remain enabled.

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
