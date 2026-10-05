#!/bin/bash
# Keep the dino you use built from main: when main moves in the main checkout, app/build.sh
# --install replaces ~/Applications/Dino.app in the background once the new one is whole, and the
# running dino offers Restart to Update. Opt in with scripts/install-hooks.sh.
#
#   scripts/dev-rebuild.sh              (post-commit, post-merge) rebuild, if this is the main
#                                       checkout and it's on main; anywhere else, nothing
#   scripts/dev-rebuild.sh --sync REV   (scripts/check.sh --push, from any worktree) fast-forward
#                                       the main checkout's main to REV, which rebuilds as above;
#                                       nothing on CI or where the hooks aren't installed
#
# Signed with the identity in `git config dino.signingIdentity` ("Developer ID Application: Name
# (TEAMID)"), else as build.sh picks. `git config dino.signingKeychain NAME` unlocks the keychain
# NAME first (it locks at restart), with its password from the login keychain's item NAME, account
# "keychain". The build's output: app/build/build.log in the main checkout.
set -u
LOCK=${DINO_REBUILD_LOCK:-/tmp/dino-rebuild.lock}   # tests use their own

sync() {
    local rev=$1 common main state
    [ -z "${CI:-}" ] || return 0
    rev=$(git rev-parse --verify -q "$rev^{commit}") || return 0
    common=$(git rev-parse --path-format=absolute --git-common-dir) || return 0
    grep -qs dev-rebuild "$(git rev-parse --path-format=absolute --git-path hooks)/post-merge" || return 0
    main=$(git worktree list --porcelain | sed -n '1s/^worktree //p')
    if [ -z "$main" ] || [ "$(git -C "$main" rev-parse --path-format=absolute --git-dir 2>/dev/null)" != "$common" ]; then
        return 0   # a bare repository: no main checkout to build
    fi
    [ "$(git -C "$main" symbolic-ref -q --short HEAD)" = main ] || {
        echo "the dino you use isn't rebuilding: $main isn't on main"; return 0; }
    git -C "$main" merge-base --is-ancestor "$rev" HEAD && return 0   # already there, and built
    state=$(git -C "$main" status --porcelain --untracked-files=no)
    if [ -n "$state" ]; then
        echo "the dino you use isn't rebuilding: $main has uncommitted changes, so it stays at $(git -C "$main" log -1 --format=%h)"
        return 0
    fi
    if git -C "$main" merge -q --ff-only "$rev" 2>/dev/null; then
        echo "$main is at $(git -C "$main" log -1 --format=%h): rebuilding the dino you use (app/build/build.log)"
    else
        echo "the dino you use isn't rebuilding: main in $main can't fast-forward (git -C $main merge --ff-only $rev says why)"
    fi
}

build() {
    local root=$1 log holder pw built
    log="$root/app/build/build.log"
    # One build at a time: a newer commit waits for the running build, then builds what's checked
    # out, unless that's built already. A lock whose build died is taken over.
    while ! mkdir "$LOCK" 2>/dev/null; do
        holder=$(cat "$LOCK/pid" 2>/dev/null)
        if [ -n "$holder" ] && ! kill -0 "$holder" 2>/dev/null; then rm -rf "$LOCK"; continue; fi
        sleep 2
    done
    echo $$ >"$LOCK/pid"
    trap 'rm -rf "$LOCK"' EXIT
    built=$(git -C "$root" rev-parse HEAD)
    if [ "$built" = "$(cat "$root/app/build/.rebuilt" 2>/dev/null)" ] && git -C "$root" diff --quiet HEAD; then
        exit 0
    fi
    local kc; kc=$(git -C "$root" config dino.signingKeychain)
    if [ -n "$kc" ] && [ -f "$HOME/Library/Keychains/$kc.keychain-db" ] &&
        pw=$(security find-generic-password -s "$kc" -a keychain -w 2>/dev/null); then
        security unlock-keychain -p "$pw" "$HOME/Library/Keychains/$kc.keychain-db" 2>/dev/null
    fi
    unset pw
    local id; id=$(git -C "$root" config dino.signingIdentity)
    if [ -n "$id" ]; then export DEVELOPER_ID_APP="$id"; fi
    if "$root/app/build.sh" --install >"$log" 2>&1; then
        echo "$built" >"$root/app/build/.rebuilt"
        osascript -e "display notification \"Restart to Update in dino to use $(git -C "$root" log -1 --format=%h).\" with title \"dino rebuilt\"" 2>/dev/null
    else
        osascript -e 'display notification "See app/build/build.log" with title "dino build failed"' 2>/dev/null
    fi
}

case "${1:-}" in
    --sync) sync "${2:?usage: scripts/dev-rebuild.sh --sync REV}" ;;
    --build) build "$2" ;;
    "")
        [ "$(git symbolic-ref -q --short HEAD)" = main ] || exit 0
        [ "$(git rev-parse --path-format=absolute --git-dir)" = "$(git rev-parse --path-format=absolute --git-common-dir)" ] || exit 0   # a worktree
        root=$(git rev-parse --show-toplevel)
        mkdir -p "$root/app/build"
        unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE   # set for the hook; the build uses git -C
        nohup "$0" --build "$root" </dev/null >/dev/null 2>&1 &
        ;;
    *) echo "usage: scripts/dev-rebuild.sh [--sync REV]" >&2; exit 2 ;;
esac
exit 0
