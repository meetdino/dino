//! How each agent keeps the MCP servers it starts: the agent's own command that adds or removes
//! one, or, for an agent that has none, its own config file. dino adds a server to an agent only
//! for computer use (Settings → Agents, on unless turned off), always this way, and shows exactly
//! what it runs.
//! What it reads back says whether a server by that name is there and what it runs, so dino
//! removes only what it added.

use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::{Value, json};

/// The agents dino can add an MCP server to.
/// Not Cursor Agent: it reads `mcp.json` but has no command that adds a server (`agent mcp` only
/// lists, logs in, enables and disables). Not Amp: it has no record on this Mac of the tools it
/// calls, so dino couldn't show it using the Mac.
pub const AGENTS: &[&str] = &["claude", "codex", "qwen", "kimi", "pi", "hermes", "codewhale", "opencode", "copilot"];

/// A stdio MCP server: the program and its arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Server {
    pub command: String,
    pub args: Vec<String>,
}

/// How a server is added to or removed from an agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    /// Run the agent's own command (these arguments after its program), answering its questions
    /// with `stdin` when it asks any.
    Run { args: Vec<String>, stdin: Option<&'static str> },
    /// Edit its config file, which no command of its own changes.
    Edit { file: PathBuf },
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

fn env_dir(var: &str, default: &str) -> PathBuf {
    std::env::var_os(var).filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| home().join(default))
}

/// The file the agent keeps its (user-wide) MCP servers in.
pub fn config_file(agent: &str) -> Option<PathBuf> {
    Some(match agent {
        "claude" => match std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()) {
            Some(dir) => PathBuf::from(dir).join(".claude.json"),
            None => home().join(".claude.json"),
        },
        "codex" => env_dir("CODEX_HOME", ".codex").join("config.toml"),
        "qwen" => env_dir("QWEN_HOME", ".qwen").join("settings.json"),
        "kimi" => env_dir("KIMI_CODE_HOME", ".kimi-code").join("mcp.json"),
        "pi" => env_dir("PI_CODING_AGENT_DIR", ".pi/agent").join("mcp.json"),
        "hermes" => env_dir("HERMES_HOME", ".hermes").join("config.yaml"),
        "codewhale" => env_dir("CODEWHALE_HOME", ".codewhale").join("mcp.json"),
        "copilot" => env_dir("COPILOT_HOME", ".copilot").join("mcp-config.json"),
        "opencode" => match std::env::var_os("OPENCODE_CONFIG_DIR").filter(|v| !v.is_empty()) {
            Some(dir) => PathBuf::from(dir).join("opencode.json"),
            None => env_dir("XDG_CONFIG_HOME", ".config").join("opencode/opencode.json"),
        },
        _ => return None,
    })
}

/// Server `name` as agent `agent`'s config has it, if it's there.
pub fn find(agent: &str, name: &str) -> Option<Server> {
    let text = std::fs::read_to_string(config_file(agent)?).ok()?;
    find_in(agent, name, &text)
}

fn find_in(agent: &str, name: &str, text: &str) -> Option<Server> {
    let strings = |v: &Value| -> Vec<String> { v.as_array().into_iter().flatten().filter_map(|a| a.as_str().map(String::from)).collect() };
    match agent {
        "codex" => {
            let v: toml::Value = toml::from_str(text).ok()?;
            let s = v.get("mcp_servers")?.get(name)?;
            let args = s.get("args").and_then(|a| a.as_array()).into_iter().flatten().filter_map(|a| a.as_str().map(String::from)).collect();
            Some(Server { command: s.get("command")?.as_str()?.into(), args })
        }
        // YAML without a YAML reader: the entry under `mcp_servers:` as Hermes writes it.
        "hermes" => hermes_entry(text, name),
        "opencode" => {
            let v: Value = serde_json::from_str(text).ok()?;
            let all = strings(&v["mcp"][name]["command"]);
            let (command, args) = all.split_first()?;
            Some(Server { command: command.clone(), args: args.to_vec() })
        }
        _ => {
            let v: Value = serde_json::from_str(text).ok()?;
            // CodeWhale writes `servers`, and reads `mcpServers` too.
            let s = [&v["mcpServers"][name], &v["servers"][name]].into_iter().find(|s| s.is_object())?;
            Some(Server { command: s["command"].as_str()?.into(), args: strings(&s["args"]) })
        }
    }
}

