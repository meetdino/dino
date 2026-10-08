# Settings

dino keeps user settings in `~/.config/dino/settings.toml`. If `DINO_HOME` is set, it uses
`$DINO_HOME/settings.toml` instead. `dinod` owns this TOML file; the app reads and changes it
through `dinod`, and the `dino` command reads it directly. Missing fields use the defaults below,
so a file only needs to contain the choices you changed.

```toml
[routing]
proxy = true

[worktrees]
location = "~/.dino/worktrees"
branch_prefix = "dino/"

[agents.claude]
model = "opus"
```

The app's **Settings** window edits the same settings. Some values are recorded automatically
(for example, the shell integration options read from Ghostty); these are described below.

## Sync and local settings

When you're signed in to a dino account, these settings sync to your other Macs: `routing`,
`policies`, `worktrees`, `agents`, `ssh`, `terminal`, `tmux`, and `fallbacks`. Repository variables
sync by the repository's Git remote, so different checkout paths on two Macs still match; a
repository with no remote doesn't sync its variables. Only
well-formed variable names that are safe to sync travel; variables that can change which code runs
or where traffic goes (such as `PATH`, `SHELL`, and proxy variables) stay on the Mac where they
were set. Repository variables are stored unencrypted; don't put passwords or tokens in them.

The `machine` settings stay on each Mac, except `machine.shell_integration`, which syncs as a
terminal setting. The `experimental` table stays local. API keys, account tokens, and other values
in dino's separate key store are not settings and never sync.

## Tables and keys

All names and strings below are written in TOML. Empty arrays and maps mean “not restricted” or
“not set” where stated. A value shown as *unset* is omitted until you choose one.

### `[routing]`

Synced. In the app: **Models & Providers → Providers**.

- `proxy` (`boolean`, default `true`): send agent traffic through dino's local proxy, which enables
  usage counting, token limits, fallbacks, and provider routing. Applies to new sessions.

### `[policies]`

Synced. The app location for each setting is listed below.

- `allowed_agents` (array of strings, default `[]`): agent short names dino may start. Empty means
  all agents are allowed; `shell` is always allowed. App: **Agents**.
- `default_agent` (optional string, default *unset*): agent short name started by ⌘N. Unset uses
  Claude Code, or the first allowed agent when Claude Code is unavailable. App: **Agents**.
- `worktree_trust` (`boolean`, default `true`): when a folder inside a repository is trusted in
  Claude Code, mark the same folder trusted in each worktree dino makes, and remove the mark when the
  worktree is removed. Claude Code already carries the repository's own trust into its worktrees.
  App: **Workspaces → Worktrees → Trust**.
- `session_token_budget` (unsigned 64-bit integer, default `0`): maximum input, cached, and output
  tokens for one routed session. `0` means no limit. App: **Agents → Limits**.
- `close_merged` (`boolean`, default `false`): archive a worktree session after its pull request is
  merged or closed. After a merge, dino also removes its worktree when doing so would not lose work;
  after a close, it keeps the worktree. App: **Workspaces → Worktrees**.
- `allow_bypass` (`boolean`, default `true`): allow the `bypass` permission mode, which never asks.
  Turn this off to hide and refuse that mode. App: **Agents → Permissions**.
- `session_tools` (`boolean`, default `false`): give Claude sessions dino's cross-session tools.
  App: **Experimental**.
- `fallback_providers` (array of strings, default `[]`): provider IDs agents may use as fallbacks.
  This key has no Settings control. When set, dino skips fallback steps on other providers, and
  **Agents → Limits → When [agent] Hits a Limit** and `dino fallback` offer only these. Empty means
  any compatible provider.

### `[machine]`

Local to this Mac, except `shell_integration` (see the sync note above). The app exposes most
user-facing values under **General**, **Terminal**, **Agents**, **Power**, and
**Workspaces → Worktrees**. `onboarded` and the two Ghostty-derived values are maintained by dino.

- `onboarded` (`boolean`, default `false`): whether first-run onboarding has finished. dino updates
  this itself.
- `keep_awake` (`boolean`, default `false`): keep the Mac awake while scheduled automations exist.
  App: **Power**.
- `awake_while_working` (`boolean`, default `true`): keep the Mac awake while any agent is working.
  App: **Power**.
- `shell_integration` (`boolean`, default `true`): enable dino's prompt and folder integration for
  new shells. App: **Terminal → Shell**. This one value syncs.
- `shell_agents` (`boolean`, default `true`): treat agents typed into an integrated shell as dino
  sessions and show them in the sidebar. App: **Agents**.
