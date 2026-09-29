//! `settings.toml`: the one document of user choices, owned by dinod. The app reads and writes it
//! through dinod as JSON; the TUI, being in-process, uses it directly.
//!
//! Split for a later sync: `routing` travels with the user, `machine` stays on this Mac.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

use crate::{config_dir, keys_file};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default)]
pub struct Settings {
    pub routing: Routing,
    pub policies: Policies,
    pub machine: Machine,
    pub worktrees: Worktrees,
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
}

impl Default for Policies {
    fn default() -> Self {
        Self { allowed_agents: vec![], default_agent: None, worktree_trust: true, session_token_budget: 0, close_merged: false }
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

    /// The saved settings; on first use, what the old `config` file said.
    pub fn load() -> Self {
        match std::fs::read_to_string(Self::path()) {
            Ok(text) => toml::from_str(&text).unwrap_or_default(),
            Err(_) => {
                let old = std::fs::read_to_string(config_dir().join("config")).unwrap_or_default();
                let get = |k: &str| old.lines().find_map(|l| l.strip_prefix(k)?.strip_prefix('=').map(str::trim).map(String::from));
                Self {
                    routing: Routing { proxy: get("route").as_deref() != Some("false") },
                    policies: Policies::default(),
                    machine: Machine { onboarded: get("onboarded").as_deref() == Some("true"), ..Default::default() },
                    ..Default::default()
                }
            }
        }
    }

    pub fn save(&self) -> anyhow::Result<()> {
        std::fs::create_dir_all(config_dir())?;
        // Write then rename, so a reader never sees half a file.
        let tmp = Self::path().with_extension("toml.tmp");
        std::fs::write(&tmp, toml::to_string(self)?)?;
        std::fs::rename(tmp, Self::path())?;
        Ok(())
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
}
