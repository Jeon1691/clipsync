# Project governance

ClipSync is maintained by [Jeon1691](https://github.com/Jeon1691). The maintainer
makes final decisions about scope, compatibility, contributions and releases.
Use issues and pull requests to propose or dispute a decision with technical
evidence; the maintainer records the resolution in that discussion. Unresolved
disagreement does not remove anyone's rights under the MIT license.

| Role | Current holder | Responsibilities |
| --- | --- | --- |
| Maintainer and release manager | Jeon1691 | Triage issues; maintain the roadmap; review changes and CI; manage repository access, tags, release notes, attestations and package updates |
| Security contact | Jeon1691 | Privately acknowledge, reproduce and assess reports; coordinate fixes, disclosure and reporter credit under SECURITY.md |
| Conduct contact | Jeon1691 | Apply CODE_OF_CONDUCT.md to project spaces, review reports and explain moderation decisions |
| Contributor | Each contributor | Follow CONTRIBUTING.md; document and test changes; disclose vulnerabilities privately |

Automated tools assist the maintainer. Bots and AI assistance do not count as
additional independent maintainers, significant human contributors or human
reviewers. The current project has a bus factor of one.

## Access and continuity

Anyone with write, release or private security access must use GitHub two-factor
authentication. Prefer a passkey, security key or TOTP application over SMS.
Never publish recovery codes, deploy keys, access tokens or password-vault data.
The current sole maintainer confirmed use of a cryptographic 2FA method on
2026-11-15. Access changes must include checking this requirement.

The repository, documentation, build instructions and release verification are
public. These make a fork possible but do not transfer control of the original
repository, advisories, tap or credentials. On 2026-11-15, the maintainer
confirmed that a real emergency custodian,
necessary access and legal rights, and recovery procedures are established so
that another person can continue original-repository issue management, merging
and releases within one week of confirmed loss of the maintainer. Custody and
recovery material remain private; this public record contains no credentials.
This access arrangement does not establish a second active developer or
independent contributor.

To maintain continuity, the maintainer must keep a willing custodian,
arrange lawful access to the repository, package tap and required credentials,
and periodically exercise recovery covering issue management, a pull request
and a verified release. Record exercise dates and public outcomes here without
disclosing recovery material. The confirmation records an actual arrangement,
not merely this procedure. It does not establish a bus factor of two.

## Protected development branch

The active master ruleset requires pull requests, resolution of review threads,
the configured CI/security checks, and verified commit signatures. It blocks
branch deletion and force pushes and has no bypass actors. The signing rule was
enabled after registering the existing public signing key and verifying all
current PR commits and a GitHub-created merge commit. Independent approval is
not claimed: the current solo maintainer still cannot supply a second reviewer.
