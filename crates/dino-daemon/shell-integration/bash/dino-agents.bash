# An agent typed into a dino shell (`claude …`) reports to dino from its first moment, as a session
# dino starts does: its turns, its questions, its tasks, its context window (through the user's own
# statusline, when they have one in the shell's folder). dinod writes the settings that say where
# (hooks to this session, that statusline) into $DINO_CLAUDE_SETTINGS; they're read at each run, so
# "Keep as terminal" and Settings → Agents take effect without a new shell. No hooks for a run with its own
# --settings, or a one-off (-p, --version, subcommands); left alone altogether: a claude function of
# the user's own, and tmux panes (tmux owns what runs there).
#
# While it reports (the settings file is there), its model calls go through dino's proxy too, for
# its usage and limits: $DINO_CLAUDE_BASE_URL becomes its ANTHROPIC_BASE_URL, for that agent alone.
# Off, or "Keep as terminal", it's a plain claude: no hooks and no proxy. The shell itself never
# has the URL, so other programs using the Anthropic SDK go straight to the API. An
# ANTHROPIC_BASE_URL of the user's own wins.
# Written for bash 3.2 (macOS's /bin/bash) too.

if [[ -z ${TMUX-} ]] && ! declare -F claude >/dev/null; then
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
        if [[ -n ${DINO_CLAUDE_SETTINGS-} && -r $DINO_CLAUDE_SETTINGS ]]; then
            _dino_agent_run "$@" && set -- --settings "$DINO_CLAUDE_SETTINGS" "$@"
            if [[ -z ${ANTHROPIC_BASE_URL-} && -n ${DINO_CLAUDE_BASE_URL-} ]]; then
                ANTHROPIC_BASE_URL=$DINO_CLAUDE_BASE_URL command claude "$@"
                return
            fi
        fi
        command claude "$@"
    }
fi
