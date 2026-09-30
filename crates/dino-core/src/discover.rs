//! Machine inventory for the first-run screen: agents, their logins, API keys, local model servers.
//! Never keeps or shows a secret in full; keys are masked at read time.

use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::{AgentKind, KNOWN_AGENTS, which};

#[derive(Clone, Debug)]
pub struct AgentInfo {
    pub kind: AgentKind,
    pub path: Option<PathBuf>,
    pub version: Option<String>,
    /// e.g. "Claude Max", "ChatGPT login", "signed out".
    pub auth: Option<String>,
    /// dino can route this agent's traffic through the proxy.
    pub meterable: bool,
    pub install: &'static str,
}

#[derive(Clone, Debug)]
pub struct KeyInfo {
    pub var: &'static str,
    pub provider: &'static str,
    pub masked: String,
    /// "env", "~/.zshrc", ".env", ...
    pub source: String,
}

#[derive(Clone, Debug)]
pub struct LocalServer {
    pub name: &'static str,
    pub addr: &'static str,
    pub up: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Inventory {
    pub agents: Vec<AgentInfo>,
    pub keys: Vec<KeyInfo>,
    pub local: Vec<LocalServer>,
}

const KEYS: &[(&str, &str)] = &[
    ("ANTHROPIC_API_KEY", "Anthropic"),
    ("OPENAI_API_KEY", "OpenAI"),
    ("OPENROUTER_API_KEY", "OpenRouter"),
    ("NVIDIA_API_KEY", "NVIDIA NIM"),
    ("NGC_API_KEY", "NVIDIA NIM"),
    ("GEMINI_API_KEY", "Google Gemini"),
    ("GOOGLE_API_KEY", "Google"),
    ("GROQ_API_KEY", "Groq"),
    ("CEREBRAS_API_KEY", "Cerebras"),
    ("DEEPSEEK_API_KEY", "DeepSeek"),
    ("MOONSHOT_API_KEY", "Moonshot / Kimi"),
    ("DASHSCOPE_API_KEY", "Alibaba / Qwen"),
    ("MISTRAL_API_KEY", "Mistral"),
    ("XAI_API_KEY", "xAI"),
    ("TOGETHER_API_KEY", "Together"),
    ("FIREWORKS_API_KEY", "Fireworks"),
    ("HF_TOKEN", "Hugging Face"),
    ("TYPESAFE_API_KEY", "TypeSafe Jev"),
];

const LOCAL: &[(&str, &str)] = &[("Ollama", "127.0.0.1:11434"), ("LM Studio", "127.0.0.1:1234"), ("llama.cpp", "127.0.0.1:8080")];

/// Each agent's official install command.
pub fn install_hint(id: &str) -> &'static str {
    match id {
        "claude" => "curl -fsSL https://claude.ai/install.sh | bash",
        "codex" => "npm i -g @openai/codex",
        "gemini" => "npm i -g @google/gemini-cli",
        "qwen" => "npm i -g @qwen-code/qwen-code",
        "kimi" => "npm i -g @moonshot-ai/kimi-code",
        "pi" => "npm i -g --ignore-scripts @earendil-works/pi-coding-agent",
        "hermes" => "curl -fsSL https://hermes-agent.nousresearch.com/install.sh | bash",
        "opencode" => "curl -fsSL https://opencode.ai/install | bash",
        "crush" => "brew install charmbracelet/tap/crush",
        "aider" => "pip install aider-install && aider-install",
        "amp" => "npm i -g @sourcegraph/amp",
        "cursor" => "curl https://cursor.com/install -fsS | bash",
        _ => "",
    }
}

/// Where to read about an agent, and how it signs in: its own command, which dino runs in a shell
/// pane so the user sees it and answers its prompts. Some agents sign in from inside their own
/// terminal UI; `sign_in_hint` says what to type there.
pub struct Setup {
    pub homepage: &'static str,
    pub sign_in: Option<&'static str>,
    pub sign_in_hint: Option<&'static str>,
}

pub fn setup(id: &str) -> Setup {
    let (homepage, sign_in, sign_in_hint) = match id {
        "claude" => ("https://code.claude.com/docs", Some("claude auth login"), None),
        "codex" => ("https://developers.openai.com/codex", Some("codex login"), None),
        "kimi" => ("https://moonshotai.github.io/kimi-code/en/", Some("kimi login"), None),
        "qwen" => ("https://qwenlm.github.io/qwen-code-docs/", Some("qwen"), Some("/auth")),
        "pi" => ("https://pi.dev", Some("pi"), Some("/login")),
        "hermes" => ("https://hermes-agent.nousresearch.com/docs/", Some("hermes setup"), None),
        "gemini" => ("https://github.com/google-gemini/gemini-cli", None, None),
        "opencode" => ("https://opencode.ai", None, None),
        "crush" => ("https://github.com/charmbracelet/crush", None, None),
        "aider" => ("https://aider.chat", None, None),
        "amp" => ("https://ampcode.com", None, None),
        "cursor" => ("https://cursor.com/cli", None, None),
        _ => ("", None, None),
    };
    Setup { homepage, sign_in, sign_in_hint }
}

