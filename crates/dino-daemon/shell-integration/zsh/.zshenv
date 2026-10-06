# dino's zsh bootstrap: dinod starts zsh with ZDOTDIR here, so zsh reads this file first.
#
# ghostty.zshenv puts the user's ZDOTDIR back, runs their .zshenv and queues the integration for
# the first prompt, in precmd_functions. A .zprofile or .zshrc that reassigns precmd_functions
# drops it; zsh still calls a function named precmd, so that's the fallback.

typeset -g _dino_zsh_dir=${${(%):-%x}:A:h}
# What an agent typed here gets from dino (dino-agents.zsh) is this shell's alone: nothing it
# starts, a tmux server among them, has it in its environment.
(( ${+DINO_CLAUDE_SETTINGS} )) && typeset -g +x DINO_CLAUDE_SETTINGS
(( ${+DINO_CLAUDE_BASE_URL} )) && typeset -g +x DINO_CLAUDE_BASE_URL
builtin source -- "$_dino_zsh_dir/ghostty.zshenv"

# The AI line loads at the first prompt, after the user's .zshrc, so its keys win. Nothing of
# dino's in a pane of tmux: a server a dino shell started while dino still handed tmux this folder
# as ZDOTDIR keeps it for its panes, and what runs in tmux is tmux's (a .zshrc can load
# dino-tmux.zsh there).
if [[ -z ${TMUX-} ]]; then
    _dino_late_init() {
        precmd_functions=(${precmd_functions:#_dino_late_init})
        builtin unfunction _dino_late_init
        builtin source -- "$_dino_zsh_dir/dino-ai.zsh"
        builtin source -- "$_dino_zsh_dir/dino-tmux-start.zsh"
        builtin source -- "$_dino_zsh_dir/dino-term.zsh"
        builtin source -- "$_dino_zsh_dir/dino-agents.zsh"
    }
    [[ -o interactive ]] && precmd_functions+=(_dino_late_init)
fi

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
