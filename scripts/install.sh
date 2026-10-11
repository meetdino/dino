#!/bin/sh
# Install the dino command line (which is also dinod, its background service) into ~/.local/bin,
# on a Mac or on Linux. On Linux it also installs dino-tui, dino's terminal UI, when its download
# is available, and offers to run dinod as a systemd user service.
#
#   curl -fsSL https://meetdino.com/install.sh | sh
#   curl -fsSL https://meetdino.com/install.sh | sh -s -- --version 0.1.0
#
# Releases are GitHub release assets in DINO_RELEASES_REPO, tagged v<version>; each has
# dino-<version>-<os>-<arch>.tar.gz (darwin-arm64, darwin-x86_64 or darwin-universal; linux-x64,
# linux-arm64) and SHA256SUMS. dino-tui's are in DINO_TUI_RELEASES_REPO, its newest release:
# dino-tui-<version>-<os>-<arch>.tar.gz and SHA256SUMS. Each download is checked against its
# release's SHA256SUMS before anything is installed. No sudo; safe to run again.
#
#   --tui                only dino-tui (what `dino tui` runs when dino-tui is missing)
#   --no-tui             not dino-tui
#   --service yes|no     on Linux, run `dino service install` (yes) or don't (no) without asking
#
#   DINO_RELEASES_REPO      GitHub repository with dino's releases (default: meetdino/dino)
#   DINO_DOWNLOAD_BASE      a plain web server instead, laid out <base>/dino/latest and
#                           <base>/dino/<version>/<files>
#   DINO_TUI_RELEASES_REPO  GitHub repository with dino-tui's releases (default: meetdino/dino-tui)
#   DINO_TUI_DOWNLOAD_BASE  a plain web server for dino-tui, laid out the same way (dino-tui/latest)
#   DINO_INSTALL_DIR        where the binaries go (default: ~/.local/bin)

# Everything is in main, called on the last line, so a half-downloaded script does nothing.
main() {
    set -eu
    repo="${DINO_RELEASES_REPO:-meetdino/dino}"
    base="${DINO_DOWNLOAD_BASE:-}"
    dir="${DINO_INSTALL_DIR:-$HOME/.local/bin}"
    version=""
    tui=auto
    service=ask
    while [ $# -gt 0 ]; do
        case "$1" in
            --version) version="${2#v}"; shift 2 ;;
            --dir) dir="$2"; shift 2 ;;
            --tui) tui=only; shift ;;
            --no-tui) tui=no; shift ;;
            --service) service="$2"; shift 2 ;;
            -h|--help) echo "usage: install.sh [--version X.Y.Z] [--dir DIR] [--tui | --no-tui] [--service yes|no]"; return 0 ;;
            *) fail "unknown option $1" ;;
        esac
    done
    case "$service" in ask|yes|no) ;; *) fail "--service takes yes or no" ;; esac

    case "$(uname -s)" in
        Darwin)
            os=darwin
            case "$(uname -m)" in
                arm64) arch=arm64 ;;
                x86_64) arch=x86_64 ;;
                *) fail "dino doesn't run on $(uname -m) Macs" ;;
            esac
            ;;
        Linux)
            os=linux
            case "$(uname -m)" in
                x86_64|amd64) arch=x64 ;;
                aarch64|arm64) arch=arm64 ;;
                *) fail "dino doesn't run on $(uname -m) yet: there are builds for x86_64 and arm64" ;;
            esac
            ;;
        *) fail "dino runs on macOS and Linux only" ;;
    esac
    need curl
    need tar
    command -v sha256sum >/dev/null 2>&1 || command -v shasum >/dev/null 2>&1 || fail "needs sha256sum or shasum"
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT
    mkdir -p "$dir"

    if [ "$tui" = only ]; then
        if install_tui; then return 0; else got=$?; fi
        [ "$got" = 1 ] && fail "dino-tui, dino's terminal UI, isn't available to download yet"
        exit 1
    fi
    install_dino
    if [ "$os" = linux ]; then
        if [ "$tui" = auto ] && ! install_tui; then
            echo "dino-tui, dino's terminal UI, isn't installed: \`dino tui\` installs it once its download is available."
        fi
        offer_service
    fi
    path_hint
}

