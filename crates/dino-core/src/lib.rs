//! Agent catalog and discovery. Grows into the harness SDK.

use std::path::{Path, PathBuf};

pub mod agent;
pub mod agent_mcp;
pub mod ask;
pub mod build_cache;
pub mod claude_config;
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
pub mod triggers;
pub mod trust;
pub mod usage;
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

/// Where a running agent's program is, as its process says, for one dino didn't find on the
/// `PATH` (installed by a version manager only an interactive shell sets up, in a folder of its
/// own): a native program is its own path; a Node or Bun CLI is run as `node <script>`, so it's
/// the command it was started as (still in its arguments, unless it renamed itself; else as its
/// shell noted it, `$_`), npm's link to its package's script, or the one next to the `node` that
/// runs it (nvm, fnm, Volta).
pub fn program_of(kind: &AgentKind, pid: u32) -> Option<PathBuf> {
    let names: Vec<&str> = std::iter::once(kind.bin).chain(kind.was.iter().copied()).collect();
    let named = |p: &Path| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| names.contains(&n)) && is_executable(p);
    let exe = procinfo::exe_of(pid).map(PathBuf::from);
    if let Some(e) = exe.as_ref().filter(|e| named(e)) {
        return Some(e.clone());
    }
    let (args, env) = procinfo::args_and_env(pid).unwrap_or_default();
    let started_as = args.iter().skip(1).take(2).map(String::as_str).filter(|a| a.starts_with('/')).chain(env.iter().filter_map(|e| e.strip_prefix("_="))).map(PathBuf::from);
    for c in started_as {
        if named(&c) {
            return Some(c);
        }
        // `<prefix>/lib/node_modules/<package>/…`: npm links its command at `<prefix>/bin`.
        let prefix = c.ancestors().find(|a| a.ends_with("lib/node_modules")).and_then(|m| m.parent()?.parent());
        if let Some(found) = prefix.and_then(|p| names.iter().map(|n| p.join("bin").join(n)).find(|b| is_executable(b))) {
            return Some(found);
        }
    }
    let dir = exe?.parent()?.to_path_buf();
    names.iter().map(|n| dir.join(n)).find(|p| is_executable(p))
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

