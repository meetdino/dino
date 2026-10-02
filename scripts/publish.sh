#!/bin/bash
# Publish what scripts/release.sh built in dist/: a GitHub release with the DMG, the CLI tarball and
# SHA256SUMS in the binaries-only releases repository, then the cask and formula in the tap.
#
#   scripts/publish.sh --dry-run    # print what it would do
#   scripts/publish.sh              # do it (asks first)
#
#   RELEASES_REPO   default asdf9384/dino-releases
#   TAP_REPO        default asdf9384/homebrew-tap
# Needs `gh` signed in as an account that can push to both.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DIST="$ROOT/dist"
RELEASES_REPO="${RELEASES_REPO:-asdf9384/dino-releases}"
TAP_REPO="${TAP_REPO:-asdf9384/homebrew-tap}"
DRY=0
[ "${1:-}" = "--dry-run" ] && DRY=1

run() {
    if [ "$DRY" = 1 ]; then printf '  would run: %q' "$1"; shift; printf ' %q' "$@"; echo; else "$@"; fi
}

VERSION="$(cat "$DIST/download/dino/latest" 2>/dev/null)" || { echo "error: no release in dist/; run scripts/release.sh first" >&2; exit 1; }
TAG="v$VERSION"
DMG="$(ls "$DIST"/Dino-"$VERSION"-*.dmg)"
TAR="$(ls "$DIST"/dino-"$VERSION"-darwin-*.tar.gz)"
# The update files, when the release was built with the release key.
UPDATES=()
if [ -f "$DIST/appcast.xml" ]; then
    UPDATES=("$(ls "$DIST"/Dino-"$VERSION"-*.zip)" "$DIST/appcast.xml")
else
    echo "warning: no appcast.xml (built without DINO_RELEASE_KEY): installs of this release won't update themselves."
fi
(cd "$DIST" && shasum -a 256 -c SHA256SUMS >/dev/null) || { echo "error: dist/SHA256SUMS doesn't match the files" >&2; exit 1; }
if ! codesign --verify --strict "$DIST/Dino.app" 2>/dev/null || ! spctl --assess --type execute "$DIST/Dino.app" 2>/dev/null; then
    echo "warning: Dino.app isn't signed with a Developer ID and notarized; Gatekeeper will block it for people who download it."
fi

echo "Publishing dino $VERSION"
echo "  release $TAG in $RELEASES_REPO: $(basename "$DMG"), $(basename "$TAR"), SHA256SUMS${UPDATES[*]:+, $(basename "${UPDATES[0]}"), appcast.xml}"
echo "  tap $TAP_REPO: Casks/dino.rb, Formula/dino-cli.rb"
if [ "$DRY" = 0 ]; then
    read -r -p "Go ahead? [y/N] " ok
    [ "$ok" = y ] || [ "$ok" = Y ] || exit 1
fi

if gh release view "$TAG" --repo "$RELEASES_REPO" >/dev/null 2>&1; then
    echo "error: $TAG already exists in $RELEASES_REPO" >&2
    [ "$DRY" = 1 ] || exit 1
fi
run gh release create "$TAG" --repo "$RELEASES_REPO" --title "dino $VERSION" \
    --notes "dino $VERSION. Install with \`brew install asdf9384/tap/dino\`, or the command line alone with \`curl -fsSL https://meetdino.com/install.sh | sh\`." \
    "$DMG" "$TAR" "$DIST/SHA256SUMS" ${UPDATES[@]+"${UPDATES[@]}"}
# The same DMG under a name that never changes, for the website's releases/latest/download link.
STABLE="$(mktemp -d)/Dino.dmg"
[ "$DRY" = 1 ] || cp "$DMG" "$STABLE"
run gh release upload "$TAG" --repo "$RELEASES_REPO" "$STABLE"

TAP="$(mktemp -d)"
trap 'rm -rf "$TAP"' EXIT
run gh repo clone "$TAP_REPO" "$TAP" -- --quiet --depth 1
if [ "$DRY" = 1 ]; then
    echo "  would copy dist/tap/ into the tap, commit \"dino $VERSION\" and push"
else
    cp -R "$DIST/tap/." "$TAP/"
    git -C "$TAP" add -A
    git -C "$TAP" commit -q -m "dino $VERSION"
    git -C "$TAP" push -q
fi
echo "done"
