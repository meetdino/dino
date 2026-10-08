//! `settings.toml`: the one document of user choices, owned by dinod. The app reads and writes it
//! through dinod as JSON; the `dino` command line reads it directly.
//!
//! Split for a later sync: `routing` travels with the user, `machine` stays on this Mac.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

use crate::controls::Controls;
use crate::{config_dir, keys_file};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default)]
pub struct Settings {
    pub routing: Routing,
    pub policies: Policies,
    pub machine: Machine,
    pub worktrees: Worktrees,
    /// Mode, model and effort new sessions start with, by agent id ("claude", "codex").
    pub agents: BTreeMap<String, Controls>,
    /// Per repository, by the path of its main checkout; also applies in its worktrees.
    pub repos: BTreeMap<String, Repo>,
    /// Machines to run sessions on over SSH, by the host as `ssh` takes it (an alias from
    /// `~/.ssh/config`, or `user@host`).
    pub ssh: BTreeMap<String, SshHost>,
    pub terminal: Terminal,
    pub tmux: Tmux,
    pub experimental: Experimental,
    /// What each agent falls back to when the route it uses hits a limit, by agent id ("claude",
    /// "codex"). Only routes are named here; their keys stay in the key store.
    pub fallbacks: BTreeMap<String, Fallback>,
}

/// When an agent's route is spent (its plan's window, its subscription's limit, its balance): the
/// routes dinod's proxy sends its calls to instead, in order, and the agent new sessions start
/// with meanwhile. A running conversation stays with its agent; only routes change under it.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default)]
pub struct Fallback {
    /// Tried in order. Each must serve the API the agent speaks; the free models only while
    /// they're turned on (Settings → Experimental).
    pub steps: Vec<FallbackStep>,
    /// Also when a route is down (server errors several times in a row, or unreachable), not
    /// only at its limit.
    pub on_outage: bool,
    /// While the agent is at its limit, new sessions and scheduled tasks start with this agent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_sessions: Option<AgentSwitch>,
    /// Fields from a newer dino, kept as they are.
    #[serde(flatten)]
    pub extra: Extra,
}

/// One route to fall back to: a provider (as Settings → Models & Providers lists it:
/// "plan-zai", "openrouter", "ollama", "chatgpt", or "free") and its model.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default)]
pub struct FallbackStep {
    pub provider: String,
    pub model: String,
    #[serde(flatten)]
    pub extra: Extra,
}

/// Another agent to start new sessions with, and its model (none: its own default).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default)]
pub struct AgentSwitch {
    pub agent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// Fields a newer dino wrote that this one doesn't know: kept, and written back as they came.
/// Nulls go (TOML has none).
#[derive(Serialize, Debug, Clone, PartialEq, Default)]
pub struct Extra(pub BTreeMap<String, serde_json::Value>);

impl<'de> Deserialize<'de> for Extra {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        fn strip(v: &mut serde_json::Value) {
            match v {
                serde_json::Value::Object(o) => {
                    o.retain(|_, x| !x.is_null());
                    o.values_mut().for_each(strip);
                }
                serde_json::Value::Array(a) => {
                    a.retain(|x| !x.is_null());
                    a.iter_mut().for_each(strip);
                }
                _ => {}
            }
        }
        let mut m = BTreeMap::<String, serde_json::Value>::deserialize(d)?;
        m.retain(|_, v| !v.is_null());
        m.values_mut().for_each(strip);
        Ok(Self(m))
    }
}

/// Features still being tried out, each off until turned on, and for this Mac only (it never
/// syncs). Every one is a switch, so a client that doesn't know one yet can carry it through.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default)]
pub struct Experimental {
    /// The free models pool: agents on free hosted models (NVIDIA NIM), with dino picking one for
    /// each turn. With a TypeSafe key, picking sends the turn's text (up to 8,000 characters) to
    /// api.typesafe.ai. Off, the free tier isn't offered and its requests are refused unsent.
    pub free_models: bool,
    /// Where computer use was switched while it was being tried out. Read only for a Mac whose
    /// [machine] doesn't say ([`Settings::computer_use`]): a `false` here keeps it off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub computer_use: Option<bool>,
    /// Switches this dino doesn't know (a newer one's, or a feature since removed), kept as they are.
    #[serde(flatten)]
    pub extra: Extra,
}

/// For people who live in tmux. Their tmux stays theirs: dino never edits its config, never takes
/// a key from it, and only ever adds, renames and removes windows it made itself.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct Tmux {
    /// Show dino's agents as windows in your tmux, each running `dino attach`. Closing one (or the
    /// whole server) leaves the agent running in dinod; its window comes back.
    pub show_agents: bool,
    /// The session they go in, made when needed; empty: the session you're attached to.
    pub session: String,
    /// New tabs attach to this tmux session (`tmux new -A -s`), made when needed; empty: off.
    pub new_tabs: String,
}

impl Default for Tmux {
    fn default() -> Self {
        Self { show_agents: false, session: "dino".into(), new_tabs: String::new() }
    }
}

impl Tmux {
    /// A session name dino passes to tmux as is: letters, digits, `-`, `_` and `.`.
    pub fn valid_name(name: &str) -> bool {
        !name.is_empty() && name.len() <= 64 && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    }
}

