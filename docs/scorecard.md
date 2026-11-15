# OpenSSF Scorecard improvement plan

## Measured baseline

The [published report](https://api.securityscorecards.dev/projects/github.com/Jeon1691/clipsync)
on 2026-11-15 evaluates commit `85c24217751734954582cb6daf0cf6bf6a48f8e2`
with Scorecard v5.1.1 and gives **6.3/10**. The overall result is weighted;
it is not a simple average of the checks.

| Check | Score | Next action |
| --- | --- | --- |
| Signed-Releases | 0 | Publish and verify signed attestation bundles with future release archives. |
| Branch-Protection | 4 | Add an independent reviewer before requiring approvals or last-push approval; then evaluate administrator bypass and stale-review dismissal. |
| Vulnerabilities | 4 | Track the six upstream maintenance advisories in [security-alerts.md](security-alerts.md). Native audits currently report no patched-version vulnerabilities. |
| Code-Review | 0 | Merge real changes after independent human review and approval; recent commit history currently contains no qualifying approvals. |
| Maintained | 0 | The repository is younger than 90 days. Recheck after 2026-12-15 while continuing normal development and issue triage. |
| CII-Best-Practices | 0 | The maintainer must register and complete an accurate [OpenSSF Best Practices assessment](https://www.bestpractices.dev/). |
| Contributors | 0 | Reflects actual contributor/organization history. Broader contributions must be genuine; changing profile metadata is not a security improvement. |
| Packaging | Not scored | GitHub Release archives and the Homebrew tap exist, but Scorecard does not detect a supported package-publishing workflow. This inconclusive result is excluded from the current total. |

CI-Tests, Dependency-Update-Tool, Binary-Artifacts, Dangerous-Workflow,
Security-Policy, Token-Permissions, Pinned-Dependencies, SAST, License, and
Fuzzing are already **10/10**. Preserve these controls while improving the
remaining checks.

## Release authenticity

The release workflow now generates a signed SLSA provenance attestation using
GitHub OIDC and `actions/attest`, verifies the archive digest and workflow/tag
identity, and publishes a matching `.sigstore` bundle. The signature is bound
to the workflow that actually built the archive; no historical binary is
retroactively described as built by this workflow. See
[release verification](packaging.md) for consumer commands.

Scorecard v5.1.1 recognizes `.sigstore` files as signatures and examines the
latest five releases. Its current signature-only release credit is 8/10 per
qualifying release, averaged over that window and rounded down. If all five
currently unsigned releases are gradually replaced by signed releases, the
Signed-Releases component would progress through **1, 3, 4, 6, 8**, assuming
the same evaluator and a five-release window. This is a component estimate,
not a promise of a particular overall score. The published score does not
increase merely because the workflow change is merged.

The bundle contains genuine signed build provenance. It is retained in its
Sigstore format rather than renamed to `.intoto.jsonl` to obtain a different
Scorecard classification. Scorecard's filename detection does not replace
cryptographic verification.

## Review and branch policy

At the baseline, only `Jeon1691` is a repository collaborator. The current
ruleset prevents force pushes/deletion and requires PRs, resolved threads,
up-to-date branches, and the Linux/macOS/Windows CI checks. One maintainer
cannot independently approve their own PR. Requiring an approver without
first adding a trusted reviewer would block the current contribution flow.

After an independent reviewer is available, consider requiring at least one
approval, dismissing stale approvals, requiring approval of the latest push,
and removing administrative bypass. Two independent approvals and enforced
CODEOWNERS review are further steps for a larger team. Require the security
audit, fuzz, minimum-Rust, and CodeQL checks once their operation is established.
These settings change who can merge and are not changed by this patch.

Code-Review is based on recent history, so a policy change alone will not erase
the current 0/25 approved-changeset measurement. Use normal reviewed development;
do not create empty commits, reviews, or releases just to alter the score.

## Maintenance and assessment

The remaining RustSec findings describe unmaintained upstream GUI/font/D-Bus
or code-generation dependencies. Removing accessibility, suppressing advisories,
or reducing audit coverage would hide the exposure rather than fix it. Prefer
tested upstream migrations that keep supported platforms and the declared
minimum Rust version consistent.

The Best Practices badge is a separate project self-assessment. Existing
security policy, CI, tests, fuzzing, dependency updates, and release verification
provide evidence, but the maintainer must confirm that each requirement is met.
Adding an unearned badge image does not satisfy the assessment.

The Scorecard action update is already proposed by Dependabot in
[PR #8](https://github.com/Jeon1691/clipsync/pull/8). Review it separately;
the current measurement and estimates above explicitly use v5.1.1 so an
evaluator upgrade is not mistaken for a security improvement.

References: [Scorecard checks](https://github.com/ossf/scorecard/blob/v5.1.1/docs/checks.md),
[signature detection](https://github.com/ossf/scorecard/blob/v5.1.1/probes/releasesAreSigned/impl.go),
[release scoring](https://github.com/ossf/scorecard/blob/v5.1.1/checks/evaluation/signed_releases.go),
and [GitHub artifact attestations](https://docs.github.com/en/actions/how-tos/secure-your-work/use-artifact-attestations/use-artifact-attestations).
