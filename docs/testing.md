# Testing and coverage

The [CI workflow](../.github/workflows/ci.yml) tests the Rust workspace on Linux,
macOS, and Windows, checks the Rust 1.88 minimum, and runs formatting, compiler
warnings, Clippy, stability, platform, relay, and format checks. Run the workspace
tests locally with:

```sh
cargo test --workspace --locked -- --test-threads=1
```

Clipboard tests write to the system clipboard. On Linux, install `xclip`, `xvfb`,
`libgtk-3-dev`, `libxkbcommon-dev`, `libwayland-dev`, and `libxkbcommon-x11-dev`, and
run that command through `xvfb-run -a`. Native clipboard behavior still needs the
three-platform CI matrix and manual platform testing.

## Measuring coverage

The [coverage workflow](../.github/workflows/coverage.yml) runs every workspace
package, all Cargo targets, and all features with LLVM source instrumentation.
It includes the desktop and mobile Rust crates, even when their code is untested.
No project source files are excluded to improve the result. The tool's default
filters omit dependency sources, generated build output, and standalone test,
example, and benchmark directories. It runs on pull requests, default branch
pushes, weekly, and on manual dispatch.

The `workspace-coverage` artifact contains `lcov.info`, an HTML report in
`html/index.html`, `summary.json`, and `summary.txt`. The Actions job summary also
shows per-file and total line, region, function, and branch results. Coverage
reports reuse the same test run; exporting each format does not rerun tests.
LCOV export uses the supported `--skip-functions` option because the native LLVM
exporter can crash when serializing per-function records. Line and branch
records remain in LCOV; the other reports retain function metrics.

