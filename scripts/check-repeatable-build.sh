#!/usr/bin/env bash
# Copyright the ClipSync contributors.
# SPDX-License-Identifier: MIT
# Compare Linux outputs from committed HEAD in two fresh source/target trees.
# This checks repeatability inside one recorded environment, not all release
# archives, independently provisioned environments, or macOS/Windows artifacts.
set -euo pipefail

repeat_single=0
if [[ "${1:-}" == --single ]]; then
  repeat_single=1
  shift
fi
if [[ $# -gt 1 ]]; then
  echo "usage: $0 [--single] [report-directory]" >&2
  exit 2
fi
if [[ "$(uname -s)" != Linux ]]; then
  echo "This check requires Linux and GNU build utilities." >&2
  exit 2
fi

repeat_repo="$(git rev-parse --show-toplevel)"
repeat_report="${1:-$repeat_repo/target/repeatability-report}"
mkdir -p "$repeat_report"
repeat_report="$(cd "$repeat_report" && pwd)"
repeat_root="$(mktemp -d /tmp/clipsync-repeatable.XXXXXXXX)"
cleanup() {
  # Remove only the directory this invocation created with mktemp.
  if [[ -n "$repeat_root" && "$repeat_root" == /tmp/clipsync-repeatable.* ]]; then
    rm -rf -- "$repeat_root"
  fi
}
trap cleanup EXIT

export RUSTUP_TOOLCHAIN="${RUSTUP_TOOLCHAIN:-1.100.0}"
export SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-$(git -C "$repeat_repo" show -s --format=%ct HEAD)}"
if [[ ! "$SOURCE_DATE_EPOCH" =~ ^[0-9]+$ ]]; then
  echo "SOURCE_DATE_EPOCH must contain a Unix timestamp." >&2
  exit 2
fi
export TZ=UTC
export LC_ALL=C.UTF-8
export CARGO_INCREMENTAL=0
repeat_cargo_home="${CARGO_HOME:-$HOME/.cargo}"
mkdir -p "$repeat_cargo_home"
repeat_cargo_home="$(cd "$repeat_cargo_home" && pwd)"
export CARGO_HOME="$repeat_cargo_home"
repeat_sysroot="$(rustc --print sysroot)"
# Cargo gives encoded flags precedence over whitespace-separated RUSTFLAGS.
repeat_base_flags="$(python3 - <<'PY'
import os
print(os.environ.get("CARGO_ENCODED_RUSTFLAGS",
                     "\x1f".join(os.environ.get("RUSTFLAGS", "").split())), end="")
PY
)"

{
  echo "Scope: Linux workspace executables and mobile shared/static libraries."
  echo "Source: committed HEAD; uncommitted changes are excluded."
  echo "Commit: $(git -C "$repeat_repo" rev-parse HEAD)"
  echo "SOURCE_DATE_EPOCH=$SOURCE_DATE_EPOCH"
  echo "Container: ${REPEATABILITY_CONTAINER:-caller-provided environment}"
  echo "Native packages below are recorded; Dockerfile.reproducible freezes the reconstruction environment."
  echo "No compiled objects are shared between the two builds."
  uname -a
  rustc -vV
  cargo -vV
  cc --version
  ld --version
} > "$repeat_report/environment.txt"
if command -v dpkg-query >/dev/null; then
  dpkg-query -W > "$repeat_report/native-packages.txt"
fi

git -C "$repeat_repo" archive --format=tar HEAD > "$repeat_report/source.tar"
repeat_builds=(first second)
if [[ "$repeat_single" == 1 ]]; then repeat_builds=(first); fi
for repeat_build in "${repeat_builds[@]}"; do
  mkdir -p "$repeat_root/$repeat_build/source" "$repeat_root/$repeat_build/target"
  tar -xf "$repeat_report/source.tar" -C "$repeat_root/$repeat_build/source"
  find "$repeat_root/$repeat_build/source" -exec touch -h -d "@$SOURCE_DATE_EPOCH" {} +
done

(
  cd "$repeat_root/first/source"
  cargo fetch --locked
  cargo metadata --no-deps --locked --format-version 1
) > "$repeat_report/metadata.json" 2> "$repeat_report/fetch.log"
python3 - "$repeat_report/metadata.json" > "$repeat_report/artifacts.txt" <<'PY'
import json
import sys
with open(sys.argv[1], encoding="utf-8") as metadata_file:
    metadata = json.load(metadata_file)