/// How the terminal itself behaves: the app's choices, kept here so they follow the person to
/// their other Macs. The values are the app's own names for them.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct Terminal {
    /// What opening dino shows: "last" (the session you left) or "shell" (a new shell).
    pub start_with: String,
    /// The quick terminal's shortcut, by the app's name for it ("off" for none).
    pub quick_key: String,
    /// Hide the quick terminal when something else is clicked.
    pub quick_autohide: bool,
    /// What quitting does with running agents: empty asks, else the choice the app remembered.
    pub on_quit: String,
    /// The app's look: "system" (follow the Mac), "light" or "dark"; its panes follow it too.
    pub appearance: String,
    /// Who ⌘I in a shell asks for a command (`dino ai suggest`), by agent id; empty: the default
    /// agent if it can answer that way, else Claude Code, Codex or another that can, whichever is
    /// here first.
    pub ask_agent: String,
    /// The model ⌘I asks `ask_agent`, by the agent's own name for it; empty, or with no agent
    /// chosen: the one its new sessions start with.
    pub ask_model: String,
    /// Who ⌘⏎ hands the line to as a new session (`dino ai agent`), by launcher; empty: ⌘I's agent.
    pub handoff_agent: String,
}

impl Default for Terminal {
    fn default() -> Self {
        Self {
            start_with: "last".into(),
            quick_key: "cmd-grave".into(),
            quick_autohide: true,
            on_quit: String::new(),
            appearance: "system".into(),
            ask_agent: String::new(),
            ask_model: String::new(),
            handoff_agent: String::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default)]
pub struct SshHost {
    /// Where sessions start when no folder is given: a path on that machine, `~` for its home.
    pub folder: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default)]
pub struct Repo {
    /// Set in the environment of every session started in the repo.
    pub env: BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct Routing {
    /// Send agent API traffic through dino's local proxy (usage, free tier). New sessions only.
    pub proxy: bool,
}

impl Default for Routing {
    fn default() -> Self {
        Self { proxy: true }
    }
}

/// Rules for what agents may do. Global for now; per repo (keyed by remote) later.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct Policies {
    /// Launchers dino may start (by short name); empty means all. The shell is always allowed.
    pub allowed_agents: Vec<String>,
    /// What ⌘N starts; none means Claude Code, or the first allowed agent.
    pub default_agent: Option<String>,
    /// A trusted folder inside a repo is trusted in its worktrees too, so Claude doesn't ask again
    /// for each one (the repo's own trust carries over by itself).
    pub worktree_trust: bool,
    /// Most tokens (input, cache and output) one routed session may use; 0 means no limit.
    pub session_token_budget: u64,
    /// When a session's PR merges or closes, archive the session once its turn is over. After a
    /// merge its dino worktree goes too when nothing would be lost (and comes back from the branch
    /// if it's started again); after a close the worktree stays.
    pub close_merged: bool,
    /// Offer the permission mode that never asks (on unless turned off). Off, it's hidden and
    /// refused. On, agents that can are started with it in their mode key's cycle, to be switched
    /// to without a restart (see `Agent::reach_args`).
    pub allow_bypass: bool,
    /// Opt-in: give Claude sessions dino's tools (`dino mcp`) to list, read, message and start other sessions.
    pub session_tools: bool,
    /// The providers agents may fall back to when theirs is spent (Settings → Agents), by id;
    /// empty means any. An organization narrows it here.
    pub fallback_providers: Vec<String>,
}

impl Default for Policies {
    fn default() -> Self {
        Self { allowed_agents: vec![], default_agent: None, worktree_trust: true, session_token_budget: 0, close_merged: false, allow_bypass: true, session_tools: false, fallback_providers: vec![] }
    }
}

impl Policies {
    pub fn allows(&self, short: &str) -> bool {
        short == "shell" || self.allowed_agents.is_empty() || self.allowed_agents.iter().any(|a| a == short)
    }

    /// Whether agents may fall back to provider `id`.
    pub fn allows_fallback(&self, id: &str) -> bool {
        self.fallback_providers.is_empty() || self.fallback_providers.iter().any(|p| p == id)
    }
}

/// Where the worktrees dino makes go, and what their branches are called.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct Worktrees {
    /// Relative: inside each repo (and excluded from its status). Absolute or `~/…`: one folder
    /// per repo under it.
    pub location: String,
    /// Put before every branch dino makes.
    pub branch_prefix: String,
}

/// Outside the repo, so its worktrees don't nest copies of it in its own file tree.
pub const DEFAULT_WORKTREE_LOCATION: &str = "~/.dino/worktrees";
pub const DEFAULT_BRANCH_PREFIX: &str = "dino/";

impl Default for Worktrees {
    fn default() -> Self {
        Self { location: DEFAULT_WORKTREE_LOCATION.into(), branch_prefix: DEFAULT_BRANCH_PREFIX.into() }
    }
}

