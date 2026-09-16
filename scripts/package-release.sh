#!/usr/bin/env bash
# Build and tar a clipsync release binary for the host or --target triple.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
target="${1:-$(rustc -vV | awk '/host:/{print $2}')}"
out="${ROOT}/dist"
mkdir -p "$out"
echo "building clipsync for ${target}"
if [[ "$target" == "$(rustc -vV | awk '/host:/{print $2}')" ]]; then
  cargo build --release --locked -p clipsync-cli
  bin="target/release/clipsync"
else
  rustup target add "$target"
  cargo build --release --locked --target "$target" -p clipsync-cli
  bin="target/${target}/release/clipsync"
fi
chmod +x "$bin"
case "$target" in
  *apple-darwin*)
    strip -x "$bin" || true
    codesign --sign - --force --timestamp=none "$bin" || true
    ;;
  *)
    strip "$bin" || true
    ;;
esac
stage="clipsync-${target}"
rm -rf "${out}/${stage}"
mkdir -p "${out}/${stage}"
cp "$bin" "${out}/${stage}/clipsync"
cp README.md LICENSE-MIT LICENSE-APACHE "${out}/${stage}/"
tar -C "$out" -czf "${out}/${stage}.tar.gz" "$stage"
(cd "$out" && shasum -a 256 "${stage}.tar.gz" | tee "${stage}.sha256")
echo "wrote ${out}/${stage}.tar.gz"
