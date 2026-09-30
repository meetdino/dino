# dino's way into bash 4 and later: dinod points ENV here. Ghostty's integration reads the user's
# startup files and attaches its hooks; then the AI line.

builtin source "${BASH_SOURCE[0]%/*}/ghostty.bash"
[[ $- == *i* ]] && builtin source "${BASH_SOURCE[0]%/*}/dino-ai.bash"