impl Worktrees {
    /// The branch prefix, or the default when it's blank or can't start a branch name.
    pub fn prefix(&self) -> String {
        let p = self.branch_prefix.trim();
        let ok = !p.is_empty() && !p.starts_with(['/', '-', '.']) && !p.contains("..") && !p.contains("//") && !p.chars().any(|c| c.is_whitespace() || c.is_control() || "~^:?*[\\".contains(c));
        if ok { p.to_string() } else { DEFAULT_BRANCH_PREFIX.into() }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct Machine {
    /// Onboarding finished; skip the welcome scan.
    pub onboarded: bool,
    /// Keep the Mac from idle-sleeping while automations are scheduled, so they run on time. Closing the lid still sleeps it.
    pub keep_awake: bool,
    /// Keep the Mac from idle-sleeping while any agent is working, whichever agent it is: dinod
    /// holds a power assertion of its own until none is. Closing the lid still sleeps it.
    pub awake_while_working: bool,
    /// Shells dino starts mark their prompts and report their folder, as in Ghostty (new shells only).
    pub shell_integration: bool,
    /// An agent typed into a dino shell (`claude`, `codex`) is that shell's session's agent while it
    /// runs, as one dino started is: its turns, questions, tasks, mode and model, and its
    /// conversation resumed in the shell when dinod restarts. Off, it's a plain program. Needs the
    /// shell integration.
    pub shell_agents: bool,
    /// Keep agents running with the lid closed. Off unless turned on, and for this Mac only.
    pub lid: Lid,
    /// Where Claude Code gets the Claude subscription token (`crate::claude_token`).
    pub claude_token: ClaudeTokenUse,
    /// Look for a new dino once a day and install it: the app through Sparkle, a `dino` installed
    /// with install.sh by dinod itself. Homebrew installs are left to `brew upgrade`.
    pub check_updates: bool,
    /// One compiler cache shared by every session's builds on this Mac (see [`BuildCache`]).
    pub build_cache: BuildCache,
    /// Ghostty's `shell-integration` in the user's Ghostty config, as the app last read it:
    /// `detect` (by the shell's name), `none`, or the shell to set shells up as (`fish`…).
    pub shell_integration_mode: String,
    /// Ghostty's `shell-integration-features` as Ghostty hands them to a shell
    /// (GHOSTTY_SHELL_FEATURES: `cursor:blink,path,title`), as the app last read them.
    pub shell_features: String,
    /// Agents can use the Mac's apps: dino installs open-computer-use (a pinned, checked release)
    /// in its own folder and adds it to every agent here that takes MCP servers, with each agent's
    /// own command, but for one you turned it off for. Off, dino removes every one it added (see
    /// dinod's `computer_use`). Nothing leaves the Mac either way. Unset: [`Settings::computer_use`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub computer_use: Option<bool>,
}

/// One compiler cache for the whole Mac: every agent dino starts, and every dino shell, builds Rust
/// through sccache (when it's installed), so a new worktree compiles only what no other worktree
/// has compiled before. Each worktree keeps its own `target/`. A broken or full cache never fails a
/// build: it compiles as if there were none (see `crate::build_cache`).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct BuildCache {
    /// On unless turned off; it does nothing until sccache is installed.
    pub enabled: bool,
    /// The most the cache keeps on disk, in GB; the least recently used goes first.
    pub size_gb: u32,
}

impl BuildCache {
    pub const DEFAULT_SIZE_GB: u32 = 10;
    /// What `size_gb` is held to.
    pub const SIZES_GB: std::ops::RangeInclusive<u32> = 1..=500;

    /// `size_gb`, held to [`Self::SIZES_GB`].
    pub fn size(&self) -> u32 {
        self.size_gb.clamp(*Self::SIZES_GB.start(), *Self::SIZES_GB.end())
    }
}

impl Default for BuildCache {
    fn default() -> Self {
        Self { enabled: true, size_gb: Self::DEFAULT_SIZE_GB }
    }
}

/// Which Claude Code sessions the Claude subscription token goes to, beyond the rule that only
/// the real Claude Code ever gets it.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct ClaudeTokenUse {
    /// Sessions on SSH environments, where Claude Code usually isn't signed in.
    pub ssh: bool,
    /// Sessions and dino's own `claude -p` on this Mac too, over its own sign-in. Off: they use
    /// it only while Claude Code here isn't signed in.
    pub local: bool,
}

impl Default for ClaudeTokenUse {
    fn default() -> Self {
        Self { ssh: true, local: false }
    }
}

impl Default for Machine {
    fn default() -> Self {
        Self {
            onboarded: false,
            keep_awake: false,
            awake_while_working: true,
            shell_integration: true,
            shell_agents: true,
            lid: Lid::default(),
            claude_token: ClaudeTokenUse::default(),
            check_updates: true,
            build_cache: BuildCache::default(),
            shell_integration_mode: "detect".into(),
            shell_features: "cursor,title".into(),
            computer_use: None,
        }
    }
}

/// When the Mac stays awake with its lid closed (`pmset disablesleep`, see [`crate::power`]).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct Lid {
    pub enabled: bool,
    /// Only while an agent is working, or whenever dino has an agent session open.
    pub when: LidWhen,
    /// Also on battery, down to `min_battery` percent; otherwise only on the power adapter.
    pub on_battery: bool,
    pub min_battery: u8,
    /// Sleep comes back after this many hours awake in a row; 0 for no limit.
    pub max_hours: f64,
}

