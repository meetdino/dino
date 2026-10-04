# dino shell integration for bash (`dino init bash`).
#
# The AI line: start a line with # and press Enter, and your own agent (the one Settings →
# Terminal names, or Claude Code or Codex; run with no tools) answers with one command. On bash 4 or later ⌘I in Dino (Alt+I elsewhere,
# $DINO_AI_KEY) puts it straight on the prompt; on the old bash macOS ships, ⌘I asks the same way
# as the # line, and ↑ puts the answer on the prompt.
# Nothing runs by itself. A command that could destroy something arrives commented out: delete
# the # to run it. ⌘⏎ in Dino (Alt+Enter elsewhere) hands the line to the agent as a session.
# Alt+R (Ctrl+R with DINO_SEARCH_CTRL_R=1) searches history and dino's sessions together.

[[ $- == *i* ]] || return 0
[[ -n $_DINO_BASH ]] && return 0
_DINO_BIN=${DINO_BIN:-__DINO_BIN__}
_DINO_BASH=1

_dino_suggest_for() {
  local line=$1 err out rc why nl=$'\n'
  # A private file of its own: a fixed name in a shared folder could be read, or planted.
  err=$(command mktemp "${TMPDIR:-/tmp}/dino-ai.XXXXXX") || { printf '✗ dino: no temp file\n' >/dev/tty; return 1; }
  out=$(command "$_DINO_BIN" ai suggest --shell bash --cwd "$PWD" -- "$line" 2>"$err" </dev/null)
  rc=$?
  why=$(<"$err")
  command rm -f "$err"
  case $rc in
    0) _DINO_OUT=$out ;;
    # Every line commented out: bash runs each line of a multi-line one on the one Enter.
    10) _DINO_OUT="# ${out//$nl/$nl# }"; printf '\e[31m⚠ %s: delete the # to run it\e[0m\n' "$why" >/dev/tty ;;
    *) _DINO_OUT=; printf '✗ %s\n' "${why:-dino ai failed}" >/dev/tty; return 1 ;;
  esac
}

# The line to an agent: a session of its own in Dino, the command to start one elsewhere.
_dino_hand_off() {
  local out
  if [[ -n $DINO_SESSION ]]; then
    if out=$(command "$_DINO_BIN" ai agent --cwd "$PWD" -- "$1" 2>&1 </dev/null); then
      printf 'handed to your agent, in session %s\n' "$out" >/dev/tty
    else
      printf '✗ %s\n' "$out" >/dev/tty
    fi
  else
    out="$(printf '%q' "$_DINO_BIN") ai agent -- $(printf '%q' "$1")"
    builtin history -s -- "$out"
    printf '→ %s   (↑ puts it on the prompt)\n' "$out"
  fi
}

# Keyless, any bash: a line starting with # is a request, answered after it "runs".
_dino_prompt_command() {
  local last
  last=$(HISTTIMEFORMAT= builtin history 1)
  last=${last#*[0-9]  }
  [[ $last == \#* && $last != \#!* && $last != "$_DINO_ASKED" ]] || return 0
  _DINO_ASKED=$last
  local line=$last
  # `#@ …` is ⌘⏎ on old bash: the line goes to an agent.
  if [[ $line == \#@* ]]; then
    line=${line#\#@}
    line=${line# }
    [[ -n ${line// } ]] || return 0
    _dino_hand_off "$line"
    return 0
  fi
  while [[ $line == \#* ]]; do line=${line#\#}; line=${line# }; done
  [[ -n ${line// } ]] || return 0
  _dino_suggest_for "$line" || return 0
  builtin history -s -- "$_DINO_OUT"
  printf '→ %s   (↑ puts it on the prompt)\n' "$_DINO_OUT"
}
PROMPT_COMMAND="_dino_prompt_command${PROMPT_COMMAND:+;$PROMPT_COMMAND}"

if (( BASH_VERSINFO[0] >= 4 )); then
  _dino_ai_line() {
    local line=${READLINE_LINE#\#}
    line=${line# }
    [[ -n ${line// } ]] || return 0
    printf '\r\e[K  … asking your agent\r' >/dev/tty
    _dino_suggest_for "$line"
    local rc=$?
    printf '\r\e[K' >/dev/tty
    (( rc == 0 )) || return 0
    READLINE_LINE=$_DINO_OUT
    READLINE_POINT=${#READLINE_LINE}
  }
  _dino_ai_agent() {
    local line=${READLINE_LINE#\#} out
    line=${line# }
    [[ -n ${line// } ]] || return 0
    if [[ -n $DINO_SESSION ]]; then
      if out=$(command "$_DINO_BIN" ai agent --cwd "$PWD" -- "$line" 2>&1 </dev/null); then
        READLINE_LINE=
        printf 'handed to your agent, in session %s\n' "$out" >/dev/tty
      else
        printf '✗ %s\n' "$out" >/dev/tty
      fi
    else
      READLINE_LINE="$(printf '%q' "$_DINO_BIN") ai agent -- $(printf '%q' "$line")"
      READLINE_POINT=${#READLINE_LINE}
    fi
  }
  _dino_search() {
    local picked hist
    hist=$(command mktemp "${TMPDIR:-/tmp}/dino-hist.XXXXXX") || return 0
    HISTTIMEFORMAT= builtin history | sed 's/^ *[0-9]*  //' | awk '{a[NR]=$0} END {for (i = NR; i > 0; i--) print a[i]}' >"$hist"
    # The terminal's own device: macOS can't wait for keys on /dev/tty.
    picked=$(command "$_DINO_BIN" search --pick --query "$READLINE_LINE" --history "$hist" <"$(tty)")
    command rm -f "$hist"
    if [[ -n $picked ]]; then
      READLINE_LINE=$picked
      READLINE_POINT=${#READLINE_LINE}
    fi
  }
  bind -x '"\e[57300~": _dino_ai_line'
  bind -x "\"${DINO_AI_KEY:-\\ei}\": _dino_ai_line"
  bind -x '"\e[57301~": _dino_ai_agent'
  bind -x '"\e\C-m": _dino_ai_agent'
  bind -x '"\er": _dino_search'
  [[ -n $DINO_SEARCH_CTRL_R ]] && bind -x '"\C-r": _dino_search'
else
  # Old bash can't change the line from a key, so ⌘I (Alt+I) makes it a # request and ⌘⏎
  # (Alt+Enter) a #@ one, and enters it, through keys of its own for the start of the line and
  # Enter, whatever the user bound.
  bind '"\e[57397~": beginning-of-line'
  bind '"\e[57398~": accept-line'
  bind '"\e[57300~": "\e[57397~# \e[57398~"'
  bind "\"${DINO_AI_KEY:-\\ei}\": \"\\e[57397~# \\e[57398~\""
  bind '"\e[57301~": "\e[57397~#@ \e[57398~"'
  bind '"\e\C-m": "\e[57397~#@ \e[57398~"'
fi
