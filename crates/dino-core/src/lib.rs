//! Agent catalog and discovery. Grows into the harness SDK.

use std::path::{Path, PathBuf};

pub mod agent;
pub mod ask;
pub mod claude_token;
pub mod compat;
pub mod controls;
pub mod discover;
pub mod found;
pub mod history;
pub mod ipc;
pub mod mcp;
pub mod models;
pub mod plans;
pub mod power;
pub mod pr;
pub mod preview;
pub mod procinfo;
pub mod providers;
pub mod review;
pub mod schedule;
pub mod settings;
pub mod ssh;
pub mod status;
pub mod statusline;
pub mod transcript;
pub mod trust;
pub mod worktree;

/// A coding agent (or plain program) dino knows how to launch.
#[derive(Clone, Debug)]
pub struct AgentKind {
    pub id: &'static str,
    pub name: &'static str,
    pub bin: &'static str,
    /// Its command's older names, looked for when `bin` isn't found: an install from before a rename.
    pub was: &'static [&'static str],
}

pub const KNOWN_AGENTS: &[AgentKind] = &[
    AgentKind { id: "claude", name: "Claude Code", bin: "claude", was: &[] },
    AgentKind { id: "codex", name: "Codex", bin: "codex", was: &[] },
    AgentKind { id: "qwen", name: "Qwen Code", bin: "qwen", was: &[] },
    AgentKind { id: "kimi", name: "Kimi Code", bin: "kimi", was: &[] },
    AgentKind { id: "pi", name: "Pi", bin: "pi", was: &[] },
    AgentKind { id: "hermes", name: "Hermes Agent", bin: "hermes", was: &[] },
    AgentKind { id: "codewhale", name: "CodeWhale", bin: "codewhale", was: &["deepseek-tui"] },
    AgentKind { id: "copilot", name: "GitHub Copilot CLI", bin: "copilot", was: &[] },
    AgentKind { id: "opencode", name: "OpenCode", bin: "opencode", was: &[] },
    AgentKind { id: "crush", name: "Crush", bin: "crush", was: &[] },
    AgentKind { id: "aider", name: "Aider", bin: "aider", was: &[] },
    AgentKind { id: "amp", name: "Amp", bin: "amp", was: &[] },
    AgentKind { id: "cursor", name: "Cursor Agent", bin: "cursor-agent", was: &[] },
];

#[derive(Clone, Debug)]
pub struct Detected {
    pub kind: AgentKind,
    pub path: PathBuf,
}

/// Known agents found on `PATH`, in catalog order.
pub fn detect_agents() -> Vec<Detected> {
    detect_agents_in(&std::env::var_os("PATH").unwrap_or_default())
}

/// Known agents found on `path` (a `PATH`-style list), else where their installers put them (see
/// `which`), in catalog order.
pub fn detect_agents_in(path: &std::ffi::OsStr) -> Vec<Detected> {
    KNOWN_AGENTS
        .iter()
        .filter_map(|kind| find_in(kind, path).map(|path| Detected { kind: kind.clone(), path }))
        .collect()
}

/// `kind`'s command on `path`, else where installers put it: by its name now, then its older ones.
fn find_in(kind: &AgentKind, path: &std::ffi::OsStr) -> Option<PathBuf> {
    let names = || std::iter::once(kind.bin).chain(kind.was.iter().copied());
    names().find_map(|bin| which_in(bin, path)).or_else(|| {
        let dirs = install_dirs();
        names().find_map(|bin| which_in(bin, &dirs))
    })
}

/// `kind`'s command on the `PATH` (see `which`), by any of its names.
pub fn which_agent(kind: &AgentKind) -> Option<PathBuf> {
    find_in(kind, &std::env::var_os("PATH").unwrap_or_default())
}

/// The user's login shell, for plain terminal sessions.
pub fn user_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into())
}

/// `bin` on the `PATH`, else in the folders agents' own installers put themselves: a fresh Mac's
/// `PATH` often lacks them (Claude Code's installer says so and leaves it to you), and an agent
/// installed from dino's Welcome must still be found.
pub fn which(bin: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|p| which_in(bin, &p)).or_else(|| which_in(bin, &install_dirs()))
}

