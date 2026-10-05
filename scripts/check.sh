#!/bin/sh
# The checks main has to pass: run after merging and before pushing, against the current base.
#   scripts/check.sh            build and test the workspace and the app
#   scripts/check.sh --push     also fetch, refuse if origin/main moved, and push only on success;
#                               then, where scripts/install-hooks.sh ran, the main checkout's main
#                               follows and the dino you use rebuilds (scripts/dev-rebuild.sh)
set -eu
cd "$(dirname "$0")/.."

if [ "${1:-}" = "--push" ]; then
    git fetch -q origin
    if ! git merge-base --is-ancestor origin/main HEAD; then
        echo "origin/main has commits this branch doesn't: merge them first, then run this again" >&2
        exit 1
    fi
fi

cargo build --release
cargo test --workspace --release
(cd app && swift build -c release)
if git grep -n 'TEST-ONLY' -- crates app cloud >/dev/null; then
    echo "TEST-ONLY code is still in the tree" >&2
    exit 1
fi
# No DisclosureGroup in the sidebar's list: the list expanded an open one from inside making its
# row and lost the row view of a row below, left drawn over another row (see OpeningRows).
if git grep -n 'DisclosureGroup(' -- app/Sources/Dino/Tree.swift app/Sources/Dino/DinoApp.swift app/Sources/Dino/Schedule.swift; then
    echo "the sidebar lists rows that open with OpeningRows, not DisclosureGroup" >&2
    exit 1
fi

if [ "${1:-}" = "--push" ]; then
    git push origin HEAD:main
    scripts/dev-rebuild.sh --sync HEAD || true
fi
echo "all checks passed"
