# Settings

Dino keeps user settings in `~/.config/dino/settings.toml`. If `DINO_HOME` is set, it uses
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

When Dino Account sync is enabled, these settings follow the account to its other Macs: `routing`,
`policies`, `worktrees`, `agents`, `ssh`, `terminal`, `tmux`, and `fallbacks`. Repository variables
sync by the repository's Git remote, so different checkout paths on two Macs still match. Only
well-formed variable names that are safe to sync travel; variables that can change which code runs
or where traffic goes (such as `PATH`, `SHELL`, and proxy variables) stay on the Mac where they
were set. Repository variables are stored unencrypted; don't put passwords or tokens in them.

The `machine` settings stay on each Mac, except `machine.shell_integration`, which syncs as a
terminal setting. The `experimental` table stays local. API keys, account tokens, and other values
in Dino's separate key store are not settings and never sync.

## Tables and keys

All names and strings below are written in TOML. Empty arrays and maps mean “not restricted” or
“not set” where stated. A value shown as *unset* is omitted until you choose one.

### `[routing]`

Synced. In the app: **Models & Providers → Providers**.

- `proxy` (`boolean`, default `true`): send agent traffic through Dino's local proxy, which enables
  usage counting, token limits, fallbacks, and provider routing. Applies to new sessions.

### `[policies]`

Synced. The app shows these across **Agents**, **Agents → Limits**, **Workspaces → Worktrees**, and
**Experimental**.

- `allowed_agents` (array of strings, default `[]`): agent short names Dino may start. Empty means
  all agents are allowed; `shell` is always allowed.
- `default_agent` (optional string, default *unset*): agent short name started by ⌘N. Unset uses
  Claude Code, or the first allowed agent when Claude Code is unavailable.
- `worktree_trust` (`boolean`, default `true`): trust a folder inside a repository in its Dino
  worktrees when that folder is trusted in the repository.
- `session_token_budget` (unsigned 64-bit integer, default `0`): maximum input, cached, and output
  tokens for one routed session. `0` means no limit.
- `close_merged` (`boolean`, default `false`): archive a worktree session after its pull request is
  merged or closed. After a merge, Dino also removes its worktree when doing so would not lose work;
  after a close, it keeps the worktree.
- `allow_bypass` (`boolean`, default `true`): allow permission modes that never ask. Turn this off
  to hide and refuse those modes.
- `session_tools` (`boolean`, default `false`): give Claude sessions Dino's cross-session tools.
  Although this key lives under `[policies]`, the app places its switch in **Experimental**.
- `fallback_providers` (array of strings, default `[]`): provider IDs agents may use as fallbacks.
  Empty means any compatible provider.

### `[machine]`

Local to this Mac, except `shell_integration` (see the sync note above). The app exposes most
user-facing values under **General**, **Terminal**, **Agents**, **Power**, and
**Workspaces → Worktrees**. `onboarded` and the two Ghostty-derived values are maintained by Dino.

- `onboarded` (`boolean`, default `false`): whether first-run onboarding has finished. Dino updates
  this itself.
- `keep_awake` (`boolean`, default `false`): keep the Mac awake while scheduled automations exist.
  App: **Power**.
- `awake_while_working` (`boolean`, default `true`): keep the Mac awake while any agent is working.
  App: **Power**.
- `shell_integration` (`boolean`, default `true`): enable Dino's prompt and folder integration for
  new shells. App: **Terminal → Shell**. This one value syncs.
- `shell_agents` (`boolean`, default `true`): treat agents typed into an integrated shell as Dino
  sessions and show them in the sidebar. App: **Agents**.
- `lid` (table, defaults below): whether agents may keep running with the lid closed. App: **Power**.
- `claude_token` (table, defaults below): where the Claude Code subscription token may be used.
  The token itself is stored separately and locally. App: **Agents → Claude Code Subscription
  Token**.
- `check_updates` (`boolean`, default `true`): check daily for updates and install them automatically
  where Dino manages updates. Homebrew installations remain managed by Homebrew. App: **General →
  Updates**.
- `build_cache` (table, defaults below): whether Rust builds share an `sccache` cache on this Mac.
  App: **Workspaces → Worktrees → Build Cache**.
- `shell_integration_mode` (string, default `"detect"`): the shell integration mode last read from
  the user's Ghostty configuration.
- `shell_features` (string, default `"cursor,title"`): Ghostty shell-integration features last read
  by Dino. Dino updates both `shell_integration_mode` and `shell_features` from the Ghostty config;
  they are not separate controls in Settings.

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

- `enabled` (`boolean`, default `true`): share one `sccache` cache across this Mac's Dino sessions
  and shells. It has no effect until `sccache` is installed.
- `size_gb` (32-bit unsigned integer, default `10`, allowed range `1`–`500`): maximum cache size;
  least-recently-used entries are removed first.

### `[worktrees]`

Synced. In the app: **Workspaces → Worktrees**.

