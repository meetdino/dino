# dino

A home for the coding agents on your Mac: start them, see which ones need you, and act on them,
all in one place.

dino runs agents such as Claude Code, Codex, Qwen, Kimi, Pi and Hermes inside a background
daemon (`dinod`), so they keep running when you close the window. You can reach them from a
terminal UI, a CLI, a native macOS app built on Ghostty, or other agents over MCP.

## Features

- **Persistent sessions.** `dinod` owns each agent's PTY and terminal state. Clients attach and
  detach over a Unix socket.
- **Repo → worktree → session tree.** Sessions are grouped under the git worktree they run in.
- **Fan-out.** Give one prompt to several agents, each in its own worktree, then compare the
  diffs and keep the best one.
- **Model routing.** A local proxy can point any agent at another provider's model, such as
  OpenRouter, the ChatGPT plan, a model server on this Mac, or the free `auto` router.
- **Shell integration.** An AI line for zsh, bash and fish (`dino ai`) and history search
  (`dino search`).
- **MCP server.** `dino mcp` lets agents list, read, message and start other sessions.
- **Settings sync (optional).** Sign in with GitHub (or a link by email) and your settings follow
  you to every Mac; API keys and tokens never leave the Mac. dino works without an account.

## Layout

| Path | What it is |
| --- | --- |
| `crates/dino` | The `dino` binary: TUI and CLI |
| `crates/dino-daemon` | `dinod`: sessions, PTYs, proxy and router; shell integration scripts |
| `crates/dino-core` | Agent catalog and discovery, IPC, settings, worktrees, PRs, schedules |
| `crates/dino-term` | Terminal panes: `alacritty_terminal` emulation rendered with ratatui |
| `crates/dino-proxy` | Local pass-through proxy and provider adapters |
| `crates/dino-router` | The `auto` model: picks a free model per request |
| `crates/dino-sync` | Settings sync: records, hybrid logical clocks, merge, encryption |
| `app/` | `Dino.app`, the SwiftUI macOS app with Ghostty terminal surfaces |
| `docs/` | Design notes, such as [`management-plane.md`](docs/management-plane.md) |

This workspace will split into several repositories; [ARCHITECTURE.md](ARCHITECTURE.md) has the plan, and `crates/boundaries` keeps the crates to it.

## Building

Requirements: a Rust toolchain that supports edition 2024 (Rust 1.85 or later). The app also
needs macOS 14 or later and Swift 6.

Build the CLI and daemon:

```bash
cargo build --release
```

The binary is at `target/release/dino`.

Build the macOS app. This also builds the Rust workspace:

```bash
./app/build.sh
```

The script prints the path to `app/build/Dino.app`, which is ad-hoc signed.

Run the tests:

```bash
cargo test
```

## Usage

```text
dino [agent [args...]] | --welcome
dino ls | new [--worktree] <agent> [--on <provider> <model>] [args...] | attach <id> | resume <id> | kill <id> | ping | stop | daemon
dino found | continue <session-id prefix>
dino mcp [--read-only]
dino fan [--agents claude,codex,...] <prompt> | groups | diff <id> | keep <id> | discard <group>
dino ai suggest|agent -- <request> | search [--json|--pick]
dino init zsh|bash|fish | shell install|uninstall [zsh|bash|fish]
dino login | logout openrouter|chatgpt
```

Some examples:

```bash
dino                                   # open the TUI
dino new --worktree claude             # start Claude Code in a new worktree
dino fan --agents claude,codex "fix the flaky test"
dino shell install zsh                 # add the shell integration to ~/.zshrc
```

Settings live in `~/.config/dino/settings.toml`. `dinod` reads and writes this file.

## License

MIT. See [LICENSE](LICENSE). The bundled shell integration includes third-party code under its
own licenses. See `crates/dino-daemon/shell-integration/`.
