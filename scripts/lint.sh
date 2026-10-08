#!/bin/sh
# Checks on the tree itself, no build needed: scripts/check.sh runs them, and so does CI.
set -eu
cd "$(dirname "$0")/.."

if git grep -n 'TEST-ONLY' -- crates app cloud; then
    echo "TEST-ONLY code is still in the tree" >&2
    exit 1
fi
# No DisclosureGroup in the sidebar's list: the list expanded an open one from inside making its
# row and lost the row view of a row below, left drawn over another row (see OpeningRows).
if git grep -n 'DisclosureGroup(' -- app/Sources/Dino/Tree.swift app/Sources/Dino/DinoApp.swift app/Sources/Dino/Schedule.swift; then
    echo "the sidebar lists rows that open with OpeningRows, not DisclosureGroup" >&2
    exit 1
fi
# The Rust code is formatted with cargo fmt, per rustfmt.toml; cloud/ is a workspace of its own.
if ! cargo fmt --all --check || ! (cd cloud && cargo fmt --all --check); then
    echo "the Rust code isn't formatted: run cargo fmt --all, and in cloud/ too" >&2
    exit 1
fi
# Commits git blame skips: full commit ids only. Anything else, a placeholder left in, fails every
# git blame that reads the file.
if grep -vE '^([0-9a-f]{40})?$|^#' .git-blame-ignore-revs; then
    echo ".git-blame-ignore-revs lists something other than full commit ids" >&2
    exit 1
fi
