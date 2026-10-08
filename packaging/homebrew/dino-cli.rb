# frozen_string_literal: true

# dino's command-line client and dinod, without the terminal app.
class DinoCli < Formula
  desc "Command-line client and daemon for the dino terminal"
  homepage "https://meetdino.com/"
  url "https://github.com/@RELEASES_REPO@/releases/download/v@VERSION@/dino-@VERSION@-darwin-@LABEL@.tar.gz"
  version "@VERSION@"
  sha256 "@TAR_SHA256@"
  license "MIT"

  livecheck do
    url :stable
    strategy :github_latest
  end

  depends_on arch: :@ARCH@
  depends_on :macos

  conflicts_with cask: "dino", because: "the dino app includes the dino command"

  def install
    bin.install "dino"
    # Tab completion, in the folders bash, zsh and fish load completions from.
    generate_completions_from_executable(bin/"dino", "completions")
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dino --version")
  end
end
