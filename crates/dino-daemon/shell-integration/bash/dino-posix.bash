# dino's way into bash 4 and later: dinod points ENV here. Ghostty's integration reads the user's
# startup files and attaches its hooks; then the AI line, and agents typed here reporting to dino.

# What an agent typed here gets from dino (dino-agents.bash) is this shell's alone: nothing it
# starts, a tmux server among them, has it in its environment.
export -n DINO_CLAUDE_SETTINGS DINO_CLAUDE_BASE_URL

builtin source "${BASH_SOURCE[0]%/*}/ghostty.bash"
[[ $- == *i* ]] && builtin source "${BASH_SOURCE[0]%/*}/dino-ai.bash"
[[ $- == *i* ]] && builtin source "${BASH_SOURCE[0]%/*}/dino-term.bash"
[[ $- == *i* ]] && builtin source "${BASH_SOURCE[0]%/*}/dino-agents.bash"
