# frozen_string_literal: true

cask "dino" do
  version "@VERSION@"
  sha256 "@DMG_SHA256@"

  url "https://github.com/@RELEASES_REPO@/releases/download/v#{version}/Dino-#{version}-@LABEL@.dmg",
      verified: "github.com/@RELEASES_REPO@/"
  name "dino"
  desc "Terminal for the agent era, on Ghostty's core"
  homepage "https://meetdino.com/"

  livecheck do
    url :url
    strategy :github_latest
  end

  depends_on macos: :sonoma

  app "Dino.app"
  binary "#{appdir}/Dino.app/Contents/Helpers/dino"

  # dinod's launch agents: the app's (SMAppService; it goes with the app) and the CLI's.
  zap launchctl: ["dev.dino.app.dinod", "dev.dino.app.dinod-cli"],
      trash:     [
    "~/.config/dino",
    "~/Library/LaunchAgents/dev.dino.app.dinod-cli.plist",
    "~/Library/Preferences/dev.dino.app.plist",
    "~/Library/Saved Application State/dev.dino.app.savedState",
  ]
end