/// `name`'s `command` and `args` in Hermes's config.yaml, as `hermes mcp add` writes them: two
/// spaces in under `mcp_servers:`, its fields four, a list's items six.
fn hermes_entry(text: &str, name: &str) -> Option<Server> {
    let mut lines = text.lines().skip_while(|l| l.trim_end() != "mcp_servers:").skip(1);
    let unquote = |s: &str| s.trim().trim_matches('"').trim_matches('\'').to_string();
    let key = |l: &str| -> Option<String> {
        let l = l.trim_end().strip_prefix("  ").filter(|l| !l.starts_with(' '))?;
        Some(unquote(l.strip_suffix(':')?))
    };
    // Its section ends at the next line that isn't indented.
    lines.by_ref().take_while(|l| l.starts_with(' ') || l.trim().is_empty()).find(|l| key(l).as_deref() == Some(name))?;
    let (mut command, mut args, mut in_args) = (None, vec![], false);
    for l in lines.take_while(|l| l.starts_with("    ") || l.trim().is_empty()) {
        let t = l.trim();
        if let Some(c) = t.strip_prefix("command:") {
            command = Some(unquote(c));
            in_args = false;
        } else if let Some(rest) = t.strip_prefix("args:") {
            in_args = rest.trim().is_empty();
            if rest.trim().starts_with('[') {
                args = rest.trim().trim_matches(['[', ']']).split(',').map(unquote).filter(|a| !a.is_empty()).collect();
            }
        } else if let Some(a) = t.strip_prefix("- ").filter(|_| in_args) {
            args.push(unquote(a));
        } else if !t.is_empty() {
            in_args = false;
        }
    }
    Some(Server { command: command?, args })
}

/// How `server` is added to `agent` as `name`.
pub fn adding(agent: &str, name: &str, server: &Server) -> Option<Change> {
    let mut args: Vec<String> = match agent {
        "claude" => ["mcp", "add", "-s", "user", name, "--"].map(String::from).to_vec(),
        "codex" | "copilot" => ["mcp", "add", name, "--"].map(String::from).to_vec(),
        "qwen" => ["mcp", "add", "-s", "user", name].map(String::from).to_vec(),
        // Declared to the model as tools of their own, not reached through its code mode: they're
        // few, and their names then say what it's doing.
        "pi" => ["mcp", "add", name, "--exposure", "direct", "--"].map(String::from).to_vec(),
        "hermes" => {
            let mut a: Vec<String> = ["mcp", "add", name, "--command", server.command.as_str()].map(String::from).to_vec();
            if !server.args.is_empty() {
                a.push("--args".into());
                a.extend(server.args.iter().cloned());
            }
            // It lists the tools it found and asks whether to turn them all on.
            return Some(Change::Run { args: a, stdin: Some("y\n") });
        }
        "codewhale" => {
            let mut a: Vec<String> = ["mcp", "add", name, "--command", server.command.as_str()].map(String::from).to_vec();
            for arg in &server.args {
                a.push("--arg".into());
                a.push(arg.clone());
            }
            return Some(Change::Run { args: a, stdin: None });
        }
        "kimi" | "opencode" => return Some(Change::Edit { file: config_file(agent)? }),
        _ => return None,
    };
    args.push(server.command.clone());
    args.extend(server.args.iter().cloned());
    Some(Change::Run { args, stdin: None })
}

/// How server `name` is removed from `agent`.
pub fn removing(agent: &str, name: &str) -> Option<Change> {
    let args: Vec<&str> = match agent {
        "claude" => vec!["mcp", "remove", "-s", "user", name],
        "qwen" => vec!["mcp", "remove", "-s", "user", name],
        "codex" | "pi" | "codewhale" | "copilot" => vec!["mcp", "remove", name],
        // It asks to be sure.
        "hermes" => return Some(Change::Run { args: vec!["mcp".into(), "remove".into(), name.into()], stdin: Some("y\n") }),
        "kimi" | "opencode" => return Some(Change::Edit { file: config_file(agent)? }),
        _ => return None,
    };
    Some(Change::Run { args: args.into_iter().map(String::from).collect(), stdin: None })
}

/// What a change does, as the user would type it or as a sentence: shown before it's made.
pub fn shown(program: &str, change: &Change, adding: Option<(&str, &Server)>) -> String {
    match change {
        Change::Run { args, .. } => std::iter::once(program).chain(args.iter().map(String::as_str)).map(quoted).collect::<Vec<_>>().join(" "),
        Change::Edit { file } => {
            let file = file.strip_prefix(home()).map(|p| format!("~/{}", p.display())).unwrap_or_else(|_| file.display().to_string());
            match adding {
                Some((name, s)) => format!("Adds “{name}” ({}) to {file}", std::iter::once(&s.command).chain(&s.args).map(|a| quoted(a)).collect::<Vec<_>>().join(" ")),
                None => format!("Removes it from {file}"),
            }
        }
    }
}