members = set(metadata["workspace_members"])
outputs = set()
for package in metadata["packages"]:
    if package["id"] not in members:
        continue
    for target in package["targets"]:
        if "bin" in target["kind"]:
            outputs.add(target["name"])
        name = target["name"].replace("-", "_")
        if "cdylib" in target["crate_types"]:
            outputs.add(f"lib{name}.so")
        if "staticlib" in target["crate_types"]:
            outputs.add(f"lib{name}.a")
if not outputs:
    raise SystemExit("No workspace production outputs found.")
print("\n".join(sorted(outputs)))
PY

for repeat_build in "${repeat_builds[@]}"; do
  (
    repeat_tree="$repeat_root/$repeat_build"
    cd "$repeat_tree/source"
    repeat_remaps="--remap-path-prefix=$repeat_tree=/build"$'\x1f'
    repeat_remaps+="--remap-path-prefix=$repeat_cargo_home=/cargo"$'\x1f'
    repeat_remaps+="--remap-path-prefix=$repeat_sysroot=/rust-toolchain"
    export CARGO_ENCODED_RUSTFLAGS="${repeat_base_flags:+$repeat_base_flags$'\x1f'}$repeat_remaps"
    # Native C/C++ dependencies can contain source paths too. The pinned CI
    # container uses space-free paths; retain any caller-provided compiler flags.
    repeat_native_flags="-ffile-prefix-map=$repeat_tree=/build -fdebug-prefix-map=$repeat_tree=/build"
    repeat_native_flags+=" -ffile-prefix-map=$repeat_cargo_home=/cargo -fdebug-prefix-map=$repeat_cargo_home=/cargo"
    export CFLAGS="${CFLAGS:-} $repeat_native_flags"
    export CXXFLAGS="${CXXFLAGS:-} $repeat_native_flags"
    # Prevent compiler wrappers/caches from sharing compiled objects.
    export RUSTC_WRAPPER=""
    export RUSTC_WORKSPACE_WRAPPER=""
    export CCACHE_DISABLE=1
    export SCCACHE_DISABLE=1
    cargo build --release --workspace --all-features --locked --offline \
      --target-dir "$repeat_tree/target"
  ) 2>&1 | tee "$repeat_report/build-$repeat_build.log"
done

repeat_result=0
: > "$repeat_report/first.sha256"
: > "$repeat_report/second.sha256"
: > "$repeat_report/comparison.txt"
while IFS= read -r repeat_artifact; do
  repeat_first="$repeat_root/first/target/release/$repeat_artifact"
  repeat_second="$repeat_root/second/target/release/$repeat_artifact"
  if [[ "$repeat_single" == 1 ]]; then
    test -f "$repeat_first"
    (cd "$repeat_root/first/target/release" && sha256sum "$repeat_artifact") >> "$repeat_report/first.sha256"
    continue
  fi
  if [[ ! -f "$repeat_first" || ! -f "$repeat_second" ]]; then
    echo "Missing required output: $repeat_artifact" | tee -a "$repeat_report/comparison.txt"
    repeat_result=1
    continue
  fi
  (cd "$repeat_root/first/target/release" && sha256sum "$repeat_artifact") >> "$repeat_report/first.sha256"
  (cd "$repeat_root/second/target/release" && sha256sum "$repeat_artifact") >> "$repeat_report/second.sha256"
  if cmp -s "$repeat_first" "$repeat_second"; then
    echo "Identical: $repeat_artifact" | tee -a "$repeat_report/comparison.txt"
  else
    echo "DIFFERS: $repeat_artifact" | tee -a "$repeat_report/comparison.txt"
    repeat_result=1
    mkdir -p "$repeat_report/differences/first" "$repeat_report/differences/second"
    cp "$repeat_first" "$repeat_report/differences/first/"
    cp "$repeat_second" "$repeat_report/differences/second/"
    # The comparison above determines failure; this bounded byte diff is diagnostic.
    cmp -l "$repeat_first" "$repeat_second" | head -n 40 \
      > "$repeat_report/differences/$repeat_artifact.bytes.txt" || true
  fi
done < "$repeat_report/artifacts.txt"

echo "Report: $repeat_report"
echo "This result does not verify release archive or cross-platform reproducibility."
exit "$repeat_result"
