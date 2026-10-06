#!/bin/bash
# Publish what scripts/release.sh built in dist/: a GitHub release in this repository, tagged at the
# commit it was built from, with the DMG, the CLI tarball, SHA256SUMS and the update files; the same
# release in the old releases repository, for the installs that still look there (the bridge,
# below); then the cask and formula in the tap.
#
#   scripts/publish.sh --dry-run    # print what it would do
#   scripts/publish.sh              # do it (asks first)
#
#   RELEASES_REPO   default meetdino/dino
#   BRIDGE_REPO     default meetdino/dino-releases; empty for none
#   TAP_REPO        default meetdino/homebrew-tap
# Needs `gh` signed in as an account that can push to all three.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DIST="${DIST:-$ROOT/dist}"
RELEASES_REPO="${RELEASES_REPO:-meetdino/dino}"
# TEMPORARY, the bridge (https://github.com/meetdino/dino/issues/63, "Retire dino-releases"). dino
# 0.1.5 and older, the app and an install.sh `dino` alike, read their feed from
# https://github.com/asdf9384/dino-releases/releases/latest/download/appcast.xml, which GitHub
# redirects to meetdino/dino-releases. So each release goes there too, as its latest: those installs
# find it, and the version they update to reads its feed from RELEASES_REPO. The appcast's downloads
# point at RELEASES_REPO either way; the copies there keep old links to its files working.
# Remove this, and the step below, once most installs are on a release published with it (the issue
# says how to tell); then archive meetdino/dino-releases. Never delete it, and never create a
# repository by that name or as asdf9384/dino-releases: either strands every install still on 0.1.5.
BRIDGE_REPO="${BRIDGE_REPO-meetdino/dino-releases}"
TAP_REPO="${TAP_REPO:-meetdino/homebrew-tap}"
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
    # Without an appcast, the bridge would leave the old installs' feed with none: they'd stay on what they have.
    if [ -n "$BRIDGE_REPO" ]; then echo "warning: so nothing goes to $BRIDGE_REPO either."; BRIDGE_REPO=""; fi
fi
# The tag goes on the commit the release was built from, which must be on main here.
COMMIT="$(cat "$DIST/commit" 2>/dev/null)" || { echo "error: dist/ wasn't built from a commit (it had uncommitted changes); a release is tagged at the commit it was built from" >&2; exit 1; }
case "$(gh api "repos/$RELEASES_REPO/compare/$COMMIT...main" --jq .status 2>/dev/null || true)" in
    ahead|identical) ;;
    *) echo "error: $COMMIT, the commit dist/ was built from, isn't on $RELEASES_REPO's main" >&2; [ "$DRY" = 1 ] || exit 1 ;;
esac
(cd "$DIST" && shasum -a 256 -c SHA256SUMS >/dev/null) || { echo "error: dist/SHA256SUMS doesn't match the files" >&2; exit 1; }
if ! codesign --verify --strict "$DIST/Dino.app" 2>/dev/null || ! spctl --assess --type execute "$DIST/Dino.app" 2>/dev/null; then
    echo "warning: Dino.app isn't signed with a Developer ID and notarized; Gatekeeper will block it for people who download it."
fi

echo "Publishing dino $VERSION"
echo "  release $TAG in $RELEASES_REPO at ${COMMIT:0:9}: $(basename "$DMG"), $(basename "$TAR"), SHA256SUMS${UPDATES[*]:+, $(basename "${UPDATES[0]}"), appcast.xml}, Dino.dmg"
[ -z "$BRIDGE_REPO" ] || echo "  the same in $BRIDGE_REPO, for installs of 0.1.5 and older (temporary)"
echo "  tap $TAP_REPO: Casks/dino.rb, Formula/dino-cli.rb"
if [ "$DRY" = 0 ]; then
    read -r -p "Go ahead? [y/N] " ok
    [ "$ok" = y ] || [ "$ok" = Y ] || exit 1
fi

for repo in "$RELEASES_REPO" $BRIDGE_REPO; do
    if gh release view "$TAG" --repo "$repo" >/dev/null 2>&1; then
        echo "error: $TAG already exists in $repo" >&2
        [ "$DRY" = 1 ] || exit 1
    fi
done
# A tag pushed by hand wins over --target: it has to be the same commit.
tagged="$(gh api "repos/$RELEASES_REPO/commits/refs/tags/$TAG" --jq .sha 2>/dev/null)" || tagged=""
if [ -n "$tagged" ] && [ "$tagged" != "$COMMIT" ]; then
    echo "error: $TAG in $RELEASES_REPO is $tagged, not $COMMIT" >&2
    [ "$DRY" = 1 ] || exit 1
fi
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
# The same DMG under a name that never changes, for the website's releases/latest/download link.
STABLE="$WORK/Dino.dmg"
[ "$DRY" = 1 ] || cp "$DMG" "$STABLE"
FILES=("$DMG" "$TAR" "$DIST/SHA256SUMS" ${UPDATES[@]+"${UPDATES[@]}"} "$STABLE")
# --latest, always: the feed and the download link are this release's only while it's the latest.
# gh publishes it once every file is up, so neither is ever missing from the latest.
run gh release create "$TAG" --repo "$RELEASES_REPO" --target "$COMMIT" --latest --title "dino $VERSION" \
    --notes "dino $VERSION. Install with \`brew install meetdino/tap/dino\`, or the command line alone with \`curl -fsSL https://meetdino.com/install.sh | sh\`." \
    "${FILES[@]}"

# TEMPORARY, the bridge (see BRIDGE_REPO above).
if [ -n "$BRIDGE_REPO" ]; then
    run gh release create "$TAG" --repo "$BRIDGE_REPO" --latest --title "dino $VERSION" \
        --notes "dino $VERSION. dino's releases are at https://github.com/$RELEASES_REPO/releases now; this copy is for installs of dino 0.1.5 and older, which look for updates here." \
        "${FILES[@]}" || {
        echo "error: $TAG is out in $RELEASES_REPO but not in $BRIDGE_REPO, so installs of 0.1.5 and older won't see it." >&2
        echo "Publish it there by hand, as --latest, with the same files as in $RELEASES_REPO (gh release download $TAG --repo $RELEASES_REPO)." >&2
        exit 1
    }
fi

TAP="$WORK/tap"
run gh repo clone "$TAP_REPO" "$TAP" -- --quiet --depth 1
if [ "$DRY" = 1 ]; then
    echo "  would copy dist/tap/ into the tap, commit \"dino $VERSION\" and push"
else
    cp -R "$DIST/tap/." "$TAP/"
    git -C "$TAP" add -A
    # As this repository's own identity, never the machine's global one (the tap is a fresh clone).
    git -C "$TAP" -c user.name="$(git -C "$ROOT" config user.name)" -c user.email="$(git -C "$ROOT" config user.email)" \
        commit -q -m "dino $VERSION"
    git -C "$TAP" push -q
fi
echo "done"
