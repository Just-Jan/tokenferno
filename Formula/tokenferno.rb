class Tokenferno < Formula
  desc "Real-time TUI showing token burn for Claude Code and Copilot CLI"
  homepage "https://github.com/Just-Jan/tokenferno"
  url "https://github.com/Just-Jan/tokenferno/archive/refs/tags/v0.1.0.tar.gz"
  sha256 "PLACEHOLDER_SHA256"
  license "MIT"
  head "https://github.com/Just-Jan/tokenferno.git", branch: "main"

  depends_on "rust" => :build

  def install
    system "cargo", "install", "--bin", "tokenferno", *std_cargo_args
  end

  test do
    assert_match "tokenferno #{version}", shell_output("#{bin}/tokenferno --version")
  end
end
