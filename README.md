# dino

A terminal for the agent era, on Ghostty's core. dino finds every coding agent already on your
Mac, and runs, watches and resumes them all in one place: Claude Code, Codex, Qwen Code, Kimi Code,
Pi and Hermes, plus your shells.

- **Finds what's already running.** Agents you started in other terminals, in tmux or in a dino
  shell show up in the sidebar with what they're doing, and you can pick any of them up in dino.
- **Agents keep running.** A background daemon, `dinod`, owns every session. Close the window, quit
  the app, even restart dinod: sessions come back where they were.
- **Knows which one needs you.** Working, Needs you, Done and Idle, from each agent's own signals,
  with notifications and a Dock badge.
- **A real terminal.** Tabs, splits, your Ghostty config, themes and keybinds, and real tmux inside
  it, untouched.
- **Worktrees, reviews and PRs.** A session per git worktree, a diff view to comment on, fan-out of
  one prompt to several agents, and pull requests from the app.
- **Any model in any agent.** A local proxy routes an agent to another provider's model, and
  counts what each session uses. Traffic goes from your Mac to the provider, nowhere else.
- **No account needed.** Sign in only to sync settings between Macs. API keys never leave the Mac.

## Install

macOS 14 or later, on Apple silicon.

```sh
brew install asdf9384/tap/dino            # the app, with the dino command
```

Or download the DMG from the [latest release](https://github.com/asdf9384/dino-releases/releases/latest).
For just the command line:

```sh
curl -fsSL https://meetdino.com/install.sh | sh
```

dino updates itself.

## Build from source

You need the Xcode Command Line Tools (`xcode-select --install`) and Rust
([rustup](https://rustup.rs)). No Apple developer account or signing keys.

```sh
git clone https://github.com/asdf9384/dino.git
cd dino
cargo build --release      # the dino command and dinod: target/release/dino
./app/build.sh             # Dino.app, ad hoc signed: app/build/Dino.app
```

`./app/build.sh` builds the Rust workspace too, and prints where the app is. Open it from there;
it uses the `dino` it was built with.

Try a build without touching the dino you use every day: give it its own home, and it runs its own
`dinod` there.

```sh
DINO_HOME=/tmp/dino-dev ./target/release/dino new shell
```

## Using the command line

```sh
dino .                    # a shell in this folder, shown in the app
dino . claude             # Claude Code in this folder
dino ls                   # every session
dino attach <id>          # a session in this terminal
dino found                # agents running elsewhere on this Mac
dino --help               # the rest
```

Settings live in `~/.config/dino/settings.toml`, which `dinod` reads and writes. The app's
Settings window edits the same file.

## How it's built

- `dinod` (Rust) owns sessions, the local proxy, settings and sign-in. Everything else is a client
  of it over a Unix socket.
- The app (Swift, SwiftUI) draws terminals with [libghostty](https://github.com/Lakr233/libghostty-spm).
- `dino` (Rust) is the command line, and `dinod` itself.

[ARCHITECTURE.md](ARCHITECTURE.md) has the whole picture: crates, how they may depend on each
other, and the contracts other projects build on.

## Contributing

Issues and pull requests are welcome. [CONTRIBUTING.md](CONTRIBUTING.md) covers setup, tests, the
performance budget, and signing off your commits (DCO). Please report security issues privately:
see [SECURITY.md](SECURITY.md).

## License

MIT, see [LICENSE](LICENSE). Third-party code dino includes, and its licenses, are listed in
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
