//! dino's settings through sync records and back, as two Macs with different folders see them.

use dino_core::controls::Controls;
use dino_core::settings::{Repo, Settings, SshHost};
use dino_sync::record::RecordId;
use dino_sync::settings::{Entries, diff, flatten, syncable_env, unflatten};
use serde_json::json;
use std::collections::BTreeMap;

/// A Mac's repos: its checkout paths and their remotes.
fn mac(repos: &[(&str, &str)]) -> (impl Fn(&str) -> Option<String> + use<>, impl Fn(&str) -> Option<String> + use<>) {
    let by_path: BTreeMap<String, String> = repos.iter().map(|(p, r)| (p.to_string(), r.to_string())).collect();
    let by_remote: BTreeMap<String, String> = repos.iter().map(|(p, r)| (r.to_string(), p.to_string())).collect();
    (move |p: &str| by_path.get(p).cloned(), move |r: &str| by_remote.get(r).cloned())
}

fn realistic() -> Settings {
    let mut s = Settings::default();
    s.policies.allowed_agents = vec!["claude".into(), "codex".into()];
    s.policies.default_agent = Some("claude".into());
    s.policies.allow_bypass = false;
    s.policies.session_token_budget = 2_000_000;
    s.policies.close_merged = true;
    s.routing.proxy = false;
    s.worktrees.branch_prefix = "ben/".into();
    s.agents.insert("claude".into(), Controls { mode: Some("plan".into()), model: Some("opus".into()), effort: Some("high".into()) });
    s.agents.insert("codex".into(), Controls { mode: None, model: Some("gpt-5.5".into()), effort: None });
    s.ssh.insert("devbox".into(), SshHost { folder: "~/src".into() });
    s.repos.insert("/Users/a/code/api".into(), Repo { env: [("AWS_PROFILE".to_string(), "client-x".to_string())].into() });
    s.repos.insert("/Users/a/scratch".into(), Repo { env: [("LOCAL_ONLY".to_string(), "1".to_string())].into() });
    s.machine.onboarded = true;
    s.machine.keep_awake = true;
    s.machine.shell_integration = false;
    s
}

#[test]
fn settings_cross_to_another_mac() {
    let a = realistic();
    let (a_remote, _) = mac(&[("/Users/a/code/api", "git@github.com:acme/api.git")]);
    let keys: BTreeMap<String, String> = [("OPENROUTER_API_KEY".to_string(), "sk-or-x".to_string())].into();
    let entries = flatten(&a, &a_remote, Some(&keys));

    // Defaults aren't records; per-Mac and remote-less things never leave.
    assert!(!entries.contains_key(&RecordId::new("policies", "worktree_trust")));
    assert!(entries.keys().all(|id| id.collection != "machine"));
    assert!(!entries.keys().any(|id| id.key.contains("LOCAL_ONLY")));
    assert_eq!(entries[&RecordId::new("repos", "git@github.com:acme/api.git AWS_PROFILE")], json!("client-x"));
    assert_eq!(entries[&RecordId::new("agents", "claude.effort")], json!("high"));

    // Mac B: another folder for the same repo, its own machine settings, one repo not cloned.
    let mut b_local = Settings::default();
    b_local.machine.onboarded = false;
    b_local.repos.insert("/Users/b/w/api".into(), Repo { env: [("STALE".to_string(), "1".to_string())].into() });
    b_local.repos.insert("/Users/b/notes".into(), Repo { env: [("MINE".to_string(), "1".to_string())].into() });
    let (b_remote, b_path) = mac(&[("/Users/b/w/api", "git@github.com:acme/api.git")]);
    let got = unflatten(&b_local, &entries, &b_path, &b_remote);

    let s = &got.settings;
    assert_eq!(s.policies, a.policies);
    assert_eq!(s.routing, a.routing);
    assert_eq!(s.worktrees, a.worktrees);
    assert_eq!(s.agents, a.agents);
    assert_eq!(s.ssh, a.ssh);
    assert!(!s.machine.shell_integration, "shell integration travels");
    assert!(!s.machine.onboarded && !s.machine.keep_awake, "the rest of machine stays per Mac");
    assert_eq!(s.repos["/Users/b/w/api"].env, [("AWS_PROFILE".to_string(), "client-x".to_string())].into(), "synced repos come from the records");
    assert_eq!(s.repos["/Users/b/notes"].env["MINE"], "1", "repos without a remote stay as they are");
    assert_eq!(got.keys, keys);
    assert!(got.pending.is_empty());

    // Round trip: B's settings flatten to the same records.
    assert_eq!(flatten(s, &b_remote, Some(&got.keys)), entries);

    // A Mac without the repo holds its variables until it's cloned.
    let (c_remote, c_path) = mac(&[]);
    let c = unflatten(&Settings::default(), &entries, &c_path, &c_remote);
    assert_eq!(c.pending, vec![("git@github.com:acme/api.git".to_string(), "AWS_PROFILE".to_string(), "client-x".to_string())]);
}

