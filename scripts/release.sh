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
# and, for notarization, the first of these that is set:
#   NOTARY_KEY, NOTARY_KEY_ID, NOTARY_ISSUER   an App Store Connect API key: the .p8 file, its key ID
#                      and issuer ID. No keychain is involved, so a locked screen doesn't stop it.
#   NOTARY_KEY_ENV     a file setting NOTARY_KEY_ID and NOTARY_ISSUER (or NOTARY_ISSUER_ID), and
#                      optionally NOTARY_KEY; the key defaults to AuthKey_<NOTARY_KEY_ID>.p8 beside it
#   NOTARY_PROFILE     a profile saved with `xcrun notarytool store-credentials`, which lives in the
#                      login keychain: notarytool can't read it while the screen is locked
#   APPLE_ID, TEAM_ID, APPLE_PASSWORD (app-specific password)
# Without them the app is signed ad hoc, which Gatekeeper refuses for downloads.
#   RELEASES_REPO      the GitHub repository the release is published in (default meetdino/dino, this
#                      one); the update feed, the appcast's downloads and the tap's URLs point at its releases
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
#   DINO_AGENT_HOME    the $DINO_HOME its dinod and the app run with (default: the user's own, ~/.config/dino)
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
. scripts/bundle.sh

VERSION="$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)"
BUILD="$(git rev-list --count HEAD 2>/dev/null || echo 1)"
BUILD_ID="$(dino_build_id "$ROOT")"
RELEASES_REPO="${RELEASES_REPO:-meetdino/dino}"
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

# Notarization's API key from a file, checked before the build: read as KEY=value lines, not run,
# and never printed.
if [ -n "${NOTARY_KEY_ENV:-}" ]; then
    [ -r "$NOTARY_KEY_ENV" ] || { echo "error: NOTARY_KEY_ENV $NOTARY_KEY_ENV isn't a readable file" >&2; exit 1; }
    while IFS='=' read -r k v || [ -n "$k" ]; do
        k="${k#export }"; k="${k// /}"; v="${v%$'\r'}"; v="${v#[\"\']}"; v="${v%[\"\']}"
        case "$k" in
            NOTARY_KEY_ID) NOTARY_KEY_ID="${NOTARY_KEY_ID:-$v}" ;;
            NOTARY_ISSUER|NOTARY_ISSUER_ID) NOTARY_ISSUER="${NOTARY_ISSUER:-$v}" ;;
            NOTARY_KEY) NOTARY_KEY="${NOTARY_KEY:-$v}" ;;
        esac
    done < "$NOTARY_KEY_ENV"
    NOTARY_KEY="${NOTARY_KEY:-$(dirname "$NOTARY_KEY_ENV")/AuthKey_${NOTARY_KEY_ID:-}.p8}"
    [ -r "$NOTARY_KEY" ] || { echo "error: no API key at $NOTARY_KEY (set NOTARY_KEY)" >&2; exit 1; }
fi

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
    DINO_UPDATE_PUBLIC_KEY="$PUBKEY" DINO_UPDATE_FEED_URL="$FEED_URL" DINO_BUILD="$BUILD_ID" \
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
/usr/libexec/PlistBuddy -c "Add :DinoBuild string $BUILD_ID" "$APP/Contents/Info.plist"

# dinod as the app's launch agent (scripts/bundle.sh).
dino_agent "$APP" "$BUNDLE_ID" "${DINO_AGENT_HOME:-}"
if [ -n "$PUBKEY" ]; then
    /usr/libexec/PlistBuddy -c "Add :SUPublicEDKey string $PUBKEY" "$APP/Contents/Info.plist"
else
    say "no DINO_RELEASE_KEY: this build won't update itself"
fi

# Signing (scripts/bundle.sh): a Developer ID and a timestamp when given, ad hoc otherwise.
say "signing (${DEVELOPER_ID_APP:-ad hoc})"
dino_sign "$APP" "${DEVELOPER_ID_APP:--}" --timestamp app/AdHoc.entitlements

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
if [ -n "${NOTARY_KEY:-}" ] && [ -n "${NOTARY_KEY_ID:-}" ] && [ -n "${NOTARY_ISSUER:-}" ]; then notary=(--key "$NOTARY_KEY" --key-id "$NOTARY_KEY_ID" --issuer "$NOTARY_ISSUER")
elif [ -n "${NOTARY_PROFILE:-}" ]; then notary=(--keychain-profile "$NOTARY_PROFILE")
elif [ -n "${APPLE_ID:-}" ] && [ -n "${TEAM_ID:-}" ] && [ -n "${APPLE_PASSWORD:-}" ]; then notary=(--apple-id "$APPLE_ID" --team-id "$TEAM_ID" --password "$APPLE_PASSWORD")
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
# A build for one architecture says so, and brew refuses it on other Macs instead of installing a
# dino that won't open there; a universal build runs on both.
if [ "$LABEL" = universal ]; then ARCH_LINE=(-e '/@ARCH@/d'); else ARCH_LINE=(-e "s|@ARCH@|$LABEL|g"); fi
fill() {
    sed -e "s|@VERSION@|$VERSION|g" -e "s|@DMG_SHA256@|$DMG_SHA|g" -e "s|@TAR_SHA256@|$TAR_SHA|g" \
        -e "s|@RELEASES_REPO@|$RELEASES_REPO|g" -e "s|@LABEL@|$LABEL|g" "${ARCH_LINE[@]}" "$1" > "$2"
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
# The commit this was built from, which publish.sh tags; only when nothing uncommitted went in.
case "$BUILD_ID" in ""|*-dirty*) ;; *) git rev-parse HEAD > "$DIST/commit" ;; esac
cp scripts/install.sh "$DIST/download/install.sh"

# The app stays here for publish.sh to check, but isn't one LaunchServices opens: `open -b`,
# Open With and dino's own links go to the dino you use (app/build.sh --install), not this one.
/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -u "$APP" 2>/dev/null || true

say "done: dino $VERSION"
ls -lh "$DMG" "$DIST/$TAR" | awk '{print "  " $5 "  " $9}'
echo "  publish with scripts/publish.sh (GitHub release in $RELEASES_REPO, then the tap)"
