# dino shell integration for fish (`dino init fish | source`).
#
# ⌘I in Dino (Alt+I elsewhere) turns the line, in plain words, into one command from your own
# agent, put on the prompt and never run. A command that could destroy something arrives
# commented out. ⌘⏎ / Alt+Enter hands the line to the agent as a session; Alt+R searches history
# and dino's sessions together.

status is-interactive; or return
set -q _dino_fish; and return
set -g _dino_fish 1
set -q DINO_BIN; or set -g DINO_BIN __DINO_BIN__

function __dino_request
    string trim -- (string replace -r '^#' '' -- (commandline))
end

function __dino_ai_line
    set -l line (__dino_request)
    test -n "$line"; or return
    set -l err (mktemp)
    set -l out (command $DINO_BIN ai suggest --shell fish --cwd $PWD -- $line 2>$err </dev/null)
    set -l rc $status
    set -l why (cat $err)
    rm -f $err
    switch $rc
        case 0
            commandline -r -- $out
        case 10
            # Every line commented out: each line of a multi-line one runs on the one Enter.
            commandline -r -- '# '$out
            echo (set_color red)"⚠ $why: delete the # to run it"(set_color normal) >/dev/tty
        case '*'
            echo "✗ $why" >/dev/tty
    end
    commandline -f repaint
end

function __dino_ai_agent
    set -l line (__dino_request)
    test -n "$line"; or return
    if set -q DINO_SESSION
        set -l out (command $DINO_BIN ai agent --cwd $PWD -- $line 2>&1 </dev/null)
        and commandline -r ''
        echo "$out" >/dev/tty
    else
        commandline -r -- (string escape -- $DINO_BIN)" ai agent -- "(string escape -- $line)
        commandline -f execute
    end
end

function __dino_search
    set -l hist (mktemp)
    history >$hist
    # The terminal's own device: macOS can't wait for keys on /dev/tty.
    set -l tty (tty)
    set -l picked (command $DINO_BIN search --pick --query (commandline) --history $hist <$tty)
    rm -f $hist
    test -n "$picked"; and commandline -r -- $picked
    commandline -f repaint
end

bind \e\[57300~ __dino_ai_line
bind \ei __dino_ai_line
bind \e\[57301~ __dino_ai_agent
bind \e\r __dino_ai_agent
bind \er __dino_search
set -q DINO_SEARCH_CTRL_R; and bind \cr __dino_search
