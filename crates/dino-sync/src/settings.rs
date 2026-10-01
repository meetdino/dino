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
//! | `keys`     | the key's name         | the key store, only when the person turned key sync on |
//!
//! Only values that differ from the defaults are records, so a missing record means "default".
//! The rest of `machine` stays on each Mac, and a repo without a remote isn't synced.

use serde_json::{Map, Value};
use std::collections::BTreeMap;

use dino_core::controls::Controls;
use dino_core::settings::{Machine, Policies, Repo, Routing, Settings, SshHost, Terminal, Worktrees};

use crate::record::RecordId;

/// The shape of setting values this dino writes and reads.
pub const SCHEMA: u32 = 1;

/// A setting's value by where it lives.
pub type Entries = BTreeMap<RecordId, Value>;

/// Records for `settings`: `remote_of` gives a repo path's git remote (None: not synced), and
/// `keys` the key store when key sync is on.
pub fn flatten(settings: &Settings, remote_of: &dyn Fn(&str) -> Option<String>, keys: Option<&BTreeMap<String, String>>) -> Entries {
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
        for (var, v) in &repo.env {
            out.insert(RecordId::new("repos", repo_key(&remote, var)), Value::String(v.clone()));
        }
    }
    if settings.machine.shell_integration != Machine::default().shell_integration {
        out.insert(RecordId::new("terminal", "shell_integration"), Value::Bool(settings.machine.shell_integration));
    }
    fields(&mut out, "terminal", &settings.terminal, &Terminal::default());
    for (name, v) in keys.into_iter().flatten() {
        out.insert(RecordId::new("keys", name.clone()), Value::String(v.clone()));
    }
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
    /// The key store's synced entries (only when key sync is on).
    pub keys: BTreeMap<String, String>,
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

    // Repos with a remote come from the records; those without stay as this Mac has them.
    s.repos.retain(|path, _| remote_of(path).is_none());
    let mut pending = vec![];
    for (id, v) in in_collection(entries, "repos") {
        let (Some((remote, var)), Value::String(v)) = (id.key.rsplit_once(' '), v) else { continue };
        match path_of(remote) {
            Some(path) => {
                s.repos.entry(path).or_insert_with(Repo::default).env.insert(var.to_string(), v.clone());
            }
            None => pending.push((remote.to_string(), var.to_string(), v.clone())),
        }
    }

    let keys = in_collection(entries, "keys").filter_map(|(id, v)| Some((id.key.clone(), v.as_str()?.to_string()))).collect();
    Applied { settings: s, keys, pending }
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
