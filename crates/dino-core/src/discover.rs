//! What's on this Mac for each agent: how to install it, whether it's signed in, its version.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

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
        "copilot" => "npm i -g @github/copilot",
        "opencode" => "curl -fsSL https://opencode.ai/install | bash",
        "crush" => "brew install charmbracelet/tap/crush",
        "aider" => "pip install aider-install && aider-install",
        "amp" => "curl -fsSL https://ampcode.com/install.sh | bash",
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
        "copilot" => ("https://docs.github.com/copilot/how-tos/copilot-cli", Some("copilot login"), None),
        "opencode" => ("https://opencode.ai/docs", Some("opencode auth login"), None),
        "crush" => ("https://github.com/charmbracelet/crush", None, None),
        "aider" => ("https://aider.chat", None, None),
        "amp" => ("https://ampcode.com/manual", Some("amp login"), None),
        "cursor" => ("https://cursor.com/docs/cli/overview", Some("cursor-agent login"), None),
        _ => ("", None, None),
    };
    let sign_in_note = match id {
        "pi" => Some("Pi has no models of its own. Sign in to an AI provider you already use, with your account or an API key: click Sign In, then type /login in Pi. Or run Pi on a provider from Settings → Models & Providers."),
        "copilot" => Some("Copilot CLI runs on your GitHub account's Copilot plan (Copilot Free included). It also uses the GitHub CLI's sign-in when there is one."),
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
        "cursor" => cursor_status(&run_quietly(bin, &["status", "--format", "json"])?),
        "amp" => crate::agent::amp::signed_in(bin).map(|on| (on, None)),
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

/// `cursor-agent status --format json`: `isAuthenticated`, and nothing else of it kept (it names the
/// account).
fn cursor_status(out: &str) -> Option<(bool, Option<String>)> {
    let v: serde_json::Value = serde_json::from_str(&out[out.find('{')?..]).ok()?;
    Some((v["isAuthenticated"].as_bool()?, None))
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

pub fn version_of(bin: &Path) -> Option<String> {
    let mut child = Command::new(bin).arg("--version").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    // Some CLIs are slow to start; don't let one hold up the whole screen.
    for _ in 0..30 {
        if child.try_wait().ok()?.is_some() {
            let out = child.wait_with_output().ok()?;
            let text = String::from_utf8_lossy(&out.stdout);
            // "GitHub Copilot CLI 1.0.91." ends its sentence.
            return text.split_whitespace().find(|w| w.trim_start_matches('v').starts_with(|c: char| c.is_ascii_digit())).map(|v| v.trim_start_matches('v').trim_end_matches(['.', ',']).to_string());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    None
}

pub(crate) fn read_json(path: PathBuf) -> Option<serde_json::Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::KNOWN_AGENTS;

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
    fn cursor_status_is_read_without_the_account() {
        // Cursor Agent 2026.10.01, signed out.
        let out = "{\n  \"status\": \"unauthenticated\",\n  \"isAuthenticated\": false,\n  \"hasAccessToken\": false,\n  \"hasRefreshToken\": false,\n  \"message\": \"Not logged in\"\n}\n";
        assert_eq!(cursor_status(out), Some((false, None)));
        let signed = r#"{"status":"authenticated","isAuthenticated":true,"userInfo":{"email":"me@example.com"}}"#;
        assert_eq!(cursor_status(signed), Some((true, None)), "the account isn't kept");
        assert_eq!(cursor_status("Error: oops"), None);
    }

    #[test]
    fn every_agent_has_a_way_in() {
        for kind in KNOWN_AGENTS {
            assert!(!install_hint(kind.id).is_empty(), "{} has no install command", kind.id);
            assert!(setup(kind.id).homepage.starts_with("https://"), "{} has no homepage", kind.id);
        }
    }
}