impl Default for Lid {
    fn default() -> Self {
        Self { enabled: false, when: LidWhen::Working, on_battery: false, min_battery: 30, max_hours: 8.0 }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum LidWhen {
    #[default]
    Working,
    Open,
}

impl Settings {
    pub fn path() -> PathBuf {
        config_dir().join("settings.toml")
    }

    /// What's in effect: the user's settings with the organization's (see [`Managed`]) over them.
    pub fn load() -> Self {
        let m = Managed::load();
        let mut s = Self::load_user().managed_by(&m);
        // An organization that set computer use where it was (Experimental) still sets it.
        if let Some(on) = m.doc.pointer("/experimental/computer_use").and_then(|v| v.as_bool())
            && m.doc.pointer("/machine/computer_use").is_none()
        {
            s.machine.computer_use = Some(on);
        }
        s
    }

    /// Whether agents get open-computer-use: on unless turned off, here or (before it left
    /// Experimental, where saving wrote `false` until it was turned on) in [experimental].
    pub fn computer_use(&self) -> bool {
        self.machine.computer_use.or(self.experimental.computer_use).unwrap_or(true)
    }

    /// The user's own settings; on first use, what the old `config` file said. A file that doesn't
    /// parse stays as it is, and nothing saves over it (see `error`); until it's fixed, the last
    /// settings read from it here are in effect, or the defaults.
    pub fn load_user() -> Self {
        static LAST_GOOD: std::sync::Mutex<Option<Settings>> = std::sync::Mutex::new(None);
        match std::fs::read_to_string(Self::path()) {
            Ok(text) => match toml::from_str::<Self>(&text) {
                Ok(s) => {
                    *LAST_GOOD.lock().unwrap() = Some(s.clone());
                    s
                }
                Err(e) => {
                    log_once(&parse_error(&text, &e));
                    LAST_GOOD.lock().unwrap().clone().unwrap_or_default()
                }
            },
            Err(_) => {
                let old = std::fs::read_to_string(config_dir().join("config")).unwrap_or_default();
                let get = |k: &str| old.lines().find_map(|l| l.strip_prefix(k)?.strip_prefix('=').map(str::trim).map(String::from));
                Self {
                    routing: Routing { proxy: get("route").as_deref() != Some("false") },
                    policies: Policies::default(),
                    machine: Machine { onboarded: get("onboarded").as_deref() == Some("true"), ..Default::default() },
                    ..Self::default()
                }
            }
        }
    }

    /// Why `settings.toml` doesn't parse, when it doesn't: "settings.toml has an error on line 3: …".
    pub fn error() -> Option<String> {
        let text = std::fs::read_to_string(Self::path()).ok()?;
        toml::from_str::<Self>(&text).err().map(|e| parse_error(&text, &e))
    }

    /// What `agent_id` starts with where a new session leaves a control open.
    pub fn agent_defaults(&self, agent_id: &str) -> Controls {
        self.agents.get(agent_id).cloned().unwrap_or_default()
    }

    /// `self` with the managed values over it. A managed document that doesn't fit is ignored whole,
    /// so a typo can't leave half a policy.
    fn managed_by(self, m: &Managed) -> Self {
        if m.locked.is_empty() {
            return self;
        }
        let Ok(mut v) = serde_json::to_value(&self) else { return self };
        merge(&mut v, m.doc.clone());
        serde_json::from_value(v).unwrap_or_else(|e| {
            eprintln!("dino: ignoring managed settings: {e}");
            self
        })
    }

    /// Save as the user's settings. Values the organization sets may come back unchanged (the app
    /// sends the whole document) but not changed, and the user's own value for them is kept, so it
    /// returns if the managed file goes.
    pub fn save(&self) -> anyhow::Result<()> {
        let m = Managed::load();
        let mut mine = serde_json::to_value(self)?;
        if !m.locked.is_empty() {
            let effective = serde_json::to_value(self.clone().managed_by(&m))?;
            if let Some(path) = m.locked.iter().find(|p| at(&mine, p) != at(&effective, p)) {
                anyhow::bail!("“{}” is set by your organization and can't be changed", path.join("."));
            }
            let user = serde_json::to_value(Self::load_user())?;
            for path in &m.locked {
                put(&mut mine, path, at(&user, path).cloned());
            }
        }
        serde_json::from_value::<Self>(mine)?.write()
    }

    fn write(&self) -> anyhow::Result<()> {
        // Saving now would write over the file the user is in the middle of fixing.
        if let Some(e) = Self::error() {
            anyhow::bail!("{e}. Fix it, then change the setting again");
        }
        for k in self.repos.values().flat_map(|r| r.env.keys()) {
            let valid = !k.is_empty() && !k.starts_with(|c: char| c.is_ascii_digit()) && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            anyhow::ensure!(valid, "{k:?} isn't a valid environment variable name");
        }
        std::fs::create_dir_all(config_dir())?;
        // Write then rename, so a reader never sees half a file.
        let tmp = Self::path().with_extension("toml.tmp");
        // Private: repo environments can hold secrets.
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
        f.write_all(toml::to_string(self)?.as_bytes())?;
        drop(f);
        std::fs::rename(tmp, Self::path())?;
        Ok(())
    }
}

fn parse_error(text: &str, e: &toml::de::Error) -> String {
    let line = e.span().map_or(1, |s| text[..s.start.min(text.len())].matches('\n').count() + 1);
    format!("settings.toml has an error on line {line}: {}", e.message().trim_end_matches('\n'))
}

/// Into dinod's log, once per different error: settings are read all the time.
fn log_once(message: &str) {
    static LAST: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());
    let mut last = LAST.lock().unwrap();
    if *last != message {
        eprintln!("dino: {message}; until it's fixed, the settings before it are in effect and nothing saves over it");
        *last = message.to_string();
    }
}

/// Settings an organization deploys, like Claude Code's `managed-settings.json`: the same shape as
/// `Settings`, any part of it. What it sets wins over the user's value and can't be changed in dino.
/// `managed-settings.json` is the base; `managed-settings.d/*.json` beside it go on top in name order.
/// A file that isn't a JSON object is skipped.
#[derive(Debug, Clone, Default)]
pub struct Managed {
    doc: serde_json::Value,
    /// Key paths it sets, like `["policies", "allow_bypass"]`.
    pub locked: Vec<Vec<String>>,
    /// The file each locked path's value comes from, by dotted path: the last to set it.
    pub from: std::collections::BTreeMap<String, PathBuf>,
}

/// Where an admin puts dino's managed settings.
#[cfg(target_os = "macos")]
const MANAGED_PATH: &str = "/Library/Application Support/Dino/managed-settings.json";
#[cfg(not(target_os = "macos"))]
const MANAGED_PATH: &str = "/etc/dino/managed-settings.json";

impl Managed {
    /// Only an admin can write it. `DINO_MANAGED_SETTINGS` names another file (for tests).
    pub fn path() -> PathBuf {
        std::env::var_os("DINO_MANAGED_SETTINGS").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(MANAGED_PATH))
    }

