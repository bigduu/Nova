# Homebrew formula for Nova — the canonical template.
#
# The Release workflow (.github/workflows/release.yml) REGENERATES this file in
# the tap repo (bigduu/homebrew-tap → Formula/nova.rb) on every version tag,
# filling in version / url / sha256. This copy is the source of truth and is
# what you commit to a brand-new tap to bootstrap it before the first release.
#
# One-time tap setup:
#   gh repo create bigduu/homebrew-tap --public
#   # then add a fine-grained PAT with Contents:write on that repo as the
#   # nova repo secret HOMEBREW_TAP_TOKEN (Settings → Secrets → Actions).
#
# Install:  brew install bigduu/tap/nova
class Nova < Formula
  desc "Computer Use MCP server — macOS desktop control for LLM agents"
  homepage "https://github.com/bigduu/Nova"
  version "0.1.0"
  url "https://github.com/bigduu/Nova/releases/download/v0.1.0/nova-v0.1.0-universal-apple-darwin.tar.gz"
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"
  license "MIT"
  depends_on :macos

  def install
    bin.install "nova"
  end

  test do
    system "#{bin}/nova", "--version"
  end
end
