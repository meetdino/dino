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
# Hardened runtime: no DYLD_INSERT_LIBRARIES or other injection into dino, which holds your
# folder and automation grants. The only code is the main executable (the .bundle holds data),
# so no --deep; sign anything nested separately, first, if that changes.
codesign --force --options runtime --sign - "$APP" >/dev/null
echo "$PWD/$APP"