    pub fn load() -> Self {
        let base = Self::path();
        let mut files = vec![base.clone()];
        if let Some(dir) = base.parent().and_then(|p| std::fs::read_dir(p.join("managed-settings.d")).ok()) {
            let mut more: Vec<PathBuf> = dir.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect();
            more.sort();
            files.extend(more);
        }
        let mut doc = serde_json::Value::Object(Default::default());
        let mut from = std::collections::BTreeMap::new();
        for f in files {
            let Ok(text) = std::fs::read_to_string(&f) else { continue };
            match serde_json::from_str(&text) {
                Ok(v @ serde_json::Value::Object(_)) => {
                    let mut set = vec![];
                    leaves(&v, &mut vec![], &mut set);
                    from.extend(set.into_iter().map(|p| (p.join("."), f.clone())));
                    merge(&mut doc, v);
                }
                _ => eprintln!("dino: ignoring {}: not a JSON object", f.display()),
            }
        }
        let mut locked = vec![];
        leaves(&doc, &mut vec![], &mut locked);
        Self { doc, locked, from }
    }

    /// Locked key paths joined with dots, as clients get them.
    pub fn locked_paths(&self) -> Vec<String> {
        self.locked.iter().map(|p| p.join(".")).collect()
    }

    /// Each locked path's file, as clients get them.
    pub fn locked_from(&self) -> std::collections::BTreeMap<String, String> {
        self.locked_paths()
            .into_iter()
            .map(|p| {
                let f = self.from.get(&p).cloned().unwrap_or_else(Self::path);
                (p, f.display().to_string())
            })
            .collect()
    }
}

/// `over` into `into`: objects key by key, anything else replaced.
fn merge(into: &mut serde_json::Value, over: serde_json::Value) {
    match (into, over) {
        (serde_json::Value::Object(a), serde_json::Value::Object(b)) => {
            for (k, v) in b {
                merge(a.entry(k).or_insert(serde_json::Value::Null), v);
            }
        }
        (a, b) => *a = b,
    }
}

fn leaves(v: &serde_json::Value, path: &mut Vec<String>, out: &mut Vec<Vec<String>>) {
    match v {
        serde_json::Value::Object(m) => {
            for (k, v) in m {
                path.push(k.clone());
                leaves(v, path, out);
                path.pop();
            }
        }
        _ => out.push(path.clone()),
    }
}

fn at<'a>(v: &'a serde_json::Value, path: &[String]) -> Option<&'a serde_json::Value> {
    path.iter().try_fold(v, |v, k| v.get(k))
}

/// Set `path` in `v`, making objects on the way; `None` removes it.
fn put(v: &mut serde_json::Value, path: &[String], value: Option<serde_json::Value>) {
    let Some((last, parents)) = path.split_last() else { return };
    let mut cur = v;
    for k in parents {
        if !cur.is_object() {
            *cur = serde_json::Value::Object(Default::default());
        }
        cur = cur.as_object_mut().unwrap().entry(k.clone()).or_insert(serde_json::Value::Object(Default::default()));
    }
    if let Some(obj) = cur.as_object_mut() {
        match value {
            Some(x) => {
                obj.insert(last.clone(), x);
            }
            None => {
                obj.remove(last);
            }
        }
    }
}

/// Keys dino itself uses, and what for. Others in the store are listed too, without a purpose.
pub const KNOWN_KEYS: &[(&str, &str)] = &[
    ("NVIDIA_API_KEY", "NVIDIA NIM: models for the free models pool"),
    ("TYPESAFE_API_KEY", "TypeSafe Jev: picks a model for each turn in the free models pool"),
    ("CLAUDE_CODE_OAUTH_TOKEN", "Claude subscription token from claude setup-token: only Claude Code uses it"),
];

/// A key's name and where it comes from, never its value.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct KeyInfo {
    pub name: String,
    pub purpose: Option<String>,
    /// "dino" (the key store), "environment" (wins over the store), or none: not set.
    pub source: Option<String>,
}

/// What the key store holds, without the environment's keys (`load_keys` adds those).
pub fn stored() -> Vec<(String, String)> {
    std::fs::read_to_string(keys_file())
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .filter(|(k, _)| !k.is_empty())
        .collect()
}

/// The start of every key the dino account's sign-in keeps in the key store (dinod's `cloud`).
pub const ACCOUNT_TOKEN_PREFIX: &str = "DINO_CLOUD_";

pub fn key_status() -> Vec<KeyInfo> {
    let stored = stored();
    let mut names: Vec<String> = KNOWN_KEYS.iter().map(|(k, _)| k.to_string()).collect();
    // Sign in with ChatGPT's tokens and coding plans' keys are Settings → Providers' to keep, and
    // the dino account's are Settings → Dino Account's: not keys to edit or count here.
    names.extend(stored.iter().map(|(k, _)| k.clone()).filter(|k| {
        !KNOWN_KEYS.iter().any(|(n, _)| n == k)
            && !k.starts_with("CHATGPT_")
            && !k.starts_with(ACCOUNT_TOKEN_PREFIX)
            && !k.starts_with("CLAUDE_ACCOUNT_")
            && !k.starts_with(crate::plans::KEY_PREFIX)
            && k != crate::claude_token::CREATED_KEY
    }));
    names
        .into_iter()
        .map(|name| {
            let env = std::env::var(&name).is_ok_and(|v| !v.is_empty());
            let source = if env {
                Some("environment".into())
            } else if stored.iter().any(|(k, v)| *k == name && !v.is_empty()) {
                Some("dino".into())
            } else {
                None
            };
            let purpose = KNOWN_KEYS.iter().find(|(k, _)| *k == name).map(|(_, p)| p.to_string());
            KeyInfo { name, purpose, source }
        })
        .collect()
}

