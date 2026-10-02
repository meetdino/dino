# A tmux started from a dino shell. Its server starts with the environment of the command that
# started it, and every pane in it gets that. For that one command:
# - ZDOTDIR points at dino's integration, so the panes' zsh load it (ghostty.zshenv then puts the
#   user's own ZDOTDIR back, as for any dino shell). zsh reads ZDOTDIR only as it starts, so this
#   shell isn't affected.
# - DINO_SESSION goes, or every pane of that server, also ones attached from other terminals,
#   would claim to be this dino session.
# Both are back at the next prompt. The `tmux` that runs is the user's own, untouched.

typeset -g _dino_tmux_started=0

# A command on the line (after `;`, `&&`, `|`, …, assignments and `command`/`exec`) that is tmux.
typeset -g _dino_tmux_re=$'(^|[;&|({\n]|then|do|else)[[:space:]]*([A-Za-z_][A-Za-z0-9_]*=[^[:space:]]*[[:space:]]+)*((command|exec|nocorrect|noglob|builtin)[[:space:]]+)*([^[:space:];&|]*/)?tmux([[:space:];&|)]|$)'

_dino_tmux_start() {
    [[ $3 =~ $_dino_tmux_re ]] || return 0
    typeset -g _dino_tmux_started=1 _dino_had_zdotdir=${+ZDOTDIR} _dino_zdotdir=${ZDOTDIR-} _dino_session=${DINO_SESSION-}
    if (( _dino_had_zdotdir )); then
        export GHOSTTY_ZSH_ZDOTDIR=$_dino_zdotdir
    else
        unset GHOSTTY_ZSH_ZDOTDIR
    fi
    export ZDOTDIR=$_dino_zsh_dir
    unset DINO_SESSION
}

_dino_tmux_started_back() {
    (( _dino_tmux_started )) || return 0
    _dino_tmux_started=0
    if (( _dino_had_zdotdir )); then
        export ZDOTDIR=$_dino_zdotdir
    else
        unset ZDOTDIR
    fi
    unset GHOSTTY_ZSH_ZDOTDIR
    [[ -n $_dino_session ]] && export DINO_SESSION=$_dino_session
}

preexec_functions+=(_dino_tmux_start)
precmd_functions=(_dino_tmux_started_back $precmd_functions)
