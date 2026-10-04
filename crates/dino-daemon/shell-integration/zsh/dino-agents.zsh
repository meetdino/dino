# An agent typed into a dino shell (`claude …`) reports to dino from its first moment, as a session
# dino starts does: its turns, its questions, its tasks. dinod writes the settings that say where
# (hooks to this session) into $DINO_CLAUDE_SETTINGS; they're read at each run, so "Keep as
# terminal" and Settings → Agents take effect without a new shell. No hooks for a run with its own
# --settings, or a one-off (-p, --version, subcommands); a claude function of the user's own is left
# alone altogether.
#
# Its model calls go through dino's proxy, for its usage and limits: $DINO_CLAUDE_BASE_URL becomes
# its ANTHROPIC_BASE_URL, for that agent alone. The shell itself never has it, so other programs
# using the Anthropic SDK go straight to the API. An ANTHROPIC_BASE_URL of the user's own wins.

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
    local url=
    [[ -z ${ANTHROPIC_BASE_URL-} ]] && url=${DINO_CLAUDE_BASE_URL-}
    if [[ -n ${DINO_CLAUDE_SETTINGS-} && -r $DINO_CLAUDE_SETTINGS ]] && _dino_agent_run "$@"; then
        builtin set -- --settings "$DINO_CLAUDE_SETTINGS" "$@"
    fi
    if [[ -n $url ]]; then
        ANTHROPIC_BASE_URL=$url builtin command claude "$@"
    else
        builtin command claude "$@"
    fi
}
