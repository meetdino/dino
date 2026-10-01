//! dino's settings as sync records, and back.
//!
//! What travels (one record per value, so two devices changing different settings never clash):
//!
//! | collection | key                    | from                                  |
//! |------------|------------------------|---------------------------------------|
//! | `routing`  | `proxy`                | `routing`                             |
//! | `policies` | a field name           | `policies`                            |
//! | `worktrees`| `location`, `branch_prefix` | `worktrees`                      |
//! | `agents`   | `<agent>.<mode/model/effort>` | `agents`                       |
//! | `ssh`      | the host               | `ssh`                                 |
//! | `repos`    | `<git remote> <VAR>`   | `repos.<path>.env`, by remote: paths differ between Macs |
//! | `terminal` | `shell_integration`, and a field name | `machine.shell_integration`, `terminal` |
//!
//! Only values that differ from the defaults are records, so a missing record means "default".
//! The rest of `machine` stays on each Mac, and a repo without a remote isn't synced. Neither are
//! repo variables that could make a session run code (`syncable_env`): dinod sets repo variables
//! in every session in the repo, so one device could otherwise run code on all the others. The key
//! store (API keys, tokens) never syncs: secrets stay on the Mac they were set on.

use serde_json::{Map, Value};
use std::collections::BTreeMap;

use dino_core::controls::Controls;
use dino_core::settings::{Machine, Policies, Repo, Routing, Settings, SshHost, Terminal, Worktrees};

use crate::record::RecordId;

/// The shape of setting values this dino writes and reads.
pub const SCHEMA: u32 = 1;

/// A setting's value by where it lives.
pub type Entries = BTreeMap<RecordId, Value>;

/// Records for `settings`: `remote_of` gives a repo path's git remote (None: not synced).
pub fn flatten(settings: &Settings, remote_of: &dyn Fn(&str) -> Option<String>) -> Entries {
    let mut out = Entries::new();
    fields(&mut out, "routing", &settings.routing, &Routing::default());
    fields(&mut out, "policies", &settings.policies, &Policies::default());
    fields(&mut out, "worktrees", &settings.worktrees, &Worktrees::default());
    for (agent, c) in &settings.agents {
        for (field, v) in [("mode", &c.mode), ("model", &c.model), ("effort", &c.effort)] {
            if let Some(v) = v {
                out.insert(RecordId::new("agents", format!("{agent}.{field}")), Value::String(v.clone()));
            }
        }
    }
    for (host, h) in &settings.ssh {
        out.insert(RecordId::new("ssh", host.clone()), serde_json::to_value(h).expect("ssh host"));
    }
    for (path, repo) in &settings.repos {
        let Some(remote) = remote_of(path) else { continue };
        for (var, v) in repo.env.iter().filter(|(var, _)| syncable_env(var.as_str())) {
            out.insert(RecordId::new("repos", repo_key(&remote, var)), Value::String(v.clone()));
        }
    }
    if settings.machine.shell_integration != Machine::default().shell_integration {
        out.insert(RecordId::new("terminal", "shell_integration"), Value::Bool(settings.machine.shell_integration));
    }
    fields(&mut out, "terminal", &settings.terminal, &Terminal::default());
    out
}

/// What changed from `old` to `new`: a value to write, or None to delete.
pub fn diff(old: &Entries, new: &Entries) -> Vec<(RecordId, Option<Value>)> {
    let mut out: Vec<(RecordId, Option<Value>)> = new.iter().filter(|(id, v)| old.get(*id) != Some(*v)).map(|(id, v)| (id.clone(), Some(v.clone()))).collect();
    out.extend(old.keys().filter(|id| !new.contains_key(*id)).map(|id| (id.clone(), None)));
    out
}

/// Settings rebuilt from synced records, on top of this Mac's own.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Applied {
    pub settings: Settings,
    /// Repo variables for remotes not checked out here: `(remote, var, value)`. They apply when
    /// the repo shows up.
    pub pending: Vec<(String, String, String)>,
}

/// `local` with every synced setting taken from `entries`. `path_of` finds the checkout here
/// for a git remote; `remote_of` is the reverse, to tell which local repos are synced at all.
pub fn unflatten(local: &Settings, entries: &Entries, path_of: &dyn Fn(&str) -> Option<String>, remote_of: &dyn Fn(&str) -> Option<String>) -> Applied {
    let mut s = local.clone();
    s.routing = rebuild(entries, "routing", Routing::default());
    s.policies = rebuild(entries, "policies", Policies::default());
    s.worktrees = rebuild(entries, "worktrees", Worktrees::default());
    s.machine.shell_integration = match entries.get(&RecordId::new("terminal", "shell_integration")) {
        Some(Value::Bool(b)) => *b,
        _ => Machine::default().shell_integration,
    };
    s.terminal = rebuild(entries, "terminal", Terminal::default());

    s.agents = BTreeMap::new();
    for (id, v) in in_collection(entries, "agents") {
        let (Some((agent, field)), Value::String(v)) = (id.key.rsplit_once('.'), v) else { continue };
        let c: &mut Controls = s.agents.entry(agent.to_string()).or_default();
        match field {
            "mode" => c.mode = Some(v.clone()),
            "model" => c.model = Some(v.clone()),
            "effort" => c.effort = Some(v.clone()),
            _ => {}
        }
    }

    s.ssh = in_collection(entries, "ssh").filter_map(|(id, v)| Some((id.key.clone(), serde_json::from_value::<SshHost>(v.clone()).ok()?))).collect();

    // Repos with a remote come from the records; those without stay as this Mac has them, and so
    // do variables that never sync.
    for (path, repo) in s.repos.iter_mut() {
        if remote_of(path).is_some() {
            repo.env.retain(|var, _| !syncable_env(var));
        }
    }
    s.repos.retain(|path, repo| remote_of(path).is_none() || !repo.env.is_empty());
    let mut pending = vec![];
    for (id, v) in in_collection(entries, "repos") {
        let (Some((remote, var)), Value::String(v)) = (id.key.rsplit_once(' '), v) else { continue };
        if !syncable_env(var) {
            continue;
        }
        match path_of(remote) {
            Some(path) => {
                s.repos.entry(path).or_insert_with(Repo::default).env.insert(var.to_string(), v.clone());
            }
            None => pending.push((remote.to_string(), var.to_string(), v.clone())),
        }
    }
    Applied { settings: s, pending }
}

