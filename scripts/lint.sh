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
