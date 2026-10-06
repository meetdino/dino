# A tmux started from a dino shell. Its server starts with the environment of the command that
# started it, and every pane in it gets that, also ones attached from other terminals. DINO_SESSION
# goes for that one command, or every pane of that server would claim to be this dino session; it's
# back at the next prompt. ZDOTDIR is the user's own by then (ghostty.zshenv put it back), so the
# panes' shells start as in any terminal (dino-tmux.zsh says how to have dino's marks there). The
# `tmux` that runs is the user's own, untouched.

typeset -g _dino_tmux_started=0

# A command on the line (after `;`, `&&`, `|`, …, assignments and `command`/`exec`) that is tmux.
typeset -g _dino_tmux_re=$'(^|[;&|({\n]|then|do|else)[[:space:]]*([A-Za-z_][A-Za-z0-9_]*=[^[:space:]]*[[:space:]]+)*((command|exec|nocorrect|noglob|builtin)[[:space:]]+)*([^[:space:];&|]*/)?tmux([[:space:];&|)]|$)'

_dino_tmux_start() {
    [[ $3 =~ $_dino_tmux_re ]] || return 0
    typeset -g _dino_tmux_started=1 _dino_session=${DINO_SESSION-}
    unset DINO_SESSION
}

_dino_tmux_started_back() {
    (( _dino_tmux_started )) || return 0
    _dino_tmux_started=0
    [[ -n $_dino_session ]] && export DINO_SESSION=$_dino_session
}

preexec_functions+=(_dino_tmux_start)
precmd_functions=(_dino_tmux_started_back $precmd_functions)
