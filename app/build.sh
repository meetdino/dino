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
cp Info.plist "$APP/Contents/Info.plist"
[ -f AppIcon.icns ] && cp AppIcon.icns "$APP/Contents/Resources/"
codesign --force --deep --sign - "$APP" >/dev/null
echo "$PWD/$APP"
