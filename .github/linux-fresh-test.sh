#!/bin/sh
# dino's install on a fresh Linux, as the official distro images come: no curl, no CA certificates,
# no libsqlite3, no systemd. Run as root in such a container (.github/workflows/linux-install.yml
# runs it in ubuntu:22.04, ubuntu:24.04, debian:12 and ubuntu:20.04; `docker run` runs it the same):
#   linux-fresh-test.sh <target> <folder with dino-<version>-<target>.tar.gz> [ok|old-glibc]
# It installs what the README says to (curl), adds a user as a fresh Ubuntu does (useradd -m:
# /etc/skel's ~/.profile and ~/.bashrc), and as that user:
# 1. `curl … | sh` installs dino (from the tarball, laid out as a release) and dino-tui (its real
#    release, from GitHub), without a terminal: nothing to answer, nothing read from the script;
#    or, where glibc is too old (old-glibc), stops with one sentence saying what it needs.
# 2. The line it prints for PATH works in this shell, in a new terminal and in a new login.
# 3. Both programs run; dinod starts, runs a stand-in agent session and stops.
# 4. In a terminal, the question about systemd is asked on the terminal and its answer read there,
#    both ways (systemctl is a stand-in here: there's no systemd in a container).
# 5. `dino service install` says why it can't, and leaves nothing behind.
# Each step says "RESULT <step>: pass|fail"; it ends non-zero if one failed.
set -u
target="$1"
art="$2"
expect="${3:-ok}"
repo="$(cd "$(dirname "$0")/.." && pwd)"
failed=0
result() {
    echo "RESULT $1: $2"
    [ -n "${GITHUB_STEP_SUMMARY:-}" ] && echo "- $1: $2" >> "$GITHUB_STEP_SUMMARY"
    case "$2" in fail*) failed=1 ;; esac
}
section() { printf '\n== %s\n' "$*"; }

. /etc/os-release
echo "fresh $PRETTY_NAME $(uname -m), $(getconf GNU_LIBC_VERSION), expecting $expect"
# What the image has before anything is installed.
for c in curl wget sha256sum tar gzip systemctl; do printf '%s: %s  ' "$c" "$(command -v "$c" >/dev/null && echo yes || echo no)"; done
echo
echo "libsqlite3: $(ls /usr/lib/*/libsqlite3.so.0 /lib/*/libsqlite3.so.0 2>/dev/null | head -n 1 || true)"

section "0. what the README says to install first"
if command -v apt-get >/dev/null; then
    apt-get update -qq >/dev/null && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq curl >/dev/null
elif command -v dnf >/dev/null; then
    dnf install -y -q curl >/dev/null
fi
curl --version | head -n 1
echo "libsqlite3 after: $(ls /usr/lib/*/libsqlite3.so.0 /lib/*/libsqlite3.so.0 2>/dev/null | head -n 1 || true)"

