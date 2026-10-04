# An agent typed into a dino shell (`claude …`) reports to dino from its first moment, as a session
# dino starts does: its turns, its questions, its tasks. dinod writes the settings that say where
# (hooks to this session) into $DINO_CLAUDE_SETTINGS; they're read at each run, so "Keep as
# terminal" and Settings → Agents take effect without a new shell. Left alone: a claude function of
# the user's own, a run with its own --settings, and one-off runs (-p, --version, subcommands).

(( ${+functions[claude]} )) && return 0

_dino_agent_run() {
    local a
    for a in "$@"; do
        case $a in
            -p|--print|-v|--version|-h|--help|--settings|--settings=*) return 1 ;;
        esac
    done
    case ${1-} in
        config|mcp|migrate-installer|setup-token|doctor|update|install|plugin|plugins) return 1 ;;
    esac
    return 0
}

claude() {
    if [[ -n ${DINO_CLAUDE_SETTINGS-} && -r $DINO_CLAUDE_SETTINGS ]] && _dino_agent_run "$@"; then
        builtin command claude --settings "$DINO_CLAUDE_SETTINGS" "$@"
    else
        builtin command claude "$@"
    fi
}
