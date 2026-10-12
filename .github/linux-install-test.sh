#!/bin/bash
# What .github/workflows/linux-install.yml runs on each Linux machine, as the user it runs as, on
# the throwaway machine GitHub gives it:
#   linux-install-test.sh <target> <folder with the release tarball> <throwaway release key, PEM>
# 1. scripts/install.sh installs dino from a local copy laid out as a release, with no dino-tui to
#    download (a place that has none): it has to carry on without it.
# 2. `dino` starts dinod; a stand-in agent runs as a session.
# 3. Where there's a systemd user session, `dino service install` runs dinod under it.
# 4. `dino tui` installs dino-tui (a stand-in, from a local copy) and runs it; so does plain `dino`
#    in a terminal; a dino-tui whose checksum doesn't match isn't installed.
# 5. dinod takes an update from a local feed, signed with the throwaway key built in.
# 6. `dino stop` stops dinod, and nothing of it is left.
# Each step says "RESULT <step>: pass|fail|skipped (why)"; it ends non-zero if one failed.
set -uo pipefail
target="$1"
art="$2"
key="$3"
repo="$(cd "$(dirname "$0")/.." && pwd)"
failed=0
result() {
    echo "RESULT $1: $2"
    [ -n "${GITHUB_STEP_SUMMARY:-}" ] && echo "- $1: $2" >> "$GITHUB_STEP_SUMMARY"
    case "$2" in fail*) failed=1 ;; esac
}
section() { printf '\n== %s\n' "$*"; }
# Waits up to $1 seconds for the command after it to succeed.
until_ok() { local n=$(( $1 * 4 )); shift; while [ "$n" -gt 0 ]; do "$@" >/dev/null 2>&1 && return 0; sleep 0.25; n=$((n - 1)); done; return 1; }
# dinod's processes by their command line, `<path>/dino daemon`: not a shell whose own command
# line names it. A process that ended but wasn't reaped (in a container with no init) isn't one.
dinods() { for p in $(pgrep -u "$(id -u)" -f '^[^ ]*dino daemon$'); do alive "$p" && echo "$p"; done; }
alive() { local s; s="$(ps -o stat= -p "$1" 2>/dev/null)"; [ -n "$s" ] && [ "${s#Z}" = "$s" ]; }
dinod_running() { [ -n "$(dinods)" ]; }
no_dinod() { [ -z "$(dinods)" ]; }

tarball="$(ls "$art"/dino-*-"$target".tar.gz | head -n 1)"
version="$(basename "$tarball" | sed "s/^dino-\(.*\)-$target\.tar\.gz$/\1/")"
echo "testing $(basename "$tarball") (dino $version) on $(. /etc/os-release && echo "$PRETTY_NAME") $(uname -m), $(getconf GNU_LIBC_VERSION), as $(id -un)"

W="$(mktemp -d)"
REL="$W/rel"
mkdir -p "$REL/dino/$version" "$REL/dino-tui/0.0.1" "$W/bin" "$W/agent"
cp "$tarball" "$REL/dino/$version/"
(cd "$REL/dino/$version" && sha256sum "$(basename "$tarball")" > SHA256SUMS)
echo "$version" > "$REL/dino/latest"
# A stand-in dino-tui, packaged as dino-tui's release packages it: a folder in the tarball.
mkdir -p "$W/tui/dino-tui-0.0.1-$target"
cat > "$W/tui/dino-tui-0.0.1-$target/dino-tui" <<'EOF'
#!/bin/sh
if [ -t 0 ] && [ -t 1 ]; then where=terminal; else where=pipe; fi
# Whether dinod is running, by its process: `dino ping` would start it.
echo "STANDIN dino-tui $* ($where) dinod=$(pgrep -u "$(id -u)" -f '^[^ ]*dino daemon$' >/dev/null && echo up || echo down)"
EOF
chmod 755 "$W/tui/dino-tui-0.0.1-$target/dino-tui"
tar -C "$W/tui" -czf "$REL/dino-tui/0.0.1/dino-tui-0.0.1-$target.tar.gz" "dino-tui-0.0.1-$target"
(cd "$REL/dino-tui/0.0.1" && sha256sum "dino-tui-0.0.1-$target.tar.gz" > SHA256SUMS)
echo 0.0.1 > "$REL/dino-tui/latest"
# A stand-in agent on PATH, named as Claude Code: it notes that dinod started it, and what with.
cat > "$W/bin/claude" <<EOF
#!/bin/sh
case "\$1" in --version) echo "2.1.296 (Claude Code)"; exit 0 ;; auth) echo '{"loggedIn":false}'; exit 0 ;; -p|--print) echo ok; exit 0 ;; esac
echo "started base=\${ANTHROPIC_BASE_URL:+yes} args=\$*" >> "$W/agent/log"
echo "stand-in agent ready"
while read -r line; do echo "got \$line" >> "$W/agent/log"; done
EOF
chmod 755 "$W/bin/claude"
export PATH="$HOME/.local/bin:$W/bin:$PATH"
(cd "$REL" && exec python3 -m http.server 8765 --bind 127.0.0.1 >/dev/null 2>&1) &
server=$!
until_ok 10 curl -fsS http://127.0.0.1:8765/dino/latest || echo "the local server didn't start"
BASE=http://127.0.0.1:8765

