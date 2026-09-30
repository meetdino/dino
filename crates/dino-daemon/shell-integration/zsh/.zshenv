# dino's zsh bootstrap: dinod starts zsh with ZDOTDIR here, so zsh reads this file first.
#
# ghostty.zshenv puts the user's ZDOTDIR back, runs their .zshenv and queues the integration for
# the first prompt, in precmd_functions. A .zprofile or .zshrc that reassigns precmd_functions
# drops it; zsh still calls a function named precmd, so that's the fallback.

builtin source -- "${${(%):-%x}:A:h}/ghostty.zshenv"

if [[ -o interactive ]] && (( ! ${+functions[precmd]} )); then
    precmd() {
        builtin unfunction precmd
        # Still queued: it loads itself.
        (( ${precmd_functions[(Ie)_ghostty_deferred_init]} )) && return 0
        (( ${+functions[_ghostty_deferred_init]} )) && builtin unfunction _ghostty_deferred_init
        # Its hook joins precmd_functions, which zsh runs right after this.
        (( ${+_ghostty_integration_loaded} )) || builtin source -- "$GHOSTTY_ZSH_INTEGRATION_DIR/ghostty-integration"
    }
fi