/// Where agent installers put their programs when it isn't on the `PATH` yet.
fn install_dirs() -> std::ffi::OsString {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let dirs = [
        home.join(".local/bin"),
        home.join(".claude/local"),
        home.join(".npm-global/bin"),
        home.join(".bun/bin"),
        home.join(".amp/bin"),
        home.join(".cargo/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ];
    std::env::join_paths(dirs).unwrap_or_default()
}

pub fn which_in(bin: &str, path: &std::ffi::OsStr) -> Option<PathBuf> {
    std::env::split_paths(path).map(|dir| dir.join(bin)).find(|p| is_executable(p))
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata().map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

/// How to route an agent's API traffic through dino's proxy: extra env vars and CLI args.
/// `base(provider)` yields the proxy base URL for that provider; `status_line` is a Claude
/// `statusLine` setting to add (see `statusline::wrapper`).
/// Agents we don't know how to wire (or that the user already pointed elsewhere) run untouched.
pub fn proxy_wiring(agent_id: &str, route: bool, base: &dyn Fn(&str) -> String, status_line: Option<String>) -> agent::Wiring {
    match agent::agent(agent_id) {
        Some(a) => a.wiring(route, base, status_line),
        // So `claude` started inside a shell is metered too: for that agent alone, never the
        // shell, where every program using the Anthropic SDK would come through dino.
        None if agent_id == "shell" && !user_set(route, "ANTHROPIC_BASE_URL") => (vec![(SHELL_CLAUDE_BASE_URL.into(), base("anthropic"))], vec![]),
        None => (vec![], vec![]),
    }
}

/// The variable a dino shell's integration hands an agent typed there as its `ANTHROPIC_BASE_URL`
/// (`dino-agents.*`), unless the user set one of their own.
pub const SHELL_CLAUDE_BASE_URL: &str = "DINO_CLAUDE_BASE_URL";

/// With routing off, only status hooks are wired; API traffic goes direct. Otherwise, whether the
/// user pointed `var` elsewhere themselves. A dino proxy URL in our own environment was inherited
/// from a dino pane (dinod started from one), not set by the user: it points at another session,
/// or another dinod.
pub(crate) fn user_set(route: bool, var: &str) -> bool {
    !route || std::env::var(var).is_ok_and(|v| !is_proxy_url(&v))
}

/// A base URL of a dino proxy's (`http://127.0.0.1:<port>/k/<secret>/s/<session>/<provider>`).
pub fn is_proxy_url(v: &str) -> bool {
    v.starts_with("http://127.0.0.1:") && v.contains("/s/")
}

/// Random v4 UUID, for an agent's conversation id picked up front.
pub fn new_uuid() -> String {
    let mut b = [0u8; 16];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = std::io::Read::read_exact(&mut f, &mut b);
    }
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

/// Per-session settings layered on top of the user's own: HTTP hooks that report lifecycle events
/// to dino, and optionally a `statusLine` (JSON). Hook entries merge with existing ones, and an
/// unreachable URL never blocks Claude.
pub fn claude_hook_settings(url: &str, status_line: Option<String>) -> String {
    const EVENTS: &[&str] = &[
        "SessionStart", "UserPromptSubmit", "PreToolUse", "PostToolUse", "PostToolUseFailure",
        "PermissionRequest", "Notification", "Stop", "StopFailure", "SubagentStart", "SubagentStop",
    ];
    let entry = format!(r#"[{{"hooks":[{{"type":"http","url":"{url}","timeout":5}}]}}]"#);
    let hooks: Vec<String> = EVENTS.iter().map(|e| format!(r#""{e}":{entry}"#)).collect();
    let status_line = status_line.map(|s| format!(r#","statusLine":{s}"#)).unwrap_or_default();
    format!(r#"{{"hooks":{{{}}}{status_line}}}"#, hooks.join(","))
}

/// What a Claude Code session sets for the programs it runs. dinod started from one (a Claude's
/// Bash tool, a terminal it opened) would hand them to every agent it starts, and a Claude under
/// them takes itself for that session's child: among other things, it saves no transcript. The
/// user's own settings (`CLAUDE_CONFIG_DIR`, `CLAUDE_CODE_EFFORT_LEVEL`, …) aren't among them.
pub const PARENT_AGENT_ENV: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_SSE_PORT",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_CODE_VERSION",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_PID",
    "CLAUDE_EFFORT",
    "AI_AGENT",
];

/// `~/.config/dino`, or `$DINO_HOME` (a second, isolated dino: tests, development).
pub fn config_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("DINO_HOME") {
        return PathBuf::from(dir);
    }
    // Tests never write the user's own dino: a session's files there belong to a running dinod.
    #[cfg(test)]
    return std::env::temp_dir().join(format!("dino-core-test-{}", std::process::id()));
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    home.join(".config/dino")
}

/// dino's own key store, `~/.config/dino/keys` (`VAR=value` lines, mode 600).
pub fn keys_file() -> PathBuf {
    config_dir().join("keys")
}

/// Provider keys dino can use itself: its key store, overridden by the environment.
pub fn load_keys() -> std::collections::HashMap<String, String> {
    let mut keys: std::collections::HashMap<String, String> = std::fs::read_to_string(keys_file())
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect();
    for (k, v) in std::env::vars() {
        if k.ends_with("_API_KEY") && !v.is_empty() {
            keys.insert(k, v);
        }
    }
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_agent_is_found_by_its_older_name_too() {
        let dir = std::env::temp_dir().join(format!("dino-which-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let make = |name: &str| {
            let p = dir.join(name);
            std::fs::write(&p, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
            p
        };
        let codewhale = KNOWN_AGENTS.iter().find(|k| k.id == "codewhale").unwrap();
        let old = make("deepseek-tui");
        assert_eq!(find_in(codewhale, dir.as_os_str()), Some(old), "an install from before its rename");
        let new = make("codewhale");
        assert_eq!(find_in(codewhale, dir.as_os_str()), Some(new), "its name now first");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
