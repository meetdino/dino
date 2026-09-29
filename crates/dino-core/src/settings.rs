//! `settings.toml`: the one document of user choices, owned by dinod. The app reads and writes it
//! through dinod as JSON; the TUI, being in-process, uses it directly.
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
    /// A fan-out worktree is trusted when its repo is, so Claude doesn't ask again for each one.
    pub worktree_trust: bool,
    /// Most tokens (input, cache and output) one routed session may use; 0 means no limit.
    pub session_token_budget: u64,
    /// When a session's PR merges and its dino worktree has nothing left to lose, archive the session
    /// (its worktree goes, and comes back from the branch if it's started again).
    pub close_merged: bool,
    /// Offer the permission mode that never asks. Off, it's hidden and refused.
    pub allow_bypass: bool,
}

impl Default for Policies {
    fn default() -> Self {
        Self { allowed_agents: vec![], default_agent: None, worktree_trust: true, session_token_budget: 0, close_merged: false, allow_bypass: false }
    }
}

impl Policies {
    pub fn allows(&self, short: &str) -> bool {
        short == "shell" || self.allowed_agents.is_empty() || self.allowed_agents.iter().any(|a| a == short)
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

pub const DEFAULT_WORKTREE_LOCATION: &str = ".dino/worktrees";
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
        let ok = !p.is_empty()
            && !p.starts_with(['/', '-', '.'])
            && !p.contains("..")
            && !p.contains("//")
            && !p.chars().any(|c| c.is_whitespace() || c.is_control() || "~^:?*[\\".contains(c));
        if ok { p.to_string() } else { DEFAULT_BRANCH_PREFIX.into() }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default)]
pub struct Machine {
    /// Onboarding finished; skip the welcome scan.
    pub onboarded: bool,
    /// Keep the Mac from idle-sleeping while tasks are scheduled, so they run on time. Closing the lid still sleeps it.
    pub keep_awake: bool,
}

impl Settings {
    pub fn path() -> PathBuf {
        config_dir().join("settings.toml")
    }

    /// What's in effect: the user's settings with the organization's (see [`Managed`]) over them.
    pub fn load() -> Self {
        Self::load_user().managed_by(&Managed::load())
    }

    /// The user's own settings; on first use, what the old `config` file said.
    pub fn load_user() -> Self {
        match std::fs::read_to_string(Self::path()) {
            Ok(text) => toml::from_str(&text).unwrap_or_default(),
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

/// Settings an organization deploys, like Claude Code's `managed-settings.json`: the same shape as
/// `Settings`, any part of it. What it sets wins over the user's value and can't be changed in dino.
/// `managed-settings.json` is the base; `managed-settings.d/*.json` beside it go on top in name order.
/// A file that isn't a JSON object is skipped.
#[derive(Debug, Clone, Default)]
pub struct Managed {
    doc: serde_json::Value,
    /// Key paths it sets, like `["policies", "allow_bypass"]`.
    pub locked: Vec<Vec<String>>,
}

impl Managed {
    /// Only an admin can write it. `DINO_MANAGED_SETTINGS` names another file (for tests).
    pub fn path() -> PathBuf {
        std::env::var_os("DINO_MANAGED_SETTINGS")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/Library/Application Support/Dino/managed-settings.json"))
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
        for f in files {
            let Ok(text) = std::fs::read_to_string(&f) else { continue };
            match serde_json::from_str(&text) {
                Ok(v @ serde_json::Value::Object(_)) => merge(&mut doc, v),
                _ => eprintln!("dino: ignoring {}: not a JSON object", f.display()),
            }
        }
        let mut locked = vec![];
        leaves(&doc, &mut vec![], &mut locked);
        Self { doc, locked }
    }

    /// Locked key paths joined with dots, as clients get them.
    pub fn locked_paths(&self) -> Vec<String> {
        self.locked.iter().map(|p| p.join(".")).collect()
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
    ("NVIDIA_API_KEY", "NVIDIA NIM: the free tier's models"),
    ("TYPESAFE_API_KEY", "TypeSafe Jev: picks the model for each free-tier turn"),
];

/// A key's name and where it comes from, never its value.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct KeyInfo {
    pub name: String,
    pub purpose: Option<String>,
    /// "dino" (the key store), "environment" (wins over the store), or none: not set.
    pub source: Option<String>,
}

fn stored() -> Vec<(String, String)> {
    std::fs::read_to_string(keys_file())
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .filter(|(k, _)| !k.is_empty())
        .collect()
}

pub fn key_status() -> Vec<KeyInfo> {
    let stored = stored();
    let mut names: Vec<String> = KNOWN_KEYS.iter().map(|(k, _)| k.to_string()).collect();
    names.extend(stored.iter().map(|(k, _)| k.clone()).filter(|k| !KNOWN_KEYS.iter().any(|(n, _)| n == k)));
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
    let valid = !name.is_empty() && name.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    anyhow::ensure!(valid, "key names are like NVIDIA_API_KEY");
    let value = value.map(str::trim).filter(|v| !v.is_empty());
    anyhow::ensure!(value.is_none_or(|v| !v.contains(['\n', '\r', '='])), "that doesn't look like a key");
    let mut keys: Vec<_> = stored().into_iter().filter(|(k, _)| k != name).collect();
    if let Some(v) = value {
        keys.push((name.to_string(), v.to_string()));
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
        let mut s3 = Settings::default();
        s3.policies = Policies { default_agent: Some("codex".into()), session_token_budget: 5, ..p };
        s3.save().unwrap();
        assert_eq!(Settings::load(), s3);
        assert!(!Settings::default().policies.allow_bypass, "bypass hidden by default");

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
        let status = key_status();
        assert!(status.iter().any(|k| k.name == "DINO_TEST_B_KEY" && k.source.as_deref() == Some("dino")));
        assert!(set_key("bad name", Some("x")).is_err());
        assert!(set_key("DINO_TEST_A_KEY", Some("a\nb")).is_err());
        std::fs::remove_dir_all(dir).unwrap();
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
