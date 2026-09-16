class Clipsync < Formula
  desc "End-to-end encrypted multi-device clipboard sync"
  homepage "https://github.com/Jeon1691/clipsync"
  version "0.1.0"
  license any_of: ["MIT", "Apache-2.0"]

  livecheck do
    url :homepage
    regex(/^v?(\d+(?:\.\d+)+)$/i)
  end

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/Jeon1691/clipsync/releases/download/v0.1.0/clipsync-aarch64-apple-darwin.tar.gz"
      sha256 "8eef1f9cb1ee07c727b35ce3902154dbc6372f986ec918395d17a3193818f68a"
    else
      url "https://github.com/Jeon1691/clipsync/releases/download/v0.1.0/clipsync-x86_64-apple-darwin.tar.gz"
      sha256 "5fd8b222c51c4bc050d3a062dfc59b65682888348189cd21806873c27005e6b0"
    end
  end

  on_linux do
    url "https://github.com/Jeon1691/clipsync/releases/download/v0.1.0/clipsync-x86_64-unknown-linux-gnu.tar.gz"
    sha256 "ca9fb561070cb5be69237338cea3e098d78a4c05dd5cb945f70e73bbe1cf2e45"
  end

  def install
    binary = File.exist?("clipsync") ? "clipsync" : Dir["clipsync-*/clipsync"].first
    odie "clipsync binary missing from archive" if binary.nil?
    bin.install binary
  end

  def caveats
    <<~EOS
      Default relay is https://clipsync.develicit.dev

        clipsync init
        clipsync room create
    EOS
  end

  test do
    assert_match "Usage: clipsync", shell_output("#{bin}/clipsync --help")
  end
end