- `lid` (table, defaults below): whether agents may keep running with the lid closed. App: **Power**.
- `claude_token` (table, defaults below): where the Claude Code subscription token may be used.
  The token itself is stored separately and locally. App: **Agents → Claude Code Subscription
  Token**.
- `check_updates` (`boolean`, default `true`): check daily for updates and install them automatically
  where dino manages updates. Homebrew installations remain managed by Homebrew. App: **General →
  Updates**.
- `build_cache` (table, defaults below): whether Rust builds share an `sccache` cache on this Mac.
  App: **Workspaces → Worktrees → Build Cache**.
- `shell_integration_mode` (string, default `"detect"`): the shell integration mode last read from
  the user's Ghostty configuration.
- `shell_features` (string, default `"cursor,title"`): Ghostty shell-integration features last read
  by dino. dino updates both `shell_integration_mode` and `shell_features` from the Ghostty config;
  they are not separate controls in Settings.
- `computer_use` (optional `boolean`, default *unset*, which means on): let agents use the Mac's
  apps. dino installs a pinned, checked release of open-computer-use in its own folder and adds it to
  each agent on this Mac that takes MCP servers, except the agents you turn it off for. When off,
  dino removes what it added. App: **Agents → Computer Use**, and the Welcome screen. While this key
  is unset, an older `[experimental].computer_use` applies.

#### `[machine.lid]`

- `enabled` (`boolean`, default `false`): allow the Mac to stay awake with its lid closed.
- `when` (string enum, default `"working"`; `"working"` or `"open"`): keep awake only while an
  agent is working, or whenever an agent session is open.
- `on_battery` (`boolean`, default `false`): also allow this while on battery power.
- `min_battery` (8-bit unsigned integer percentage, default `30`): battery threshold when
  `on_battery` is enabled.
- `max_hours` (64-bit float, default `8.0`): maximum consecutive hours to keep awake. `0` means no
  limit.

#### `[machine.claude_token]`

- `ssh` (`boolean`, default `true`): let Claude Code sessions started on SSH hosts use the token.
- `local` (`boolean`, default `false`): let sessions on this Mac use it even when Claude Code is
  signed in here. When off, the token is used locally only if Claude Code is not signed in.

#### `[machine.build_cache]`

- `enabled` (`boolean`, default `true`): share one `sccache` cache across this Mac's dino sessions
  and shells. It has no effect until `sccache` is installed.
- `size_gb` (32-bit unsigned integer, default `10`, allowed range `1`–`500`): maximum cache size;
  least-recently-used entries are removed first.

### `[worktrees]`

Synced. In the app: **Workspaces → Worktrees**.

- `location` (string, default `"~/.dino/worktrees"`): where dino creates worktrees. A relative path
  is inside each repository; an absolute path or `~/…` uses a separate folder per repository.
- `branch_prefix` (string, default `"dino/"`): prefix for branches dino creates. A blank value or
  one that cannot start a branch name falls back to `"dino/"`.

### `[agents.<agent>]`

Synced, by agent ID (for example, `claude` or `codex`). In the app: **Agents → New [agent] Sessions**.
Each key is optional and defaults to the agent's own choice.

- `mode` (optional string, default *unset*): permission mode ID the agent starts with, such as
  `ask`, `edits`, `plan`, `auto`, or `bypass` when allowed.
- `model` (optional string, default *unset*): model name, alias, or model ID the agent starts with.
- `effort` (optional string, default *unset*): reasoning-effort level the agent starts with.

### `[repos."<repository path>"]`

One table per repository's main checkout path; the variables also apply to its worktrees. In the
app: **Workspaces → Repositories**.

- `env` (map of strings to strings, default `{}`): environment variables set for every new or
  restarted session in this repository. See the sync note above for which variables are shared.

Example:

```toml
[repos."/Users/me/src/project".env]
RUST_LOG = "debug"
```

### `[ssh."<host>"]`

One table per SSH host or alias, synced. In the app: **Workspaces → SSH Hosts**.

- `folder` (string, default `""`): remote directory where a new session starts when no folder was
  selected. Empty uses that host's home directory.

SSH usernames, ports, keys, and jump hosts remain in `~/.ssh/config`; dino does not copy them into
these settings.

### `[terminal]`

Synced. In the app: `start_with` and `on_quit` are in **General**; appearance, quick-terminal, and
shell AI choices are in **Terminal**.

- `start_with` (string, default `"last"`; `"last"` or `"shell"`): open the last session or a new shell
  when dino opens.
- `quick_key` (string, default `"cmd-grave"`; `"off"`, `"cmd-grave"`, `"opt-space"`, or
  `"ctrl-opt-space"`): app key name for the quick-terminal shortcut. `"off"` disables it.
