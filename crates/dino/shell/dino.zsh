# dino shell integration for zsh (`dino init zsh`).
#
# The AI line: ⌘I in Dino, Alt+I elsewhere ($DINO_AI_KEY), or start the line with #. Type what
# you want in plain words and press Enter: your own agent (Claude Code or Codex, run with no
# tools) puts one command on the prompt. Nothing runs until you press Enter on it, and a command
# that could destroy something is shown in red and needs Enter twice. ⌘⏎ in Dino (Alt+Enter
# elsewhere) hands the line to the agent as a new session instead.
#
# Search: Alt+R (or Ctrl+R with DINO_SEARCH_CTRL_R=1) lists this shell's history and dino's
# sessions together; what you pick goes on the prompt.

[[ -o interactive ]] || return 0
(( ${+_DINO_ZSH} )) && return 0
typeset -g _DINO_ZSH=1
typeset -g _DINO_BIN=${DINO_BIN:-__DINO_BIN__}
typeset -gi _DINO_AI=0 _DINO_ARMED=0 _DINO_STATUS=0
typeset -g _DINO_RISKY= _DINO_WHY= _DINO_LAST= _DINO_HL=

autoload -Uz add-zle-hook-widget add-zsh-hook

_dino_precmd() { _DINO_STATUS=$? }
_dino_preexec() { _DINO_LAST=$1 }
precmd_functions=(_dino_precmd ${precmd_functions:#_dino_precmd})
add-zsh-hook preexec _dino_preexec

if [[ -n $DINO_SESSION ]]; then
  typeset -g _DINO_HINT='  ⏎ suggest · ⌘⏎ hand to agent · esc cancel'
else
  typeset -g _DINO_HINT='  ⏎ suggest · ⌥⏎ hand to agent · esc cancel'
fi

# Our one highlight, redone before every redraw so it follows what's typed. zsh 5.9 tags it; older
# ones read an entry back as they like, so it's found by its text there.
autoload -Uz is-at-least
if is-at-least 5.9; then
  typeset -g _DINO_MEMO=' memo=dino'
else
  typeset -g _DINO_MEMO=
fi
_dino_highlight() {
  if [[ -n $_DINO_MEMO ]]; then
    region_highlight=( ${region_highlight:#*memo=dino} )
  elif [[ -n $_DINO_HL ]]; then
    region_highlight=( ${region_highlight:#$_DINO_HL} )
  fi
  _DINO_HL=
  if (( _DINO_AI )); then
    _DINO_HL="0 ${#BUFFER} bg=22,fg=15$_DINO_MEMO"
  elif [[ -n $_DINO_RISKY ]]; then
    if [[ $BUFFER == $_DINO_RISKY ]]; then
      _DINO_HL="0 ${#BUFFER} bg=52,fg=15$_DINO_MEMO"
    else
      # Edited: it's their command now.
      _DINO_RISKY= _DINO_ARMED=0 POSTDISPLAY=
    fi
  fi
  [[ -n $_DINO_HL ]] && region_highlight+=( $_DINO_HL )
}

_dino_off() {
  _DINO_AI=0 _DINO_RISKY= _DINO_ARMED=0 POSTDISPLAY=
}

_dino_finish() {
  _dino_off
  _dino_highlight
}

_dino_ai_toggle() {
  if (( _DINO_AI )); then
    _dino_off
  else
    _DINO_AI=1 _DINO_RISKY= _DINO_ARMED=0 POSTDISPLAY=$_DINO_HINT
  fi
  _dino_highlight
}

# The request without a leading # and spaces.
_dino_request() {
  local line=${BUFFER#\#}
  print -r -- ${line#"${line%%[![:space:]]*}"}
}

_dino_suggest() {
  local line=$(_dino_request)
  if [[ -z ${line//[[:space:]]/} ]]; then
    _dino_off
    return
  fi
  POSTDISPLAY='  … asking your agent'
  zle -R
  local err out rc
  # A private file of its own: a fixed name in a shared folder could be read, or planted.
  if ! err=$(command mktemp "${TMPDIR:-/tmp}/dino-ai.XXXXXX"); then
    POSTDISPLAY='  ✗ dino: no temp file'
    _dino_highlight
    return
  fi
  out=$(command $_DINO_BIN ai suggest --shell zsh --cwd $PWD --last "$_DINO_LAST" --status $_DINO_STATUS -- $line 2>$err </dev/null)
  rc=$?
  local why=$(<$err)
  command rm -f $err
  case $rc in
    0|10)
      _dino_off
      BUFFER=$out
      CURSOR=${#BUFFER}
      if (( rc == 10 )); then
        _DINO_RISKY=$out _DINO_WHY=$why
        POSTDISPLAY="  ⚠ $why"
      fi
      ;;
    *)
      # Still an AI line, to rephrase or cancel.
      POSTDISPLAY="  ✗ ${why:-dino ai failed}"
      ;;
  esac
  _dino_highlight
}

zle -A accept-line _dino_accept_orig 2>/dev/null
_dino_accept() {
  if (( ! _DINO_AI )) && [[ -z $_DINO_RISKY && $BUFFER == \#* && $BUFFER != \#!* ]]; then
    _DINO_AI=1
  fi
  if (( _DINO_AI )); then
    _dino_suggest
    return
  fi
  if [[ -n $_DINO_RISKY && $BUFFER == $_DINO_RISKY ]] && (( ! _DINO_ARMED )); then
    _DINO_ARMED=1
    POSTDISPLAY="  ⚠ $_DINO_WHY · ⏎ again to run it"
    return
  fi
  _dino_off
  zle _dino_accept_orig
}

zle -A self-insert-unmeta _dino_meta_return_orig 2>/dev/null
_dino_ai_agent() {
  local line=$(_dino_request)
  if (( ! _DINO_AI )) && [[ $BUFFER != \#* ]]; then
    # Not an AI line: Alt+Enter does what it did.
    [[ $KEYS == $'\e[57301~' ]] || zle _dino_meta_return_orig
    return
  fi
  [[ -z ${line//[[:space:]]/} ]] && return
  if [[ -n $DINO_SESSION ]]; then
    local out
    out=$(command $_DINO_BIN ai agent --cwd $PWD --last "$_DINO_LAST" --status $_DINO_STATUS -- $line 2>&1 </dev/null)
    if (( $? )); then
      POSTDISPLAY="  ✗ $out"
      _dino_highlight
      return
    fi
    _dino_off
    BUFFER=
    _dino_highlight
    zle -M "handed to your agent, in session $out"
  else
    _dino_off
    BUFFER="${(q)_DINO_BIN} ai agent -- ${(q)line}"
    zle _dino_accept_orig
  fi
}

_dino_escape() {
  if (( _DINO_AI )) || [[ -n $_DINO_RISKY ]]; then
    _dino_off
    _dino_highlight
    return
  fi
  # Esc still leaves insert mode for vi users.
  [[ $KEYMAP == viins || ( $KEYMAP == main && -o vi ) ]] && zle vi-cmd-mode
}

_dino_search() {
  local picked hist k
  hist=$(command mktemp "${TMPDIR:-/tmp}/dino-hist.XXXXXX") || return
  # fc can't list history inside a widget; $history can, newest first.
  zmodload -F zsh/parameter p:history 2>/dev/null
  for k in ${(Onk)history}; do print -r -- ${history[$k]//$'\n'/\\n}; done >$hist
  # The terminal's own device: macOS can't wait for keys on /dev/tty.
  picked=$(command $_DINO_BIN search --pick --query "$BUFFER" --history $hist <$TTY)
  command rm -f $hist
  zle reset-prompt
  if [[ -n $picked ]]; then
    BUFFER=$picked
    CURSOR=${#BUFFER}
  fi
}

zle -N _dino_ai_toggle
zle -N _dino_ai_agent
zle -N _dino_escape
zle -N _dino_search
zle -N accept-line _dino_accept
add-zle-hook-widget line-pre-redraw _dino_highlight
add-zle-hook-widget line-finish _dino_finish

for _dino_map in emacs viins; do
  bindkey -M $_dino_map '\e[57300~' _dino_ai_toggle
  bindkey -M $_dino_map '\e[57301~' _dino_ai_agent
  bindkey -M $_dino_map ${DINO_AI_KEY:-'\ei'} _dino_ai_toggle
  bindkey -M $_dino_map '\e\r' _dino_ai_agent
  bindkey -M $_dino_map '\e' _dino_escape
  bindkey -M $_dino_map '\er' _dino_search
  [[ -n $DINO_SEARCH_CTRL_R ]] && bindkey -M $_dino_map '^R' _dino_search
done
unset _dino_map