section "1. install.sh"
# Not dino-tui's real release, which is public now: step 4 installs a stand-in from the local copy.
out="$(DINO_DOWNLOAD_BASE=$BASE DINO_TUI_DOWNLOAD_BASE=$BASE/none sh "$repo/scripts/install.sh" --service no 2>&1)"
code=$?
echo "$out"
if [ "$code" = 0 ] && [ -x "$HOME/.local/bin/dino" ] && "$HOME/.local/bin/dino" --version | grep -q "dino $version" \
    && echo "$out" | grep -q "dino-tui, dino's terminal UI, isn't installed" && [ ! -e "$HOME/.local/bin/dino-tui" ]; then
    result "install.sh (dino from a local release, no dino-tui to download: carries on)" pass
else
    result "install.sh (dino from a local release, no dino-tui to download: carries on)" "fail (exit $code)"
fi

section "2. dinod and a stand-in session"
dino </dev/null > "$W/home.out" 2>&1
code=$?
cat "$W/home.out"
if [ "$code" = 0 ] && until_ok 15 dino ping && dinod_running; then result "dino starts dinod" pass; else result "dino starts dinod" "fail (exit $code)"; fi
id="$(dino new claude hello there 2>&1)"
echo "dino new claude: $id"
if until_ok 15 grep -q "started base=yes args=.*hello there" "$W/agent/log" && dino ls | grep -q "$id"; then
    dino ls
    result "a stand-in agent session (started with its args and dinod's proxy)" pass
else
    cat "$W/agent/log" 2>/dev/null; dino ls
    result "a stand-in agent session (started with its args and dinod's proxy)" fail
fi
dino kill "$id" && until_ok 10 sh -c "! dino ls | grep -q '$id'" && result "the session closes" pass || result "the session closes" fail

section "3. dino service install"
if command -v systemctl >/dev/null && [ "$(id -u)" != 0 ] && sudo -n true 2>/dev/null; then
    # GitHub's runner has no login session: lingering starts the user's systemd.
    sudo loginctl enable-linger "$(id -un)"
    export XDG_RUNTIME_DIR="/run/user/$(id -u)"
    export DBUS_SESSION_BUS_ADDRESS="unix:path=$XDG_RUNTIME_DIR/bus"
    until_ok 20 systemctl --user show-environment
fi
if command -v systemctl >/dev/null && systemctl --user show-environment >/dev/null 2>&1; then
    dino service install
    code=$?
    if [ "$code" = 0 ] && until_ok 15 systemctl --user is-active --quiet dinod.service && until_ok 15 dino ping && dino service status | grep -q "under systemd"; then
        dino service status
        result "dino service install (dinod under systemd --user)" pass
    else
        systemctl --user status dinod.service --no-pager 2>&1 | tail -20
        result "dino service install (dinod under systemd --user)" "fail (exit $code)"
    fi
    dino service uninstall && ! systemctl --user is-active --quiet dinod.service && result "dino service uninstall" pass || result "dino service uninstall" fail
else
    result "dino service install" "skipped (no systemd user session here: $(command -v systemctl >/dev/null && echo 'systemctl --user has no bus' || echo 'no systemd'))"
fi
dino stop >/dev/null 2>&1
until_ok 15 no_dinod

section "4. dino tui"
out="$(DINO_TUI_DOWNLOAD_BASE=$BASE dino tui --version </dev/null 2>&1)"
echo "$out"
if echo "$out" | grep -q "STANDIN dino-tui --version (pipe) dinod=up" && [ -x "$HOME/.local/bin/dino-tui" ]; then
    result "dino tui installs dino-tui (checked against SHA256SUMS) and runs it" pass
else
    result "dino tui installs dino-tui (checked against SHA256SUMS) and runs it" fail
