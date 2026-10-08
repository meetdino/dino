# dino's completions for fish (`dino completions fish`): commands, flags, session ids, agents.
#
# fish loads it from a completions folder (Homebrew's, or ~/.config/fish/completions/dino.fish).
# dino says what fits (`dino __complete`) and never starts dinod for it.

function __dino_complete
    set -l bin __DINO_BIN__
    set -q DINO_BIN; and set bin $DINO_BIN
    set -l current (commandline -ct)
    for line in (command $bin __complete -- (commandline -opc) $current 2>/dev/null)
        switch $line
            case :dirs
                __fish_complete_directories $current
            case :files
                __fish_complete_path $current
            case '*'
                echo $line
        end
    end
end

complete -c dino -f -a '(__dino_complete)'