# The release, laid out as install.sh's DINO_DOWNLOAD_BASE, read through file:// (curl reads it as
# it reads https; no web server to install).
T=/tmp/fresh
rm -rf "$T"
mkdir -p "$T/src" "$T/bin"
tarball="$(ls "$art"/dino-*-"$target".tar.gz | head -n 1)"
version="$(basename "$tarball" | sed "s/^dino-\(.*\)-$target\.tar\.gz$/\1/")"
mkdir -p "$T/rel/dino/$version"
cp "$tarball" "$T/rel/dino/$version/"
(cd "$T/rel/dino/$version" && sha256sum "$(basename "$tarball")" > SHA256SUMS)
echo "$version" > "$T/rel/dino/latest"
cp "$repo/scripts/install.sh" "$T/src/install.sh"
# A stand-in agent, named as Claude Code.
cat > "$T/bin/claude" <<'EOF'
#!/bin/sh
case "$1" in --version) echo "2.1.296 (Claude Code)"; exit 0 ;; auth) echo '{"loggedIn":false}'; exit 0 ;; esac
echo "started args=$*" >> /tmp/fresh/agent.log
echo "stand-in agent ready"
while read -r line; do :; done
EOF
# systemctl as a systemd user session would answer, for the question in step 4.
mkdir -p "$T/systemd"
cat > "$T/systemd/systemctl" <<'EOF'
#!/bin/sh
case "$*" in *is-active*) exit 3 ;; *) exit 0 ;; esac
EOF
# in-terminal <answer> <command>: <command> in a terminal (script(1)), <answer> typed there, and the
# terminal kept open until <command> ends (script ends the session when its input does).
cat > "$T/in-terminal" <<'EOF'
#!/bin/sh
f="$(mktemp -u)"
mkfifo "$f"
script -qec "$2" /dev/null < "$f" &
pid=$!
exec 3>"$f"
printf '%s\n' "$1" >&3
wait "$pid"
code=$?
exec 3>&-
rm -f "$f"
exit "$code"
EOF
chmod -R a+rX "$T"
chmod 755 "$T/bin/claude" "$T/systemd/systemctl" "$T/in-terminal"
touch "$T/agent.log" && chmod 666 "$T/agent.log"
userdel -r ada >/dev/null 2>&1
useradd -m -s /bin/bash ada
# As ada, in a login shell, as an ssh login would be (su - reads ~/.profile, as sshd's shell does).
as_ada() { su - ada -c "$1"; }
BASE="file://$T/rel"
HINT_TEXT="~/.local/bin isn't on your PATH yet. Run this to add it, in this terminal and every new one:"
HINT_LINE="  echo 'export PATH=\"\$HOME/.local/bin:\$PATH\"' >> ~/.bashrc && export PATH=\"\$HOME/.local/bin:\$PATH\""

section "1. curl -fsSL …/install.sh | sh"
out="$(as_ada "curl -fsSL file://$T/src/install.sh | DINO_DOWNLOAD_BASE=$BASE timeout 300 sh" 2>"$T/err")"
code=$?
err="$(cat "$T/err")"
echo "$out"
echo "(stderr)"
echo "$err"
if [ "$expect" = old-glibc ]; then
    # One sentence, and nothing of the loader's.
    if [ "$code" != 0 ] && [ "$(printf '%s\n' "$err" | grep -c '^error: ')" = 1 ] \
        && printf '%s\n' "$err" | grep -q "^error: dino $version needs glibc 2\.[0-9]* or newer (Ubuntu 22\.04, Debian 12 or later), and $PRETTY_NAME has glibc " \
        && ! printf '%s\n' "$err" | grep -q "version \`GLIBC_" && [ ! -e /home/ada/.local/bin/dino ]; then
        result "on glibc $(getconf GNU_LIBC_VERSION | cut -d' ' -f2), install.sh stops with one sentence saying what dino needs" pass
    else
        result "on glibc $(getconf GNU_LIBC_VERSION | cut -d' ' -f2), install.sh stops with one sentence saying what dino needs" "fail (exit $code)"
    fi
    exit "$failed"
fi
if [ "$code" = 0 ] && [ -x /home/ada/.local/bin/dino ] && [ -x /home/ada/.local/bin/dino-tui ] \
    && printf '%s\n' "$out" | grep -q "^Installed dino $version to /home/ada/.local/bin/dino" \
    && printf '%s\n' "$out" | grep -q "^Installed dino-tui .* to /home/ada/.local/bin/dino-tui" \
    && printf '%s\n' "$out" | grep -qF "dinod starts when a dino command needs it. (This system has no systemd, so it can't start at login.)"; then
    result "curl | sh, no terminal: dino and dino-tui installed, nothing asked" pass
else
    result "curl | sh, no terminal: dino and dino-tui installed, nothing asked" "fail (exit $code)"
fi
if printf '%s\n' "$out" | grep -qxF "$HINT_TEXT" && printf '%s\n' "$out" | grep -qxF "$HINT_LINE" && printf '%s\n' "$out" | tail -n 1 | grep -qx "Then run: dino"; then
    result "it says how to put ~/.local/bin on PATH, for bash" pass
else
    result "it says how to put ~/.local/bin on PATH, for bash" fail
fi

