#compdef dino
# dino's completions for zsh (`dino completions zsh`): commands, flags, session ids, agents.
#
# As a file named _dino in a folder on $fpath, compinit loads it. Sourced, or with `dino init zsh`,
# it registers itself once compinit has run, unless something else completes dino already. It
# never runs compinit. dino says what fits (`dino __complete`) and never starts dinod for it.

_dino_complete() {
  local -a lines specs
  local line value about dirs= files= ret=1
  lines=( ${(f)"$(command ${_DINO_BIN:-${DINO_BIN:-__DINO_BIN__}} __complete -- "${(@)words[1,CURRENT]}" 2>/dev/null)"} )
  for line in $lines; do
    case $line in
      :dirs) dirs=1 ;;
      :files) files=1 ;;
      *)
        value=${line%%$'\t'*}
        about=${line#*$'\t'}
        [[ $about == $line ]] && about=
        specs+=( "${value//:/\\:}${about:+:$about}" )
        ;;
    esac
  done
  (( $#specs )) && _describe -t dino 'dino' specs && ret=0
  [[ -n $dirs ]] && _files -/ && ret=0
  [[ -n $files ]] && _files && ret=0
  return ret
}

if [[ ${funcstack[1]-} == _dino ]]; then
  # Loaded from $fpath by compinit, for a Tab.
  _dino_complete "$@"
else
  _dino_compdef() {
    (( ${+functions[compdef]} )) || return 1
    [[ -n ${_comps[dino]-} ]] || compdef _dino_complete dino
    precmd_functions=( ${precmd_functions:#_dino_compdef} )
    return 0
  }
  # Before compinit, at the first prompt it has run by.
  _dino_compdef || precmd_functions+=( _dino_compdef )
fi
