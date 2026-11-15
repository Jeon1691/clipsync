# Contributing to ClipSync

Use [GitHub issues](https://github.com/Jeon1691/clipsync/issues) for bug reports,
enhancement proposals, and questions about changes. Reports and code comments
in English are welcome. Search existing issues and pull requests before opening
a new discussion. Include the ClipSync version, operating system, reproduction
steps, expected behavior, and relevant logs with sensitive information removed.

For vulnerabilities, use the private reporting process in
[SECURITY.md](SECURITY.md). Keep clipboard contents, real pairing codes, device
tokens, and private keys out of public issues, logs, and test fixtures.

## Propose a change

1. Fork the repository and create a branch from the current `master` branch.
2. Keep the change focused. For a substantial feature or protocol change,
   describe the intended behavior and compatibility impact in an issue or draft
   pull request so it can be discussed before implementation grows.
3. Open a pull request against `master`. Explain the problem, resulting behavior,
   relevant issue, and how you verified the change. Record any platform you could
   not test and any remaining limitations.
4. Respond to review comments and keep the branch current with `master`. Check
   the repository's current required checks and wait for them to pass before
   merging. The maintainer decides whether a contribution is accepted.

This is currently a solo-maintainer project. A pull request gives changes a
public discussion and CI record; it does not itself establish independent
approval of changes authored by the maintainer. Review requirements follow the
[repository rules](https://github.com/Jeon1691/clipsync/rules).

Contributions are made under the project's [MIT license](LICENSE-MIT).

## Build and check

Install Rust through rustup and update the stable toolchain:

```sh
rustup update stable
rustup component add rustfmt clippy
```

The repository selects stable in [rust-toolchain.toml](rust-toolchain.toml).
The minimum supported Rust version is the workspace `rust-version` in
[Cargo.toml](Cargo.toml); the minimum-version CI job checks it separately.
Keep the manifest and CI consistent if a change raises that minimum.

On Debian/Ubuntu, install the native desktop dependencies used in CI:

```sh
sudo apt-get update
sudo apt-get install -y libgtk-3-dev libxkbcommon-dev libwayland-dev libxkbcommon-x11-dev xclip xvfb
```

From the repository root:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked
cargo build --workspace --locked
```

Run the workspace tests on macOS or Windows:

```sh
cargo test --workspace --locked
```

On Linux, the clipboard tests need an X server. For a headless session, use the
same serialized invocation as CI:

```sh
xvfb-run -a cargo test --workspace --locked -- --test-threads=1
```

Clipboard integration tests can temporarily replace the system clipboard. Use a
test desktop session and synthetic text, images, and files. For an individual
crate, use `cargo test -p <crate-name> --locked`; this supplements the full
workspace checks rather than replacing them for a cross-crate change.

The [CI workflow](.github/workflows/ci.yml) is the executable reference for
platform dependencies and additional checks. It covers Linux, macOS, Windows,
the minimum Rust version, stability and platform contracts, relay end-to-end
tests, image/file format transfers, desktop builds, CLI smoke tests, and script
parsing. The [security workflow](.github/workflows/security.yml) audits both
lockfiles and fuzzes the protocol. CodeQL analysis is configured through
GitHub's code-scanning setup.

## Requirements for accepted changes

- Follow rustfmt and the existing crate structure and naming. Address compiler
  and Clippy warnings introduced by the change; justify any targeted lint
  exception in the code.
- Add automated tests for major new functionality. For a bug fix, add a focused
  regression test when the behavior can be reproduced reliably. Exercise failure
  paths and boundaries, especially untrusted protocol input and file handling.
- Update the CLI help and relevant documentation when commands, configuration,
  protocol behavior, or supported platforms change. Describe user-visible
  upgrade or compatibility impact in the pull request so it can inform release
  notes.
- Keep Cargo manifests and the committed workspace and fuzz lockfiles consistent.
  Explain dependency upgrades and check their impact on the minimum Rust version
  and supported platforms. CI audits must remain enabled.
- For security-sensitive changes, describe the affected trust boundary and
  failure behavior. Consult the [security model](docs/security.md) and
  [protocol](docs/protocol.md). Use established cryptographic libraries and keep
  secrets out of diagnostics and public test data.
- For decoder changes, run the [protocol fuzz target](fuzz/README.md). Add a
  regression seed for a discovered decoding failure, using synthetic input that
  contains no private data.

Use the checks appropriate to the change and report their results in the pull
request. Documentation-only changes should verify commands and links; they do
not need an unrelated full build.
