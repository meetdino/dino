#!/bin/bash
# Build dino.app: the Swift shell (sidebar + Ghostty surfaces) around the Rust dino CLI/daemon.
set -euo pipefail
cd "$(dirname "$0")"
(cd .. && cargo build --release -q)
swift build -c release
BIN=$(swift build -c release --show-bin-path)
APP=build/Dino.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN/Dino" "$APP/Contents/MacOS/Dino"
cp -R "$BIN"/*.bundle "$APP/Contents/Resources/"
mkdir -p "$APP/Contents/Frameworks"
cp -R "$BIN/Sparkle.framework" "$APP/Contents/Frameworks/"
cp Info.plist "$APP/Contents/Info.plist"
# Its own identity: macOS pins a privacy grant (Screen Recording, Accessibility…) to the code that
# got it, and an ad hoc build's changes with every rebuild. Sharing the release's bundle id would
# leave the installed dino's grants pinned to a dev build that no longer exists.
/usr/libexec/PlistBuddy -c "Set :CFBundleIdentifier dev.dino.app.dev" -c "Set :CFBundleName dino dev" -c "Set :CFBundleDisplayName dino dev" "$APP/Contents/Info.plist"
[ -f AppIcon.icns ] && cp AppIcon.icns "$APP/Contents/Resources/"
# Hardened runtime: no DYLD_INSERT_LIBRARIES or other injection into dino, which holds your
# folder and automation grants. No --deep: nested code is signed first, inside out (Sparkle, as
# scripts/release.sh does it; this build has no release key, so it never updates itself).
codesign --force --options runtime --sign - "$APP/Contents/Frameworks/Sparkle.framework" >/dev/null
# Ad hoc, so the framework has no Team ID to match the app's: see AdHoc.entitlements.
codesign --force --options runtime --entitlements AdHoc.entitlements --sign - "$APP" >/dev/null
echo "$PWD/$APP"