/// Variables that make a shell, interpreter, loader, git or an agent run code or trust something
/// of the setter's choosing, or change which programs run. They stay on the Mac that set them.
const UNSYNCED_ENV: &[&str] = &[
    // Where programs, shells and their startup files are found.
    "PATH", "HOME", "SHELL", "ENV", "BASH_ENV", "ZDOTDIR", "PROMPT_COMMAND", "PS0", "PS1", "PS2", "PS4", "IFS", "CDPATH",
    // Programs other tools run.
    "EDITOR", "VISUAL", "PAGER", "MANPAGER", "BROWSER", "LESSOPEN", "LESSCLOSE", "SSH_ASKPASS", "SUDO_ASKPASS",
    // Interpreters' startup code and module paths.
    "NODE_OPTIONS", "NODE_PATH", "PYTHONSTARTUP", "PYTHONPATH", "PYTHONHOME", "PYTHONUSERBASE", "PERL5OPT", "PERL5LIB", "PERLLIB", "PERL5DB",
    "RUBYOPT", "RUBYLIB", "GEM_HOME", "GEM_PATH", "JAVA_TOOL_OPTIONS", "_JAVA_OPTIONS", "JDK_JAVA_OPTIONS", "CLASSPATH", "LUA_INIT", "LUA_PATH",
    "LUA_CPATH", "PHPRC", "RUSTC", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "RUSTDOC", "RUSTFLAGS",
    // Who is trusted, and where traffic (and the keys in it) goes.
    "NODE_EXTRA_CA_CERTS", "SSL_CERT_FILE", "SSL_CERT_DIR", "CURL_CA_BUNDLE", "REQUESTS_CA_BUNDLE", "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY",
    "NO_PROXY", "ANTHROPIC_BASE_URL", "OPENAI_BASE_URL", "OPENAI_API_BASE",
];

/// Prefixes of whole families of such variables: the dynamic loader's, git's, exported bash
/// functions and npm's configuration.
const UNSYNCED_ENV_PREFIXES: &[&str] = &["DYLD_", "LD_", "GIT_", "BASH_FUNC_", "NPM_CONFIG_"];

/// Whether a repo variable may travel between Macs: a well-formed name (`[A-Za-z_][A-Za-z0-9_]*`)
/// that isn't one that could make a session run code (compared ignoring case, as some tools read
/// lowercase variants).
pub fn syncable_env(var: &str) -> bool {
    let mut chars = var.chars();
    let well_formed = chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    let upper = var.to_ascii_uppercase();
    well_formed && !UNSYNCED_ENV.contains(&upper.as_str()) && !UNSYNCED_ENV_PREFIXES.iter().any(|p| upper.starts_with(p))
}

fn repo_key(remote: &str, var: &str) -> String {
    format!("{remote} {var}")
}

fn in_collection<'a>(entries: &'a Entries, collection: &'a str) -> impl Iterator<Item = (&'a RecordId, &'a Value)> {
    entries.iter().filter(move |(id, _)| id.collection == collection)
}

/// A record for each field of `value` that differs from `default`.
fn fields<T: serde::Serialize>(out: &mut Entries, collection: &str, value: &T, default: &T) {
    let (Value::Object(v), Value::Object(d)) = (serde_json::to_value(value).expect("settings"), serde_json::to_value(default).expect("settings")) else { return };
    for (field, x) in v {
        if d.get(&field) != Some(&x) {
            out.insert(RecordId::new(collection, field), x);
        }
    }
}

/// `default` with each field the records set, skipping values that don't fit (written by a dino
/// that shapes them differently).
fn rebuild<T: serde::Serialize + serde::de::DeserializeOwned>(entries: &Entries, collection: &str, default: T) -> T {
    let Value::Object(mut obj) = serde_json::to_value(&default).expect("settings") else { return default };
    for (id, v) in in_collection(entries, collection) {
        let mut trial: Map<String, Value> = obj.clone();
        trial.insert(id.key.clone(), v.clone());
        if serde_json::from_value::<T>(Value::Object(trial.clone())).is_ok() {
            obj = trial;
        }
    }
    serde_json::from_value(Value::Object(obj)).unwrap_or(default)
}
