# Template only. The published formula lives in Jeon1691/homebrew-tap.
# Generate it from a GitHub release:
#   VERSION=v0.1.0 ./scripts/update-homebrew-formula.sh
class Clipsync < Formula
  desc "End-to-end encrypted multi-device clipboard sync"
  homepage "https://github.com/Jeon1691/clipsync"
  version "0.1.0"
  license any_of: ["MIT", "Apache-2.0"]

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/Jeon1691/clipsync/releases/download/v0.1.0/clipsync-aarch64-apple-darwin.tar.gz"
      sha256 "REPLACE_ME"
    else
      url "https://github.com/Jeon1691/clipsync/releases/download/v0.1.0/clipsync-x86_64-apple-darwin.tar.gz"
      sha256 "REPLACE_ME"
    end
  end

  on_linux do
    url "https://github.com/Jeon1691/clipsync/releases/download/v0.1.0/clipsync-x86_64-unknown-linux-gnu.tar.gz"
    sha256 "REPLACE_ME"
  end

  def install
    bin.install "clipsync"
  end

  test do
    assert_match "Usage: clipsync", shell_output("#{bin}/clipsync --help")
  end
end

