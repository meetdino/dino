# One heavy build at a time on this Mac: each cargo or swift build uses every core, so builds
# several sessions start at once take turns rather than thrash. Sourced by scripts/check.sh;
# `lockf -k /tmp/dino-check.lock <command>` takes the same lock for anything else.
#   build_lock "$0" "$@"   run the calling script again under the lock, unless it's under it already
DINO_BUILD_LOCK=/tmp/dino-check.lock

build_lock() {
    _self=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
    shift
    [ -z "${DINO_BUILD_LOCKED:-}" ] || return 0
    # Under it already: a lockf on it among this process's parents (an agent's own wrap). Taking it again would wait for itself.
    _p=$$
    while [ "${_p:-1}" -gt 1 ]; do
        case "$(ps -o command= -p "$_p")" in *lockf*"$DINO_BUILD_LOCK"*) return 0 ;; esac
        _p=$(ps -o ppid= -p "$_p" | tr -d ' ')
    done
    export DINO_BUILD_LOCKED=1
    lockf -k -s -t 0 "$DINO_BUILD_LOCK" true || echo "another dino build or check is running on this Mac: waiting for it" >&2
    exec lockf -k "$DINO_BUILD_LOCK" "$_self" "$@"
}
