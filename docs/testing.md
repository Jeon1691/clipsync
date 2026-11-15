# Testing and coverage

The [CI workflow](../.github/workflows/ci.yml) tests the Rust workspace on Linux,
macOS, and Windows, checks the Rust 1.95 minimum, and runs formatting, compiler
warnings, Clippy, stability, platform, relay, and format checks. Run the workspace
tests locally with:

```sh
cargo test --workspace --locked -- --test-threads=1
```

The real host clipboard roundtrip requires `CLIPSYNC_TEST_HOST_CLIPBOARD=1`;
enable it only in a disposable test session. Ordinary local tests use private
named pasteboards or isolated adapters. Full foreground CLI completion on macOS
also requires `CLIPSYNC_TEST_DISPOSABLE_HOST=1`; local validation leaves that flag
unset. Hosted CI supplies both flags. On Linux, install `xclip`, `xvfb`,
`libgtk-3-dev`, `libxkbcommon-dev`, `libwayland-dev`, `libxkbcommon-x11-dev`,
`libfontconfig1-dev` and `libfreetype6-dev`, then run through `xvfb-run -a`.
Native behavior still needs the three-platform CI matrix and manual testing.

## Measuring coverage

The [coverage workflow](../.github/workflows/coverage.yml) runs every workspace
package, all Cargo targets, and all features with LLVM source instrumentation.
It includes the desktop and mobile Rust crates, even when their code is untested.
No project source files are excluded to improve the result. The tool's default
filters omit dependency sources, generated build output, and standalone test,
example, and benchmark directories. It runs on pull requests, default branch
pushes, weekly, and on manual dispatch.

The `workspace-coverage-linux` and `workspace-coverage-macos` artifacts contain `lcov.info`, an HTML report in
`html/index.html`, `summary.json`, and `summary.txt`. The Actions job summary also
shows per-file and total line, region, function, and branch results. Coverage
reports reuse the same test run; exporting each format does not rerun tests.
LCOV export uses the supported `--skip-functions` option because the native LLVM
exporter can crash when serializing per-function records. Line and branch
records remain in LCOV; the other reports retain function metrics.

The workflow pins [cargo-llvm-cov 0.9.1](https://github.com/taiki-e/cargo-llvm-cov/releases/tag/v0.9.1)
and a nightly compiler because [branch instrumentation is unstable](https://github.com/taiki-e/cargo-llvm-cov#usage).
Production continues to support Rust 1.95; nightly is a measurement toolchain.
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
python3 scripts/check-coverage.py coverage/summary.json
```

On Linux, prefix the instrumented test command with `xvfb-run -a`.

Silver requires at least 80% statement coverage; Gold requires at least 90%
statement and 80% branch coverage. LLVM reports source lines and regions rather
than a language-independent statement count. Review both metrics and the missing
source before using this evidence for a statement-coverage assertion. The workflow now requires at least 80% product lines and executable regions.
The checker sums only validated production-file counters, rejects missing
crates, duplicate/non-product files and inconsistent totals, and retains the
full HTML/JSON reports even when the threshold fails.

Coverage is specific to the compiler, host, active `cfg` paths, and tests. The
Linux and macOS jobs measure their own active implementations separately. They do not measure Windows paths, iOS/Android
device behavior, Swift/Kotlin apps, or C/C++/OS libraries. Documentation tests are
not instrumented by this command. The HTML report keeps uncovered desktop and
mobile Rust paths visible; compiling those crates does not demonstrate their
native UI or foreign-function behavior.

### Production-source measurement

Inline `#[cfg(test)]` modules/helpers are marked `coverage(off)` only when the
nightly coverage tool is active, following the tool's documented approach.
Standalone test directories are already excluded by the tool. Tests still run;
their assertion lines do not increase production coverage. Desktop/mobile and
all other product source stay in the denominator. Stable builds/MSRV remain
unchanged; the known coverage cfg names are declared through workspace lints.

A clean macOS measurement of the regression candidate passed all 190 Rust tests:

| Metric | Covered / total | Coverage |
| --- | ---: | ---: |
| Product-source lines | 5,116 / 5,924 | 86.36% |
| Executable regions | 5,304 / 6,591 | 80.47% |
| Branches | 751 / 1,064 | 70.58% |

Hosted Linux evidence for the same source is available in the
[coverage run](https://github.com/Jeon1691/clipsync/actions/runs/43696427313):
4,703/5,421 lines (86.76%), 4,883/6,080 executable regions (80.31%) and
659/952 branches (69.22%). The required gate passes on the real Linux runner;
platform-specific denominators are reported separately rather than summed.

Both product lines and executable regions exceed the 80% Silver gate. LLVM
executable-region mapping provides this project's Rust statement-coverage
evidence, with the additional line threshold retained. Gold's 90% statement
and 80% branch targets remain unmet. Earlier
57.74% macOS / 59.17% Linux line reports included inline test-code lines and
are historical source-report baselines, not directly comparable production-only
measurements. Do not claim an increase from changing the denominator. The new
method provides a consistent baseline for future product-path tests.

The partial-write regression deliberately enforces a file-size limit in one
child process. Its LLVM profile is discarded to `/dev/null` so exit-time
profiling is not truncated by the same limit; normal storage tests retain their
profiles. The fixture still asserts that the real failed write preserves the
old credential. This is a conservative measurement limit, not an exclusion of
a product file or feature.

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
cargo +nightly-2026-11-11 test --target aarch64-apple-darwin -p clipsync-clipboard -p clipsync-daemon -p clipsync-mobile --all-targets --locked -- --test-threads=1
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

## Regression policy and remaining higher-tier work

The [full audit](regression-audit.md) and [machine-readable ledger](regressions.json)
now register **57/98 (58.16%)** distinct corrected defects, exceeding the 50%
Silver regression requirement. The checker validates real test references and
the full denominator in all three CI platform jobs; future fixes must update
the ledger and cannot silently delete uncovered cases. This fraction is separate
from product-source statement coverage.

Local validation passed 190 Rust tests, 37 Python policy/packaging tests, and
62 native macOS clipboard/daemon/mobile ASan tests. Historical/hunk-restored implementations fail the
targeted transfer, CLI, storage, TLS, service and packaging cases. Current source
passes. Characterization tests with no reliable historical failure are not
credited in the defect fraction.

The added cases exercise sustained daemon/IPC sessions, adapter command failures,
headless desktop state transitions, mobile callbacks and service cleanup. Native
window startup, some OS failure paths and Gold thresholds still need more work.
The complete desktop/mobile source remains visible in the reports.

The v0.1.17 security candidate's earlier ASan/assertion campaigns completed
23,944,020 protocol and 2,747,280 AEAD executions in 601 seconds each, exit 0 with
no findings. The follow-up adds tests and private clipboard/UI testability boundaries and
propagates mobile constructor failures; the fuzzed crypto,
protocol and fuzz sources are unchanged. Native ASan is rerun for the expanded
clipboard/daemon/mobile tests. Public workflow artifacts retain the hosted results.