The workflow pins [cargo-llvm-cov 0.9.1](https://github.com/taiki-e/cargo-llvm-cov/releases/tag/v0.9.1)
and a nightly compiler because [branch instrumentation is unstable](https://github.com/taiki-e/cargo-llvm-cov#usage).
Production continues to support Rust 1.88; nightly is a measurement toolchain.
To reproduce the measurement from the repository root:

```sh
rustup toolchain install nightly-2026-11-11 --profile minimal --component llvm-tools-preview
cargo +stable install cargo-llvm-cov --version 0.9.1 --locked
export CARGO_LLVM_COV_TARGET_DIR=target/coverage
cargo +nightly-2026-11-11 llvm-cov clean --workspace
cargo +nightly-2026-11-11 llvm-cov --workspace --all-features --all-targets --branch --locked --no-report -- --test-threads=1
mkdir -p coverage
cargo +nightly-2026-11-11 llvm-cov report --lcov --skip-functions --output-path coverage/lcov.info
cargo +nightly-2026-11-11 llvm-cov report --html --output-dir coverage
cargo +nightly-2026-11-11 llvm-cov report --json --summary-only --output-path coverage/summary.json
cargo +nightly-2026-11-11 llvm-cov report > coverage/summary.txt
```

On Linux, prefix the instrumented test command with `xvfb-run -a`.

Silver requires at least 80% statement coverage; Gold requires at least 90%
statement and 80% branch coverage. LLVM reports source lines and regions rather
than a language-independent statement count. Review both metrics and the missing
source before using this evidence for a statement-coverage assertion. The
workflow deliberately has no percentage gate until a representative baseline and
a realistic improvement target have been reviewed. Adding a workflow alone does
not establish that a coverage threshold is met.

Coverage is specific to the compiler, host, active `cfg` paths, and tests. The
Linux workflow does not measure Windows or macOS implementations, iOS/Android
device behavior, Swift/Kotlin apps, or C/C++/OS libraries. Documentation tests are
not instrumented by this command. The HTML report keeps uncovered desktop and
mobile Rust paths visible; compiling those crates does not demonstrate their
native UI or foreign-function behavior.

### Recorded baseline

A clean measurement on 2026-11-15 of the 0.1.17 candidate, using
`aarch64-apple-darwin`, Rust `nightly-2026-11-11`, and cargo-llvm-cov 0.9.1, passed
all 74 workspace tests and reported 50 Rust source files:

| Metric | Covered / total | Coverage |
| --- | ---: | ---: |
| Lines | 3,762 / 6,515 | 57.74% |
| Regions | 4,050 / 7,439 | 54.44% |
| Branches | 422 / 1,044 | 40.42% |
| Functions | 421 / 772 | 54.53% |

Desktop `main.rs` and the mobile library were included at 0% line coverage. The
current evidence does not meet the Silver or Gold coverage thresholds. Improve
tests for uncovered application and failure paths, then record a fresh full
workspace report. The hosted Linux result is a separate baseline and must be
reviewed after the workflow runs.

## Dynamic analysis and fuzzing

The [security workflow](../.github/workflows/security.yml) runs the `protocol`
fuzzer against binary chunk and JSON control decoders. The coverage workflow also
runs `crypto_aead` against nonce-prefixed decoding, encryption/decryption round
trips, and rejection of modified ciphertext, key, nonce, and associated data.
Both run bounded 60-second fuzz campaigns in routine CI. The crypto job uploads
its sanitizer log and any reproducing crash inputs as the `crypto-asan` artifact.

The coverage workflow also runs clipboard and daemon tests on an Apple Silicon
macOS runner under AddressSanitizer. This executes native NSPasteboard paths and
the daemon's platform service helpers, including Rust unsafe FFI calls reached
by those tests, and uploads the `macos-native-asan` log. Reproduce it on an Apple
Silicon Mac with:

```sh
rustup toolchain install nightly-2026-11-11 --profile minimal --component rust-src
CARGO_TARGET_DIR=target/native-asan \
RUSTFLAGS='-Zsanitizer=address -Cdebug-assertions=yes' \
RUSTDOCFLAGS='-Zsanitizer=address -Cdebug-assertions=yes' \
ASAN_OPTIONS=detect_stack_use_after_return=1 \
cargo +nightly-2026-11-11 test --target aarch64-apple-darwin -p clipsync-clipboard -p clipsync-daemon --all-targets --locked -- --test-threads=1
```

The [Rust sanitizer guide](https://doc.rust-lang.org/unstable-book/compiler-flags/sanitizer.html)
documents this target and the `--target` setup. This job retains the production
lockfile and uses the prebuilt Rust standard library; adding `-Zbuild-std` would
require an expanded lockfile for the pinned nightly's standard-library graph.
The prebuilt standard library and macOS system frameworks remain uninstrumented,
so AddressSanitizer cannot detect every error in those libraries. These tests
also do not exercise every daemon process, Unix IPC, Windows FFI, or mobile
bridge path.

The jobs use [cargo-fuzz 0.13.2](https://github.com/rust-fuzz/cargo-fuzz/tree/0.13.2).
The crypto command explicitly enables AddressSanitizer and debug assertions.
As [documented by cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz/blob/0.13.2/src/main.rs),
its normal optimized fuzz profile retains debug assertions and overflow checks;
using plain `--release` changes that behavior. Do not disable these checks for
release evidence.

Before each major release, the release maintainer must run both fuzz targets
with AddressSanitizer and debug assertions against the release candidate, review
findings, and attach the command, toolchain, candidate commit, duration, and logs
to the release verification record. Use at least the following ten-minute
campaign per target; longer campaigns are encouraged:

```sh
rustup toolchain install nightly-2026-11-11 --profile minimal --component rust-src
cargo +stable install cargo-fuzz --version 0.13.2 --locked
cargo +nightly-2026-11-11 metadata --locked --format-version 1 --manifest-path fuzz/Cargo.toml > /dev/null
mkdir -p fuzz/corpus/protocol fuzz/corpus/crypto_aead
cargo +nightly-2026-11-11 fuzz run --sanitizer address --debug-assertions protocol fuzz/corpus/protocol fuzz/seeds/protocol -- -max_total_time=600 -max_len=1048576
cargo +nightly-2026-11-11 fuzz run --sanitizer address --debug-assertions crypto_aead fuzz/corpus/crypto_aead fuzz/seeds/crypto_aead -- -max_total_time=600 -max_len=65536
```

Investigate every crash or sanitizer diagnostic before the release. Minimize and
retain reproducing inputs under the corresponding `fuzz/seeds/` directory, add a
regression test where appropriate, and rerun the affected campaign after fixing
the cause. Generated corpora and build output are ignored by Git.

The fuzz campaigns exercise the reached Rust decoder and cryptographic paths
and their instrumented dependencies; the native test job adds reached macOS
clipboard and daemon paths. They do not establish complete unsafe-code, GUI,
mobile bridge, race, or external-library coverage. Add
targeted sanitizer/device tests for those boundaries before claiming broader
dynamic-analysis coverage.

## Higher-tier testing work

The [regression audit](regression-audit.md) reviews the complete source history
within the six-month window. Focused regression evidence covers 23/53
project-authored runtime/security defects (43.40%); its broader conservative
ledger covers 24/84 (28.57%). Neither supports the required 50% claim. The ledger
names missing reproductions rather than treating a passing smoke check as
coverage of every related bug.

The hosted Linux [coverage run](https://github.com/Jeon1691/clipsync/actions/runs/43686133645)
measured 3,489/5,897 lines (59.17%), 3,684/6,734 regions (54.71%) and 397/924
branches (42.97%). These are separate OS/cfg-specific denominators, not a merged
percentage with the macOS baseline. Both remain below the relevant thresholds.

Useful next test areas are sustained two-peer daemon sessions and IPC, adapter
selection and command failures, desktop pairing/error transitions, mobile
callbacks and local service cleanup. The macOS baseline needs at least 1,450
more existing lines covered to reach 80% lines, and roughly 1,902 more regions
to reach 80% regions; line coverage alone is not the statement assertion. This
requires substantive integration/platform testing, not excluding desktop/mobile
code or adding assertions that merely repeat implementation details.

Before the 0.1.17 candidate was released, local ASan/assertion-enabled campaigns
completed 23,944,020 protocol executions and 2,747,280 AEAD executions in 601
seconds each, with exit code 0 and no findings. Native ASan passed 12 tests.
Candidate metadata, commands, tool versions, logs and reports were retained in
`coverage/release-0.1.17/`; the public workflow artifacts independently retain
routine hosted results.