/// Whether agent `id` at `bin` is signed in, from its own status command, and how ("Claude Max",
/// "ChatGPT"). `None` when the agent has no quick way to ask. Only the verdict is kept: no
/// account names, emails or tokens.
pub fn sign_in_status(id: &str, bin: &Path) -> Option<(bool, Option<String>)> {
    match id {
        "claude" => {
            let v: serde_json::Value = serde_json::from_str(&run_quietly(bin, &["auth", "status", "--json"])?).ok()?;
            let signed_in = v["loggedIn"].as_bool()?;
            let how = match (v["subscriptionType"].as_str(), v["authMethod"].as_str()) {
                (Some("max"), _) => Some("Claude Max"),
                (Some("pro"), _) => Some("Claude Pro"),
                (Some(t), _) if t.contains("team") => Some("Claude Team"),
                (Some(t), _) if t.contains("enterprise") => Some("Claude Enterprise"),
                (_, Some("claude.ai")) => Some("claude.ai"),
                (_, Some(m)) if m.to_lowercase().contains("key") => Some("API key"),
                _ => None,
            };
            Some((signed_in, how.filter(|_| signed_in).map(String::from)))
        }
        "codex" => codex_status(&run_quietly(bin, &["login", "status"])?),
        _ => None,
    }
}

/// `codex login status`: "Logged in using ChatGPT", "Logged in using an API key", "Not logged in".
fn codex_status(out: &str) -> Option<(bool, Option<String>)> {
    if out.contains("Not logged in") {
        return Some((false, None));
    }
    let how = out.lines().find_map(|l| l.trim().strip_prefix("Logged in using ")).map(|m| {
        let m = m.trim();
        if m.to_lowercase().contains("api key") { "API key".to_string() } else { m.to_string() }
    });
    out.contains("Logged in").then_some((true, how))
}

