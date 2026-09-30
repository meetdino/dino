# dino's bash startup for a bash older than 4 (macOS's /bin/bash is 3.2), which ignores ENV in
# POSIX mode, so Ghostty's way in doesn't work: dinod starts it with --rcfile pointing here.
# Read what a login shell reads, then attach the hooks.

[[ -r /etc/profile ]] && builtin source /etc/profile
for _dino_file in ~/.bash_profile ~/.bash_login ~/.profile; do
    if [[ -r "$_dino_file" ]]; then
        builtin source "$_dino_file"
        break
    fi
done
builtin unset _dino_file

builtin source "${BASH_SOURCE[0]%/*}/ghostty.bash"

[[ $- == *i* ]] && builtin source "${BASH_SOURCE[0]%/*}/dino-ai.bash"
