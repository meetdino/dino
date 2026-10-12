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
# release's SHA256SUMS before anything is installed. No sudo; safe to run again. It needs curl (a
# fresh Ubuntu Desktop has none: sudo apt install curl); on Linux, glibc 2.35 or newer (Ubuntu
# 22.04, Debian 12 and later).
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
            # The builds are glibc's: on musl (Alpine) they can't start at all.
            if ! getconf GNU_LIBC_VERSION >/dev/null 2>&1 && ls /lib/ld-musl-* >/dev/null 2>&1; then
                fail "dino's Linux builds need glibc 2.35 or newer (Ubuntu 22.04, Debian 12 or later), and $(distro) uses musl"
            fi
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
            [ -n "$version" ] || version="$(latest_tag "$repo")"
        fi
    fi
    [ -n "$version" ] || fail "couldn't find the latest dino release (is github.com reachable from here?)"
    if [ -n "$base" ]; then from="$base/dino/$version"; else from="https://github.com/$repo/releases/download/v$version"; fi
    mkdir -p "$tmp/dino"
    curl -fsSL "$from/SHA256SUMS" > "$tmp/dino/SHA256SUMS" || fail "no checksums for dino $version"
    file=""
    for want in "dino-$version-$os-$arch.tar.gz" "dino-$version-$os-universal.tar.gz"; do
        if grep -q " \*\{0,1\}$want\$" "$tmp/dino/SHA256SUMS"; then file="$want"; break; fi
    done
    [ -n "$file" ] || fail "dino $version has no build for $os $arch"

    say "Downloading dino $version ($os $arch)"
    curl -fSL --progress-bar "$from/$file" > "$tmp/dino/$file" || fail "download failed"
    checked "$tmp/dino" "$file" || fail "checksum mismatch for $file"
    mkdir -p "$tmp/dino/x"
    tar -xzf "$tmp/dino/$file" -C "$tmp/dino/x"
    [ -f "$tmp/dino/x/dino" ] || fail "$file doesn't contain dino"
    chmod 755 "$tmp/dino/x/dino"
    # It has to run here before it replaces anything.
    if ! out="$("$tmp/dino/x/dino" --version 2>&1)"; then
        fail "$(cant_run "dino $version" "$out")"
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
        [ -n "$tv" ] || tv="$(latest_tag "$tui_repo")"
    fi
    [ -n "$tv" ] || return 1
    if [ -n "$tui_base" ]; then tfrom="$tui_base/dino-tui/$tv"; else tfrom="https://github.com/$tui_repo/releases/download/v$tv"; fi
    tfile="dino-tui-$tv-$os-$tarch.tar.gz"
    mkdir -p "$tmp/tui"
    if ! curl -fsSL "$tfrom/SHA256SUMS" > "$tmp/tui/SHA256SUMS" 2>/dev/null; then
        echo "dino-tui $tv: no checksums to download."
        return 2
    fi
    if ! grep -q " \*\{0,1\}$tfile\$" "$tmp/tui/SHA256SUMS"; then
        echo "dino-tui $tv has no build for $os $tarch."
        return 2
    fi
    say "Downloading dino-tui $tv ($os $tarch)"
    if ! curl -fSL --progress-bar "$tfrom/$tfile" > "$tmp/tui/$tfile"; then
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
    if ! out="$("$found" --version 2>&1)"; then
        echo "error: $(cant_run "dino-tui $tv" "$out")" >&2
        return 2
    fi
    mv -f "$found" "$dir/.dino-tui.new"
    mv -f "$dir/.dino-tui.new" "$dir/dino-tui"
    say "Installed dino-tui $tv to $dir/dino-tui"
}

