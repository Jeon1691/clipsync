# OpenSSF Scorecard improvement plan

## Measured result

The [published report](https://api.securityscorecards.dev/projects/github.com/Jeon1691/clipsync)
at **2026-11-15T11:25:39Z** evaluates commit
`af580d7a893e7b5316dcd945ee33161690eac4ee` with Scorecard v5.6.0 and gives
**7.3/10**. The initial report was 6.3/10 with v5.1.1; subsequent reports were
6.8 and 7.1. The overall result is weighted, not a simple average. A source
change, local audit or proposed release does not update this public measurement.

| Check | Score | Next action |
| --- | --- | --- |
| Signed-Releases | 8 | Four of the latest five releases have verified provenance. Publish the next substantive release with the same verification; preserve older unsigned releases. |
| Branch-Protection | 4 | Administrator bypass has been removed; require CI and security results. Independent review remains a prerequisite for approval requirements. |
| Vulnerabilities | 4 | The v0.2.0 migration removes all six maintenance-advisory packages; both audits are clean. See [security-alerts.md](security-alerts.md) for the explicit source-patch maintenance policy. |
| Code-Review | 0 | Merge real changes after independent human review and approval; recent commit history currently contains no qualifying approvals. |
| Maintained | 0 | The repository is younger than 90 days. Recheck after 2026-12-15 while continuing normal development and issue triage. |
| CII-Best-Practices | 7 | [Project 16709](https://www.bestpractices.dev/en/projects/16709) has earned Silver. Gold still requires verified technical evidence and actual additional people/review history. |
| Contributors | 0 | Reflects actual contributor/organization history. Broader contributions must be genuine; changing profile metadata is not a security improvement. |
| Packaging | 10 | The successful container workflow publishes tested native amd64/arm64 relay images, verifies OCI provenance, then promotes the version tag. |

CI-Tests, Dependency-Update-Tool, Binary-Artifacts, Dangerous-Workflow,
Security-Policy, Token-Permissions, Pinned-Dependencies, SAST, License, and
Fuzzing are already **10/10**. Preserve these controls while improving the
remaining checks.

## Release authenticity

The release workflow generates signed SLSA provenance using GitHub OIDC and
`actions/attest`. It preserves the original `.sigstore.json` bundle and exports
its actual DSSE envelope as `.intoto.jsonl` after `gh attestation verify`
validates the archive, repository, release workflow, source ref/commit, and
hosted-runner identity. The envelope's signed payload type, base64 payload, and
signatures remain unchanged. Certificates and transparency evidence remain in
the bundle. Consumers verify the original bundle and cross-check the export;
see [release verification](packaging.md) for commands.

The build and Linux smoke jobs verify the evidence before executing binaries.
Publication requires all supported mandatory targets and their sidecars, a
source tag on the default branch's history, and a draft bound to that exact
source commit. It refuses published releases and nonempty existing drafts,
uploads without replacement, then downloads and verifies the complete set
before publishing. Repository immutable releases are enabled; v0.1.18 is
immutable. Enabling this setting does not retroactively freeze earlier releases.

Scorecard v5.6.0 examines the latest five releases and recognizes
`.sigstore.json` as signatures and `.intoto.jsonl` as provenance. Its
[release scoring](https://github.com/ossf/scorecard/blob/v5.6.0/checks/evaluation/signed_releases.go)
assigns 8 points to a signature-only release or 10 to a release with detected
provenance, averages the window, and rounds down. v0.1.15, v0.1.16 and v0.1.17
now include verified exports of their original signed envelopes; their existing
archives, checksums and bundles were unchanged. v0.1.18 supplies all five
archives, checksums, bundles and DSSE exports in an immutable release. Four of
the latest five releases therefore have provenance, giving the measured **8/10**
component. v0.1.14 has no original provenance and remains unchanged.

Existing authentic bundles may be exported without changing any old binary,
tag, signed payload, or signature. This is another representation of original
build evidence. Releases without original attestations cannot be given
retroactive build provenance. Scorecard's filename detection does not replace
cryptographic verification, and renaming a Sigstore bundle is not a DSSE export.

## Review and branch policy

The maintainer confirmed on 2026-11-15 that this is a solo project. The active
[Protect master ruleset](https://github.com/Jeon1691/clipsync/rules/27029148)
prevents force pushes/deletion and requires signed commits, PRs, resolved
threads, up-to-date branches, and the Linux/macOS/Windows CI checks. Its 15
required contexts include dependency audit, protocol fuzzing, `Minimum Rust`,
CodeQL and all four language-analysis jobs, product coverage, dependency-source
and upstream tests, Linux native dependency smoke testing and macOS product
coverage. The macOS result passed before it became required. Required results are
bound to the GitHub App that actually produces them. CodeQL merge protection
blocks security findings at medium severity or higher and code findings at error
severity. Administrator bypass has been removed and stale approvals are
dismissed on new commits. One maintainer
cannot independently approve their own PR. Requiring an approver without
first adding a trusted reviewer would block the current contribution flow.

After an independent reviewer is available, consider requiring at least one
approval and approval of the latest push. Two independent approvals and enforced
CODEOWNERS review are further steps for a larger team. The minimum-Rust check
was made required after its stable check name passed on the improvement branch.
The settings above were applied through the repository rules API; they are
external configuration, not automatically deployed by merging this document.
Scorecard's review-dependent scoring tiers can keep Branch-Protection at 4 even
after these useful controls are enabled.

Code-Review is based on recent history, so a policy change alone will not erase
the current 0/5 approved-changeset measurement. Use normal reviewed development;
do not create empty commits, reviews, or releases just to alter the score.

## Maintenance and assessment

The v0.2.0 [dependency migration](dependency-migration.md) removes the six former
RustSec maintenance-advisory packages while retaining accessibility, title text,
clipboard formats, both renderers, and mobile bindings. Both audited lockfiles
are clean without ignores. Eight explicit source patches now require
ClipSync maintenance; their source hashes, complete diffs, licenses, review
cadence, and retirement conditions are documented. This is an actual component
migration with Rust 1.95 as the tested minimum. A clean local audit does not
claim that the published Scorecard report has already changed.

The Best Practices badge is a separate project self-assessment. The maintainer
completed GitHub account linking, and [project 16709](https://www.bestpractices.dev/en/projects/16709)
is registered for the exact repository URL. Passing and Silver are both 100%;
the saved answers and earned badge remain authoritative. The
[contribution guide](../CONTRIBUTING.md) and
[criterion-by-criterion worksheet](best-practices-assessment.md) provide a
prepared registration record, evidence, and concrete gaps. Existing
security policy, CI, tests, fuzzing, dependency updates, and release verification
provide evidence, but the maintainer must confirm that each requirement is met.
Adding an unearned badge image does not satisfy the assessment.

The Scorecard action v2.5.0 update proposed by Dependabot in
[PR #8](https://github.com/Jeon1691/clipsync/pull/8), which uses Scorecard v5.6.0
and recognizes standard `.sigstore.json` bundles, is merged. The initial
baseline used v5.1.1; the current report uses v5.6.0. Compare evaluator
versions explicitly rather than treating a version change as a security fix.

References: [Scorecard checks](https://github.com/ossf/scorecard/blob/v5.6.0/docs/checks.md),
[signature detection](https://github.com/ossf/scorecard/blob/v5.6.0/probes/releasesAreSigned/impl.go),
[provenance detection](https://github.com/ossf/scorecard/blob/v5.6.0/probes/releasesHaveProvenance/impl.go),
[release scoring](https://github.com/ossf/scorecard/blob/v5.6.0/checks/evaluation/signed_releases.go),
and [GitHub artifact attestations](https://docs.github.com/en/actions/how-tos/secure-your-work/use-artifact-attestations/use-artifact-attestations).
