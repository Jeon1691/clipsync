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

The [Linux reproducibility workflow](../.github/workflows/reproducible.yml)
reconstructs two environments independently from
[Dockerfile.reproducible](../Dockerfile.reproducible). The base image is pinned
by digest, Debian native packages resolve against authenticated dated snapshots,
and the Rust toolchain is the numbered 1.100.0 release. BuildKit layer caches are
disabled for each reconstruction. This freezes the native dependency closure as
well as the Cargo lockfile, addressing the earlier check's moving apt repository.
Snapshot metadata expiry is disabled deliberately; Debian archive signature and
package checksum verification remain enabled.

To reproduce a chosen committed checkout on a Docker host:

```sh
git checkout --detach <source-commit>
bash scripts/reproduce-linux-build.sh target/reproducibility-report
```

The wrapper exports committed source in a Git bundle, without local settings or
uncommitted changes. It builds the CLI, desktop, relay, bindgen, and mobile shared
and static libraries in each freshly created environment, using separate source,
Cargo and target directories. No downloaded Cargo sources or compiled objects
are shared between the two runs. Compilation uses `--locked --offline` after
fetching checksummed sources, a fixed source timestamp, normalized source paths,
and disabled compiler caches. Native package manifests and all six output hashes
must match. Reports retain the source commit, complete builder recipe, image
metadata, native package versions, toolchain/linker versions, fetch/build logs,
hashes and comparison result. A failure does not constitute reproducibility
evidence. The source and recipe identify a fixed native platform; comparing
different CPU architectures is outside this procedure's scope.

The first [repeatability run](https://github.com/Jeon1691/clipsync/actions/runs/43686133672)
already compared all six Linux outputs byte-for-byte in one environment. The
stronger dual-environment workflow now verifies reconstruction from the frozen
recipe; a successful run is required before claiming Gold `build_reproducible`.
An outside participant can run the same instructions for additional corroboration.
Release provenance establishes origin and integrity, not reproducibility.
Distributed archive timestamps, macOS signing and Windows packaging remain
outside this Linux output claim.

## Source-tag signatures

New security/important release tags must be SSH signed. The public key is
[release-signing.pub](keys/release-signing.pub), with fingerprint
`SHA256:PctnByIWeY8g6MvP2GQ/E9htrJ7Q736/6T0jQhMq+ic`.
The private key is held in the maintainer's 1Password signing setup, outside the
public software distribution service. The repository contains only the public
key. GitHub's Verified label depends on account key registration and is distinct
from a valid local signature. On 2026-11-15 the existing public key was also
registered as a GitHub **Signing Key** after owner reauthentication; current and
earlier signed PR commits were confirmed `verified: true`, `reason: valid`.

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
