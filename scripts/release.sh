#!/bin/bash
# Build a dino release into dist/ ($DIST to build elsewhere): Dino.app (with the dino CLI and dinod inside), a DMG, a CLI
# tarball, SHA-256 sums, Homebrew cask/formula files, and a download/ tree laid out the way
# scripts/install.sh fetches it. Nothing is uploaded.
#
#   scripts/release.sh                 # host architecture
#   ARCHS="arm64 x86_64" scripts/release.sh   # universal, when both Rust targets are installed
#
# Signing and notarization run only when their credentials are set:
#   DEVELOPER_ID_APP   "Developer ID Application: Name (TEAMID)", a codesigning identity in the keychain
#   NOTARY_PROFILE     a profile saved with `xcrun notarytool store-credentials`, or
#   APPLE_ID, TEAM_ID, APPLE_PASSWORD (app-specific password), or
#   NOTARY_KEY, NOTARY_KEY_ID, NOTARY_ISSUER (App Store Connect API key, for CI)
# Without them the app is signed ad hoc, which Gatekeeper refuses for downloads.
#   RELEASES_REPO      the binaries-only GitHub repository the files are published to
#                      (default asdf9384/dino-releases); the tap's URLs point at its releases
#
# Updates (the app through Sparkle, an install.sh `dino` through dinod) need the release key:
#   DINO_RELEASE_KEY   its private half, a file outside any repository (scripts/release-key.swift).
#                      With it the release carries Dino-<version>-<arch>.zip and appcast.xml, both
#                      signed; without it the build doesn't update itself, and there's no appcast.
#   DINO_FEED_URL      where builds look for the appcast (default: the newest release's, in RELEASES_REPO)
#   DINO_UPDATE_BASE   where the appcast says the files are (default: this release's downloads)
#
# A second, isolated dino (testing a release build beside the real one):
#   DINO_BUNDLE_ID     the app's bundle identifier (default dev.dino.app), which names its launch agent
#   DINO_AGENT_HOME    the $DINO_HOME its dinod runs with (default: the user's own, ~/.config/dino)
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

VERSION="$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)"
BUILD="$(git rev-list --count HEAD 2>/dev/null || echo 1)"
RELEASES_REPO="${RELEASES_REPO:-asdf9384/dino-releases}"
BUNDLE_ID="${DINO_BUNDLE_ID:-dev.dino.app}"
ARCHS="${ARCHS:-$(uname -m)}"
DIST="${DIST:-$ROOT/dist}"
APP="$DIST/Dino.app"
say() { printf '\033[1m==> %s\033[0m\n' "$*"; }

KEY="${DINO_RELEASE_KEY:-}"
PUBKEY=""
if [ -n "$KEY" ]; then PUBKEY="$(swift scripts/release-key.swift public "$KEY")"; fi
FEED_URL="${DINO_FEED_URL:-https://github.com/$RELEASES_REPO/releases/latest/download/appcast.xml}"
UPDATE_BASE="${DINO_UPDATE_BASE:-https://github.com/$RELEASES_REPO/releases/download/v$VERSION}"

rust_target() { case "$1" in arm64) echo aarch64-apple-darwin ;; x86_64) echo x86_64-apple-darwin ;; *) echo "unknown arch $1" >&2; exit 1 ;; esac; }
read -r -a arch_list <<<"$ARCHS"
if [ "${#arch_list[@]}" -gt 1 ]; then LABEL=universal; else LABEL="${arch_list[0]}"; fi

rm -rf "$DIST"
mkdir -p "$DIST"

say "dino $VERSION ($BUILD) for $ARCHS"

# The CLI and dinod: one binary, one per architecture, joined with lipo.
bins=()
for a in "${arch_list[@]}"; do
    t="$(rust_target "$a")"
    # No build machine paths (home folder, checkout) in what ships.
    # dinod checks the updates it installs with the release key's public half, and only with it.
    DINO_UPDATE_PUBLIC_KEY="$PUBKEY" DINO_UPDATE_FEED_URL="$FEED_URL" \
    RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$HOME/.cargo=cargo --remap-path-prefix=$PWD=dino" \
        cargo build --release -q -p dino --target "$t"
    bins+=("target/$t/release/dino")
