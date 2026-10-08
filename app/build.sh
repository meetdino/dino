#!/bin/bash
# Build dino.app: the Swift shell (sidebar + Ghostty surfaces) around the Rust dino CLI/daemon.
#
#   app/build.sh --install   the dino you use every day, from this checkout: built as a release is
#                            (but for thin LTO: Cargo.toml's devapp profile), with the dino CLI and
#                            dinod inside it and dinod as its launch agent, and put in
#                            ~/Applications/Dino.app ($DINO_APP: elsewhere) once it's whole, or
#                            beside it for Restart to Update while dino runs from there.
#                            scripts/dev-rebuild.sh runs this when main moves.
#   app/build.sh             app/build/Dino.app, a build to try things in: "dino dev"
#                            (dev.dino.app.dev), which runs the dino on your PATH (or DINO_BIN)
#
# Both are signed with $DEVELOPER_ID_APP, else the keychain's Developer ID Application identity,
# else ad hoc ("-" asks for ad hoc). macOS keeps a privacy grant (Screen Recording, Accessibility…)
# for the bundle id and Team ID a Developer ID signature carries, so it outlasts every rebuild; an
# ad hoc build is a stranger each time.
#
# A second, isolated installed dino (testing this beside the one you use), as scripts/release.sh:
#   DINO_BUNDLE_ID   its bundle identifier (default dev.dino.app), which names its launch agent
#   DINO_AGENT_HOME  the $DINO_HOME its dinod and the app run with (default ~/.config/dino)
set -euo pipefail
cd "$(dirname "$0")"
ROOT="$(cd .. && pwd)"
. "$ROOT/scripts/bundle.sh"

INSTALL=false
case "${1:-}" in
    --install) INSTALL=true ;;
    "") ;;
    *) echo "usage: app/build.sh [--install]" >&2; exit 2 ;;
esac

BUILD_ID="$(dino_build_id "$ROOT")"
# Installed: the devapp profile (Cargo.toml), a quicker release. Alone: target/release/dino, the
# dino on your PATH that "dino dev" runs.
PROFILE=release
if $INSTALL; then PROFILE=devapp; fi
(cd .. && DINO_BUILD="$BUILD_ID" cargo build --profile "$PROFILE" -q)
swift build -c release
BIN=$(swift build -c release --show-bin-path)

if $INSTALL; then
    DEST="${DINO_APP:-$HOME/Applications/Dino.app}"
    # Put together out of sight (a hidden folder: neither Spotlight nor LaunchServices lists it),
    # and in place only once it's whole and signed: the dino running from DEST keeps working.
    APP=build/.install/Dino.app
else
    APP=build/Dino.app
