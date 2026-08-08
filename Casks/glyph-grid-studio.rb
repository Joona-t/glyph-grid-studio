cask "glyph-grid-studio" do
  version "0.1.9"
  sha256 "8936b4b5e2054ea5889f43dbf4689f7d596d18ae218e3eacd333e561aaa756ad"

  url "https://github.com/Joona-t/glyph-grid-studio/releases/download/v#{version}/Glyph-Grid-Studio-macOS.dmg"
  name "Glyph Grid Studio"
  desc "Real-time character-grid image renderer (GUI + CLI + MCP server)"
  homepage "https://github.com/Joona-t/glyph-grid-studio"

  depends_on macos: ">= :big_sur"

  app "Glyph Grid Studio.app"

  # Symlink the binary to /opt/homebrew/bin so `glyph-grid-studio render ...`
  # and `glyph-grid-studio mcp` work from any shell.
  binary "#{appdir}/Glyph Grid Studio.app/Contents/MacOS/glyph-grid-studio"

  zap trash: [
    "~/Library/Application Support/art.lovespark.glyph-grid-studio",
    "~/Library/Caches/art.lovespark.glyph-grid-studio",
    "~/Library/Preferences/art.lovespark.glyph-grid-studio.plist",
    "~/Library/WebKit/art.lovespark.glyph-grid-studio",
  ]
end
