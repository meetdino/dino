//! Machine inventory for the first-run screen: agents, their logins, API keys, local model servers.
//! Never keeps or shows a secret in full; keys are masked at read time.

use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::{AgentKind, KNOWN_AGENTS, which_agent};

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
        "qwen" => "npm i -g @qwen-code/qwen-code",
        "kimi" => "npm i -g @moonshot-ai/kimi-code",
        "pi" => "npm i -g --ignore-scripts @earendil-works/pi-coding-agent",
        "hermes" => "curl -fsSL https://hermes-agent.nousresearch.com/install.sh | bash",
        "codewhale" => "npm i -g codewhale",
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
    /// What signing in means for it, when that isn't obvious: Pi has no models of its own.
    pub sign_in_note: Option<&'static str>,
}

pub fn setup(id: &str) -> Setup {
    let (homepage, sign_in, sign_in_hint) = match id {
        "claude" => ("https://code.claude.com/docs", Some("claude auth login"), None),
        "codex" => ("https://developers.openai.com/codex", Some("codex login"), None),
        "kimi" => ("https://moonshotai.github.io/kimi-code/en/", Some("kimi login"), None),
        "qwen" => ("https://qwenlm.github.io/qwen-code-docs/", Some("qwen"), Some("/auth")),
        "pi" => ("https://pi.dev", Some("pi"), Some("/login")),
        "hermes" => ("https://hermes-agent.nousresearch.com/docs/", Some("hermes setup"), None),
        "codewhale" => ("https://github.com/Hmbown/CodeWhale", Some("codewhale"), Some("/provider")),
        "opencode" => ("https://opencode.ai/docs", Some("opencode auth login"), None),
        "crush" => ("https://github.com/charmbracelet/crush", None, None),
        "aider" => ("https://aider.chat", None, None),
        "amp" => ("https://ampcode.com", None, None),
        "cursor" => ("https://cursor.com/cli", None, None),
        _ => ("", None, None),
    };
    let sign_in_note = match id {
        "pi" => Some("Pi has no models of its own. Sign in with an account you already have with an AI provider, or an API key; dino opens Pi, then type /login. Or run it on a provider from Settings → Models & Providers."),
        _ => None,
    };
    Setup { homepage, sign_in, sign_in_hint, sign_in_note }
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
        "pi" => pi_status(bin),
        "opencode" => opencode_status(),
        _ => None,
    }
}

/// OpenCode runs without signing in (its own free models), so only the providers it signed in to
/// (`opencode auth login`) say anything: their names, from its `auth.json`, never what they hold.
/// None: no verdict, since it may use keys from the environment or its free models.
fn opencode_status() -> Option<(bool, Option<String>)> {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let data = std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| home.join(".local/share"));
    let auth: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(data.join("opencode/auth.json")).ok()?).ok()?;
    opencode_providers(&auth)
}

fn opencode_providers(auth: &serde_json::Value) -> Option<(bool, Option<String>)> {
    let providers: Vec<&str> = auth.as_object()?.keys().map(String::as_str).collect();
    let how = match providers.len() {
        0 => return None,
        1..=2 => providers.join(", "),
        n => format!("{n} providers"),
    };
    Some((true, Some(how)))
}

/// Pi has no models of its own: it's signed in once it has a provider (an account it signed in
/// to with `/login`, or an API key), which is when `--list-models` lists any. The providers it
/// lists say how. Someone who has never run it isn't asked: asking makes its folder.
fn pi_status(bin: &Path) -> Option<(bool, Option<String>)> {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let dir = std::env::var_os("PI_CODING_AGENT_DIR").map(PathBuf::from).unwrap_or_else(|| home.join(".pi/agent"));
    if !dir.exists() {
        return Some((false, None));
    }
    let bin = bin.to_path_buf();
    let (tx, rx) = std::sync::mpsc::channel();
    // Its list can be long (hundreds of models): read all of it, not a pipe's worth.
    std::thread::spawn(move || {
        let _ = tx.send(Command::new(&bin).args(["--list-models", "--offline"]).stdin(Stdio::null()).stderr(Stdio::null()).output());
    });
    let out = rx.recv_timeout(Duration::from_secs(5)).ok()?.ok()?;
    pi_providers(&String::from_utf8_lossy(&out.stdout))
}

/// `pi --list-models`: a header row, then one model a row, its provider first. No rows: none.
fn pi_providers(list: &str) -> Option<(bool, Option<String>)> {
    let mut rows = list.lines().map(|l| l.split_whitespace().collect::<Vec<_>>()).filter(|r| !r.is_empty());
    let header = rows.next()?;
    if header.first() != Some(&"provider") {
        return Some((false, None));
    }
    let mut providers: Vec<&str> = vec![];
    for r in rows {
        if !providers.contains(&r[0]) {
            providers.push(r[0]);
        }
    }
    let how = match providers.len() {
        0 => None,
        1..=2 => Some(providers.join(", ")),
        n => Some(format!("{n} providers")),
    };
    Some((!providers.is_empty(), how))
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
    let path = which_agent(kind);
    let version = path.as_deref().and_then(version_of);
    let auth = path.as_ref().and_then(|_| crate::agent::agent(kind.id)?.login());
    AgentInfo {
        kind: kind.clone(),
        path,
        version,
        auth,
        meterable: crate::agent::agent(kind.id).is_some_and(|a| a.metered()),
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

pub(crate) fn read_json(path: PathBuf) -> Option<serde_json::Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
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
    fn opencode_says_which_providers_it_signed_in_to() {
        let auth = serde_json::json!({"anthropic": {"type": "api", "key": "sk-x"}, "openrouter": {"type": "api", "key": "k"}});
        assert_eq!(opencode_providers(&auth), Some((true, Some("anthropic, openrouter".into()))));
        let many = serde_json::json!({"a": {}, "b": {}, "c": {}});
        assert_eq!(opencode_providers(&many), Some((true, Some("3 providers".into()))));
        assert_eq!(opencode_providers(&serde_json::json!({})), None, "it runs on its own free models: no verdict");
    }

    #[test]
    fn pi_is_signed_in_once_it_has_a_provider() {
        let list = "provider    model                context  max-out  thinking  images\nanthropic   claude-fable-5       1M       128K     yes       yes\nanthropic   claude-opus-5-5      1M       128K     yes       yes\nopenai      gpt-6                400K     128K     yes       yes\n";
        assert_eq!(pi_providers(list), Some((true, Some("anthropic, openai".into()))));
        let many = format!("{list}openrouter  x/y  1M 1K no no\ngoogle  g  1M 1K no no\n");
        assert_eq!(pi_providers(&many), Some((true, Some("4 providers".into()))));
        assert_eq!(pi_providers("No models available. Use /login to log into a provider via OAuth or API key. See:\n  docs/providers.md\n"), Some((false, None)));
    }

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