fi
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" "$APP/Contents/Frameworks"
cp "$BIN/Dino" "$APP/Contents/MacOS/Dino"
cp -R "$BIN"/*.bundle "$APP/Contents/Resources/"
cp -R "$BIN/Sparkle.framework" "$APP/Contents/Frameworks/"
cp Info.plist "$APP/Contents/Info.plist"
[ -f AppIcon.icns ] && cp AppIcon.icns "$APP/Contents/Resources/"
# The licenses of what ships (scripts/third-party.py), which the About window shows.
cp ../THIRD_PARTY_NOTICES.md "$APP/Contents/Resources/"
PLIST="$APP/Contents/Info.plist"
VERSION="$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' ../Cargo.toml)"
# Numbered as a release from the same commit is: a dino installed from a release made earlier on
# main is never the newer one, to LaunchServices (`open -b`) or anything else.
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $VERSION" \
    -c "Set :CFBundleVersion $(git -C "$ROOT" rev-list --count HEAD 2>/dev/null || echo 1)" "$PLIST"
if [ -n "$BUILD_ID" ]; then /usr/libexec/PlistBuddy -c "Add :DinoBuild string $BUILD_ID" "$PLIST"; fi
# Never updated by Sparkle to a release: no feed (and, as in every build but a release, no key).
/usr/libexec/PlistBuddy -c "Delete :SUFeedURL" -c "Delete :SUEnableAutomaticChecks" "$PLIST"

if $INSTALL; then
    BUNDLE_ID="${DINO_BUNDLE_ID:-dev.dino.app}"
    /usr/libexec/PlistBuddy -c "Set :CFBundleIdentifier $BUNDLE_ID" "$PLIST"
    # The dino it was built with, run by the app and as dinod (scripts/bundle.sh: its launch agent).
    mkdir -p "$APP/Contents/Helpers"
    cp "../target/$PROFILE/dino" "$APP/Contents/Helpers/dino"
    dino_agent "$APP" "$BUNDLE_ID" "${DINO_AGENT_HOME:-}"
else
    # Its own identity: macOS keeps a privacy grant for the bundle id that got it, and the
    # app's settings, sessions shown and window under it. A build to try things in takes none of
    # them from the dino you use, and carries no dinod of its own to restart yours into.
    /usr/libexec/PlistBuddy -c "Set :CFBundleIdentifier dev.dino.app.dev" -c "Set :CFBundleName dino dev" -c "Set :CFBundleDisplayName dino dev" "$PLIST"
fi

if [ -n "${DEVELOPER_ID_APP+set}" ]; then
    IDENTITY="${DEVELOPER_ID_APP:--}"
else
    IDENTITY="$(security find-identity -v -p codesigning 2>/dev/null | sed -n 's/^ *[0-9]*) [0-9A-F]* "\(Developer ID Application: .*\)"$/\1/p' | head -1)"
    IDENTITY="${IDENTITY:--}"
fi
# A locked or missing keychain doesn't stop the build: ad hoc then, and the log says so.
if ! dino_sign "$APP" "$IDENTITY" --timestamp=none AdHoc.entitlements 2>build/sign.log; then
    echo "warning: couldn't sign with $IDENTITY ($(tail -1 build/sign.log)); signed ad hoc, so macOS asks again for its permissions" >&2
    dino_sign "$APP" - --timestamp=none AdHoc.entitlements
fi

# Whether anything runs from the app at $1: the app, dinod, or the dino CLI inside it.
running_from() {
    local f
    for f in "$1/Contents/MacOS/Dino" "$1/Contents/Helpers/dino"; do
        if [ -e "$f" ] && lsof -t -- "$f" >/dev/null 2>&1; then return 0; fi
    done
    return 1
}

if $INSTALL; then
    # In place only while nothing runs from DEST. macOS knows a running dino (the app, dinod and
    # every program in its terminals) by the bundle at the path it started from: moved aside, it's a
    # bare executable without dino's permissions (Screen Recording…), and deleted, it's no one, until
    # it restarts. So while dino runs, the new build waits beside it, in a hidden folder, and Restart
    # to Update puts it in place once dino and dinod have stopped (app/Sources/Dino/Updates.swift).
    mkdir -p "$(dirname "$DEST")"
    NEXT="$(dirname "$DEST")/.$(basename "$DEST").next"
    rm -rf "$NEXT"
    if running_from "$DEST"; then
        # There whole or not at all: the running dino looks when its folder changes.
        rm -rf "$NEXT.part"
        mkdir "$NEXT.part"
        mv "$APP" "$NEXT.part/$(basename "$DEST")"
        mv "$NEXT.part" "$NEXT"
        echo "$NEXT/$(basename "$DEST"): dino runs from $DEST, so Restart to Update installs it"
    else
        OLD="$(dirname "$DEST")/.$(basename "$DEST").old.$$"
        if [ -e "$DEST" ]; then mv "$DEST" "$OLD"; fi
        mv "$APP" "$DEST"
        rm -rf "$OLD"
        /System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f "$DEST" 2>/dev/null || true
        echo "$DEST"
    fi
else
    echo "$PWD/$APP"
fi