/// Store (or with `None`, remove) a key in the 600 key store, keeping the others.
pub fn set_key(name: &str, value: Option<&str>) -> anyhow::Result<()> {
    set_keys(&[(name, value)])
}

/// Several keys at once (each kept, or removed with no value), all or none.
pub fn set_keys(changes: &[(&str, Option<&str>)]) -> anyhow::Result<()> {
    let mut keys = stored();
    for &(name, value) in changes {
        let valid = !name.is_empty() && name.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
        anyhow::ensure!(valid, "key names are like NVIDIA_API_KEY");
        let value = value.map(str::trim).filter(|v| !v.is_empty());
        anyhow::ensure!(value.is_none_or(|v| !v.contains(['\n', '\r', '='])), "that doesn't look like a key");
        keys.retain(|(k, _)| k != name);
        if let Some(v) = value {
            keys.push((name.to_string(), v.to_string()));
        }
    }
    std::fs::create_dir_all(config_dir())?;
    let tmp = keys_file().with_extension("tmp");
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
    for (k, v) in keys {
        writeln!(f, "{k}={v}")?;
    }
    drop(f);
    std::fs::rename(tmp, keys_file())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// docs/settings.md documents every key, in its table's section, and nothing that isn't one.
    #[test]
    fn settings_reference_lists_every_key() {
        let doc = include_str!("../../../docs/settings.md");
        // Every optional value and map filled in, so that each key is written.
        let mut s = Settings::default();
        s.policies.default_agent = Some("claude".into());
        s.machine.computer_use = Some(true);
        s.experimental.computer_use = Some(true);
        s.agents.insert("claude".into(), Controls { mode: Some("ask".into()), model: Some("opus".into()), effort: Some("high".into()) });
        s.repos.insert("/repo".into(), Repo { env: [("VAR".to_string(), "1".to_string())].into() });
        s.ssh.insert("host".into(), SshHost::default());
        let step = FallbackStep { provider: "openrouter".into(), model: "m".into(), extra: Extra::default() };
        let switch = AgentSwitch { agent: "codex".into(), model: Some("m".into()), extra: Extra::default() };
        s.fallbacks.insert("claude".into(), Fallback { steps: vec![step], new_sessions: Some(switch), ..Default::default() });
        // Each key as (the table whose section documents it, the key, directly in that table).
        fn keys(v: &serde_json::Value, table: &str, direct: bool, out: &mut Vec<(String, String, bool)>) {
            let serde_json::Value::Object(m) = v else { return };
            for (k, v) in m {
                out.push((table.to_string(), k.clone(), direct));
                match v {
                    // A table of its own, with its own section: `[machine.lid]`.
                    serde_json::Value::Object(_) if table == "machine" => keys(v, &format!("{table}.{k}"), true, out),
                    // Values that are the user's own names (repo variables) aren't keys.
                    serde_json::Value::Object(_) if k == "env" => {}
                    serde_json::Value::Object(_) => keys(v, table, false, out),
                    serde_json::Value::Array(a) => a.iter().for_each(|x| keys(x, table, false, out)),
                    _ => {}
                }
            }
        }
        let mut found = vec![];
        for (table, v) in serde_json::to_value(&s).unwrap().as_object().unwrap() {
            if matches!(table.as_str(), "agents" | "repos" | "ssh" | "fallbacks") {
                // Keyed by agent, path or host: each entry is the table.
                v.as_object().unwrap().values().for_each(|e| keys(e, table, true, &mut found));
            } else {
                keys(v, table, true, &mut found);
            }
        }
        // The doc's sections, by table: `### `[agents.<agent>]`` is agents' section.
        let mut sections: BTreeMap<String, String> = BTreeMap::new();
        let mut current = None;
        for line in doc.lines() {
            if line.starts_with("## ") {
                current = None;
            } else if let Some(h) = line.strip_prefix("### `[").or_else(|| line.strip_prefix("#### `[")) {
                let name = h.split("]`").next().unwrap();
                let table = name.split(".<").next().unwrap().split(".\"").next().unwrap().to_string();
                current = Some(table);
            } else if let Some(t) = &current {
                let text = sections.entry(t.clone()).or_default();
                text.push_str(line);
                text.push('\n');
            }
        }
        for t in serde_json::to_value(&s).unwrap().as_object().unwrap().keys() {
            assert!(sections.contains_key(t), "docs/settings.md has no section for [{t}]");
        }
        for (table, key, direct) in &found {
            let text = sections.get(table).unwrap_or_else(|| panic!("docs/settings.md has no section for [{table}]"));
            let said = if *direct { format!("- `{key}` (") } else { format!("`{key}`") };
            assert!(text.contains(&said), "docs/settings.md doesn't document {table}.{key}");
        }
        // Its examples are settings.
        for example in doc.split("```toml\n").skip(1).map(|b| b.split("```").next().unwrap()) {
            toml::from_str::<Settings>(example).unwrap_or_else(|e| panic!("an example in docs/settings.md doesn't parse: {e}\n{example}"));
        }
        // And each key it lists is one.
        for (table, text) in &sections {
            for line in text.lines() {
                let Some(key) = line.strip_prefix("- `").and_then(|l| l.split_once("` (")).map(|(k, _)| k) else { continue };
                assert!(found.iter().any(|(t, k, d)| t == table && k == key && *d), "docs/settings.md lists {table}.{key}, which isn't a setting");
            }
        }
    }

    #[test]
    fn settings_and_keys() {
        let dir = std::env::temp_dir().join(format!("dino-settings-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        unsafe { std::env::set_var("DINO_HOME", &dir) };

        std::fs::write(dir.join("config"), "onboarded=true\nroute=false\n").unwrap();
        let s = Settings::load();
        assert!(s.machine.onboarded && !s.routing.proxy, "migrated from config");
        let mut s2 = s.clone();
        s2.routing.proxy = true;
        s2.save().unwrap();
        assert_eq!(Settings::load(), s2);
        std::fs::write(Settings::path(), "[routing]\nproxy = false\n").unwrap();
        assert_eq!(Settings::load(), Settings { routing: Routing { proxy: false }, ..Settings::default() }, "missing tables default");
        assert!(Settings::load().policies.worktree_trust, "trust on by default");
        assert!(!Settings::load().policies.close_merged, "closing merged sessions off by default");
        let p = Policies { allowed_agents: vec!["codex".into()], ..Policies::default() };
        assert!(p.allows("codex") && p.allows("shell") && !p.allows("claude"));
        let s3 = Settings { policies: Policies { default_agent: Some("codex".into()), session_token_budget: 5, ..p }, ..Settings::default() };
        s3.save().unwrap();
        assert_eq!(Settings::load(), s3);
        assert!(Settings::default().policies.allow_bypass, "bypass offered by default");
        assert!(!Settings::default().policies.session_tools, "session tools off by default");
        assert!(!Settings::load().experimental.free_models, "free models off unless turned on");

        // A switch for a feature since removed (fan out) loads, and stays in the file.
        std::fs::write(Settings::path(), "[experimental]\ncomputer_use = true\nfan_out = true\n").unwrap();
        let old = Settings::load();
        assert!(old.computer_use());
        old.save().unwrap();
        let text = std::fs::read_to_string(Settings::path()).unwrap();
        assert!(text.contains("fan_out = true"), "{text}");
        assert_eq!(Settings::load(), old);

        // Computer use: on for a new Mac; a `false` from when it was Experimental stays off, and
        // the switch where it is now says last.
        assert!(Settings::default().computer_use(), "on by default");
        std::fs::write(Settings::path(), "[routing]\nproxy = true\n").unwrap();
        assert!(Settings::load().computer_use(), "on where nothing says");
        std::fs::write(Settings::path(), "[experimental]\ncomputer_use = false\n").unwrap();
        let mut was_off = Settings::load();
        assert!(!was_off.computer_use(), "off as it was");
        was_off.save().unwrap();
        assert!(!Settings::load().computer_use(), "still off once saved again");
        was_off.machine.computer_use = Some(true);
        was_off.save().unwrap();
        assert!(Settings::load().computer_use(), "turned on where it is now");
        let mut on = Settings::default();
        on.machine.computer_use = Some(false);
        on.save().unwrap();
        let text = std::fs::read_to_string(Settings::path()).unwrap();
        assert!(text.contains("computer_use = false") && !text.contains("[experimental]\ncomputer_use"), "{text}");
        assert!(!Settings::load().computer_use());
        std::fs::write(Settings::path(), "").unwrap();
        Settings::default().save().unwrap();
        let text = std::fs::read_to_string(Settings::path()).unwrap();
        assert!(!text.contains("computer_use"), "a default isn't written, so it stays the default: {text}");

        let mut s4 = Settings::default();
        s4.agents.insert("claude".into(), Controls { model: Some("haiku".into()), ..Controls::default() });
        s4.repos.insert("/src/app".into(), Repo { env: [("API_TOKEN".to_string(), "t0k=en".to_string())].into() });
        s4.save().unwrap();
        let back = Settings::load();
        assert_eq!(back, s4);
        assert_eq!(back.agent_defaults("claude").model.as_deref(), Some("haiku"));
        assert!(back.agent_defaults("codex").is_empty());
        assert_eq!(std::fs::metadata(Settings::path()).unwrap().permissions().mode() & 0o777, 0o600, "may hold secrets");
        s4.repos.insert("/x".into(), Repo { env: [("BAD NAME".to_string(), "v".to_string())].into() });
        assert!(s4.save().is_err());

        // A typo in the file: the last good settings stay in effect, the file stays, and nothing
        // saves over it.
        let good = Settings::load_user();
        let typo = "[routing]\nproxy = false\n\n[policies\nclose_merged = true\n";
        std::fs::write(Settings::path(), typo).unwrap();
        assert_eq!(Settings::load_user(), good);
        let error = Settings::error().unwrap();
        assert!(error.starts_with("settings.toml has an error on line 4: "), "{error}");
        let err = Settings::default().save().unwrap_err().to_string();
        assert!(err.starts_with(&error), "{err}");
        assert_eq!(std::fs::read_to_string(Settings::path()).unwrap(), typo, "the user's file is untouched");
        std::fs::write(Settings::path(), "").unwrap();
        assert!(Settings::error().is_none());

        // Managed: wins, locks, and leaves the user's own value in the file.
        let managed = dir.join("managed-settings.json");
        unsafe { std::env::set_var("DINO_MANAGED_SETTINGS", &managed) };
        let mut mine = Settings::default();
        mine.policies.allow_bypass = true;
        mine.policies.session_token_budget = 7;
        mine.save().unwrap();
        std::fs::write(&managed, r#"{"policies": {"allow_bypass": false, "allowed_agents": ["claude"]}, "routing": {"proxy": true}}"#).unwrap();
        std::fs::create_dir_all(dir.join("managed-settings.d")).unwrap();
        std::fs::write(dir.join("managed-settings.d/10-budget.json"), r#"{"policies": {"session_token_budget": 1000000}}"#).unwrap();
        std::fs::write(dir.join("managed-settings.d/20-broken.json"), "{nope").unwrap();
        let m = Managed::load();
        assert_eq!(m.locked_paths(), ["policies.allow_bypass", "policies.allowed_agents", "policies.session_token_budget", "routing.proxy"]);
        let from = m.locked_from();
        assert_eq!(from["policies.allow_bypass"], managed.display().to_string());
        assert_eq!(from["policies.session_token_budget"], dir.join("managed-settings.d/10-budget.json").display().to_string());
        let s = Settings::load();
        assert!(!s.policies.allow_bypass && s.policies.allowed_agents == ["claude"] && s.policies.session_token_budget == 1_000_000);
        assert!(Settings::load_user().policies.allow_bypass);
        let mut changed = s.clone();
        changed.policies.allow_bypass = true;
        let err = changed.save().unwrap_err().to_string();
        assert!(err.contains("policies.allow_bypass"), "{err}");
        let mut other = s.clone();
        other.policies.close_merged = true;
        other.save().unwrap();
        let user = Settings::load_user();
        assert!(user.policies.close_merged && user.policies.allow_bypass && user.policies.session_token_budget == 7, "own values kept under the lock");
        std::fs::write(&managed, r#"{"policies": {"allow_bypass": "yes"}}"#).unwrap();
        std::fs::remove_dir_all(dir.join("managed-settings.d")).unwrap();
        assert!(Settings::load().policies.allow_bypass, "a managed value that doesn't fit is ignored");
        std::fs::remove_file(&managed).unwrap();
        assert!(Managed::load().locked.is_empty());
        unsafe { std::env::remove_var("DINO_MANAGED_SETTINGS") };

        set_key("DINO_TEST_A_KEY", Some(" abc ")).unwrap();
        set_key("DINO_TEST_B_KEY", Some("def")).unwrap();
        set_key("DINO_TEST_A_KEY", None).unwrap();
        assert_eq!(std::fs::read_to_string(keys_file()).unwrap(), "DINO_TEST_B_KEY=def\n");
        assert_eq!(std::fs::metadata(keys_file()).unwrap().permissions().mode() & 0o777, 0o600);
        set_key("DINO_CLOUD_REFRESH_TOKEN", Some("rt")).unwrap();
        let status = key_status();
        assert!(status.iter().any(|k| k.name == "DINO_TEST_B_KEY" && k.source.as_deref() == Some("dino")));
        assert!(!status.iter().any(|k| k.name.starts_with(ACCOUNT_TOKEN_PREFIX)), "the dino account's sign-in isn't a key");
        assert!(set_key("bad name", Some("x")).is_err());
        assert!(set_key("DINO_TEST_A_KEY", Some("a\nb")).is_err());
        // DINO_HOME is the whole test process's: other tests are using the folder right now (a lock
        // file, an agent's extension), so only this test's own files go, never the folder.
        for f in ["config", "settings.toml", "keys"] {
            let _ = std::fs::remove_file(dir.join(f));
        }
    }

    /// A fallback chain saves and loads; what a newer dino added to it comes back as it went.
    #[test]
    fn fallbacks_keep_what_a_newer_dino_wrote() {
        let json = serde_json::json!({
            "steps": [{"provider": "plan-zai", "model": "glm-x", "weight": 2}, {"provider": "ollama", "model": "qwen3:4b"}],
            "on_outage": true,
            "new_sessions": {"agent": "codex", "model": null, "note": {"a": null, "b": [1, null]}},
            "max_spend": 5,
            "nothing": null,
        });
        let f: Fallback = serde_json::from_value(json).unwrap();
        assert_eq!(f.steps[0].extra.0.get("weight"), Some(&serde_json::json!(2)));
        assert_eq!(f.new_sessions.as_ref().map(|n| (n.agent.as_str(), n.model.clone())), Some(("codex", None)));
        assert_eq!(f.extra.0.keys().collect::<Vec<_>>(), ["max_spend"], "nulls go");
        let mut s = Settings::default();
        s.fallbacks.insert("claude".into(), f.clone());
        let text = toml::to_string(&s).unwrap();
        let back: Settings = toml::from_str(&text).unwrap();
        assert_eq!(back.fallbacks["claude"], f, "{text}");
        let again: Fallback = serde_json::from_value(serde_json::to_value(&f).unwrap()).unwrap();
        assert_eq!(again, f);
        assert_eq!(serde_json::to_value(&f).unwrap()["new_sessions"]["note"], serde_json::json!({"b": [1]}));
        assert!(Settings::default().fallbacks.is_empty(), "nothing falls back unless set");
        let p = Policies { fallback_providers: vec!["ollama".into()], ..Policies::default() };
        assert!(p.allows_fallback("ollama") && !p.allows_fallback("openrouter") && Policies::default().allows_fallback("openrouter"));
    }

    #[test]
    fn the_ai_line_picks_its_agents_until_told() {
        let older: Settings = toml::from_str("[terminal]\nstart_with = \"shell\"\n").unwrap();
        assert_eq!(older.terminal.start_with, "shell");
        assert!(older.terminal.ask_agent.is_empty() && older.terminal.ask_model.is_empty() && older.terminal.handoff_agent.is_empty());
        let mut s = Settings::default();
        s.terminal.ask_agent = "pi".into();
        s.terminal.ask_model = "small".into();
        s.terminal.handoff_agent = "codex".into();
        assert_eq!(toml::from_str::<Settings>(&toml::to_string(&s).unwrap()).unwrap(), s);
    }

    #[test]
    fn branch_prefix_falls_back_when_unusable() {
        let w = |p: &str| Worktrees { branch_prefix: p.into(), ..Default::default() }.prefix();
        assert_eq!(w("agents/"), "agents/");
        assert_eq!(w(" ben- "), "ben-");
        for bad in ["", "  ", "/x", "-x", "a b/", "a..b/", "x~/", "a:b"] {
            assert_eq!(w(bad), DEFAULT_BRANCH_PREFIX, "{bad:?}");
        }
    }
}