fn quoted(a: &str) -> String {
    if !a.is_empty() && a.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:@+,".contains(c)) {
        a.to_string()
    } else {
        format!("'{}'", a.replace('\'', r"'\''"))
    }
}

/// Make the change with the agent's program `bin`: its own command, or an edit of its file.
pub fn apply(agent: &str, bin: &Path, name: &str, change: &Change, server: Option<&Server>) -> anyhow::Result<()> {
    match change {
        Change::Run { args, stdin } => run(bin, args, *stdin),
        Change::Edit { file } => edit(agent, file, name, server),
    }
}

/// Its own command, answering what it asks; what it said when it fails.
fn run(bin: &Path, args: &[String], stdin: Option<&str>) -> anyhow::Result<()> {
    let mut child = Command::new(bin)
        .args(args)
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let (Some(text), Some(mut input)) = (stdin, child.stdin.take()) {
        let _ = input.write_all(text.as_bytes());
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let pid = child.id();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    // Hermes connects to the server to list its tools; the others only write a file.
    let out = match rx.recv_timeout(Duration::from_secs(60)) {
        Ok(out) => out?,
        Err(_) => {
            // Our own child, by its pid.
            kill(pid);
            anyhow::bail!("{} didn't finish within a minute", bin.display());
        }
    };
    let said = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    anyhow::ensure!(out.status.success(), "{}", last_lines(&said));
    // Hermes saves a server it couldn't start, turned off, and still succeeds.
    anyhow::ensure!(!said.contains("(disabled)"), "{}", last_lines(&said));
    Ok(())
}

fn kill(pid: u32) {
    let _ = Command::new("/bin/kill").arg(pid.to_string()).status();
}

fn last_lines(said: &str) -> String {
    let lines: Vec<&str> = said.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    lines[lines.len().saturating_sub(3)..].join("\n")
}

/// Add (`server`) or remove (`None`) entry `name` in the file the agent reads, keeping everything
/// else as it was.
fn edit(agent: &str, file: &Path, name: &str, server: Option<&Server>) -> anyhow::Result<()> {
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    let next = edited(agent, &text, name, server).map_err(|e| anyhow::anyhow!("{}: {e}", file.display()))?;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Write then rename, keeping its permissions, so the agent never reads half a file.
    let mode = file.metadata().map(|m| m.permissions().mode() & 0o777).unwrap_or(0o600);
    let tmp = file.with_extension("dino.tmp");
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(mode).open(&tmp)?;
    f.write_all(next.as_bytes())?;
    drop(f);
    std::fs::rename(tmp, file)?;
    Ok(())
}

fn edited(agent: &str, text: &str, name: &str, server: Option<&Server>) -> anyhow::Result<String> {
    let mut v: Value = if text.trim().is_empty() { json!({}) } else { serde_json::from_str(text).map_err(|e| anyhow::anyhow!("not JSON dino can edit ({e})"))? };
    anyhow::ensure!(v.is_object(), "not a JSON object");
    let (key, entry) = match agent {
        "opencode" => ("mcp", server.map(|s| json!({"type": "local", "command": std::iter::once(&s.command).chain(&s.args).collect::<Vec<_>>(), "enabled": true}))),
        _ => ("mcpServers", server.map(|s| json!({"command": s.command, "args": s.args}))),
    };
    match entry {
        Some(e) => {
            if !v[key].is_object() {
                v[key] = json!({});
            }
            v[key][name] = e;
        }
        None => {
            if let Some(m) = v[key].as_object_mut() {
                m.remove(name);
            }
        }
    }
    Ok(serde_json::to_string_pretty(&v)? + "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ocu() -> Server {
        Server { command: "/d/Open Computer Use.app/Contents/MacOS/OpenComputerUse".into(), args: vec!["mcp".into()] }
    }

    #[test]
    fn each_agent_is_changed_with_its_own_command() {
        let s = ocu();
        let shown_for = |agent: &str| shown(agent, &adding(agent, "open-computer-use", &s).unwrap(), Some(("open-computer-use", &s)));
        assert_eq!(shown_for("claude"), "claude mcp add -s user open-computer-use -- '/d/Open Computer Use.app/Contents/MacOS/OpenComputerUse' mcp");
        assert_eq!(shown_for("codex"), "codex mcp add open-computer-use -- '/d/Open Computer Use.app/Contents/MacOS/OpenComputerUse' mcp");
        assert_eq!(shown_for("qwen"), "qwen mcp add -s user open-computer-use '/d/Open Computer Use.app/Contents/MacOS/OpenComputerUse' mcp");
        assert_eq!(shown_for("pi"), "pi mcp add open-computer-use --exposure direct -- '/d/Open Computer Use.app/Contents/MacOS/OpenComputerUse' mcp");
        assert_eq!(shown_for("hermes"), "hermes mcp add open-computer-use --command '/d/Open Computer Use.app/Contents/MacOS/OpenComputerUse' --args mcp");
        assert_eq!(shown_for("codewhale"), "codewhale mcp add open-computer-use --command '/d/Open Computer Use.app/Contents/MacOS/OpenComputerUse' --arg mcp");
        assert_eq!(shown_for("copilot"), "copilot mcp add open-computer-use -- '/d/Open Computer Use.app/Contents/MacOS/OpenComputerUse' mcp");
        assert_eq!(removing("copilot", "open-computer-use"), Some(Change::Run { args: ["mcp", "remove", "open-computer-use"].map(String::from).to_vec(), stdin: None }));
        assert!(matches!(adding("kimi", "x", &s), Some(Change::Edit { .. })));
        assert_eq!(removing("claude", "open-computer-use"), Some(Change::Run { args: ["mcp", "remove", "-s", "user", "open-computer-use"].map(String::from).to_vec(), stdin: None }));
        assert!(adding("aider", "x", &s).is_none());
    }

    #[test]
    fn what_each_config_has_is_read_back() {
        let s = ocu();
        let claude = r#"{"numStartups": 3, "mcpServers": {"open-computer-use": {"type": "stdio", "command": "/d/Open Computer Use.app/Contents/MacOS/OpenComputerUse", "args": ["mcp"], "env": {}}}}"#;
        assert_eq!(find_in("claude", "open-computer-use", claude), Some(s.clone()));
        assert_eq!(find_in("claude", "other", claude), None);
        let codex = "model = \"x\"\n[mcp_servers.open-computer-use]\ncommand = \"/d/Open Computer Use.app/Contents/MacOS/OpenComputerUse\"\nargs = [\"mcp\"]\n";
        assert_eq!(find_in("codex", "open-computer-use", codex), Some(s.clone()));
        let codewhale = r#"{"servers": {"open-computer-use": {"command": "/d/Open Computer Use.app/Contents/MacOS/OpenComputerUse", "args": ["mcp"], "env": {}, "enabled": true}}}"#;
        assert_eq!(find_in("codewhale", "open-computer-use", codewhale), Some(s.clone()));
        // As `copilot mcp add` 1.0.91 wrote it.
        let copilot = r#"{"mcpServers": {"open-computer-use": {"tools": ["*"], "type": "local", "command": "/d/Open Computer Use.app/Contents/MacOS/OpenComputerUse", "args": ["mcp"]}}}"#;
        assert_eq!(find_in("copilot", "open-computer-use", copilot), Some(s.clone()));
        // As `hermes mcp add` 0.19 wrote it.
        let hermes = "model:\n  default: x\nmcp_servers:\n  other:\n    command: npx\n  open-computer-use:\n    command: /d/Open Computer Use.app/Contents/MacOS/OpenComputerUse\n    args:\n      - mcp\n    enabled: true\n\n# ── Security\n";
        assert_eq!(find_in("hermes", "open-computer-use", hermes), Some(s.clone()));
        assert_eq!(find_in("hermes", "other", hermes), Some(Server { command: "npx".into(), args: vec![] }));
        assert_eq!(find_in("hermes", "missing", hermes), None);
    }

    #[test]
    fn a_file_is_edited_keeping_everything_else() {
        let s = ocu();
        let theirs = "{\n  \"$schema\": \"https://opencode.ai/config.json\",\n  \"mcp\": {\"mine\": {\"type\": \"remote\", \"url\": \"https://x\"}},\n  \"model\": \"a\"\n}";
        let added = edited("opencode", theirs, "open-computer-use", Some(&s)).unwrap();
        assert_eq!(find_in("opencode", "open-computer-use", &added), Some(s.clone()));
        let v: Value = serde_json::from_str(&added).unwrap();
        assert_eq!(v["mcp"]["mine"]["url"], "https://x", "theirs stays");
        assert_eq!(v["mcp"]["open-computer-use"]["type"], "local");
        assert_eq!(v.as_object().unwrap().keys().collect::<Vec<_>>(), ["$schema", "mcp", "model"], "in its order");
        let removed = edited("opencode", &added, "open-computer-use", None).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&removed).unwrap(), serde_json::from_str::<Value>(theirs).unwrap());
        let kimi = edited("kimi", "", "open-computer-use", Some(&s)).unwrap();
        assert_eq!(find_in("kimi", "open-computer-use", &kimi), Some(s));
        assert!(edited("opencode", "{ // a comment\n}", "x", None).is_err(), "JSONC isn't rewritten");
    }
}
