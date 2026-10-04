#!/bin/sh
# Copy crates/dino-sync from a local checkout of the dino repo (default ../dino-app-poc), leaving
# out the settings mapping only dinod uses, and record which commit it came from.
set -eu
cd "$(dirname "$0")/.."
DINO="${1:-../dino-app-poc}"
src="$DINO/crates/dino-sync"
[ -d "$src" ] || { echo "no dino checkout at $DINO" >&2; exit 1; }
rev="$(git -C "$DINO" rev-parse --short HEAD)"
for f in hlc lib merge record; do cp "$src/src/$f.rs" crates/dino-sync/src/; done
for f in converge.rs; do cp "$src/tests/$f" crates/dino-sync/tests/; done
# The settings module isn't copied, so its declaration goes too.
sed -i '' '/#\[cfg(feature = "settings")\]/{N;/pub mod settings;/d;}' crates/dino-sync/src/lib.rs
sed -i '' "2s/at .*/at/; 3s/^[0-9a-f]* /$rev /" crates/dino-sync/UPSTREAM
echo "crates/dino-sync now matches $DINO at $rev; run cargo test"