#[test]
fn keys_only_when_turned_on() {
    let (r, _) = mac(&[]);
    assert!(flatten(&Settings::default(), &r, None).is_empty(), "defaults and no keys: nothing to sync");
}

#[test]
fn diff_writes_and_deletes() {
    let (r, _) = mac(&[]);
    let before = flatten(&realistic(), &r, None);
    let mut s = realistic();
    s.agents.get_mut("claude").unwrap().effort = None;
    s.policies.allow_bypass = true; // back to the default
    s.ssh.insert("gpu".into(), SshHost { folder: "~".into() });
    let changes = diff(&before, &flatten(&s, &r, None));
    let find = |c: &str, k: &str| changes.iter().find(|(id, _)| id == &RecordId::new(c, k)).map(|(_, v)| v.clone());
    assert_eq!(find("agents", "claude.effort"), Some(None));
    assert_eq!(find("policies", "allow_bypass"), Some(None));
    assert_eq!(find("ssh", "gpu"), Some(Some(json!({"folder": "~"}))));
    assert_eq!(changes.len(), 3);
}

#[test]
fn values_a_newer_dino_shapes_differently_are_skipped() {
    let (r, p) = mac(&[]);
    let mut entries = flatten(&realistic(), &r, None);
    entries.insert(RecordId::new("policies", "session_token_budget"), json!({"per_day": 5}));
    entries.insert(RecordId::new("policies", "brand_new_policy"), json!(true));
    let got = unflatten(&Settings::default(), &entries, &p, &r);
    assert_eq!(got.settings.policies.session_token_budget, 0, "a value that doesn't fit leaves the default");
    assert!(!got.settings.policies.allow_bypass, "the rest still apply");
}

#[test]
fn repo_variables_that_run_code_never_sync() {
    let remote = "git@github.com:acme/api.git";
    let hostile = [
        "NODE_OPTIONS", "BASH_ENV", "GIT_SSH_COMMAND", "GIT_CONFIG_COUNT", "DYLD_INSERT_LIBRARIES", "LD_PRELOAD", "PATH", "PYTHONSTARTUP", "PERL5OPT", "RUBYOPT",
        "ZDOTDIR", "ENV", "PROMPT_COMMAND", "node_options", "Path", "ANTHROPIC_BASE_URL", "HTTPS_PROXY", "BASH_FUNC_ls%%", "A=B", "1X", "",
    ];
    for var in hostile {
        assert!(!syncable_env(var), "{var} must not sync");
    }
    for var in ["AWS_PROFILE", "RUST_LOG", "_PRIVATE", "DATABASE_URL"] {
        assert!(syncable_env(var), "{var} syncs");
    }

    // A compromised Mac writes them anyway: the others ignore them, cloned there or not.
    let mut entries: Entries = hostile.iter().map(|var| (RecordId::new("repos", format!("{remote} {var}")), json!("/tmp/evil"))).collect();
    entries.insert(RecordId::new("repos", format!("{remote} AWS_PROFILE")), json!("client-x"));
    let (b_remote, b_path) = mac(&[("/Users/b/w/api", remote)]);
    let got = unflatten(&Settings::default(), &entries, &b_path, &b_remote);
    assert_eq!(got.settings.repos["/Users/b/w/api"].env, [("AWS_PROFILE".to_string(), "client-x".to_string())].into());
    let (c_remote, c_path) = mac(&[]);
    let c = unflatten(&Settings::default(), &entries, &c_path, &c_remote);
    assert_eq!(c.pending, vec![(remote.to_string(), "AWS_PROFILE".to_string(), "client-x".to_string())]);

    // One set on this Mac stays on this Mac: never a record, and kept when records apply.
    let mut local = Settings::default();
    local.repos.insert("/Users/b/w/api".into(), Repo { env: [("NODE_OPTIONS".to_string(), "--inspect".to_string()), ("STALE".to_string(), "1".to_string())].into() });
    let mine = flatten(&local, &b_remote, None);
    assert_eq!(mine.keys().collect::<Vec<_>>(), vec![&RecordId::new("repos", format!("{remote} STALE"))]);
    let got = unflatten(&local, &entries, &b_path, &b_remote);
    let env = &got.settings.repos["/Users/b/w/api"].env;
    assert_eq!(env.get("NODE_OPTIONS").map(String::as_str), Some("--inspect"));
    assert_eq!(env.get("AWS_PROFILE").map(String::as_str), Some("client-x"));
    assert!(!env.contains_key("STALE"), "synced variables come from the records");
}