section "2. the PATH line"
printf '%s\n' "$out" | grep -A1 -xF "$HINT_TEXT" | tail -n 1 | sed 's/^  //' > "$T/line"
chmod 644 "$T/line"
cat "$T/line"
# A desktop's terminals: interactive bash, not a login, with the PATH the desktop session started
# with (no ~/.local/bin: it wasn't there at login). Before the line; this one, with the line typed
# in; then a new one.
fresh_term="env -i HOME=/home/ada USER=ada TERM=dumb PATH=/usr/local/bin:/usr/bin:/bin bash -ic"
before="$(as_ada "$fresh_term 'command -v dino' 2>/dev/null")"
now="$(as_ada "$fresh_term '. $T/line; command -v dino' 2>/dev/null")"
term="$(as_ada "$fresh_term 'command -v dino' 2>/dev/null")"
# A new login (ssh): Ubuntu's ~/.profile adds ~/.local/bin, which is there now.
login="$(as_ada "command -v dino")"
echo "before: ${before:-not found} | now: $now | new terminal: $term | new login: $login"
if [ -z "$before" ] && [ "$now" = /home/ada/.local/bin/dino ] && [ "$term" = /home/ada/.local/bin/dino ] && [ "$login" = /home/ada/.local/bin/dino ]; then
    result "after it, dino is on PATH here, in a new terminal and in a new login" pass
else
    result "after it, dino is on PATH here, in a new terminal and in a new login" fail
fi

section "3. dino, dino-tui and dinod"
run="export PATH=$T/bin:\$PATH;"
out="$(as_ada "$run dino --version && dino-tui --version" 2>&1)"
echo "$out"
if printf '%s\n' "$out" | grep -q "^dino $version" && printf '%s\n' "$out" | grep -q "^dino-tui "; then
    result "dino and dino-tui run (no library missing)" pass
else
    result "dino and dino-tui run (no library missing)" fail
fi
out="$(as_ada "$run dino ping && id=\$(dino new claude hello there) && sleep 2 && dino ls && dino kill \$id && dino stop" 2>&1)"
code=$?
echo "$out"
cat "$T/agent.log"
if [ "$code" = 0 ] && grep -q "args=.*hello there" "$T/agent.log"; then
    result "dinod starts, runs a stand-in agent session, and stops" pass
else
    result "dinod starts, runs a stand-in agent session, and stops" "fail (exit $code)"
fi

section "4. the systemd question, in a terminal"
for answer in n y; do
    as_ada "rm -f ~/.config/systemd/user/dinod.service"
    as_ada "PATH=$T/systemd:\$PATH $T/in-terminal $answer 'curl -fsSL file://$T/src/install.sh | DINO_DOWNLOAD_BASE=$BASE timeout --foreground 120 sh -s -- --no-tui'" > "$T/tty.out" 2>&1
    code=$?
    out="$(tr -d '\r' < "$T/tty.out")"
    echo "$out"
    asked="$(printf '%s\n' "$out" | grep -c "Run dinod as a systemd user service, so it starts when you log in? \[y/N\]")"
    if [ "$answer" = n ]; then
        then_ok="$(printf '%s\n' "$out" | grep -c "dinod starts when a dino command needs it. To start it at login instead: dino service install")"
    else
        then_ok="$(printf '%s\n' "$out" | grep -c "Installed /home/ada/.config/systemd/user/dinod.service and started it.")"
    fi
    # The rest of the script ran after the answer: it wasn't read as one.
    if [ "$code" = 0 ] && [ "$asked" = 1 ] && [ "$then_ok" = 1 ] && printf '%s\n' "$out" | tail -n 1 | grep -qi "run: dino"; then
        result "asked on the terminal, answered $answer: done as answered, and the script carries on" pass
    else
        result "asked on the terminal, answered $answer: done as answered, and the script carries on" "fail (exit $code)"
    fi
done
as_ada "rm -f ~/.config/systemd/user/dinod.service"

section "5. dino service install, with no systemd"
out="$(as_ada "dino service install" 2>&1)"
code=$?
echo "$out"
if [ "$code" != 0 ] && printf '%s\n' "$out" | grep -q "no systemd" && [ ! -e /home/ada/.config/systemd/user/dinod.service ]; then
    result "dino service install says there's no systemd, and writes no unit" pass
else
    result "dino service install says there's no systemd, and writes no unit" "fail (exit $code)"
fi
exit "$failed"