/// Fills `buf` from the OS's random source, getentropy(2): the kernel's generator, with no file to
/// open (reading /dev/urandom fails once dinod has no file descriptor left).
pub fn random_bytes(buf: &mut [u8]) -> std::io::Result<()> {
    // At most 256 bytes a call.
    for chunk in buf.chunks_mut(256) {
        // SAFETY: `chunk` is valid for writes of its length, which is at most 256.
        if unsafe { libc::getentropy(chunk.as_mut_ptr().cast(), chunk.len()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Random v4 UUID, for an agent's conversation id picked up front. Panics when the OS gives no
/// random bytes (getentropy fails only for a bad buffer, too many bytes or a fatal error): the same
/// id each time would mix up conversations. A secret takes `try_new_uuid`.
pub fn new_uuid() -> String {
    try_new_uuid().expect("getentropy")
}

/// Random v4 UUID, or why the OS gave no random bytes: for a secret, which fails rather than be
/// guessable.
pub fn try_new_uuid() -> std::io::Result<String> {
    let mut b = [0u8; 16];
    random_bytes(&mut b)?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    Ok(format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32]))
}

/// The hook events dino follows, in Claude Code and in agents that take Claude's hooks (Qwen Code).
pub const HOOK_EVENTS: &[&str] = &[
    "SessionStart", "UserPromptSubmit", "PreToolUse", "PostToolUse", "PostToolUseFailure",
    "PermissionRequest", "Notification", "Stop", "StopFailure", "SubagentStart", "SubagentStop",
];

/// Claude Code's own on top of those: the model its session switches to, by `/model` or by itself
/// (2.1.251; an older Claude ignores an event it doesn't know, with a warning).
const CLAUDE_HOOK_EVENTS: &[&str] = &["PostModelSwitch"];

/// Per-session settings layered on top of Claude Code's own: HTTP hooks that report lifecycle
/// events to dino, and optionally a `statusLine` (JSON). Hook entries merge with existing ones,
/// and an unreachable URL never blocks Claude.
pub fn claude_hook_settings(url: &str, status_line: Option<String>) -> String {
    let events: Vec<&str> = HOOK_EVENTS.iter().chain(CLAUDE_HOOK_EVENTS).copied().collect();
    hook_settings(url, &events, status_line)
}

/// Settings with HTTP hooks to `url` for `events`, and optionally a `statusLine` (JSON).
pub fn hook_settings(url: &str, events: &[&str], status_line: Option<String>) -> String {
    let entry = format!(r#"[{{"hooks":[{{"type":"http","url":"{url}","timeout":5}}]}}]"#);
    let hooks: Vec<String> = events.iter().map(|e| format!(r#""{e}":{entry}"#)).collect();
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

/// Set by launchd for a dinod it runs (the job's label), in the launch agent property lists the app
/// and the CLI install: see crates/dino/src/launchd.rs.
pub const LAUNCHD_ENV: &str = "DINO_LAUNCHD";

/// Where the app says which launch agent runs this dino's dinod, and whether macOS lets it
/// (`label=`, `state=`, `app=`, `bundle=` lines): one per `$DINO_HOME`, so an isolated dino's app
/// and CLI find its own agent and never the real one.
pub fn launchd_record() -> PathBuf {
    config_dir().join("dinod.launchd")
}

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

    /// With no file descriptor left (dinod at its limit, say), ids still come from the OS's random
    /// source: never the same zeros each time, which would make the secrets they lock guessable.
    #[test]
    fn ids_are_random_without_a_free_file_descriptor() {
        const CHILD: &str = "DINO_TEST_NO_FILE_DESCRIPTORS";
        if std::env::var_os(CHILD).is_none() {
            // In a process of its own: the limit would starve the tests running beside it.
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "tests::ids_are_random_without_a_free_file_descriptor", "--test-threads=1", "--nocapture"])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(out.status.success(), "{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
            return;
        }
        // Every descriptor from the lowest free one up out of reach.
        let lowest = std::os::fd::AsRawFd::as_raw_fd(&std::fs::File::open("/dev/null").unwrap());
        let mut limit = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) }, 0);
        limit.rlim_cur = lowest as libc::rlim_t;
        assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) }, 0);
        assert!(std::fs::File::open("/dev/urandom").is_err(), "no descriptor left to open it with");
        let (a, b) = (new_uuid(), try_new_uuid().unwrap());
        assert_ne!(a, b);
        assert_ne!(a, "00000000-0000-4000-8000-000000000000");
        // More than getentropy gives at once.
        let mut many = [0u8; 600];
        random_bytes(&mut many).unwrap();
        assert!(many[512..].iter().any(|&x| x != 0));
    }

    #[test]
    fn a_running_script_says_where_its_command_is() {
        // npm's layout, run as `<interpreter> <prefix>/lib/node_modules/<package>/cli` (sh standing in for node).
        let prefix = std::env::temp_dir().join(format!("dino-program-{}", std::process::id()));
        let script = prefix.join("lib/node_modules/@earendil-works/pi-coding-agent/dist/cli.sh");
        std::fs::create_dir_all(script.parent().unwrap()).unwrap();
        std::fs::create_dir_all(prefix.join("bin")).unwrap();
        std::fs::write(&script, "sleep 5\n").unwrap();
        let link = prefix.join("bin/pi");
        std::os::unix::fs::symlink(&script, &link).unwrap();
        std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let mut child = std::process::Command::new("/bin/sh").arg(&script).spawn().unwrap();
        let pi = KNOWN_AGENTS.iter().find(|k| k.id == "pi").unwrap();
        let found = (0..50).find_map(|_| {
            std::thread::sleep(std::time::Duration::from_millis(20));
            program_of(pi, child.id())
        });
        let codex = KNOWN_AGENTS.iter().find(|k| k.id == "codex").unwrap();
        let other = program_of(codex, child.id());
        let _ = child.kill();
        let _ = child.wait();
        std::fs::remove_dir_all(&prefix).unwrap();
        assert_eq!(found, Some(link));
        assert_eq!(other, None, "only its own command");
    }

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
