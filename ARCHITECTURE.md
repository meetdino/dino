# Architecture

dino is a native terminal on Ghostty's core that also runs, watches and resumes every coding agent
on the machine. This repository holds all of it, the account service included.

| Part | What it is | Where |
|---|---|---|
| **The terminal** | The macOS app: Swift, libghostty, tabs, sidebar and panes. A client of dinod over its socket, nothing else. It bundles the `dino` binary. | `app/` |
| **dinod** | The daemon that owns sessions and agents, the local proxy, settings, the dino login and sync. | `crates/dino-daemon` |
| **`dino`** | The command line, a client of dinod. The same binary runs dinod (`dino daemon`). | `crates/dino` |
| **dino-cloud** | Account, sign-in, settings sync and the account page. Self-hostable. | `cloud/` |

## Crates

| Crate | Role | May use |
|---|---|---|
| `dino-core` | Settings, the agent adapters, models and compatibility, discovery, dinod's IPC types. | nothing of ours |
| `dino-sync` | The sync protocol, shared with dino-cloud, so what leaves the Mac is checkable. | `dino-core` |
| `dino-term` | Terminal emulation behind each session. | nothing of ours |
| `dino-router`, `dino-proxy` | The local proxy: per-session routes, coding plans, metering, the free models pool, and noticing an agent's computer or browser use. It runs inside dinod as a library, so there's no extra hop. | `dino-router` |
| `dino-daemon` | dinod, putting it together. | all of the above |
| `dino` | The CLI. | all of the above |

`crates/boundaries` fails the build if a crate reaches across these lines.

`cloud/` is a Cargo workspace of its own: it builds on Linux (dino-core doesn't), pins the Rust
version its host builds with, and keeps the server's dependencies and Postgres tests out of the
terminal's build. It uses `crates/dino-sync` by path, without the `settings` feature, so it never
links dino-core.

## How they connect

```mermaid
flowchart LR
  T["dino terminal"] -- "calls (n windows → 1)" --> D["dinod"]
  CLI["dino CLI"] -- "calls" --> D
  D -- "spawns, in its own pty" --> A["agents: Claude Code, Codex, Kimi, Qwen, Pi, Hermes, CodeWhale, OpenCode, Copilot CLI, Cursor Agent, Amp, shells"]
  A -- "model traffic" --> P["proxy (inside dinod)"]
  P --> M["the provider each agent talks to"]
  D -- "account, sync" --> C["dino-cloud"]
```

How a session starts: the terminal asks dinod for a session. dinod spawns the agent in its own pty,
with its proxy route, controls and `DINO_SESSION`, and the terminal shows it through
`dino attach`. The terminal never spawns an agent itself. An agent typed by hand into a dino shell
is a process in that shell, which dinod notices; it can report to dino from the start, or dinod can
take it over. Agents running elsewhere on the Mac (other terminals, tmux) are found the same way.

Each agent has an adapter in `dino-core/src/agent/` that reads its status from the agent's own
signals: its hooks, its session record or its server. The tool calls dinod reads there also tell it
when an agent is using the computer or a browser.

## One machine, one dinod

dinod is the only process on a machine that owns the config, the dino login and sync. The
terminal and the CLI are its clients. A client that needs dinod and doesn't find it starts it in
the background (the ssh-agent pattern). Agents live in dinod, not in the app: closing or quitting
the app leaves them running, and dinod resumes them after a restart.

Agent traffic goes through the proxy inside dinod and never leaves the machine except to the
provider the agent talks to. dino-cloud sees only synced settings (over TLS, readable by the
service so the account page can show them); API keys and tokens never leave the Mac.

## One config, in layers

`~/.config/dino/` (or `$DINO_HOME`), read through `dino-core`, lowest first:

1. Built-in defaults.
2. Account-wide, synced: agents, providers, policies, keys (opt-in), SSH hosts, repo env.
3. Per product, synced: `[terminal]` and the like; each product reads only its own.
4. This machine only: paths, recent folders, onboarding.
5. Managed by an organisation, over everything.

## Contracts

Other programs build against this repository, so these are versioned, public contracts: dinod's
IPC protocol (`dino_core::ipc`), the config schema (`dino_core::settings`) and the sync protocol
(`dino-sync`). Change them compatibly: unknown fields round-trip, new fields have defaults.