- `location` (string, default `"~/.dino/worktrees"`): where Dino creates worktrees. A relative path
  is inside each repository; an absolute path or `~/…` uses a separate folder per repository.
- `branch_prefix` (string, default `"dino/"`): prefix for branches Dino creates.

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
  restarted session in this repository. Variables are stored unencrypted. When the repository has
  a Git remote, safe variables sync by that remote; variables that may execute code or redirect
  traffic remain local. Do not store credentials here.

Example:

```toml
[repos."/Users/me/src/project".env]
RUST_LOG = "debug"
```

### `[ssh."<host>"]`

One table per SSH host or alias, synced. In the app: **Workspaces → SSH Hosts**.

- `folder` (string, default `""`): remote directory where a new session starts when no folder was
  selected. Empty uses that host's home directory.

SSH usernames, ports, keys, and jump hosts remain in `~/.ssh/config`; Dino does not copy them into
these settings.

### `[terminal]`

Synced. In the app: `start_with` and `on_quit` are in **General**; appearance, quick-terminal, and
shell AI choices are in **Terminal**.

- `start_with` (string, default `"last"`; `"last"` or `"shell"`): open the last session or a new shell
  when Dino opens.
- `quick_key` (string, default `"cmd-grave"`): app key name for the quick-terminal shortcut; `"off"`
  disables it.
- `quick_autohide` (`boolean`, default `true`): hide the quick terminal when another app or window
  is clicked.
- `on_quit` (string, default `""`): what to do with running agents when quitting. Empty asks each
  time; otherwise this stores the choice made in the app.
- `appearance` (string, default `"system"`; `"system"`, `"light"`, or `"dark"`): app and terminal
  appearance.
- `ask_agent` (string, default `""`): agent ID used for the shell's ⌘I command suggestion. Empty
  lets Dino choose an agent that can answer once.
- `ask_model` (string, default `""`): model name for `ask_agent`; empty uses that agent's default.
- `handoff_agent` (string, default `""`): agent short name that receives a shell request as a new
  session on ⌘⏎. Empty uses the ⌘I agent.

### `[tmux]`

Synced. In the app: **tmux**.

- `show_agents` (`boolean`, default `false`): show Dino agents as tmux windows.
- `session` (string, default `"dino"`): tmux session for those windows. Empty uses the session
  currently attached to the shell.
- `new_tabs` (string, default `""`): tmux session for new tabs; empty leaves new tabs outside tmux.

### `[experimental]`

Local to this Mac. In the app: **Experimental**.

- `free_models` (`boolean`, default `false`): offer Dino's free hosted-model pool.
- `computer_use` (`boolean`, default `false`): let selected agents use Dino's computer-use tools.

These features are off until enabled. Dino keeps unknown switches in this table when reading and
writing settings, so a newer or older version can carry them through.

### `[fallbacks.<agent>]`

Synced, by agent ID. In the app: **Agents → Limits → When [agent] Hits a Limit**. A running
conversation stays with its agent; only its model route changes. Fallback provider API keys remain
in the local key store and never sync.

- `steps` (array of tables, default `[]`): routes to try in order. Each step has `provider` (string,
  provider ID) and `model` (string, that provider's model name). Each provider must support the
  agent's API.
- `on_outage` (`boolean`, default `false`): use the chain when the current route is repeatedly
  failing or unreachable, as well as when it reaches a usage limit.
- `new_sessions` (optional table, default *unset*): agent and optional model for new sessions and
  scheduled tasks while this agent is at its limit.

Within `new_sessions`, `agent` is a string agent ID; `model` is an optional string and defaults to
that agent's own model. Within each `steps` item, `provider` and `model` are strings. Dino preserves
unknown fields inside fallback tables so versions can carry them through.

Example:

```toml
[fallbacks.claude]
on_outage = true

[[fallbacks.claude.steps]]
provider = "openrouter"
model = "anthropic/claude-sonnet"

[fallbacks.claude.new_sessions]
agent = "codex"
```

## Environment variables

These variables are useful when you choose to set them in the environment. They are not keys in
`settings.toml`.

- `DINO_HOME`: use another Dino state directory instead of `~/.config/dino`. Settings, sockets, and
  other local state use this directory. Useful for development or an isolated Dino instance.
- `DINO_CLOUD_URL`: choose the default Dino account and settings-sync server instead of
  `https://cloud.meetdino.com`; useful with a self-hosted server. Set it in the environment of the
  app/daemon that signs in. An explicit URL passed to `dino login` takes precedence.
- `DINO_AI_KEY`: choose the key sequence for the AI line in the installed zsh or bash shell
  integration. The default is `Alt+I` (`\ei`).
- `DINO_MANAGED_SETTINGS`: test override for the path to the organization's managed-settings JSON
  file. In normal installations Dino reads
  `/Library/Application Support/Dino/managed-settings.json` and any `managed-settings.d/*.json`
  files beside it. Managed values override user choices and appear locked in Settings.
