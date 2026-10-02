# dino's marks from a pane of a tmux started in a dino shell. tmux keeps a pane's OSC 7 and 133 for
# itself (they're how it knows the pane's folder and prompts); the same marks wrapped for
# passthrough also reach dino, for the tab's exit code, prompt marks and folder.

(( ${+_dino_tmux_loaded} )) && return 0
typeset -g _dino_tmux_loaded=1 _dino_tmux_ran=0

# Passthrough for this pane only, while it lives: nothing in the user's tmux config changes. Asked
# of the tmux running the server (this shell's parent): `tmux` may not be on this shell's PATH.
() {
    local bin=$(builtin command ps -o comm= -p $PPID 2>/dev/null)
    [[ ${bin:t} == tmux && -x $bin ]] || bin=${commands[tmux]-}
    [[ -n $bin ]] && builtin command "$bin" set -p allow-passthrough on 2>/dev/null
}

# `ESC P tmux; <the sequence, each ESC doubled> ESC \`: tmux hands the sequence on as it is.
_dino_tmux_out() {
    local e=$'\e'
    builtin print -rn -- "${e}Ptmux;${1//$e/$e$e}${e}\\"
}

_dino_tmux_where() {
    _dino_tmux_out $'\e]133;A\a'
    _dino_tmux_out $'\e]7;file://'"${HOST-}$(_ghostty_encoded_pwd 2>/dev/null || builtin print -rn -- $PWD)"$'\a'
}

# First, so `$?` is still the command's.
_dino_tmux_precmd() {
    local ret=$?
    if (( _dino_tmux_ran )); then
        _dino_tmux_out $'\e]133;D;'"$ret"$'\a'
        _dino_tmux_ran=0
    fi
    _dino_tmux_where
}

_dino_tmux_preexec() {
    _dino_tmux_ran=1
    _dino_tmux_out $'\e]133;C\a'
}

precmd_functions=(_dino_tmux_precmd $precmd_functions)
preexec_functions+=(_dino_tmux_preexec)
# Loaded from the first prompt's hooks: that prompt's marks go out now.
_dino_tmux_where