fi
if command -v script >/dev/null; then
    out="$(script -qec "dino" /dev/null </dev/null 2>&1)"
    echo "$out"
    if echo "$out" | grep -q "STANDIN dino-tui  (terminal)"; then result "plain dino in a terminal opens dino-tui" pass; else result "plain dino in a terminal opens dino-tui" fail; fi
else
    result "plain dino in a terminal opens dino-tui" "skipped (no script(1) here)"
fi
out="$(dino </dev/null 2>&1)"
if ! echo "$out" | grep -q STANDIN; then result "piped, plain dino lists sessions instead" pass; else result "piped, plain dino lists sessions instead" fail; fi
rm "$HOME/.local/bin/dino-tui"
sed -i 's/^[0-9a-f]\{8\}/00000000/' "$REL/dino-tui/0.0.1/SHA256SUMS"
out="$(DINO_TUI_DOWNLOAD_BASE=$BASE dino tui </dev/null 2>&1)"
code=$?
echo "$out"
if [ "$code" != 0 ] && echo "$out" | grep -q "checksum mismatch" && [ ! -e "$HOME/.local/bin/dino-tui" ]; then
    result "a dino-tui that doesn't match SHA256SUMS isn't installed" pass
else
    result "a dino-tui that doesn't match SHA256SUMS isn't installed" "fail (exit $code)"
fi

section "5. an update from a feed signed with the release key"
mkdir -p "$REL/upd"
cp "$tarball" "$REL/upd/dino-99.0.0-$target.tar.gz"
sha="$(sha256sum "$REL/upd/dino-99.0.0-$target.tar.gz" | cut -d' ' -f1)"
openssl pkeyutl -sign -inkey "$key" -rawin -in "$REL/upd/dino-99.0.0-$target.tar.gz" -out "$W/sig"
sig="$(base64 -w0 "$W/sig")"
# A Mac's line too, which Linux mustn't take.
cat > "$REL/upd/appcast.xml" <<EOF
<?xml version="1.0" encoding="utf-8"?>
<rss version="2.0" xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle" xmlns:dino="https://meetdino.com/xml-namespaces/dino">
  <channel><item>
    <sparkle:version>9999</sparkle:version><sparkle:shortVersionString>99.0.0</sparkle:shortVersionString>
    <dino:cli arch="arm64" url="$BASE/upd/none.tar.gz" sha256="00" signature="AA=="/>
    <dino:cli arch="$target" url="$BASE/upd/dino-99.0.0-$target.tar.gz" sha256="$sha" signature="$sig"/>
  </item></channel>
</rss>
EOF
dino stop >/dev/null 2>&1
until_ok 15 no_dinod
before="$(sha256sum "$HOME/.local/bin/dino" | cut -d' ' -f1)"
touch -d '2001-01-01' "$HOME/.local/bin/dino"
rm -f "$HOME/.config/dino/update-checked"
DINO_UPDATE_FEED=$BASE/upd/appcast.xml dino ping >/dev/null
old="$(dinods | head -n 1)"
log="$HOME/.config/dino/dinod.log"
# Restarted: the old dinod gone, a new one up in its place.
restarted() { ! alive "$old" && dinod_running && dino ping; }
if until_ok 60 grep -q "installed dino 99.0.0 in $HOME/.local/bin/dino" "$log"; then
    # The same binary, put in place again (its time says so); dinod restarts into it once idle.
    if [ "$(sha256sum "$HOME/.local/bin/dino" | cut -d' ' -f1)" = "$before" ] && [ "$(stat -c %Y "$HOME/.local/bin/dino")" -gt 1000000000 ] \
        && until_ok 30 grep -q "dinod restarting into the new dino" "$log" && until_ok 30 restarted; then
        result "dinod takes the Linux build from the feed, checks its SHA-256 and signature, installs it, restarts into it" pass
    else
        tail -20 "$log"
        result "dinod takes the Linux build from the feed, checks its SHA-256 and signature, installs it, restarts into it" "fail (after installing)"
    fi
else
    tail -20 "$log"
    result "dinod takes the Linux build from the feed, checks its SHA-256 and signature, installs it, restarts into it" fail
fi

section "6. dino stop"
dino stop
code=$?
if [ "$code" = 0 ] && until_ok 15 no_dinod; then
    result "dino stop, and no dinod left" pass
else
    for p in $(dinods); do ps -o pid=,stat=,args= -p "$p"; done
    tail -30 "$log"
    result "dino stop, and no dinod left" "fail (exit $code)"
fi

kill "$server" 2>/dev/null
exit "$failed"
