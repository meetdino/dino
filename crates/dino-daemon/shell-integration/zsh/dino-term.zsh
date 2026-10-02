# dino's panes say TERM=xterm-ghostty (with TERMINFO pointing at its entry). Where that entry isn't
# found, Ghostty's own fallback: `ssh` to a host that may not have it, and `sudo`, which keeps TERM
# but drops TERMINFO, run with xterm-256color. A user's own ssh or sudo function is left alone.

[[ ${TERM-} == xterm-ghostty ]] || return 0

(( ${+functions[ssh]} )) || ssh() {
    TERM=xterm-256color builtin command ssh "$@"
}
(( ${+functions[sudo]} )) || sudo() {
    TERM=xterm-256color builtin command sudo "$@"
}