# dino itself: this machine's build, or a universal one.
install_dino() {
    if [ -z "$version" ]; then
        if [ -n "$base" ]; then
            version="$(curl -fsSL "$base/dino/latest" | tr -d '[:space:]')" || true
        else
            # The newest release whose tag is dino's.
            version="$(curl -fsSL "https://api.github.com/repos/$repo/releases?per_page=50" \
                | sed -n 's/.*"tag_name": *"v\([0-9][^"]*\)".*/\1/p' | head -n 1)" || true
        fi
    fi
    [ -n "$version" ] || fail "couldn't find the latest dino release"
    if [ -n "$base" ]; then from="$base/dino/$version"; else from="https://github.com/$repo/releases/download/v$version"; fi
    mkdir -p "$tmp/dino"
    curl -fsSL "$from/SHA256SUMS" -o "$tmp/dino/SHA256SUMS" || fail "no checksums for dino $version"
    file=""
    for want in "dino-$version-$os-$arch.tar.gz" "dino-$version-$os-universal.tar.gz"; do
        if grep -q " \*\{0,1\}$want\$" "$tmp/dino/SHA256SUMS"; then file="$want"; break; fi
    done
    [ -n "$file" ] || fail "dino $version has no build for $os $arch"

    say "Downloading dino $version ($os $arch)"
    curl -fSL --progress-bar "$from/$file" -o "$tmp/dino/$file" || fail "download failed"
    checked "$tmp/dino" "$file" || fail "checksum mismatch for $file"
    mkdir -p "$tmp/dino/x"
    tar -xzf "$tmp/dino/$file" -C "$tmp/dino/x"
    [ -f "$tmp/dino/x/dino" ] || fail "$file doesn't contain dino"
    chmod 755 "$tmp/dino/x/dino"
    # It has to run here before it replaces anything (on Linux it needs glibc 2.35 or newer).
    if ! out="$("$tmp/dino/x/dino" --version 2>&1)"; then
        printf '%s\n' "$out" >&2
        fail "dino $version doesn't run on this machine"
    fi
    # Replace in one step, so a running copy keeps working until it exits.
    mv -f "$tmp/dino/x/dino" "$dir/.dino.new"
    mv -f "$dir/.dino.new" "$dir/dino"
    say "Installed dino $version to $dir/dino"
}

