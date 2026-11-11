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

The security workflow audits both committed lockfiles. Vulnerabilities, unsound
code advisories, and yanked packages fail the audit job. Unmaintained-package
warnings remain visible; they are not suppressed with an advisory ignore list.

## Remaining maintenance advisories

These are informational unmaintained-package findings, not patched-version
vulnerability findings. They still contribute to Scorecard alert #25. Replacing
them requires changes in upstream native GUI/accessibility or binding-generation
dependencies. Review again by **2026-12-11** and check weekly Dependabot PRs.

| Advisory | Dependency | Current dependency path |
| --- | --- | --- |
| [RUSTSEC-2026-0374](https://rustsec.org/advisories/RUSTSEC-2026-0374.html) | basic-toml | UniFFI → Askama's code generator |
| [RUSTSEC-2024-0436](https://rustsec.org/advisories/RUSTSEC-2024-0436.html) | paste | eframe's optional wgpu backend → wgpu-hal → metal; compile-time macro |
| [RUSTSEC-2026-0192](https://rustsec.org/advisories/RUSTSEC-2026-0192.html) | ttf-parser | eframe/winit → native font rendering |
| [RUSTSEC-2026-0371](https://rustsec.org/advisories/RUSTSEC-2026-0371.html) | zbus_names | eframe → AccessKit/AT-SPI on Linux |
| [RUSTSEC-2026-0370](https://rustsec.org/advisories/RUSTSEC-2026-0370.html) | zvariant | eframe → AccessKit/AT-SPI on Linux |
| [RUSTSEC-2026-0372](https://rustsec.org/advisories/RUSTSEC-2026-0372.html) | zvariant_derive | Linux accessibility stack's compile-time macros |

The latest eframe releases require a newer compiler than the locally installed
Rust 1.94; this change uses the smallest eframe upgrade that removes the
vulnerable XML parser while retaining accessibility support. A UniFFI upgrade
alone does not remove basic-toml from Askama's dependency graph.

## Repository and project-history findings

| Code Scanning alert | Current state / next action |
| --- | --- |
| #2 Branch-Protection | The active [Protect master ruleset](https://github.com/Jeon1691/clipsync/rules/27029148) already prevents deletion and force pushes, requires PRs and resolved review threads, and requires up-to-date `linux`, `macos`, and `windows` checks. The alert predates this ruleset. Re-evaluate on the next Scorecard run. |
| #19 Code-Review | Scorecard found no approved changesets in its recent history. Maintainer review and future reviewed merges are needed; repository history cannot be repaired by this patch. |
| #21 Maintained | The repository was created on 2026-09-16. Scorecard flags repositories younger than 90 days; re-evaluate after 2026-12-15 as well as maintaining normal project activity. |
| #22 SAST | GitHub CodeQL default setup is already configured for Actions, Python, Ruby, and Rust. Scorecard needs successful analysis on merged PRs to observe its use. |
| #23 CII-Best-Practices | Requires the maintainer to register and truthfully complete the [OpenSSF Best Practices self-assessment](https://www.bestpractices.dev/). A badge cannot be earned by adding an image to the README. |

GitHub alerts are re-evaluated after these changes reach the default branch and
the corresponding dependency/CodeQL/Scorecard analyses run. No alert is manually
dismissed by this change.
