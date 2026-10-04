#!/bin/sh
# The checks main has to pass: run after merging and before pushing, against the current base.
#   scripts/check.sh            build and test the workspace and the app
#   scripts/check.sh --push     also fetch, refuse if origin/main moved, and push only on success
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

if [ "${1:-}" = "--push" ]; then
    git push origin HEAD:main
fi
echo "all checks passed"
