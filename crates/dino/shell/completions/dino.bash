# dino's completions for bash (`dino completions bash`): commands, flags, session ids, agents.
#
# Works in bash 3.2 (macOS's own) and later, with or without bash-completion. Something else that
# completes dino already keeps it. dino says what fits (`dino __complete`) and never starts dinod
# for it.

_dino_complete() {
  local cur=${COMP_WORDS[COMP_CWORD]} line dirs= files=
  local -a words lines
  COMPREPLY=()
  # Copied before IFS changes: bash 3.2 joins a slice into one word with IFS a newline.
  words=( "${COMP_WORDS[@]:0:COMP_CWORD+1}" )
  local IFS=$'\n'
  lines=( $(command ${_DINO_BIN:-${DINO_BIN:-__DINO_BIN__}} __complete -- "${words[@]}" 2>/dev/null) )
  for line in "${lines[@]}"; do
    case $line in
      :dirs) dirs=1 ;;
      :files) files=1 ;;
      *)
        line=${line%%$'\t'*}
        [[ $line == "$cur"* ]] && COMPREPLY+=( "$line" )
        ;;
    esac
  done
  if [[ -n $dirs$files ]]; then
    # bash 4 and later: as file names (a / after a folder, quoted) only now. 3.2 has no compopt,
    # so it's registered that way.
    type compopt &>/dev/null && compopt -o filenames 2>/dev/null
    if [[ -n $files ]]; then
      COMPREPLY+=( $(compgen -f -- "$cur") )
    else
      COMPREPLY+=( $(compgen -d -- "$cur") )
    fi
  fi
  return 0
}

if ! complete -p dino &>/dev/null; then
  if (( BASH_VERSINFO[0] >= 4 )); then
    complete -F _dino_complete dino
  else
    complete -o filenames -F _dino_complete dino
  fi
fi
