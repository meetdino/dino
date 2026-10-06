# dino's panes say TERM=xterm-ghostty (with TERMINFO pointing at its entry). Where that entry isn't
# found, Ghostty's own fallback: `ssh` to a host that may not have it, and `sudo`, which keeps TERM
# but drops TERMINFO, run with xterm-256color. A user's own ssh or sudo function is left alone.
#
# A tmux started here: its server, and so every pane in it, starts with this shell's environment.
# DINO_SESSION goes for that one command, or every pane of that server would claim to be this dino
# session. The shells in its panes start as in any terminal, with none of dino's integration.

if [[ ${TERM-} == xterm-ghostty ]]; then
    declare -F ssh >/dev/null || ssh() { TERM=xterm-256color builtin command ssh "$@"; }
    declare -F sudo >/dev/null || sudo() { TERM=xterm-256color builtin command sudo "$@"; }
fi

_dino_tmux_started=0
# A command on the line (after `;`, `&&`, `|`, …, assignments and `command`/`exec`) that is tmux.
_dino_tmux_re=$'(^|[;&|({\n]|then|do|else)[[:space:]]*([A-Za-z_][A-Za-z0-9_]*=[^[:space:]]*[[:space:]]+)*((command|exec|nocorrect|noglob|builtin)[[:space:]]+)*([^[:space:];&|]*/)?tmux([[:space:];&|)]|$)'
_dino_tmux_start() {
    [[ $1 =~ $_dino_tmux_re ]] || return 0
    _dino_tmux_started=1
    _dino_session=${DINO_SESSION-}
    unset DINO_SESSION
}
_dino_tmux_started_back() {
    (( _dino_tmux_started )) || return 0
    _dino_tmux_started=0
    [[ -n $_dino_session ]] && export DINO_SESSION=$_dino_session
}
if [[ -z ${TMUX-} ]]; then
    preexec_functions+=(_dino_tmux_start)
    precmd_functions=(_dino_tmux_started_back "${precmd_functions[@]}")
fi
