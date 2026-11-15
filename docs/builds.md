# Building, installing and verifying

Cargo builds the workspace dependency graph without recursive subdirectory
builds. Use the committed lockfile and `--locked`. Rust compiler and linker
configuration uses standard Cargo mechanisms, including `RUSTFLAGS`,
`CARGO_ENCODED_RUSTFLAGS`, `CARGO_TARGET_<TRIPLE>_LINKER`, `CARGO_TARGET_DIR`, and
`CARGO_PROFILE_RELEASE_*`. Native dependency build scripts use their applicable
`CC`, `CXX`, `CFLAGS` and `CXXFLAGS`; Rust itself does not interpret C flags.
The project does not replace user-supplied compiler/linker variables.

For debug information in an optimized build:

```sh
CARGO_PROFILE_RELEASE_DEBUG=2 CARGO_PROFILE_RELEASE_STRIP=none cargo build --release --workspace --locked
```

The host packaging helper also honors these profile settings and skips its
extra strip step when debug information is requested:

```sh
CARGO_PROFILE_RELEASE_DEBUG=2 scripts/package-release.sh
```

`CLIPSYNC_PRESERVE_DEBUG=1` explicitly preserves symbols in that helper. The
published workflow builds compact production archives; it is separate from a
local debug package. Do not mistake symbol stripping for a security boundary.

## Installation locations and removal

Homebrew installs into its configured prefix; use `brew install
jeon1691/tap/clipsync`, `brew upgrade clipsync`, and `brew uninstall clipsync`.
For source installation use Cargo's destination convention:

```sh
cargo install --path crates/clipsync-cli --locked --root /chosen/prefix
cargo uninstall clipsync-cli --root /chosen/prefix
```

`CARGO_INSTALL_ROOT` is the equivalent Cargo environment setting. The Windows
installer accepts `-InstallDir`. These use their package-manager conventions;
the repository does not supply a `make install` that interprets `DESTDIR`.
Before removing a paired installation run `clipsync daemon stop` and, if retiring
the device, `clipsync room leave`. Uninstalling a binary does not securely erase
clipboard history, inbox files, backups or private application data.

## Repeatability and reproducibility

The [Linux repeatability workflow](../.github/workflows/reproducible.yml) runs
[the comparison script](../scripts/check-repeatable-build.sh) in a digest-pinned
builder with Rust 1.100.0. It builds all workspace executables and mobile shared/
static libraries twice in separate fresh source and target directories, retains
environment/package versions and hashes, and fails on differing bytes. It shares
downloaded registry sources, not compiled objects. This is a scoped Linux check;
native packages are recorded rather than pinned, and it does not verify
distributed archive metadata or all release platforms. Keep the badge criteria
unmet until their full scope is demonstrated.

Release provenance and `Cargo.lock` establish origin and dependency integrity;
they do not prove bit-for-bit repeatability. The current multi-platform release
archives do not yet have a verified reproducible-build procedure. The compiler
and hosted OS images move, archive timestamps vary, and native signing can add
platform-specific metadata. Both Silver `build_repeatable` and Gold
`build_reproducible` remain unmet until the produced deliverables are verified.

For future verification, record the exact source commit, rustc/linker versions,
target, container/SDK digest and dependency checksums. Set a fixed
`SOURCE_DATE_EPOCH`, normalize absolute source paths with `--remap-path-prefix`,
and normalize archive ordering, timestamps, ownership and compression. Build
twice in fresh directories without reusing produced object files and compare
hashes of every distributed artifact. Then have an independent party repeat the
same procedure. Publish logs, hashes and any target-specific exceptions; a
successful cached rebuild alone is insufficient.

## Source-tag signatures

New security/important release tags must be SSH signed. The public key is
[release-signing.pub](keys/release-signing.pub), with fingerprint
`SHA256:PctnByIWeY8g6MvP2GQ/E9htrJ7Q736/6T0jQhMq+ic`.
The private key is held in the maintainer's 1Password signing setup, outside the
public software distribution service. The repository contains only the public
key. GitHub's Verified label depends on account key registration and is distinct
from a valid local signature.

After independently establishing trust in that fingerprint/key, verify:

```sh
git fetch origin tag v0.1.16
ssh-keygen -lf docs/keys/release-signing.pub
git -c gpg.ssh.allowedSignersFile=docs/keys/allowed_signers verify-tag v0.1.16
```

The allowed principal is `16333003+Jeon1691@users.noreply.github.com`. Verify the
tag's object digest against the intended release. A key fetched from a newly
untrusted checkout alone is not independent trust establishment. Public old tags
are not rewritten to manufacture a signed history. Archive attestation
verification is documented separately in [packaging](packaging.md).
