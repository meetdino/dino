//! Agent catalog and discovery. Grows into the harness SDK.

use std::path::{Path, PathBuf};

pub mod discover;

/// A coding agent (or plain program) dino knows how to launch.
#[derive(Clone, Debug)]
pub struct AgentKind {
    pub id: &'static str,
    pub name: &'static str,
    pub bin: &'static str,
}

pub const KNOWN_AGENTS: &[AgentKind] = &[
    AgentKind { id: "claude", name: "Claude Code", bin: "claude" },
    AgentKind { id: "codex", name: "Codex", bin: "codex" },
    AgentKind { id: "gemini", name: "Gemini CLI", bin: "gemini" },
    AgentKind { id: "qwen", name: "Qwen Code", bin: "qwen" },
    AgentKind { id: "kimi", name: "Kimi CLI", bin: "kimi" },
    AgentKind { id: "opencode", name: "OpenCode", bin: "opencode" },
    AgentKind { id: "crush", name: "Crush", bin: "crush" },
    AgentKind { id: "aider", name: "Aider", bin: "aider" },
    AgentKind { id: "amp", name: "Amp", bin: "amp" },
    AgentKind { id: "cursor", name: "Cursor Agent", bin: "cursor-agent" },
];

#[derive(Clone, Debug)]
pub struct Detected {
    pub kind: AgentKind,
    pub path: PathBuf,
}

/// Known agents found on `PATH`, in catalog order.
pub fn detect_agents() -> Vec<Detected> {
    KNOWN_AGENTS
        .iter()
        .filter_map(|kind| which(kind.bin).map(|path| Detected { kind: kind.clone(), path }))
        .collect()
}

/// The user's login shell, for plain terminal sessions.
pub fn user_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into())
}

pub fn which(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|dir| dir.join(bin)).find(|p| is_executable(p))
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata().map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

/// How to route an agent's API traffic through dino's proxy: extra env vars and CLI args.
/// `base(provider)` yields the proxy base URL for that provider.
/// Agents we don't know how to wire (or that the user already pointed elsewhere) run untouched.
pub fn proxy_wiring(agent_id: &str, route: bool, base: &dyn Fn(&str) -> String) -> (Vec<(String, String)>, Vec<String>) {
    // With routing off, only status hooks are wired; API traffic goes direct.
    let user_set = |var: &str| !route || std::env::var_os(var).is_some();
    match agent_id {
        // Also used for shells, so `claude` started inside one is metered too.
        "claude" => {
            let env = if user_set("ANTHROPIC_BASE_URL") { vec![] } else { vec![("ANTHROPIC_BASE_URL".into(), base("anthropic"))] };
            (env, vec!["--settings".into(), claude_hook_settings(&base("hook"))])
        }
        // Claude Code on the free pool: dino answers as the Anthropic API and routes each request.
        // The token is a placeholder so Claude Code skips its own login; the proxy holds the real keys.
        "claude-free" => {
            let env = [
                ("ANTHROPIC_BASE_URL", base("free")),
                ("ANTHROPIC_AUTH_TOKEN", "dino-free".into()),
                ("ANTHROPIC_MODEL", "auto".into()),
                ("ANTHROPIC_DEFAULT_OPUS_MODEL", "auto".into()),
                ("ANTHROPIC_DEFAULT_SONNET_MODEL", "auto".into()),
                ("ANTHROPIC_DEFAULT_HAIKU_MODEL", "auto-fast".into()),
                ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1".into()),
            ];
            (env.into_iter().map(|(k, v)| (k.to_string(), v)).collect(), vec!["--settings".into(), claude_hook_settings(&base("hook"))])
        }
        "shell" if !user_set("ANTHROPIC_BASE_URL") => (vec![("ANTHROPIC_BASE_URL".into(), base("anthropic"))], vec![]),
        // A custom provider rather than `openai_base_url`: Codex otherwise tries WebSockets first,
        // which the proxy doesn't carry. `requires_openai_auth` keeps the user's own login.
        "codex" if !user_set("OPENAI_BASE_URL") => {
            let base_url = match codex_auth_mode().as_deref() {
                Some("chatgpt") => format!("{}/codex", base("chatgpt")),
                _ => format!("{}/v1", base("openai")),
            };
            let args = [
                "model_provider=\"dino\"".to_string(),
                "model_providers.dino.name=\"dino\"".into(),
                format!("model_providers.dino.base_url=\"{base_url}\""),
                "model_providers.dino.wire_api=\"responses\"".into(),
                "model_providers.dino.requires_openai_auth=true".into(),
                "model_providers.dino.supports_websockets=false".into(),
            ];
            (vec![], args.into_iter().flat_map(|a| ["-c".to_string(), a]).collect())
        }
        _ => (vec![], vec![]),
    }
}

/// `"chatgpt"` or `"apikey"`, from `~/.codex/auth.json`.
fn codex_auth_mode() -> Option<String> {
    let home = std::env::var_os("HOME")?;
    let auth = std::fs::read_to_string(Path::new(&home).join(".codex/auth.json")).ok()?;
    // Avoid a JSON dependency for one field: find `"auth_mode": "<value>"`.
    let rest = &auth[auth.find("\"auth_mode\"")? + 11..];
    let start = rest.find('"')? + 1;
    let len = rest[start..].find('"')?;
    Some(rest[start..start + len].to_string())
}

/// Per-session settings layered on top of the user's own: HTTP hooks that report lifecycle events
/// to dino. Hook entries merge with existing ones, and an unreachable URL never blocks Claude.
fn claude_hook_settings(url: &str) -> String {
    const EVENTS: &[&str] = &[
        "SessionStart", "UserPromptSubmit", "PreToolUse", "PostToolUse", "PostToolUseFailure",
        "PermissionRequest", "Notification", "Stop", "StopFailure",
    ];
    let entry = format!(r#"[{{"hooks":[{{"type":"http","url":"{url}","timeout":5}}]}}]"#);
    let hooks: Vec<String> = EVENTS.iter().map(|e| format!(r#""{e}":{entry}"#)).collect();
    format!(r#"{{"hooks":{{{}}}}}"#, hooks.join(","))
}

fn config_dir() -> PathBuf {
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

/// Persistent user choices, in `~/.config/dino/config` as `key=value` lines.
#[derive(Clone, Debug)]
pub struct Config {
    /// Onboarding finished; skip the welcome scan.
    pub onboarded: bool,
    /// Route agent API traffic through the local proxy.
    pub route: bool,
}

impl Config {
    fn path() -> PathBuf {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        home.join(".config/dino/config")
    }

    pub fn load() -> Self {
        let text = std::fs::read_to_string(Self::path()).unwrap_or_default();
        let get = |k: &str| text.lines().find_map(|l| l.strip_prefix(k)?.strip_prefix('=').map(str::trim).map(String::from));
        Self { onboarded: get("onboarded").as_deref() == Some("true"), route: get("route").as_deref() != Some("false") }
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, format!("onboarded={}\nroute={}\n", self.onboarded, self.route))
    }
}
