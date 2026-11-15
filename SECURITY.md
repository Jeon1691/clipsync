# Security policy

## Supported versions

Security fixes are applied to the latest release and the `master` branch. Upgrade
to the latest release before reporting a vulnerability.

## Reporting a vulnerability

Please [report vulnerabilities privately using GitHub Security Advisories](https://github.com/Jeon1691/clipsync/security/advisories/new).
Include the affected version, reproduction steps, expected impact, and any
suggested mitigation. Avoid including real pairing codes, device tokens, private
keys, or clipboard contents in your report.

If private reporting is unavailable, [open an issue requesting a private contact](https://github.com/Jeon1691/clipsync/issues/new)
without publishing vulnerability details or sensitive data.

See the [security model](docs/security.md) for trust boundaries and key handling,
and [GitHub's security advisories](https://github.com/Jeon1691/clipsync/security/advisories)
for published advisories.

## Response and disclosure

The security contact in [GOVERNANCE.md](GOVERNANCE.md) handles reports privately:

1. Acknowledge receipt, request missing reproduction information and establish a
   private discussion. The project is operated by one maintainer; this policy
   does not promise continuous monitoring or an unverified response SLA.
2. Reproduce against supported versions, identify affected releases and trust
   boundaries, assess impact/severity and discuss mitigations. Keep exploit
   details and reporter identity private during coordination.
3. Develop a focused fix and regression test; review design/compatibility,
   execute applicable tests, static analysis and fuzzing, and prepare upgrade
   guidance. Track unresolved risks rather than suppressing alerts.
4. Agree on disclosure timing with the reporter where possible, publish the
   patched release and a GitHub advisory when applicable, and state affected
   versions, remediation and any remaining limitations. Urgent protection of
   users may require disclosure before every downstream update is ready.
5. Credit reporters in the advisory/release notes using their preferred name or
   handle, unless they request anonymity. Confirm the remedy and close the
   private report, retaining the record and follow-up work.

The latest release is the upgrade target. Use `brew upgrade clipsync`, install
the newer verified release archive/Windows package, or rebuild the tagged source
with the supported Rust toolchain. Run `clipsync daemon stop` before replacing
binaries when necessary. Security release notes may require updating both peers,
rotating relay credentials and re-pairing; old stored keys and captured traffic
are not retroactively protected by installing a new binary.
