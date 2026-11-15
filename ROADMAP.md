# Roadmap: 2026-11-15–2027-11-15

This roadmap describes direction, not delivery dates or guarantees. The sole
maintainer prioritizes reproducible security defects and regressions before
features. Propose changes through issues and focused pull requests.

## Planned work

- Maintain encrypted text, image and file sync on macOS, Linux and Windows,
  the command line, desktop application and shared mobile bindings.
- Keep dependencies and minimum-Rust checks current while preserving native
  clipboard support, accessibility and platform window behavior.
- Increase meaningful test coverage of storage, transport, pairing failures,
  resource limits and platform integration. Publish full-workspace coverage
  measurements and investigate uncovered branches.
- Maintain fuzzing, vulnerability scans, signed release provenance and clear
  upgrade instructions. Address discovered defects before claiming higher
  assurance levels.
- Design compatible room-key rotation and recovery behavior before changing
  the v1 protocol. Document forward-secrecy limitations until implemented and
  validated.
- Improve keyboard/screen-reader testing, separate user-facing strings for
  localization, and make contributor setup easier to reproduce.
- Seek a willing additional maintainer and establish tested access continuity
  and independent review when an actual collaborator is available.

## Outside the current scope

ClipSync will not become a relay-side plaintext archive, a password manager,
or a replacement for secure endpoint administration. The relay will not index
clipboard contents. A badge is not a promise of complete coverage, independent
audit, forward secrecy or support for old vulnerable releases. Distribution
registry expansion follows maintenance capacity rather than score targets.

## Small contributor tasks

These tasks are suitable for a focused first pull request. Check current work
and open an issue before starting to avoid duplication.

1. Add malformed and boundary-value decoding seeds to `fuzz/seeds/protocol`,
   documenting which branch each seed exercises. Use only synthetic data.
2. Add a missing CLI help example to `docs/interfaces.md`, then verify it
   against `cargo run -p clipsync-cli -- --help` and the relevant subcommand.
3. Test one desktop flow using a platform screen reader and keyboard only;
   report the OS, reader, steps and observed labels without private clipboard
   data. Follow up with a focused accessibility fix if needed.
4. Add a focused regression test for an uncovered pure storage/configuration
   or protocol validation boundary shown by the coverage report.