/// `bin args`'s output (stdout, then stderr), or `None` if it doesn't finish within 3 s.
fn run_quietly(bin: &Path, args: &[&str]) -> Option<String> {
    let mut child = Command::new(bin).args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().ok()?;
    for _ in 0..30 {
        if child.try_wait().ok()?.is_some() {
            let out = child.wait_with_output().ok()?;
            return Some(format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    None
}

/// The login shell's `PATH` as it is now: an install may have added a folder that this process,
/// started earlier, doesn't have. `None` if the shell doesn't answer within 3 s.
pub fn login_path() -> Option<std::ffi::OsString> {
    let shell = crate::user_shell();
    let out = run_quietly(Path::new(&shell), &["-l", "-c", r#"printf '\n%s' "$PATH""#])?;
    // The last line: a chatty profile may print before it.
    let path = out.lines().last()?.trim();
    (!path.is_empty()).then(|| path.into())
}

/// Full scan. Agent version probes run in parallel; worst case is bounded by a short timeout.
pub fn scan() -> Inventory {
    let agents = std::thread::scope(|s| {
        let handles: Vec<_> = KNOWN_AGENTS.iter().map(|kind| s.spawn(move || agent_info(kind))).collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let local = LOCAL
        .iter()
        .map(|&(name, addr)| {
            let up = addr.parse::<SocketAddr>().is_ok_and(|a| TcpStream::connect_timeout(&a, Duration::from_millis(150)).is_ok());
            LocalServer { name, addr, up }
        })
        .collect();
    Inventory { agents, keys: scan_keys(), local }
}

fn agent_info(kind: &AgentKind) -> AgentInfo {
    let path = which(kind.bin);
    let version = path.as_deref().and_then(version_of);
    let auth = path.as_ref().and_then(|_| auth_of(kind.id));
    AgentInfo {
        kind: kind.clone(),
        path,
        version,
        auth,
        meterable: matches!(kind.id, "claude" | "codex"),
        install: install_hint(kind.id),
    }
}

pub fn version_of(bin: &Path) -> Option<String> {
    let mut child = Command::new(bin).arg("--version").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    // Some CLIs are slow to start; don't let one hold up the whole screen.
    for _ in 0..30 {
        if child.try_wait().ok()?.is_some() {
            let out = child.wait_with_output().ok()?;
            let text = String::from_utf8_lossy(&out.stdout);
            return text.split_whitespace().find(|w| w.trim_start_matches('v').starts_with(|c: char| c.is_ascii_digit())).map(|v| v.trim_start_matches('v').to_string());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    None
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

fn read_json(path: PathBuf) -> Option<serde_json::Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn auth_of(id: &str) -> Option<String> {
    let h = home();
    match id {
        "claude" => {
            let account = read_json(h.join(".claude.json")).map(|v| v["oauthAccount"].clone()).filter(|a| a.is_object());
            match account {
                Some(a) => Some(match a["organizationType"].as_str() {
                    Some("claude_max") => "Claude Max".into(),
                    Some("claude_pro") => "Claude Pro".into(),
                    Some(t) if t.contains("team") => "Claude Team".into(),
                    Some(t) if t.contains("enterprise") => "Claude Enterprise".into(),
                    _ => "claude.ai login".into(),
                }),
                None if std::env::var_os("ANTHROPIC_API_KEY").is_some() => Some("API key".into()),
                None => Some("signed out".into()),
            }
        }
        "codex" => Some(match read_json(h.join(".codex/auth.json")) {
            Some(v) if v["auth_mode"] == "chatgpt" && v["tokens"].is_object() => "ChatGPT login".into(),
            Some(v) if v["OPENAI_API_KEY"].is_string() => "API key".into(),
            _ => "signed out".into(),
        }),
        "gemini" => Some(if h.join(".gemini/oauth_creds.json").exists() { "Google login".into() } else { "not signed in".into() }),
        _ => None,
    }
}

fn mask(v: &str) -> String {
    let v = v.trim().trim_matches(['"', '\'']);
    let n = v.chars().count();
    if n <= 10 {
        return "•".repeat(n.min(6));
    }
    let head: String = v.chars().take(4).collect();
    let tail: String = v.chars().skip(n - 4).collect();
    format!("{head}…{tail}")
}

fn scan_keys() -> Vec<KeyInfo> {
    let h = home();
    let mut files: Vec<(String, PathBuf)> = [".zshrc", ".zprofile", ".zshenv", ".bashrc", ".bash_profile", ".profile", ".config/fish/config.fish"]
        .iter()
        .map(|f| (format!("~/{f}"), h.join(f)))
        .collect();
    if let Ok(cwd) = std::env::current_dir() {
        files.push((".env".into(), cwd.join(".env")));
    }
    files.insert(0, ("dino keys".into(), crate::keys_file()));
    let contents: Vec<(String, String)> =
        files.into_iter().filter_map(|(label, p)| std::fs::read_to_string(p).ok().map(|c| (label, c))).collect();

    let mut out = vec![];
    for &(var, provider) in KEYS {
        if let Ok(v) = std::env::var(var) {
            if !v.is_empty() {
                out.push(KeyInfo { var, provider, masked: mask(&v), source: "env".into() });
                continue;
            }
        }
        // `export VAR=value`, `VAR=value`, or fish's `set -x VAR value`.
        let found = contents.iter().find_map(|(label, text)| {
            text.lines().find_map(|line| {
                let l = line.trim();
                let l = l.strip_prefix("export ").or_else(|| l.strip_prefix("set -gx ")).or_else(|| l.strip_prefix("set -x ")).unwrap_or(l);
                let rest = l.strip_prefix(var)?;
                let value = rest.strip_prefix('=').or_else(|| rest.strip_prefix(' '))?;
                (!value.trim().is_empty()).then(|| (label.clone(), mask(value)))
            })
        });
        if let Some((source, masked)) = found {
            out.push(KeyInfo { var, provider, masked, source });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_login_status_is_read_without_the_account() {
        assert_eq!(codex_status("Logged in using ChatGPT\n"), Some((true, Some("ChatGPT".into()))));
        assert_eq!(codex_status("Logged in using an API key - sk-proj-***ABCD\n"), Some((true, Some("API key".into()))));
        assert_eq!(codex_status("Not logged in\n"), Some((false, None)));
        assert_eq!(codex_status("error: something else\n"), None);
    }

    #[test]
    fn every_agent_has_a_way_in() {
        for kind in KNOWN_AGENTS {
            assert!(!install_hint(kind.id).is_empty(), "{} has no install command", kind.id);
            assert!(setup(kind.id).homepage.starts_with("https://"), "{} has no homepage", kind.id);
        }
    }
}
