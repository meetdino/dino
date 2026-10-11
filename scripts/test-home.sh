#!/bin/sh
# Tests run in a home folder of their own, made for them and removed after, never in yours: what a
# test starts (the shells a test dinod runs, an agent or a stand-in, git) reads its startup files,
# keeps its history and writes its settings there. The shells dinod's tests start once wrote their
# commands into the user's ~/.bash_history and cut it to bash's default 500 lines.
#   scripts/test-home.sh <test program> [args...]   cargo's runner here (.cargo/config.toml)
#   scripts/test-home.sh --swift <command> [args...]   the app's tests (scripts/check.sh)
# As cargo's runner, `cargo run` and anything else not built as a test (outside target/…/deps)
# runs as it is. --swift moves Foundation's home folder too (NSHomeDirectory,
# homeDirectoryForCurrentUser), which goes by CFFIXED_USER_HOME, not HOME. UserDefaults still
# writes the user's own ~/Library/Preferences: a test leaves them alone.
set -u

swift=
if [ "$1" = --swift ]; then
    swift=1
    shift
else
    # cargo builds test programs into target/<profile>/deps.
    case "${1%/*}" in
    */deps) ;;
    *) exec "$@" ;;
    esac
fi

# cargo and rustup, for a test that runs them, keep the ones in your home.
CARGO_HOME=${CARGO_HOME:-$HOME/.cargo}
export CARGO_HOME
if [ -z "${RUSTUP_HOME:-}" ] && [ -d "$HOME/.rustup" ]; then
    RUSTUP_HOME=$HOME/.rustup
    export RUSTUP_HOME
fi

home=$(mktemp -d "${TMPDIR:-/tmp}/dino-test-home.XXXXXX") || exit 1
# The real path (/private/var/… on macOS), as a test that resolves a folder sees it.
home=$(cd "$home" && pwd -P) || exit 1
HOME=$home
export HOME
if [ -n "$swift" ]; then
    CFFIXED_USER_HOME=$home
    export CFFIXED_USER_HOME
fi
# Nothing that still points into yours: where shells read startup files and keep history, and the
# folders dino, the agents and XDG programs use instead of their default under HOME.
unset ZDOTDIR HISTFILE ENV BASH_ENV \
    XDG_CONFIG_HOME XDG_DATA_HOME XDG_STATE_HOME XDG_CACHE_HOME \
    DINO_HOME CLAUDE_CONFIG_DIR CODEX_HOME KIMI_CODE_HOME COPILOT_HOME QWEN_HOME HERMES_HOME \
    CODEWHALE_HOME PI_CODING_AGENT_DIR PI_CODING_AGENT_SESSION_DIR OPENCODE_DB

trap 'rm -rf "$home"; exit 130' INT
trap 'rm -rf "$home"; exit 143' TERM
trap 'rm -rf "$home"; exit 129' HUP
"$@"
status=$?
rm -rf "$home" 2>/dev/null
exit $status
