#!/bin/bash
# A copy of dino-cloud on this machine, standing in for the remote one while we build against it.
#
#   scripts/dev.sh start     Postgres, migrations, and the server on http://127.0.0.1:8787
#   scripts/dev.sh stop      stop everything it started
#   scripts/dev.sh status    what's running, and where
#   scripts/dev.sh reset     stop and delete the data (accounts, devices, synced settings)
#
# Postgres comes from `docker compose` when Docker is running, else from Homebrew's
# postgresql@18 in a data folder of its own. The server is built from this checkout and runs in
# development mode: sign-in codes are written to a file instead of mailed.
#
#   DINO_DEV_DIR      where the data, logs and mail go (default ~/.local/share/dino-cloud-dev)
#   DINO_DEV_PORT     the server's port (default 8787)
#   DINO_DEV_PG_PORT  Postgres' port without Docker (default 55433)
#   DINO_DEV_DOCKER   0 to use Homebrew's Postgres even when Docker is running
set -euo pipefail
cd "$(dirname "$0")/.."

DIR="${DINO_DEV_DIR:-$HOME/.local/share/dino-cloud-dev}"
PORT="${DINO_DEV_PORT:-8787}"
PG_PORT="${DINO_DEV_PG_PORT:-55433}"
URL="http://127.0.0.1:$PORT"
LOGS="$DIR/logs"
MAIL="$DIR/mail.log"
PID="$DIR/server.pid"
BIN="target/release/dino-cloud"

say() { printf '\033[1m%s\033[0m\n' "$*"; }

docker_up() {
    [ "${DINO_DEV_DOCKER:-1}" != 0 ] && command -v docker >/dev/null && docker info >/dev/null 2>&1
}

# Which Postgres this copy uses: remembered at start, so stop and status find the same one.
pg_mode() {
    if [ -f "$DIR/pg-mode" ]; then cat "$DIR/pg-mode"; elif docker_up; then echo docker; else echo local; fi
}

pg_bin() {
    if [ -x /opt/homebrew/opt/postgresql@18/bin/pg_ctl ]; then
        echo /opt/homebrew/opt/postgresql@18/bin
    elif command -v pg_ctl >/dev/null; then
        dirname "$(command -v pg_ctl)"
    else
        echo "Postgres isn't installed: brew install postgresql@18, or start Docker" >&2
        exit 1
    fi
}

# Postgres refuses to start with the C locale macOS hands to non-interactive shells
# ("postmaster became multithreaded during startup").
pg_ctl_() { LC_ALL=en_US.UTF-8 "$(pg_bin)/pg_ctl" "$@"; }

database_url() {
    if [ "$(pg_mode)" = docker ]; then
        echo "postgres://dino:dino@127.0.0.1:55432/dino_cloud"
    else
        echo "postgres://dino@127.0.0.1:$PG_PORT/dino_cloud"
    fi
}

server_running() { [ -f "$PID" ] && kill -0 "$(cat "$PID")" 2>/dev/null; }

start_postgres() {
    if [ "$(pg_mode)" = docker ]; then
        echo docker >"$DIR/pg-mode"
        say "Postgres (docker compose, port 55432)"
        docker compose up -d --wait db >"$LOGS/postgres.log" 2>&1
        return
    fi
    echo local >"$DIR/pg-mode"
    local bin data="$DIR/pg"
    bin="$(pg_bin)"
    mkdir -p "$DIR/run"
    if [ ! -f "$data/PG_VERSION" ]; then
        say "Postgres: a new data folder in $data"
        LC_ALL=en_US.UTF-8 "$bin/initdb" -D "$data" -U dino --auth=trust -E UTF8 >"$LOGS/initdb.log" 2>&1
    fi
    if pg_ctl_ -D "$data" status >/dev/null 2>&1; then
        say "Postgres: already running on port $PG_PORT"
    else
        say "Postgres (Homebrew, port $PG_PORT)"
        pg_ctl_ -D "$data" -w -l "$LOGS/postgres.log" \
            -o "-p $PG_PORT -k $DIR/run -c listen_addresses=127.0.0.1" start >/dev/null
    fi
    if ! "$bin/psql" -h 127.0.0.1 -p "$PG_PORT" -U dino -d postgres -Atc \
        "select 1 from pg_database where datname = 'dino_cloud'" | grep -q 1; then
        "$bin/createdb" -h 127.0.0.1 -p "$PG_PORT" -U dino dino_cloud
    fi
}

