<h1 align="center">dino</h1>

<p align="center"><strong>Every agent, one terminal.</strong></p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue" alt="License: MIT"></a>
  <a href="https://github.com/meetdino/dino/releases/latest"><img src="https://img.shields.io/github/v/release/meetdino/dino?label=release" alt="Latest release"></a>
  <img src="https://img.shields.io/badge/macOS-14%2B-lightgrey?logo=apple" alt="macOS 14 or later">
</p>

<p align="center">
  <a href="#install">Install</a> ·
  <a href="#quick-start">Quick start</a> ·
  <a href="#features">Features</a> ·
  <a href="#supported-agents">Agents</a> ·
  <a href="#how-it-works">How it works</a> ·
  <a href="CONTRIBUTING.md">Contributing</a> ·
  <a href="https://www.meetdino.com">Website</a>
</p>

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/images/screenshot-dark.png">
    <img src="docs/images/screenshot-light.png" width="900" alt="The dino window split into two panes: on the left, Claude Code on Opus 5.5 has finished adding retries with backoff, its diff and passing tests above the prompt; on the right, Pi has shrunk a Docker image with a multi-stage Dockerfile. The sidebar groups sessions by project: one working, one done, and one that needs you, asking to edit ci.yml.">
  </picture>
</p>

<!-- launch-video -->

dino is a native Mac terminal on Ghostty's core. It finds Claude Code, Codex and the rest wherever
they run on your Mac, keeps them going after you quit, and tells you when one needs you. Close the
sidebar and it's a plain terminal. Free and open source (MIT), no account.

- **Every agent in one sidebar:** the ones dino started, the ones you typed in a shell, and the ones
  running in iTerm2, Terminal, Ghostty or tmux.
- **Agents outlive the window:** a background daemon, `dinod`, owns them. Quit dino and they keep
  working.
- **Knows which one needs you:** Working, Needs you, Done or Idle, from each agent's own signals.
- **A real terminal:** tabs, splits, your Ghostty config, your tmux.

## Install

Requires macOS 14 or later on Apple silicon.

```sh
brew install meetdino/tap/dino
```