# `dino service install`: dinod as a systemd user service, so it starts at login (and at boot, with
# lingering on) and runs automations with nobody at the keyboard. Asked on the terminal itself
# (/dev/tty: under `curl | sh` the script is stdin), and only when there is one; else, or with no
# answer, it isn't installed, and the last line says how to do it later.
offer_service() {
    if ! command -v systemctl >/dev/null 2>&1; then
        echo "dinod starts when a dino command needs it. (This system has no systemd, so it can't start at login.)"
        return 0
    fi
    if ! systemctl --user show-environment >/dev/null 2>&1; then
        if grep -qi microsoft /proc/sys/kernel/osrelease 2>/dev/null; then
            echo "dinod starts when a dino command needs it. (WSL runs without systemd here: to start dinod at login, add [boot] systemd=true to /etc/wsl.conf, run wsl --shutdown in Windows, then dino service install.)"
        else
            echo "dinod starts when a dino command needs it. (This shell has no systemd user session, so it can't start at login: run dino service install from a desktop terminal or an ssh login to set that up.)"
        fi
        return 0
    fi
    answer="$service"
    if [ "$answer" = ask ]; then
        answer=no
        if [ -t 1 ] && (: </dev/tty) 2>/dev/null; then
            printf 'Run dinod as a systemd user service, so it starts when you log in? [y/N] '
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

# How to run dino now. When the folder isn't on PATH, a line to paste that adds it to this terminal
# and to the startup file every new one reads: Ubuntu's ~/.profile adds ~/.local/bin only at the
# next login, and only when it was there then, and a desktop's new terminals don't read it at all.
path_hint() {
    case ":$PATH:" in
        *":$dir:"*)
            first="$(command -v dino 2>/dev/null || true)"
            if [ -n "$first" ] && [ "$first" != "$dir/dino" ]; then
                echo "Note: $first comes before $dir/dino on your PATH, so \`dino\` runs that one."
            fi
            echo "Run: dino"
            return 0
            ;;
    esac
    # As typed: ~ for the home folder, so the line reads as it would be written by hand.
    case "$dir" in
        "$HOME"/*) shown="~/${dir#"$HOME"/}"; quoted="\$HOME/${dir#"$HOME"/}" ;;
        *) shown="$dir"; quoted="$dir" ;;
    esac
    # The shell they log in with: $SHELL, else the account's (a container's root may have none set).
    sh_name="${SHELL:-$(getent passwd "$(id -un)" 2>/dev/null | cut -d: -f7)}"
    sh_name="${sh_name##*/}"
    case "$sh_name" in
        bash) if [ "$os" = linux ]; then rc="~/.bashrc"; else rc="~/.bash_profile"; fi ;;
        zsh) rc="~/.zshrc" ;;
        fish) rc="" ;;
        *) rc="~/.profile" ;;
    esac
    echo "$shown isn't on your PATH yet. Run this to add it, in this terminal and every new one:"
    if [ -z "$rc" ]; then
        echo "  fish_add_path $shown"
    elif [ "$rc" = "~/.profile" ]; then
        # Read at login only: new terminals have it after the next one.
        echo "  echo 'export PATH=\"$quoted:\$PATH\"' >> ~/.profile && export PATH=\"$quoted:\$PATH\""
        echo "(New terminals have it once you log in again.)"
    else
        echo "  echo 'export PATH=\"$quoted:\$PATH\"' >> $rc && export PATH=\"$quoted:\$PATH\""
    fi
    echo "Then run: dino"
}

# The newest release's version, from where github.com/<repo>/releases/latest leads: no API, so no
# limit of 60 lookups an hour from one address (an office, a CI runner).
latest_tag() {
    curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$1/releases/latest" 2>/dev/null \
        | sed -n 's#.*/releases/tag/v\([0-9][^/]*\)$#\1#p'
}

# Why a program just downloaded ($1, named so) doesn't run here, as one sentence: what it needs,
# when that's a newer glibc (the loader names the versions it misses), else what it said.
cant_run() {
    need_glibc="$(printf '%s\n' "$2" | sed -n "s/.*version \`GLIBC_\([0-9.]*\)' not found.*/\1/p" | sort -t. -k1,1n -k2,2n | tail -n 1)"
    if [ -n "$need_glibc" ]; then
        have="$(getconf GNU_LIBC_VERSION 2>/dev/null | sed 's/^glibc //')"
        echo "$1 needs glibc $need_glibc or newer (Ubuntu 22.04, Debian 12 or later), and $(distro) has glibc ${have:-older than that}"
    else
        echo "$1 doesn't run on this machine: $(printf '%s' "$2" | head -n 3)"
    fi
}

# This system's name, as /etc/os-release has it ("Ubuntu 20.04.6 LTS").
distro() {
    name="$( (. /etc/os-release 2>/dev/null && printf '%s' "${PRETTY_NAME:-}") || true)"
    printf '%s' "${name:-this system}"
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

# Bold in a terminal; plain where the output is kept (a log, a pipe).
say() { if [ -t 1 ]; then printf '\033[1m%s\033[0m\n' "$*"; else printf '%s\n' "$*"; fi; }
fail() { printf 'error: %s\n' "$*" >&2; exit 1; }
need() {
    command -v "$1" >/dev/null 2>&1 && return 0
    if [ "$(id -u)" = 0 ]; then su=""; else su="sudo "; fi
    if command -v apt-get >/dev/null 2>&1; then fail "needs $1: ${su}apt install $1"; fi
    if command -v dnf >/dev/null 2>&1; then fail "needs $1: ${su}dnf install $1"; fi
    fail "needs $1"
}

main "$@"
