#!/bin/sh
# Install a dino command-line product (dino by default) into ~/.local/bin.
#
#   curl -fsSL https://meetdino.com/install.sh | sh
#   curl -fsSL https://meetdino.com/install.sh | sh -s -- --version 0.1.0
#
# Releases are GitHub release assets in DINO_RELEASES_REPO, tagged v<version> for dino and
# <product>-v<version> for other products; each has <product>-<version>-<os>-<arch>.tar.gz and
# SHA256SUMS. The download is checked against SHA256SUMS before anything is installed. No sudo;
# safe to run again.
#
#   DINO_PRODUCT         which product (default: dino)
#   DINO_RELEASES_REPO   GitHub repository with the releases (default: asdf9384/dino-releases)
#   DINO_DOWNLOAD_BASE   a plain web server instead, laid out <base>/<product>/latest and
#                        <base>/<product>/<version>/<files>
#   DINO_INSTALL_DIR     where the binary goes (default: ~/.local/bin)

# Everything is in main, called on the last line, so a half-downloaded script does nothing.
main() {
    set -eu
    product="${DINO_PRODUCT:-dino}"
    repo="${DINO_RELEASES_REPO:-asdf9384/dino-releases}"
    base="${DINO_DOWNLOAD_BASE:-}"
    dir="${DINO_INSTALL_DIR:-$HOME/.local/bin}"
    version=""
    while [ $# -gt 0 ]; do
        case "$1" in
            --version) version="${2#v}"; shift 2 ;;
            --dir) dir="$2"; shift 2 ;;
            -h|--help) echo "usage: install.sh [--version X.Y.Z] [--dir DIR]"; return 0 ;;
            *) fail "unknown option $1" ;;
        esac
    done

    case "$(uname -s)" in
        Darwin) os=darwin ;;
        Linux) os=linux ;;
        *) fail "$product doesn't run on $(uname -s) yet" ;;
    esac
    case "$(uname -m)" in
        arm64|aarch64) arch=arm64 ;;
        x86_64|amd64) arch=x86_64 ;;
        *) fail "$product doesn't run on $(uname -m) yet" ;;
    esac
    need curl
    need tar
    if command -v shasum >/dev/null 2>&1; then sha() { shasum -a 256 "$1" | cut -d' ' -f1; }
    elif command -v sha256sum >/dev/null 2>&1; then sha() { sha256sum "$1" | cut -d' ' -f1; }
    else fail "needs shasum or sha256sum to check the download"; fi

    if [ "$product" = dino ]; then prefix="v"; else prefix="$product-v"; fi
    if [ -z "$version" ]; then
        if [ -n "$base" ]; then
            version="$(curl -fsSL "$base/$product/latest" | tr -d '[:space:]')" || true
        else
            # The newest release whose tag is this product's.
            version="$(curl -fsSL "https://api.github.com/repos/$repo/releases?per_page=50" \
                | sed -n "s/.*\"tag_name\": *\"$prefix\([0-9][^\"]*\)\".*/\1/p" | head -n 1)" || true
        fi
    fi
    [ -n "$version" ] || fail "couldn't find the latest $product release"
    if [ -n "$base" ]; then from="$base/$product/$version"; else from="https://github.com/$repo/releases/download/$prefix$version"; fi

    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT
    curl -fsSL "$from/SHA256SUMS" -o "$tmp/SHA256SUMS" || fail "no checksums for $product $version"
    # This machine's build, or a universal one.
    file=""
    for want in "$product-$version-$os-$arch.tar.gz" "$product-$version-$os-universal.tar.gz"; do
        if grep -q " $want\$" "$tmp/SHA256SUMS"; then file="$want"; break; fi
    done
    [ -n "$file" ] || fail "$product $version has no build for $os $arch"

    say "Downloading $product $version ($os $arch)"
    curl -fSL --progress-bar "$from/$file" -o "$tmp/$file" || fail "download failed"
    expected="$(grep " $file\$" "$tmp/SHA256SUMS" | cut -d' ' -f1)"
    actual="$(sha "$tmp/$file")"
    [ "$expected" = "$actual" ] || fail "checksum mismatch for $file: expected $expected, got $actual"

    mkdir -p "$tmp/x" "$dir"
    tar -xzf "$tmp/$file" -C "$tmp/x"
    [ -f "$tmp/x/$product" ] || fail "$file doesn't contain $product"
    chmod 755 "$tmp/x/$product"
    # Replace in one step, so a running copy keeps working until it exits.
    mv -f "$tmp/x/$product" "$dir/.$product.new"
    mv -f "$dir/.$product.new" "$dir/$product"

    say "Installed $product $version to $dir/$product"
    case ":$PATH:" in
        *":$dir:"*) echo "Run: $product" ;;
        *)
            echo "$dir isn't on your PATH. Add this to your shell's startup file, then open a new shell:"
            echo "  export PATH=\"$dir:\$PATH\""
            ;;
    esac
}

say() { printf '\033[1m%s\033[0m\n' "$*"; }
fail() { printf 'error: %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || fail "needs $1"; }

main "$@"
