#!/bin/sh
# The checks main has to pass: run on a branch rebased on origin/main, before it lands.
#   scripts/check.sh            build and test the workspace and the app
# scripts/land.sh runs it, then lands the branch through a pull request; nothing pushes to main.
set -eu
cd "$(dirname "$0")/.."

if [ "${1:-}" = "--push" ]; then
    echo "main takes pull requests only: scripts/land.sh rebases, runs these checks and lands the branch" >&2
    exit 2
fi

# The dev profile: a check needs what compiles and what passes, not the shipping profile's full
# LTO on one codegen unit (app/build.sh and scripts/release.sh build that), and cargo test reuses
# what cargo build compiled.
cargo build
cargo test --workspace
(cd app && swift build)
scripts/lint.sh
# cloud/ builds crates/dino-sync, whose version is the workspace's: a version bump has to reach
# cloud/Cargo.lock too, or cloud's --locked builds (CI, its Docker image) refuse to start.
if ! cargo metadata --locked --format-version 1 --manifest-path cloud/Cargo.toml >/dev/null; then
    echo "cloud/Cargo.lock is out of date: cargo update -p dino-sync --manifest-path cloud/Cargo.toml, and commit it" >&2
    exit 1
fi
echo "all checks passed"