stop_postgres() {
    if [ "$(pg_mode)" = docker ]; then
        docker_up && docker compose stop db >/dev/null 2>&1 || true
    elif [ -f "$DIR/pg/PG_VERSION" ] && pg_ctl_ -D "$DIR/pg" status >/dev/null 2>&1; then
        pg_ctl_ -D "$DIR/pg" -m fast -w stop >/dev/null
    fi
}

start() {
    mkdir -p "$DIR" "$LOGS"
    if server_running; then
        say "dino-cloud is already running: $URL"
        hint
        return
    fi
    start_postgres
    # Sign-ins survive a restart only with a fixed key: one per copy, kept with its data.
    [ -f "$DIR/secret" ] || (umask 077 && openssl rand -base64 32 >"$DIR/secret")
    say "Building the server"
    cargo build --release -q -p dino-cloud
    say "Starting the server (migrations run on start)"
    DATABASE_URL="$(database_url)" \
        DINO_CLOUD_URL="$URL" \
        DINO_BIND="127.0.0.1:$PORT" \
        DINO_ENV=development \
        DINO_SECRET_KEY="$(cat "$DIR/secret")" \
        DINO_MAIL_LOG="$MAIL" \
        nohup "$BIN" >"$LOGS/server.log" 2>&1 &
    echo $! >"$PID"
    for _ in $(seq 1 60); do
        if curl -fsS -o /dev/null "$URL/readyz" 2>/dev/null; then
            say "dino-cloud is running: $URL"
            hint
            return
        fi
        if ! server_running; then
            echo "the server stopped while starting; see $LOGS/server.log" >&2
            tail -5 "$LOGS/server.log" >&2
            rm -f "$PID"
            exit 1
        fi
        sleep 0.5
    done
    echo "the server didn't come up within 30 s; see $LOGS/server.log" >&2
    exit 1
}

hint() {
    cat <<EOF

  Sign-in codes:  $MAIL
  Logs:           $LOGS/

  Point a test dinod at it (its own DINO_HOME, so your real one isn't touched):
    DINO_HOME=/tmp/dino-dev DINO_CLOUD_URL=$URL dino daemon
    DINO_HOME=/tmp/dino-dev dino login

  Or sign a dino in to it directly:
    dino login $URL
EOF
}

stop() {
    if server_running; then
        local p
        p="$(cat "$PID")"
        kill "$p" 2>/dev/null || true
        for _ in $(seq 1 20); do kill -0 "$p" 2>/dev/null || break; sleep 0.25; done
        kill -9 "$p" 2>/dev/null || true
        say "Server stopped"
    fi
    rm -f "$PID"
    stop_postgres
    say "Postgres stopped"
}

status() {
    if server_running; then
        if curl -fsS -o /dev/null "$URL/readyz" 2>/dev/null; then
            echo "server:   running at $URL (pid $(cat "$PID"))"
        else
            echo "server:   running (pid $(cat "$PID")) but not answering at $URL"
        fi
    else
        echo "server:   stopped"
    fi
    if [ "$(pg_mode)" = docker ]; then
        echo "postgres: docker compose ($(docker compose ps --format '{{.State}}' db 2>/dev/null || echo unknown))"
    elif [ -f "$DIR/pg/PG_VERSION" ] && pg_ctl_ -D "$DIR/pg" status >/dev/null 2>&1; then
        echo "postgres: running on port $PG_PORT ($DIR/pg)"
    else
        echo "postgres: stopped"
    fi
    echo "codes:    $MAIL"
}

reset() {
    stop
    if [ "$(pg_mode)" = docker ] && docker_up; then
        docker compose down -v >/dev/null 2>&1 || true
    fi
    rm -rf "$DIR"
    say "Deleted $DIR"
}

case "${1:-}" in
    start) start ;;
    stop) stop ;;
    status) status ;;
    reset) reset ;;
    *)
        sed -n '3,7p' "$0" | sed 's/^# //'
        exit 2
        ;;
esac
