#!/bin/sh
# Run by Vercel's Rust builder before `cargo build` (the service root's build.sh).
#
# dino-sync comes from the dino repo, which is private until v1: set DINO_GITHUB_TOKEN (a
# fine-grained token with read access to asdf9384/dino's contents) in the project's environment
# variables, and cargo fetches it with git using that token. Nothing is printed.
set -eu
if [ -n "${DINO_GITHUB_TOKEN:-}" ]; then
    git config --global url."https://x-access-token:${DINO_GITHUB_TOKEN}@github.com/asdf9384/dino".insteadOf "https://github.com/asdf9384/dino"
    mkdir -p "${CARGO_HOME:-$HOME/.cargo}"
    printf '[net]\ngit-fetch-with-cli = true\n' >> "${CARGO_HOME:-$HOME/.cargo}/config.toml"
fi
