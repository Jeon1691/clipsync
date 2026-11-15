# OpenSSF Best Practices passing assessment worksheet

Prepared on **2026-11-15** against local commit
`000d5151d1aabf59d259b4b608e1a4f7a6a3b1e9` and the accompanying
[contribution guide](../CONTRIBUTING.md). This is an evidence worksheet for the
maintainer, not a completed self-assessment or a claim that a badge was earned.
Evidence from this branch becomes a public assessment URL only after it is
published in the repository. Recheck the exact release and default-branch state
when submitting answers.

## Registration state and prepared project data

The public [exact repository URL query](https://www.bestpractices.dev/en/projects.json?url=https%3A%2F%2Fgithub.com%2FJeon1691%2Fclipsync)
returned HTTP 200 with `[]` on 2026-11-15. Prefix searches for `ClipSync` and
`Jeon1691` also returned `[]`. No matching public project entry was found;
there is no project ID or earned badge to insert into the README. The
[official API documentation](https://github.com/ossf/best-practices-badge/blob/main/docs/api.md)
defines the URL query and the public project/badge JSON interfaces.

| Field | Prepared value / evidence |
| --- | --- |
| Project name | ClipSync |
| Description | End-to-end encrypted clipboard synchronization for macOS, Linux, and Windows, with a CLI, native desktop app, and WebSocket relay. Text, images, and files can be shared between paired devices. |
| Homepage URL | `https://github.com/Jeon1691/clipsync` |
| Repository URL | `https://github.com/Jeon1691/clipsync` |
| License | MIT; [LICENSE-MIT](../LICENSE-MIT) and the workspace manifest. |
| Implementation languages | Rust for the workspace; Python, shell, and PowerShell support scripts. Review the form's scope before entering this field. |
| Public issue/discussion archive | [Issues](https://github.com/Jeon1691/clipsync/issues) and [pull requests](https://github.com/Jeon1691/clipsync/pulls). |
| Contribution requirements | [CONTRIBUTING.md](../CONTRIBUTING.md), after publication. |
| Vulnerability reporting | [SECURITY.md](../SECURITY.md); [private advisory form](https://github.com/Jeon1691/clipsync/security/advisories/new). |
| Releases | [GitHub releases](https://github.com/Jeon1691/clipsync/releases). Latest public release observed: `v0.1.14`. |
| Account/project owner, contact information, terms and data-license agreement | Owner must supply or confirm these through their own account. No personal email or credentials belong in this worksheet. |
| CPE or other optional identifiers | Leave unset unless an existing, accurate identifier is confirmed. |

The owner can use the [project registration page](https://www.bestpractices.dev/en/projects/new)
after signing in. This work does not create an account, enroll the project,
accept terms, authorize GitHub access, or submit assessment answers.

## How to use the worksheet

The [official passing criteria](https://www.bestpractices.dev/en/criteria/0?details=true&rationale=true)
define each ID below; credit for the criteria belongs to the OpenSSF Best
Practices badge contributors. The [badge rules](https://www.bestpractices.dev/en/criteria_discussion#achieving-a-badge)
require every MUST/MUST NOT to be satisfied, every SHOULD to be satisfied or
justified when unmet, and every SUGGESTED criterion to be considered. N/A is
available only where the criterion permits it, with justification where required.
The passing level accommodates single-person projects; independent review and
multiple organizations are separate Scorecard concerns, not automatic blockers
for this badge.

The worksheet uses **Evidence** for a supported candidate answer, **Verify** for
an answer needing an owner check or further technical review, **Gap** for a
specific missing requirement, and **Considered** for an optional improvement
without evidence to claim it is met. These labels are not submitted badge
answers. For each final Met answer, use a public URL and justification when the
form requires them. Prefer commit-specific links for historical evidence.

## Basics

| Criterion ID | Level | Worksheet status and evidence / remaining action |
| --- | --- | --- |
| `description_good` | MUST | Evidence: [README](../README.md) explains clipboard sync and supported platforms. |
| `interact` | MUST | Evidence: README installation instructions and GitHub's discoverable [contribution guide](../CONTRIBUTING.md) cover obtaining the software, issues, and PRs after this guide is published. |
| `contribution` | MUST | Evidence: contribution guide describes forks, branches, PRs, discussion, CI, and maintainer acceptance; supply its public URL. |
| `contribution_requirements` | SHOULD | Evidence: contribution guide specifies formatting, warning handling, tests, dependency consistency, documentation, and security-sensitive changes; supply its public URL. |
| `floss_license` | MUST | Evidence: [MIT license](../LICENSE-MIT), also declared in [Cargo.toml](../Cargo.toml). |
| `floss_license_osi` | SUGGESTED | Evidence: MIT is in the official criteria's FLOSS examples and is [OSI approved](https://opensource.org/license/mit). |
| `license_location` | MUST | Evidence: root-level `LICENSE-MIT`; supply its public URL. |
| `documentation_basics` | MUST | Evidence: README installation, startup, commands, and environment; [security model](security.md) and [self-hosting](self-hosting.md) describe security boundaries and TLS deployment. |
| `documentation_interface` | MUST | Evidence: [public interface reference](interfaces.md) describes CLI inputs/outputs, configuration, exit codes, relay HTTP/WebSocket authentication and schemas, webhooks, and exported mobile methods/callbacks. It was checked against source, all 25 CLI help paths, eight isolated client output/error cases, and eight loopback relay HTTP cases. [Protocol](protocol.md) and [mobile documentation](mobile.md) link to it. |
| `sites_https` | MUST | Evidence: repository, release downloads, and documented public relay use HTTPS. Recheck any additional project/download sites entered in the form. |
| `discussion` | MUST | Evidence: public GitHub issues and PR discussions have searchable, stable URLs and browser access. |
| `english` | SHOULD | Evidence: English README/docs and contribution guide explicitly welcome English reports and code comments. |
| `maintained` | MUST | Verify: recent commits, PRs, and security remediation show activity; owner confirms ongoing handling of significant problem and vulnerability reports. This badge criterion does not impose Scorecard's 90-day repository-age test. |

## Change control

| Criterion ID | Level | Worksheet status and evidence / remaining action |
| --- | --- | --- |
| `repo_public` | MUST | Evidence: [public source repository](https://github.com/Jeon1691/clipsync). |
| `repo_track` | MUST | Evidence: public Git history records changes, attribution, and timestamps. |
| `repo_interim` | MUST | Evidence: development commits and PRs are available between releases. |
| `repo_distributed` | SUGGESTED | Evidence: Git repository. |
| `version_unique` | MUST | Evidence: workspace package version and unique `v0.1.x` release tags. |
| `version_semver` | SUGGESTED | Evidence: three-part `0.1.x` release identifiers; confirm intended compatibility semantics rather than promising stable pre-1.0 APIs. |
| `version_tags` | SUGGESTED | Evidence: [tags](https://github.com/Jeon1691/clipsync/tags) identify releases. |
| `release_notes` | MUST | Evidence: [curated summaries](releases/v0.1.14.md) now cover v0.1.0–v0.1.14, including significant changes and upgrade impact. Their contents were applied to the existing GitHub release bodies and verified through the API on 2026-11-15. Future publication requires a matching notes file; v0.1.15 notes are prepared. See [v0.1.14](https://github.com/Jeon1691/clipsync/releases/tag/v0.1.14). |
| `release_notes_vulns` | MUST | Verify: owner checks whether any publicly identified runtime vulnerability in ClipSync itself was fixed in each release and documents its identifier. N/A requires confirmation that the criterion's conditions apply; dependency advisories are outside this particular criterion's scope. |

## Reporting

| Criterion ID | Level | Worksheet status and evidence / remaining action |
| --- | --- | --- |
| `report_process` | MUST | Evidence: contribution guide directs users to GitHub issues and specifies useful report details; supply its public URL. |
| `report_tracker` | SHOULD | Evidence: GitHub Issues is enabled and linked in the guide. |
| `report_responses` | MUST | Verify: public [issues API](https://api.github.com/repos/Jeon1691/clipsync/issues?state=all&per_page=100) returned only PRs during this review, with no ordinary bug-report issues. Owner must check all report channels and apply the required 2–12 month window honestly; do not manufacture reports or count dependency-bot PRs as user bug reports. |
| `enhancement_responses` | SHOULD | Verify: check enhancement requests and their responses across the same 2–12 month window; no ordinary enhancement issue was found in the public API response. Explain the actual history in the form. |
| `report_archive` | MUST | Evidence: public issues and PR discussions preserve reports and responses; supply the issue archive URL. |
| `vulnerability_report_process` | MUST | Evidence: [security policy](../SECURITY.md) explains reporting and a safe fallback when private reporting is unavailable; supply its public URL. |
| `vulnerability_report_private` | MUST | Evidence: security policy links GitHub's private advisory form and asks users to exclude secrets; owner verifies private reporting remains enabled. Supply the public policy URL, never a confidential report. |
| `vulnerability_report_response` | MUST | Verify: owner checks every private/public vulnerability report received in the past six months for an initial response within 14 days. N/A is available if no such reports were received. An empty [public advisory list](https://api.github.com/repos/Jeon1691/clipsync/security-advisories) cannot establish the absence of private reports. |

## Quality

| Criterion ID | Level | Worksheet status and evidence / remaining action |
| --- | --- | --- |
| `build` | MUST | Evidence: Cargo workspace and reproducible commands in the contribution guide and [CI workflow](../.github/workflows/ci.yml). Confirm successful builds for the commit entered in the form. |
| `build_common_tools` | SUGGESTED | Evidence: Rust/Cargo and platform-native build dependencies. |
| `build_floss_tools` | SHOULD | Verify: Linux CI builds use Rust/Cargo and packaged open-source libraries. Check the assessment scope and document any proprietary SDK/tool requirement for macOS/Windows/mobile instead of claiming every platform uses only FLOSS tools. |
| `test` | MUST | Evidence: public MIT-licensed Rust tests and the documented `cargo test` commands; Linux headless requirements are included. |
| `test_invocation` | SHOULD | Evidence: standard Cargo test invocation. |
| `test_most` | SUGGESTED | Considered: unit, integration, platform, and format tests exist; branch/input coverage has not been measured. Do not infer broad coverage from the passing test count. |
| `test_continuous_integration` | SUGGESTED | Evidence: pull-request/push CI on Linux, macOS, and Windows. |
| `test_policy` | MUST | Evidence: contribution guide requires automated tests for major new functionality. |
| `tests_are_added` | MUST | Verify: [format-transfer tests](https://github.com/Jeon1691/clipsync/commit/448917a) and [stability changes](https://github.com/Jeon1691/clipsync/commit/61bbfcc) provide examples. Check the most recent major feature changes and cite representative implementation-plus-test commits. |
| `tests_documented_added` | SUGGESTED | Evidence: contribution guide records the test policy. |
| `warnings` | MUST | Evidence: compiler diagnostics and `cargo clippy --workspace --all-targets --locked` run in CI. |
| `warnings_fixed` | MUST | Verify: contribution guide requires new warnings to be resolved or targeted exceptions justified. Inspect current compiler/Clippy output and existing exceptions; a successful command alone does not show the warning backlog is addressed. |
| `warnings_strict` | SUGGESTED | Considered: CI runs Clippy, but does not currently pass `-D warnings`. Assess practical strictness and record the current answer accurately. |

## Security

| Criterion ID | Level | Worksheet status and evidence / remaining action |
| --- | --- | --- |
| `know_secure_design` | MUST | Verify: a primary developer must personally confirm the secure-design knowledge described in the official details. A threat model or generated worksheet cannot attest someone's understanding. |
| `know_common_errors` | MUST | Verify: a primary developer must confirm knowledge of relevant errors and mitigations, including malicious frames, replay, file-path traversal, authorization, secret handling, and resource exhaustion. |
| `crypto_published` | MUST | Verify: [crypto crate](../crates/clipsync-crypto/src/lib.rs) uses SPAKE2, Noise, X25519, AES-GCM, SHA-256, HMAC, and HKDF. Review the published protocol/algorithm references and project-specific composition against the criterion; library names do not prove the complete protocol was independently reviewed. |
| `crypto_call` | SHOULD | Evidence: specialized Rust crates implement cryptographic primitives; project code composes them for pairing, wrapping, and transfer encryption. Review changes to that composition. |
| `crypto_floss` | MUST | Evidence: cryptographic functions use publicly available Rust implementations listed in the manifest/lockfile. Confirm their licenses and any platform-specific dependency before submission. |
| `crypto_keylength` | MUST | Evidence: 32-byte symmetric keys and X25519 keys in [key derivation](../crates/clipsync-crypto/src/kdf.rs), [AEAD](../crates/clipsync-crypto/src/aead.rs), and [identity](../crates/clipsync-crypto/src/identity.rs). Verify all default paths and any configurable alternatives against the criterion's minimums. |
| `crypto_working` | MUST | Verify: inspected paths use authenticated AES-GCM and modern primitives. Review the full default security mechanism, including TLS and algorithm configuration, before asserting the absence of broken algorithms or inappropriate modes. |
| `crypto_weaknesses` | SHOULD | Verify: no weak mechanism was identified in the inspected crypto paths; confirm the complete default dependency/configuration scope. |
| `crypto_pfs` | SHOULD | Verify / limitation: the Noise handshake uses ephemeral keys, but payloads continue to use room epoch keys. [Epoch wrapping](../crates/clipsync-crypto/src/epoch.rs) uses an ephemeral sender key and the recipient's long-term identity; a recorded wrap can be decrypted with that recipient secret. Do not claim universal forward secrecy. Review the intended scope and explain the limitation if answering Unmet. |
| `crypto_password_storage` | MUST | Verify N/A candidate: no persistent external-user password database was identified; pairing uses a transient code and SPAKE2, while stored device credentials are keys/tokens. Confirm every inbound-authentication path before selecting N/A. |
| `crypto_random` | MUST | Verify: identity/epoch generation uses `OsRng`; [nonce derivation](../crates/clipsync-crypto/src/kdf.rs) uses deterministic HKDF per content key and chunk index. Review all randomness sources, derived-key/nonce uniqueness, and the criterion's exact wording instead of describing every nonce as directly sampled from an RNG. |
| `delivery_mitm` | MUST | Evidence: GitHub releases and documented installation downloads use HTTPS. [Packaging documentation](packaging.md) adds signature verification for future attested archives; historical releases remain unsigned. |
| `delivery_unsigned` | MUST | Evidence: documented archive/checksum retrieval uses HTTPS. Localhost HTTP relay examples are development traffic, not a checksum distribution mechanism. Recheck all package/download paths. |
| `vulnerabilities_fixed_60_days` | MUST | Verify: inspect both current audits and the dependencies/code in the latest published release. [Security remediation](security-alerts.md) includes fixes on the development branch; a patch on an unpublished branch does not establish that affected users have a released fix. Check disclosure dates and severity for any applicable vulnerability. |
| `vulnerabilities_critical_fixed` | SHOULD | Verify: owner confirms critical-report history and remediation timing. Current audit configuration is evidence of monitoring, not proof of historical response times. |
| `no_leaked_credentials` | MUST | Verify: review the public repository and history, and any secret-scanning findings. No broad secret-history audit or owner attestation was performed by this worksheet. Never paste suspected credentials into the assessment. |

## Analysis

| Criterion ID | Level | Worksheet status and evidence / remaining action |
| --- | --- | --- |
| `static_analysis` | MUST | Verify: Clippy is a FLOSS static analyzer beyond rustc diagnostics; CI runs it and GitHub CodeQL is configured. Record successful analysis of the proposed major production release before publishing, its tool/configuration, and result. Configuration alone is not historical release evidence. |
| `static_analysis_common_vulnerabilities` | SUGGESTED | Evidence: CodeQL security analysis is configured. Confirm successful Rust analysis and the actual query suite/results for the release under assessment. |
| `static_analysis_fixed` | MUST | Verify: review confirmed exploitable findings and fix timing, including private findings; use N/A only if its conditions apply. Merely hiding or dismissing an alert does not demonstrate remediation. |
| `static_analysis_often` | SUGGESTED | Evidence: Clippy runs on pull requests and pushes to `master`/`main`; verify CodeQL execution history for its configured events. |
| `dynamic_analysis` | SUGGESTED | Evidence: [cargo-fuzz target](../fuzz/README.md) runs on PRs, mainline pushes, and a weekly schedule. Preserve successful fuzz evidence for a proposed major release before publishing. |
| `dynamic_analysis_unsafe` | SUGGESTED | Verify: Rust code includes native FFI/unsafe paths, notably macOS clipboard access. Existing decoder fuzzing does not exercise those platform paths. Determine applicability and sanitizer/assertion coverage; do not automatically claim N/A because the workspace is Rust. |
| `dynamic_analysis_enable_assertions` | SUGGESTED | Evidence: Cargo's test profile enables assertions; protocol fuzzing explicitly asserts round-trip behavior in [its target](../fuzz/fuzz_targets/protocol.rs). |
| `dynamic_analysis_fixed` | MUST | Verify: review confirmed exploitable fuzz/dynamic-analysis findings and fix timing; use N/A only when allowed and true. A green fuzz run cannot establish that no private finding exists. |

## Concrete remaining work

1. Publish the contribution guide and this evidence worksheet. Verify the
   published interface reference against the commit being assessed.
2. Publish the curated [historical release summaries](releases/) to their
   corresponding existing GitHub release bodies and verify the result; this
   external publication remains pending. Check whether any project vulnerability
   identifiers belong in those notes.
3. Validate release-candidate builds, tests, Clippy/CodeQL, dependency audits, and
   fuzzing; inspect warnings and confirmed security findings. Check whether
   security fixes on the development branch need a release for existing users.
   Retain links to the actual successful runs for the assessed release.
4. Have the primary developer review secure-design knowledge, cryptographic
   composition/nonce handling, and the forward-secrecy limitation. Record
   accurate answers and justified SHOULD exceptions; address any unmet MUST.
5. The owner checks all bug/enhancement channels and private vulnerability
   reports for the required response history, and confirms credential hygiene.
   Record aggregate facts and public evidence without exposing reports or
   personal contact details.
6. The owner signs in to Best Practices, searches for an existing entry again,
   creates or claims the exact project if needed, reviews the site's terms and
   permissions, and completes every criterion using verified evidence. These
   account and self-attestation steps have not been performed.
7. Once the site actually reports an earned badge, record its project ID and
   assessment URL. Query its project/badge JSON to verify the result before
   adding the official badge or expecting Scorecard to recognize it.

This worksheet does not promise a Scorecard increase. The Best Practices badge
is a separate self-assessment, and Scorecard rechecks published project state on
its own schedule.