- `quick_autohide` (`boolean`, default `true`): hide the quick terminal when another app or window
  is clicked.
- `on_quit` (string, default `""`; `""`, `"keep"`, or `"stop"`): what to do with running agents
  when quitting. `""` asks each time; `"keep"` leaves them running and `"stop"` stops them.
- `appearance` (string, default `"system"`; `"system"`, `"light"`, or `"dark"`): app and terminal
  appearance.
- `ask_agent` (string, default `""`): agent ID used for the shell's ⌘I command suggestion. Empty
  uses the ⌘N default agent if it can answer, or the first installed agent that can.
- `ask_model` (string, default `""`): model for `ask_agent`; this setting applies only when
  `ask_agent` is set. Empty uses that agent's new-session model: `[agents.<agent>].model` when
  configured, otherwise the agent's default model.
- `handoff_agent` (string, default `""`): agent short name that receives a shell request as a new
  session on ⌘⏎. Empty uses the ⌘I agent.

### `[tmux]`

Synced. In the app: **tmux**.

- `show_agents` (`boolean`, default `false`): show dino agents as tmux windows.
- `session` (string, default `"dino"`): tmux session for those windows. Empty uses the session
  currently attached to the shell.
- `new_tabs` (string, default `""`): tmux session for new tabs; empty leaves new tabs outside tmux.

### `[experimental]`

Local to this Mac. In the app: **Experimental**.

- `free_models` (`boolean`, default `false`): offer the free hosted-model pool. Requires an NVIDIA
  API key. When enabled and a TypeSafe key is configured, dino sends up to the first 8,000 characters
  of each turn's prompt to `api.typesafe.ai` for model selection; while this is off, nothing is sent.
  App: **Experimental → Free models pool**.
- `computer_use` (optional `boolean`, default *unset*): where earlier versions kept the computer-use
  switch. dino reads it only while `[machine].computer_use` is unset, so an old `false` keeps
  computer use off. New choices are saved to `[machine].computer_use`. It has no control in Settings.

`free_models` is off until you turn it on. dino keeps unknown switches in this table when reading and
writing settings, so a newer or older version can carry them through.

### `[fallbacks.<agent>]`

Synced, by agent ID. In the app: **Agents → Limits → When [agent] Hits a Limit**. A running
conversation stays with its agent; only its model route changes. Fallback provider API keys remain
in the local key store and never sync.

- `steps` (array of tables, default `[]`): routes to try in order. Each step has `provider` (string,
  provider ID from **Models & Providers**) and `model` (string, that provider's model name).
  Provider IDs are `openrouter`, `chatgpt`, a coding plan as `plan-<plan>` (for example `plan-zai`,
  `plan-kimi` or `plan-other`), a server on this Mac (`ollama`, `lmstudio`, `llamacpp` or `vllm`),
  and `free`, which requires `[experimental].free_models = true`. Each provider must support the
  agent's API.
- `on_outage` (`boolean`, default `false`): use the chain when the current route is repeatedly
  failing or unreachable, as well as when it reaches a usage limit.
- `new_sessions` (optional table, default *unset*): agent and optional model for new sessions and
  scheduled tasks while this agent is at its limit.

Within `new_sessions`, `agent` is a string agent ID; `model` is an optional string and defaults to
that agent's own model. Within each `steps` item, `provider` and `model` are strings. dino preserves
unknown fields inside fallback tables so versions can carry them through.

Example:

```toml
[fallbacks.claude]
on_outage = true

[[fallbacks.claude.steps]]
provider = "openrouter"
model = "anthropic/claude-sonnet-5.5"

[fallbacks.claude.new_sessions]
agent = "codex"
```

## Environment variables

These variables are useful when you choose to set them in the environment. They are not keys in
`settings.toml`.

- `DINO_HOME`: use another dino state directory instead of `~/.config/dino`. Settings, sockets, and
  other local state use this directory. Useful for development or an isolated dino instance.
- `DINO_CLOUD_URL`: choose the default dino account and settings-sync server instead of
  `https://cloud.meetdino.com`; useful with a self-hosted server. Only `dinod` reads this variable,
  so set it in the daemon's environment, for example `DINO_CLOUD_URL=https://cloud.example dino daemon`.
  An explicit URL passed to `dino login` takes precedence.
- `DINO_AI_KEY`: choose the key sequence for the AI line in the installed zsh, bash or fish shell
  integration. The default is `Alt+I` (`\ei`).
- `DINO_MANAGED_SETTINGS`: test override for the path to the organization's managed-settings JSON
  file. In normal installations dino reads
  `/Library/Application Support/Dino/managed-settings.json` and any `managed-settings.d/*.json`
  files beside it. Managed values override user choices and appear locked in Settings.
