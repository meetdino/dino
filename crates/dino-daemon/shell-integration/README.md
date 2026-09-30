# Shell integration

What dinod loads into the shells it starts, so they mark each prompt (OSC 133), say which folder
they're in (OSC 7) and set the title, like shells in Ghostty. dinod writes these files to
`$DINO_HOME/shell-integration` and points the shell at them (see `src/shell.rs`).

- `bash/ghostty.bash`, `zsh/ghostty-integration`, `zsh/ghostty.zshenv` (`.zshenv` there): from
  [libghostty-spm](https://github.com/Lakr233/libghostty-spm), MIT (`LICENSE-libghostty-spm`),
  unchanged. They're its own rewrite, not Ghostty's or Kitty's GPL scripts.
- `bash/bash-preexec.sh`: [bash-preexec](https://github.com/rcaloras/bash-preexec), MIT
  (`bash/LICENSE-bash-preexec.md`), unchanged.
- `zsh/.zshenv`, `bash/dino.bash`: dino's own.