Or download [Dino.dmg](https://github.com/meetdino/dino/releases/latest/download/Dino.dmg) and drag
dino to Applications; dino → Install Command Line Tool… puts the `dino` command on your `PATH`
(Homebrew does that for you). For the command line alone, without the app:

```sh
curl -fsSL https://meetdino.com/install.sh | sh
```

dino updates itself. Installed it with `brew install asdf9384/tap/dino`, the tap's old name? Run
`brew install meetdino/tap/dino` once: Homebrew stopped trusting the tap when it moved, and until
then `brew upgrade` skips dino.

## Quick start

1. Open dino. Each tab is a shell (<kbd>⌘T</kbd>), as in any terminal.
2. Type `claude`, `codex` or another [supported agent](#supported-agents). It shows in the sidebar
   with its status, and the toolbar shows its mode, model and effort.
3. <kbd>⌘N</kbd> starts an agent in the current project, <kbd>⌥⌘N</kbd> in a new git worktree of
   its own.
4. Agents already running in other terminals are listed under *On this Mac*. <kbd>⌘K</kbd> browses
   them and your recent conversations; *Continue in dino* brings one over, conversation included.
5. From any terminal, `dino . claude` starts Claude Code in that folder, in dino.
6. Quit. Your agents keep going, and `dino ls` lists them from any terminal.

<kbd>⌘J</kbd> jumps to the session that needs you. <kbd>⌘/</kbd> shows every shortcut.

## Features

- **Finds the agents you already have.** Agents running in iTerm2, Terminal, Ghostty or tmux show
  up in the sidebar with what they're doing. *Continue in dino* brings one over, conversation
  included. One you type in a dino shell is a dino session from the start.
- **Agents outlive the window.** `dinod` owns every session. Quit the app and they keep running; if
  dinod itself restarts, each agent comes back on its conversation.
- **Knows which one needs you.** A notification and a Dock badge when an agent is waiting on you.
- **A real terminal.** Tabs, splits, your Ghostty config, themes and keybinds. Your own tmux runs
  untouched, and *Show agents in tmux* (Settings → tmux) puts dino's agents in a tmux session as
  windows.
- **An AI line in your shell.** <kbd>⌘I</kbd> turns plain English into a command you can check
  before it runs. <kbd>⌘⏎</kbd> hands a bigger request to an agent in a split.
- **A browser for every session.** When an agent starts a dev server, dino offers its page in a
  preview beside the terminal, which keeps its page as you switch sessions.
- **From diff to merged.** A session per git worktree. Comment on lines of its diff and send them
  to the agent, then open the pull request with auto-fix (failed checks go back to the agent with
  their logs) and auto-merge. Uses your `gh` login.
- **Any model.** Each agent keeps its own login. Claude Code, Codex, OpenCode, Kimi Code, Qwen
  Code, Pi, Hermes and CodeWhale can also run through dino's local proxy on OpenRouter, your ChatGPT
  plan, a coding plan's key, or a model on your Mac with Ollama, LM Studio, llama.cpp or vLLM. Each
  session shows its model, and its tokens and context where the agent or the proxy reports them.
- **Keeps going at a limit.** When a subscription or plan runs out, the next call goes on to the
  routes you list for that agent (another plan, OpenRouter, a model on your Mac) until the limit
  resets, and the session says so.
- **Automations.** Start or continue an agent on a schedule, when a PR opens or merges, CI fails or
  someone writes @dino, when files change, or after another run. Start from a ready one (fix CI on
  my PRs, review new PRs, run tests on save) and adjust it. Each run keeps its summary, what it
  changed and its PR.
- **Usage across every agent.** Tokens, models, streaks and speed per provider, from dino's proxy
  and each agent's own records, and, on hover in the sidebar, what each session costs your Mac in
  CPU and memory.
- **Shows when an agent uses your Mac.** When an agent drives your apps or your browser through
  computer use, its session says so, with a Stop button.

## Supported agents

dino starts, resumes and tracks the status of
[Claude Code](https://github.com/anthropics/claude-code),
[Codex](https://github.com/openai/codex),
[GitHub Copilot CLI](https://github.com/github/copilot-cli),
[Cursor Agent](https://cursor.com/docs/cli/overview),
[Amp](https://ampcode.com),
[OpenCode](https://opencode.ai),
[Kimi Code](https://moonshotai.github.io/kimi-code/en/),
[Qwen Code](https://github.com/QwenLM/qwen-code),
[Pi](https://pi.dev),
[Hermes Agent](https://github.com/NousResearch/hermes-agent) and
[CodeWhale](https://github.com/Hmbown/CodeWhale), and your shells. It finds all of them running
in other terminals except Amp, Cursor Agent and Hermes, which it follows once they run in a dino
shell.

It also starts [Crush](https://github.com/charmbracelet/crush) and [Aider](https://aider.chat),
without status. Settings shows which agents are installed, and installs or signs in to the rest
with each agent's own commands.

## Using the command line

```sh
dino                      # the app; in a dino terminal or piped, the sessions
dino .                    # a shell in this folder, shown in the app
dino . claude             # Claude Code in this folder
dino ls                   # every session
dino attach <id>          # a session in this terminal
dino found                # agents running elsewhere on this Mac
dino stats                # usage across every agent: tokens, models, streaks
dino automations          # what dino does by itself, and how each run went
dino --help               # the rest
```

Settings live in `~/.config/dino/settings.toml`, which `dinod` reads and writes. The app's
Settings window edits the same file.

### Tab completion

Tab completes dino's commands and flags, session ids for `attach`, `kill`, `resume`, `rm` and
`fork`, and agents for `dino new` and `dino <folder>`. It's on in the app's shells. In another
terminal, `dino shell install` turns it on with the rest of dino's shell integration, and the
Homebrew formula (`dino-cli`) installs it where bash, zsh and fish look. To add it yourself:

```sh
eval "$(dino completions zsh)"     # in .zshrc; zsh completes once compinit has run
eval "$(dino completions bash)"    # in .bashrc
dino completions fish > ~/.config/fish/completions/dino.fish
```

Completing never starts `dinod`: with it stopped, there are no session ids to offer.

### Inside tmux

A tmux you start in dino runs as in any terminal: its panes load only your own startup files, and
dino still follows the active pane's folder, window name and bells. For zsh in tmux panes to also
tell dino their exit codes and pass on notifications, add this line to your `.zshrc` (it also
turns on tmux's `allow-passthrough` for those panes):

```zsh
[[ -n $TMUX ]] && source "${DINO_HOME:-$HOME/.config/dino}/shell-integration/zsh/dino-tmux.zsh" 2>/dev/null
```

## How it works

- `dinod` (Rust) owns sessions, the local proxy, settings and sign-in. Everything else is a client
  of it over a Unix socket: the app, the `dino` command, and `dino attach` in any terminal.
- The app (Swift, SwiftUI) draws terminals with libghostty, Ghostty's engine, through
  [libghostty-spm](https://github.com/Lakr233/libghostty-spm).
- Each agent has an adapter that reads its status from the agent's own signals: its hooks, the log
  or database it keeps of its turns, or its server.
- `cloud/` is the optional account server (Rust, Postgres) for sign-in and settings sync. You can
  host your own: see [cloud/README.md](cloud/README.md).

[ARCHITECTURE.md](ARCHITECTURE.md) has the whole picture: the crates, how they may depend on each
other, and the contracts other projects build on.

## Privacy

No telemetry, analytics or crash reports. Your code and prompts go only to the model providers
your agents use. Signing in is optional, only to sync settings between Macs, and API keys never
sync. The [privacy page](https://www.meetdino.com/privacy.html) lists everything that leaves your
Mac and when.

## Building from source

You need the Xcode Command Line Tools (`xcode-select --install`) and Rust
([rustup](https://rustup.rs), 1.91 or later: a dependency uses `str::floor_char_boundary`). No
Apple developer account or signing keys.

```sh
git clone https://github.com/meetdino/dino.git
cd dino
cargo build --release      # the dino command and dinod: target/release/dino
./app/build.sh --install   # Dino.app with both inside: ~/Applications/Dino.app
open ~/Applications/Dino.app
```

That's a dino to use every day: it runs dinod from the `dino` inside it, as a release does. Run
`./app/build.sh --install` again after pulling (while dino runs, the new build waits beside it),
then choose Restart to Update in dino. Builds are signed with your Developer ID if the keychain has
one (macOS then keeps dino's permissions from one build to the next), ad hoc otherwise.

`./app/build.sh` alone makes `app/build/Dino.app`, "dino dev", a build to try things in: its own
app preferences and permissions, the `dino` on your `PATH`, and the same dinod as your other dino.
To try a build without touching the dinod you use, give it its own home:

```sh
DINO_HOME=/tmp/dino-dev ./target/release/dino new shell
```

[CONTRIBUTING.md](CONTRIBUTING.md) has the rest: testing without disturbing your own dino, the
checks a pull request runs, and the performance budget.

## Contributing

Issues and pull requests are welcome; [good first issues](https://github.com/meetdino/dino/labels/good%20first%20issue)
are a place to start. [CONTRIBUTING.md](CONTRIBUTING.md) covers setup, testing, and signing off
your commits (DCO). Questions go to [Discussions](https://github.com/meetdino/dino/discussions)
([SUPPORT.md](SUPPORT.md)). Everyone taking part follows the [Code of Conduct](CODE_OF_CONDUCT.md).
Please report security issues privately, as [SECURITY.md](SECURITY.md) describes.

## License

MIT, see [LICENSE](LICENSE). Third-party code dino includes, and its licenses, are listed in
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md), and the account server's in
[cloud/THIRD_PARTY_NOTICES.md](cloud/THIRD_PARTY_NOTICES.md).
