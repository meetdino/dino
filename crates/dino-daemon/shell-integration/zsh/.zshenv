# dino's zsh bootstrap: dinod starts zsh with ZDOTDIR here, so zsh reads this file first.
#
# ghostty.zshenv puts the user's ZDOTDIR back, runs their .zshenv and queues the integration for
# the first prompt, in precmd_functions. A .zprofile or .zshrc that reassigns precmd_functions
# drops it; zsh still calls a function named precmd, so that's the fallback.

typeset -g _dino_zsh_dir=${${(%):-%x}:A:h}
builtin source -- "$_dino_zsh_dir/ghostty.zshenv"

if [[ -n ${TMUX-} ]]; then
    # A pane of a tmux started from a dino shell (see dino-tmux-start.zsh): the marks again, wrapped
    # so they reach dino through tmux. No AI line: this pane may be shown in any terminal.
    _dino_late_init() {
        precmd_functions=(${precmd_functions:#_dino_late_init})
        builtin unfunction _dino_late_init
        builtin source -- "$_dino_zsh_dir/dino-tmux.zsh"
    }
else
    # The AI line loads at the first prompt, after the user's .zshrc, so its keys win; so does
    # what passes this integration on to a tmux started here.
    _dino_late_init() {
        precmd_functions=(${precmd_functions:#_dino_late_init})
        builtin unfunction _dino_late_init
        builtin source -- "$_dino_zsh_dir/dino-ai.zsh"
        builtin source -- "$_dino_zsh_dir/dino-tmux-start.zsh"
        builtin source -- "$_dino_zsh_dir/dino-term.zsh"
        builtin source -- "$_dino_zsh_dir/dino-agents.zsh"
    }
fi
[[ -o interactive ]] && precmd_functions+=(_dino_late_init)

if [[ -o interactive ]] && (( ! ${+functions[precmd]} )); then
    precmd() {
        builtin unfunction precmd
        # Still queued: it loads itself.
        (( ${precmd_functions[(Ie)_ghostty_deferred_init]} )) && return 0
        (( ${+functions[_ghostty_deferred_init]} )) && builtin unfunction _ghostty_deferred_init
        # Its hook joins precmd_functions, which zsh runs right after this.
        (( ${+_ghostty_integration_loaded} )) || builtin source -- "$GHOSTTY_ZSH_INTEGRATION_DIR/ghostty-integration"
        (( ${+functions[_dino_late_init]} )) && _dino_late_init
    }
fi
