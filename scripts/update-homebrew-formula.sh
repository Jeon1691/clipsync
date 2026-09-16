#!/usr/bin/env bash
# Generate Formula/clipsync.rb from GitHub release checksums and push to the tap.
set -euo pipefail

OWNER="${HOMEBREW_TAP_OWNER:-Jeon1691}"
TAP_REPO="${HOMEBREW_TAP_REPO:-homebrew-tap}"
SRC_REPO="${CLIPSYNC_REPO:-Jeon1691/clipsync}"
VERSION_RAW="${VERSION:-${1:-}}"
if [[ -z "$VERSION_RAW" ]]; then
  echo "USAGE: VERSION=v0.1.0 $0" >&2
  exit 2
fi
VERSION="${VERSION_RAW#v}"
TAG="v${VERSION}"

command -v gh >/dev/null || { echo "missing gh" >&2; exit 1; }
command -v python3 >/dev/null || { echo "missing python3" >&2; exit 1; }

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
gh release download "$TAG" --repo "$SRC_REPO" --pattern "clipsync-*.sha256" --dir "$tmp"

python3 - "$tmp" "$SRC_REPO" "$VERSION" "/tmp/clipsync.rb" <<'PY'
import pathlib, sys
d, src_repo, version, out = sys.argv[1:]
shas = {}
for p in pathlib.Path(d).glob("clipsync-*.sha256"):
    target = p.name[len("clipsync-"):-len(".sha256")]
    shas[target] = p.read_text().split()[0]
base = f"https://github.com/{src_repo}/releases/download/v{version}"
need = ["aarch64-apple-darwin", "x86_64-apple-darwin", "x86_64-unknown-linux-gnu"]
missing = [t for t in need if t not in shas]
if missing:
    raise SystemExit(f"missing checksums: {missing}")

linux_arm = shas.get("aarch64-unknown-linux-gnu")
if linux_arm:
    linux = f'''  on_linux do
    if Hardware::CPU.arm?
      url "{base}/clipsync-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "{linux_arm}"
    else
      url "{base}/clipsync-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "{shas["x86_64-unknown-linux-gnu"]}"
    end
  end'''
else:
    linux = f'''  on_linux do
    url "{base}/clipsync-x86_64-unknown-linux-gnu.tar.gz"
    sha256 "{shas["x86_64-unknown-linux-gnu"]}"
  end'''

formula = f'''class Clipsync < Formula
  desc "End-to-end encrypted multi-device clipboard sync"
  homepage "https://github.com/{src_repo}"
  version "{version}"
  license any_of: ["MIT", "Apache-2.0"]

  livecheck do
    url :homepage
    regex(/^v?(\\d+(?:\\.\\d+)+)$/i)
  end

  on_macos do
    if Hardware::CPU.arm?
      url "{base}/clipsync-aarch64-apple-darwin.tar.gz"
      sha256 "{shas["aarch64-apple-darwin"]}"
    else
      url "{base}/clipsync-x86_64-apple-darwin.tar.gz"
      sha256 "{shas["x86_64-apple-darwin"]}"
    end
  end

{linux}

  def install
    bin.install "clipsync"
  end

  def caveats
    <<~EOS
      Default relay is https://clipsync.develicit.dev

        clipsync init
        clipsync room create
    EOS
  end

  test do
    assert_match "Usage: clipsync", shell_output("#{{bin}}/clipsync --help")
  end
end
'''
formula = formula.replace("#{{bin}}", "#{bin}")
pathlib.Path(out).write_text(formula)
print(formula)
PY

work="$(mktemp -d)"
trap 'rm -rf "$tmp" "$work"' EXIT
gh repo clone "${OWNER}/${TAP_REPO}" "$work" -- --depth 1
mkdir -p "$work/Formula"
cp /tmp/clipsync.rb "$work/Formula/clipsync.rb"
git -C "$work" add Formula/clipsync.rb
if git -C "$work" diff --cached --quiet; then
  echo "formula unchanged"
  exit 0
fi
git -C "$work" \
  -c user.name="clipsync-release" \
  -c user.email="clipsync@users.noreply.github.com" \
  commit -m "clipsync ${TAG}"
git -C "$work" push origin HEAD
echo "updated ${OWNER}/${TAP_REPO} Formula/clipsync.rb to ${TAG}"
