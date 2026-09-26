//! Agent catalog and discovery. Grows into the harness SDK.

use std::path::{Path, PathBuf};

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
pub fn proxy_wiring(agent_id: &str, base: &dyn Fn(&str) -> String) -> (Vec<(String, String)>, Vec<String>) {
    let user_set = |var: &str| std::env::var_os(var).is_some();
    match agent_id {
        // Also used for shells, so `claude` started inside one is metered too.
        "claude" | "shell" if !user_set("ANTHROPIC_BASE_URL") => {
            (vec![("ANTHROPIC_BASE_URL".into(), base("anthropic"))], vec![])
        }
        // Untested: needs a codex install to verify against ChatGPT-login auth.
        "codex" if !user_set("OPENAI_BASE_URL") => {
            (vec![], vec!["-c".into(), format!("openai_base_url=\"{}/v1\"", base("openai"))])
        }
        _ => (vec![], vec![]),
    }
}