done
if [ "${#bins[@]}" -gt 1 ]; then
    lipo -create -output "$DIST/dino" "${bins[@]}"
else
    cp "${bins[0]}" "$DIST/dino"
fi
strip -x "$DIST/dino" 2>/dev/null || true

# The app.
say "Dino.app"
swift_arch=()
for a in "${arch_list[@]}"; do swift_arch+=(--arch "$a"); done
(cd app && swift build -c release "${swift_arch[@]}" -q)
SWIFT_BIN="$(cd app && swift build -c release "${swift_arch[@]}" --show-bin-path)"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" "$APP/Contents/Helpers" "$APP/Contents/Frameworks"
cp "$SWIFT_BIN/Dino" "$APP/Contents/MacOS/Dino"
cp -R "$SWIFT_BIN/Sparkle.framework" "$APP/Contents/Frameworks/"
# Only the bundle's own Frameworks: no build machine paths to look in.
for r in $(otool -l "$APP/Contents/MacOS/Dino" | awk '/LC_RPATH/{getline; getline; print $2}' | grep '^/' || true); do
    install_name_tool -delete_rpath "$r" "$APP/Contents/MacOS/Dino"
done
cp -R "$SWIFT_BIN"/*.bundle "$APP/Contents/Resources/" 2>/dev/null || true
cp app/AppIcon.icns "$APP/Contents/Resources/"
cp "$DIST/dino" "$APP/Contents/Helpers/dino"
cp app/Info.plist "$APP/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $VERSION" -c "Set :CFBundleVersion $BUILD" "$APP/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :SUFeedURL $FEED_URL" "$APP/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleIdentifier $BUNDLE_ID" "$APP/Contents/Info.plist"

# dinod as the app's launch agent, registered with SMAppService (app/Sources/Dino/LaunchAgent.swift):
# what it runs then has the permissions given to the app. Restarted by launchd if it crashes, not
# otherwise (`dino stop` stays stopped), and not started at login: dino starts it when used, as before.
# Started again 2 s after it last started at the soonest, not launchd's 10: `dino stop` then a start.
LABEL_AGENT="$BUNDLE_ID.dinod"
AGENT_ENV=""
if [ -n "${DINO_AGENT_HOME:-}" ]; then
    LABEL_AGENT="$LABEL_AGENT.$(printf %s "$DINO_AGENT_HOME" | shasum -a 256 | cut -c1-8)"
    AGENT_ENV="<key>DINO_HOME</key><string>$DINO_AGENT_HOME</string>"
fi
mkdir -p "$APP/Contents/Library/LaunchAgents"
cat > "$APP/Contents/Library/LaunchAgents/$LABEL_AGENT.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>$LABEL_AGENT</string>
    <key>BundleProgram</key><string>Contents/Helpers/dino</string>
    <key>ProgramArguments</key><array><string>dino</string><string>daemon</string></array>
    <key>AssociatedBundleIdentifiers</key><array><string>$BUNDLE_ID</string></array>
    <key>EnvironmentVariables</key>
    <dict>
        <key>DINO_LAUNCHD</key><string>$LABEL_AGENT</string>$AGENT_ENV
    </dict>
    <key>KeepAlive</key><dict><key>Crashed</key><true/></dict>
    <key>ThrottleInterval</key><integer>2</integer>
    <key>ProcessType</key><string>Interactive</string>
    <key>AbandonProcessGroup</key><true/>
</dict>
</plist>
PLIST
plutil -lint -s "$APP/Contents/Library/LaunchAgents/$LABEL_AGENT.plist"
if [ -n "$PUBKEY" ]; then
    /usr/libexec/PlistBuddy -c "Add :SUPublicEDKey string $PUBKEY" "$APP/Contents/Info.plist"
else
    say "no DINO_RELEASE_KEY: this build won't update itself"
fi

# Signing: inside out, never --deep (Apple's guidance), always with the hardened runtime as
# app/build.sh does (no library injection into dino); a Developer ID and a timestamp when given,
# ad hoc otherwise.
sign() {
    if [ -n "${DEVELOPER_ID_APP:-}" ]; then
        codesign --force --options runtime --timestamp --sign "$DEVELOPER_ID_APP" "$@"
    else
        codesign --force --options runtime --sign - "$@"
    fi
}
say "signing (${DEVELOPER_ID_APP:-ad hoc})"
while IFS= read -r -d '' f; do
    if file -b "$f" | grep -q 'Mach-O'; then sign "$f"; fi
done < <(find "$APP/Contents/Resources" -type f -print0)
# Sparkle, inside out, as its documentation lists.
SPK="$APP/Contents/Frameworks/Sparkle.framework/Versions/B"
sign "$SPK/XPCServices/Installer.xpc"
sign --preserve-metadata=entitlements "$SPK/XPCServices/Downloader.xpc"
sign "$SPK/Autoupdate"
sign "$SPK/Updater.app"
sign "$APP/Contents/Frameworks/Sparkle.framework"
sign "$APP/Contents/Helpers/dino"
if [ -n "${DEVELOPER_ID_APP:-}" ]; then
    sign "$APP"
else
    # Ad hoc: no Team ID for the framework to share with the app (app/AdHoc.entitlements).
    sign --entitlements app/AdHoc.entitlements "$APP"
fi
codesign --verify --strict --verbose=1 "$APP"

# The DMG: the app beside a link to /Applications.
say "DMG"
DMG="$DIST/Dino-$VERSION-$LABEL.dmg"
STAGE="$DIST/dmg"
mkdir -p "$STAGE"
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
hdiutil create -quiet -volname "dino $VERSION" -srcfolder "$STAGE" -fs HFS+ -format UDZO -imagekey zlib-level=9 -ov "$DMG"
rm -rf "$STAGE"
if [ -n "${DEVELOPER_ID_APP:-}" ]; then codesign --force --timestamp --sign "$DEVELOPER_ID_APP" "$DMG"; fi

notary=()
if [ -n "${NOTARY_PROFILE:-}" ]; then notary=(--keychain-profile "$NOTARY_PROFILE")
elif [ -n "${APPLE_ID:-}" ] && [ -n "${TEAM_ID:-}" ] && [ -n "${APPLE_PASSWORD:-}" ]; then notary=(--apple-id "$APPLE_ID" --team-id "$TEAM_ID" --password "$APPLE_PASSWORD")
elif [ -n "${NOTARY_KEY:-}" ] && [ -n "${NOTARY_KEY_ID:-}" ] && [ -n "${NOTARY_ISSUER:-}" ]; then notary=(--key "$NOTARY_KEY" --key-id "$NOTARY_KEY_ID" --issuer "$NOTARY_ISSUER")
fi
if [ -n "${DEVELOPER_ID_APP:-}" ] && [ "${#notary[@]}" -gt 0 ]; then
    say "notarizing"
    xcrun notarytool submit "$DMG" "${notary[@]}" --wait
    xcrun stapler staple "$DMG"
    xcrun stapler validate "$DMG"
    spctl --assess --type open --context context:primary-signature --verbose "$DMG"
else
    say "not notarized: set DEVELOPER_ID_APP and notary credentials to notarize"
fi

# The CLI on its own, for scripts/install.sh and the formula.
say "CLI tarball"
TAR="dino-$VERSION-darwin-$LABEL.tar.gz"
CLI="$DIST/cli"
mkdir -p "$CLI"
cp "$APP/Contents/Helpers/dino" "$CLI/dino"
cp LICENSE "$CLI/"
tar -C "$CLI" -czf "$DIST/$TAR" dino LICENSE
rm -rf "$CLI"

(cd "$DIST" && shasum -a 256 "$(basename "$DMG")" "$TAR" > SHA256SUMS)
DMG_SHA="$(shasum -a 256 "$DMG" | cut -d' ' -f1)"
TAR_SHA="$(shasum -a 256 "$DIST/$TAR" | cut -d' ' -f1)"

# Updates: the app as a zip for Sparkle, and the appcast naming it and the CLI tarball, each
# signed with the release key. The tarball's line is dino's own (`dino:cli`), which Sparkle skips.
if [ -n "$KEY" ]; then
    say "appcast"
    ZIP="Dino-$VERSION-$LABEL.zip"
    ditto -c -k --sequesterRsrc --keepParent "$APP" "$DIST/$ZIP"
    ZIP_SIG="$(swift scripts/release-key.swift sign "$KEY" "$DIST/$ZIP")"
    TAR_SIG="$(swift scripts/release-key.swift sign "$KEY" "$DIST/$TAR")"
    cat > "$DIST/appcast.xml" <<XML
<?xml version="1.0" encoding="utf-8"?>
<rss version="2.0" xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle" xmlns:dino="https://meetdino.com/xml-namespaces/dino">
  <channel>
    <title>dino</title>
    <item>
      <title>dino $VERSION</title>
      <pubDate>$(LC_ALL=C date -u '+%a, %d %b %Y %H:%M:%S +0000')</pubDate>
      <sparkle:version>$BUILD</sparkle:version>
      <sparkle:shortVersionString>$VERSION</sparkle:shortVersionString>
      <sparkle:minimumSystemVersion>14.0</sparkle:minimumSystemVersion>
      <link>https://github.com/$RELEASES_REPO/releases/tag/v$VERSION</link>
      <enclosure url="$UPDATE_BASE/$ZIP" length="$(stat -f %z "$DIST/$ZIP")" type="application/octet-stream" sparkle:edSignature="$ZIP_SIG"/>
      <dino:cli arch="$LABEL" url="$UPDATE_BASE/$TAR" sha256="$TAR_SHA" signature="$TAR_SIG"/>
    </item>
  </channel>
</rss>
XML
    (cd "$DIST" && shasum -a 256 "$ZIP" >> SHA256SUMS)
fi

# The tap: the cask for the app (CLI included) and the formula for the CLI alone, filled in.
mkdir -p "$DIST/tap/Casks" "$DIST/tap/Formula"
fill() {
    sed -e "s|@VERSION@|$VERSION|g" -e "s|@DMG_SHA256@|$DMG_SHA|g" -e "s|@TAR_SHA256@|$TAR_SHA|g" \
        -e "s|@RELEASES_REPO@|$RELEASES_REPO|g" -e "s|@LABEL@|$LABEL|g" "$1" > "$2"
}
fill packaging/homebrew/dino.rb "$DIST/tap/Casks/dino.rb"
fill packaging/homebrew/dino-cli.rb "$DIST/tap/Formula/dino-cli.rb"
cp packaging/homebrew/README.md "$DIST/tap/README.md"

# The same files laid out for a plain web server (install.sh's DINO_DOWNLOAD_BASE mode).
D="$DIST/download/dino/$VERSION"
mkdir -p "$D"
cp "$DMG" "$DIST/$TAR" "$DIST/SHA256SUMS" "$D/"
if [ -n "$KEY" ]; then cp "$DIST/$ZIP" "$DIST/appcast.xml" "$D/"; fi
echo "$VERSION" > "$DIST/download/dino/latest"
cp scripts/install.sh "$DIST/download/install.sh"

say "done: dino $VERSION"
ls -lh "$DMG" "$DIST/$TAR" | awk '{print "  " $5 "  " $9}'
echo "  publish with scripts/publish.sh (GitHub release in $RELEASES_REPO, then the tap)"
