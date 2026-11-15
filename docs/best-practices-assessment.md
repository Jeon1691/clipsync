# OpenSSF Best Practices passing assessment worksheet

Passing was earned on **2026-11-15** in [project 16709](https://www.bestpractices.dev/en/projects/16709), with all required criteria satisfied. The tables below retain the original evidence worksheet and its historical review labels. The public entry is authoritative for current answers. See [the Silver/Gold register](best-practices-levels.md) for the complete higher-tier assessment and remaining requirements.

## Registration state and prepared project data

The initial public exact-repository lookup returned no entry. After the
maintainer completed GitHub login/account linking, [project 16709](https://www.bestpractices.dev/en/projects/16709)
was registered for `https://github.com/Jeon1691/clipsync` on 2026-11-15.
The assessment earned Passing (100%) on 2026-11-15; consult the entry for current
answers and badge state. The
[official API documentation](https://github.com/ossf/best-practices-badge/blob/main/docs/api.md)
defines the public project/badge interfaces.

| Field | Prepared value / evidence |
| --- | --- |
| Project name | ClipSync |
| Description | End-to-end encrypted clipboard synchronization for macOS, Linux, and Windows, with a CLI, native desktop app, and WebSocket relay. Text, images, and files can be shared between paired devices. |
| Homepage URL | `https://github.com/Jeon1691/clipsync` |
| Repository URL | `https://github.com/Jeon1691/clipsync` |
| License | MIT; [LICENSE-MIT](../LICENSE-MIT) and the workspace manifest. |
| Implementation languages | Rust for the workspace; Python, shell, PowerShell, and Ruby support/packaging scripts. Review the form's scope before entering this field. |
| Public issue/discussion archive | [Issues](https://github.com/Jeon1691/clipsync/issues) and [pull requests](https://github.com/Jeon1691/clipsync/pulls). |
| Contribution requirements | [CONTRIBUTING.md](../CONTRIBUTING.md), after publication. |
| Vulnerability reporting | [SECURITY.md](../SECURITY.md); [private advisory form](https://github.com/Jeon1691/clipsync/security/advisories/new). |
| Releases | [GitHub releases](https://github.com/Jeon1691/clipsync/releases). The v0.1.16 security release is published; newer release candidates are described in their release notes. |
| Account/project owner, contact information, terms and data-license agreement | The owner completed GitHub account linking directly. No personal email or credentials belong in this worksheet. |
| CPE or other optional identifiers | Leave unset unless an existing, accurate identifier is confirmed. |

The [registered assessment](https://www.bestpractices.dev/en/projects/16709)
contains the verified evidence and the maintainer's reporting-history and
secure-development confirmations. Required items still under validation remain
unknown until their evidence is complete; the site determines the earned state.

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
| `license_location` | MUST | Evidence: root-level `LICENSE` mirrors the existing MIT text in `LICENSE-MIT` without changing copyright or permissions. The standard filename also supports automated assessment discovery. |
| `documentation_basics` | MUST | Evidence: README installation, startup, commands, and environment; [security model](security.md) and [self-hosting](self-hosting.md) describe security boundaries and TLS deployment. |
| `documentation_interface` | MUST | Evidence: [public interface reference](interfaces.md) describes CLI inputs/outputs, configuration, exit codes, relay HTTP/WebSocket authentication and schemas, webhooks, and exported mobile methods/callbacks. It was checked against source, all 25 CLI help paths, eight isolated client output/error cases, and eight loopback relay HTTP cases. [Protocol](protocol.md) and [mobile documentation](mobile.md) link to it. |
| `sites_https` | MUST | Evidence: repository, release downloads, and documented public relay use HTTPS. Recheck any additional project/download sites entered in the form. |
| `discussion` | MUST | Evidence: public GitHub issues and PR discussions have searchable, stable URLs and browser access. |
| `english` | SHOULD | Evidence: English README/docs and contribution guide explicitly welcome English reports and code comments. |
| `maintained` | MUST | Evidence: recent releases, security remediation, PRs, CI, and weekly dependency maintenance demonstrate current activity; no bug or enhancement reports were received in the assessed window, as confirmed by the maintainer. |

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
| `release_notes` | MUST | Evidence: [curated summaries](releases/v0.1.14.md) now cover v0.1.0–v0.1.14, including significant changes and upgrade impact. Their contents were applied to the existing GitHub release bodies and verified through the API on 2026-11-15. v0.1.15 was published using its curated notes; future publication requires a matching notes file, and v0.1.16 notes are prepared. See [v0.1.14](https://github.com/Jeon1691/clipsync/releases/tag/v0.1.14). |
| `release_notes_vulns` | MUST | Evidence for N/A: no runtime vulnerability in ClipSync itself had a CVE or similar identifier when these releases were created. The criterion excludes dependency advisories; those fixes remain described in release notes. |

## Reporting

| Criterion ID | Level | Worksheet status and evidence / remaining action |
| --- | --- | --- |
| `report_process` | MUST | Evidence: contribution guide directs users to GitHub issues and specifies useful report details; supply its public URL. |
| `report_tracker` | SHOULD | Evidence: GitHub Issues is enabled and linked in the guide. |
| `report_responses` | MUST | Evidence: the maintainer confirmed no bug reports through other channels in the required 2–12 month window. The public issues API review also found no ordinary bug reports; no response history is fabricated. |
| `enhancement_responses` | SHOULD | Evidence: the maintainer confirmed no enhancement requests through other channels in the required 2–12 month window; no ordinary enhancement issues were found in the public archive. |
| `report_archive` | MUST | Evidence: public issues and PR discussions preserve reports and responses; supply the issue archive URL. |
| `vulnerability_report_process` | MUST | Evidence: [security policy](../SECURITY.md) explains reporting and a safe fallback when private reporting is unavailable; supply its public URL. |
| `vulnerability_report_private` | MUST | Evidence: security policy links GitHub's private advisory form and asks users to exclude secrets; owner verifies private reporting remains enabled. Supply the public policy URL, never a confidential report. |
| `vulnerability_report_response` | MUST | Evidence: the maintainer confirmed no prior external vulnerability reports on 2026-11-15. The internal findings discovered later in this review were acknowledged immediately and addressed in the same session; retain the v0.1.16 release evidence. |

## Quality

| Criterion ID | Level | Worksheet status and evidence / remaining action |
| --- | --- | --- |
| `build` | MUST | Evidence: Cargo workspace and reproducible commands in the contribution guide and [CI workflow](../.github/workflows/ci.yml). Confirm successful builds for the commit entered in the form. |
| `build_common_tools` | SUGGESTED | Evidence: Rust/Cargo and platform-native build dependencies. |
| `build_floss_tools` | SHOULD | Evidence: Linux CI builds the workspace with Rust/Cargo and FLOSS native dependencies. This does not claim that every supported platform SDK is FLOSS or assess separate native-app repositories. |
| `test` | MUST | Evidence: public MIT-licensed Rust tests and the documented `cargo test` commands; Linux headless requirements are included. |
| `test_invocation` | SHOULD | Evidence: standard Cargo test invocation. |
| `test_most` | SUGGESTED | Considered: unit, integration, platform, and format tests exist; branch/input coverage has not been measured. Do not infer broad coverage from the passing test count. |
| `test_continuous_integration` | SUGGESTED | Evidence: pull-request/push CI on Linux, macOS, and Windows. |
| `test_policy` | MUST | Evidence: contribution guide requires automated tests for major new functionality. |
| `tests_are_added` | MUST | Evidence: [pasteboard/sidecar tests](https://github.com/Jeon1691/clipsync/commit/399c845), [stability/boundary tests](https://github.com/Jeon1691/clipsync/commit/61bbfcc), desktop join-code tests, and the new pairing/relay regressions accompany major changes. |
| `tests_documented_added` | SUGGESTED | Evidence: contribution guide records the test policy. |
| `warnings` | MUST | Evidence: compiler diagnostics and `cargo clippy --workspace --all-targets --locked` run in CI. |
| `warnings_fixed` | MUST | Evidence: [three-platform warning review](compiler-warnings.md) records all 113 distinct locations, OS-specific counts, causes, accepted API/performance tradeoffs, and tracked cleanup. These are reviewed warnings, not blanket false positives; new diagnostics require review. |
| `warnings_strict` | SUGGESTED | Evidence: CI now enforces Clippy `-D warnings` after practical cleanup and narrow documented exceptions; see [the current warning policy](compiler-warnings.md). |

## Security

| Criterion ID | Level | Worksheet status and evidence / remaining action |
| --- | --- | --- |
| `know_secure_design` | MUST | Evidence: the solo primary developer personally confirmed understanding of secure design principles on 2026-11-15. |
| `know_common_errors` | MUST | Evidence: the primary developer confirmed knowledge of relevant vulnerability classes and mitigations, including authentication/authorization, replay, path traversal, secret handling, and resource exhaustion. |
| `crypto_published` | MUST | Evidence: default primitives/protocols are publicly published and reviewed: SPAKE2 (RFC 9382), Noise, AES-GCM (NIST SP 800-38D), X25519 (RFC 7748), HKDF (RFC 5869), HMAC/SHA-256, and BLAKE3. The criterion does not require an independent audit of this entire project; none is claimed. |
| `crypto_call` | SHOULD | Evidence: specialized Rust crates implement cryptographic primitives; project code composes them for pairing, wrapping, and transfer encryption. Review changes to that composition. |
| `crypto_floss` | MUST | Evidence: cryptographic functions use publicly available Rust implementations listed in the manifest/lockfile. Confirm their licenses and any platform-specific dependency before submission. |
| `crypto_keylength` | MUST | Evidence for the v0.1.16 candidate: AES/HKDF/HMAC use 256-bit keys, X25519/Ed25519 provide modern elliptic-curve security, TLS defaults use modern AEAD, and explicitly configured relay keys require at least 32 bytes. Validate and publish the candidate before submitting a final Met answer. |
| `crypto_working` | MUST | Evidence for the v0.1.16 candidate: sender-specific pairing nonces fix the discovered GCM misuse while preserving v1 framing. Primitive/default-path review and legacy/new wire tests found no other nonce conflict. Submit Met only after the candidate passes and is released. |
| `crypto_weaknesses` | SHOULD | Evidence for the v0.1.16 candidate: the known pairing nonce misuse and shared Compose key are corrected; no broken default primitive or mode was identified in the reviewed paths. Full forward secrecy remains a separately documented limitation. |
| `crypto_pfs` | SHOULD | Verify / limitation: the Noise handshake uses ephemeral keys, but payloads continue to use room epoch keys. [Epoch wrapping](../crates/clipsync-crypto/src/epoch.rs) uses an ephemeral sender key and the recipient's long-term identity; a recorded wrap can be decrypted with that recipient secret. Do not claim universal forward secrecy. Review the intended scope and explain the limitation if answering Unmet. |
| `crypto_password_storage` | MUST | Evidence for N/A: there is no external-user password database. Pairing uses transient SPAKE2 codes and device authentication uses signed tokens; OS credential storage is outbound/device-secret storage. |
| `crypto_random` | MUST | Evidence for the v0.1.16 candidate: default identity, room, token, SPAKE2, and Noise randomness comes from OS-seeded CSPRNGs. HKDF derives keys/nonces from those secrets with separate role/chunk domains. Compose follows the random-key default, and explicit nonblank environment keys shorter than 32 bytes are rejected. Operators must generate and protect the entropy of explicitly supplied keys. |
| `delivery_mitm` | MUST | Evidence: GitHub releases and documented installation downloads use HTTPS. [Packaging documentation](packaging.md) adds signature verification for future attested archives; historical releases remain unsigned. |
| `delivery_unsigned` | MUST | Evidence: documented archive/checksum retrieval uses HTTPS. Localhost HTTP relay examples are development traffic, not a checksum distribution mechanism. Recheck all package/download paths. |
| `vulnerabilities_fixed_60_days` | MUST | Evidence after publication: v0.1.15 releases the dependency fixes; v0.1.16 addresses the pairing/default-key findings discovered during this review. Both lockfiles report zero vulnerability findings. Confirm the v0.1.16 release and its successful audit before submitting Met. |
| `vulnerabilities_critical_fixed` | SHOULD | Evidence after publication: the maintainer reports no prior vulnerability reports, and the newly discovered pairing/default-key issues are fixed and released in the same review session. Record the actual v0.1.16 release timing before submitting Met. |
| `no_leaked_credentials` | MUST | Evidence: Gitleaks v8.30.1 checked all locally reachable Git history/refs with full redaction and found zero findings on 2026-11-15; GitHub secret scanning reported zero open alerts. Public test/sample values are not claimed to be valid private production credentials. |

## Analysis

| Criterion ID | Level | Worksheet status and evidence / remaining action |
| --- | --- | --- |
| `static_analysis` | MUST | Evidence: Clippy and all four CodeQL analyses run on release candidates before publication; retain successful v0.1.16 PR analysis links with the release verification record. |
| `static_analysis_common_vulnerabilities` | SUGGESTED | Evidence: CodeQL security analysis is configured. Confirm successful Rust analysis and the actual query suite/results for the release under assessment. |
| `static_analysis_fixed` | MUST | Evidence: confirmed exploitable static-analysis findings have been remediated; the remaining Scorecard history/maintenance findings are tracked separately. Candidate checks must pass before release, and false-positive dispositions retain their source justification. |
| `static_analysis_often` | SUGGESTED | Evidence: Clippy runs on pull requests and pushes to `master`/`main`; verify CodeQL execution history for its configured events. |
| `dynamic_analysis` | SUGGESTED | Evidence: [cargo-fuzz target](../fuzz/README.md) runs on PRs, mainline pushes, and a weekly schedule. Preserve successful fuzz evidence for a proposed major release before publishing. |
| `dynamic_analysis_unsafe` | SUGGESTED | Verify: Rust code includes native FFI/unsafe paths, notably macOS clipboard access. Existing decoder fuzzing does not exercise those platform paths. Determine applicability and sanitizer/assertion coverage; do not automatically claim N/A because the workspace is Rust. |
| `dynamic_analysis_enable_assertions` | SUGGESTED | Evidence: Cargo's test profile enables assertions; protocol fuzzing explicitly asserts round-trip behavior in [its target](../fuzz/fuzz_targets/protocol.rs). |
| `dynamic_analysis_fixed` | MUST | Evidence: protocol fuzzing has run and no unpatched confirmed exploitable dynamic-analysis finding remains. The new pairing regression reproduces the discovered misuse and verifies the correction before release. |

## Validation and submission sequence

1. Complete v0.1.16 candidate validation: workspace tests, minimum Rust, Clippy,
   four-language CodeQL, audits, fuzzing, and signed archive verification. The
   pairing/default-key issues discovered during this assessment are addressed
   in that patch; do not submit the affected final answers before it is released.
2. Publish and verify the security patch, retaining actual CI/release evidence.
   Re-pair old clients and rotate example relay keys as described in the release
   notes; no source change retroactively secures an earlier handshake.
3. Update the registered project's final answers using the release evidence,
   [warning review](compiler-warnings.md), and the maintainer confirmations.
   Perfect forward secrecy, measured broad coverage, strict warnings, and native
   sanitizer coverage must retain their accurate limitations rather than being
   marked Met just to complete the form.
4. Query the project's public JSON/badge endpoints to confirm the state actually
   earned by the saved assessment before displaying a passing badge. The site
   is authoritative for the current result.

This worksheet does not promise a Scorecard increase. The Best Practices badge
is a separate self-assessment, and Scorecard rechecks published project state on
its own schedule.