# dino-tui, the newest release. Fails with 1 when there's no release to download, 2 (having said
# why) when the download didn't install. Its tarball holds a folder named after it, with dino-tui
# inside.
install_tui() {
    tui_repo="${DINO_TUI_RELEASES_REPO:-meetdino/dino-tui}"
    tui_base="${DINO_TUI_DOWNLOAD_BASE:-}"
    case "$os-$arch" in
        darwin-x86_64) tarch=x64 ;;
        *) tarch="$arch" ;;
    esac
    if [ -n "$tui_base" ]; then
        tv="$(curl -fsSL "$tui_base/dino-tui/latest" 2>/dev/null | tr -d '[:space:]')" || true
    else
        tv="$(curl -fsSL "https://api.github.com/repos/$tui_repo/releases/latest" 2>/dev/null \
            | sed -n 's/.*"tag_name": *"v\([0-9][^"]*\)".*/\1/p' | head -n 1)" || true
    fi
    [ -n "$tv" ] || return 1
    if [ -n "$tui_base" ]; then tfrom="$tui_base/dino-tui/$tv"; else tfrom="https://github.com/$tui_repo/releases/download/v$tv"; fi
    tfile="dino-tui-$tv-$os-$tarch.tar.gz"
    mkdir -p "$tmp/tui"
    if ! curl -fsSL "$tfrom/SHA256SUMS" -o "$tmp/tui/SHA256SUMS" 2>/dev/null; then
        echo "dino-tui $tv: no checksums to download."
        return 2
    fi
    if ! grep -q " \*\{0,1\}$tfile\$" "$tmp/tui/SHA256SUMS"; then
        echo "dino-tui $tv has no build for $os $tarch."
        return 2
    fi
    say "Downloading dino-tui $tv ($os $tarch)"
    if ! curl -fSL --progress-bar "$tfrom/$tfile" -o "$tmp/tui/$tfile"; then
        echo "dino-tui $tv: the download failed."
        return 2
    fi
    if ! checked "$tmp/tui" "$tfile"; then
        echo "error: checksum mismatch for $tfile: not installed" >&2
        return 2
    fi
    mkdir -p "$tmp/tui/x"
    tar -xzf "$tmp/tui/$tfile" -C "$tmp/tui/x"
    found=""
    for f in "$tmp/tui/x/dino-tui" "$tmp/tui/x"/*/dino-tui; do
        if [ -f "$f" ]; then found="$f"; break; fi
    done
    if [ -z "$found" ]; then
        echo "dino-tui $tv: $tfile doesn't contain dino-tui."
        return 2
    fi
    chmod 755 "$found"
    mv -f "$found" "$dir/.dino-tui.new"
    mv -f "$dir/.dino-tui.new" "$dir/dino-tui"
    say "Installed dino-tui $tv to $dir/dino-tui"
}

# `dino service install`: dinod as a systemd user service, so it starts at login and runs
# automations with nobody logged in. Asked when there's a terminal to ask in.
offer_service() {
    if ! command -v systemctl >/dev/null 2>&1 || ! systemctl --user show-environment >/dev/null 2>&1; then
        echo "dinod starts when a dino command needs it. (No systemd user session here, so it can't start at login.)"
        return 0
    fi
    answer="$service"
    if [ "$answer" = ask ]; then
        answer=no
        if [ -t 1 ] && (: </dev/tty) 2>/dev/null; then
            printf 'Start dinod at login with systemd, so automations run while you'"'"'re away? [y/N] '
            read -r reply </dev/tty || reply=""
            case "$reply" in y|Y|yes|Yes) answer=yes ;; esac
        fi
    fi
    if [ "$answer" = yes ]; then
        "$dir/dino" service install || echo "dino service install failed; dinod starts when a dino command needs it."
    else
        echo "dinod starts when a dino command needs it. To start it at login instead: dino service install"
    fi
}

path_hint() {
    case ":$PATH:" in
        *":$dir:"*) echo "Run: dino" ;;
        *)
            # The startup file of the shell they use, so the line can be pasted as is.
            case "${SHELL##*/}" in
                zsh) rc="~/.zshrc" ;;
                bash) if [ "$os" = linux ]; then rc="~/.bashrc"; else rc="~/.bash_profile"; fi ;;
                fish) rc="" ;;
                *) rc="~/.profile" ;;
            esac
            echo "$dir isn't on your PATH yet. To add it, run this, then open a new terminal:"
            if [ -n "$rc" ]; then
                echo "  echo 'export PATH=\"$dir:\$PATH\"' >> $rc"
            else
                echo "  fish_add_path $dir"
            fi
            ;;
    esac
}

# `$2` in folder `$1` matches its line in `$1/SHA256SUMS`.
checked() {
    expected="$(grep " \*\{0,1\}$2\$" "$1/SHA256SUMS" | head -n 1 | cut -d' ' -f1)"
    if command -v sha256sum >/dev/null 2>&1; then
        actual="$(sha256sum "$1/$2" | cut -d' ' -f1)"
    else
        actual="$(shasum -a 256 "$1/$2" | cut -d' ' -f1)"
    fi
    [ -n "$expected" ] && [ "$expected" = "$actual" ]
}

say() { printf '\033[1m%s\033[0m\n' "$*"; }
fail() { printf 'error: %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || fail "needs $1"; }

main "$@"
