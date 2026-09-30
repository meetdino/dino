//! The shell's AI line (`dino ai`): a request in plain words becomes one command, from the user's
//! own agent run headless with no tools, or goes to that agent as a new session. Nothing here runs
//! the command: the shell integration puts it on the prompt for the user to read and run.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use dino_core::controls::{self, Controls};
use dino_core::ipc::{Request, Response};
use dino_core::models;
use dino_core::settings::Settings;
use dino_core::trust;

/// Exit status of `dino ai suggest` when the command it prints could destroy something.
pub const RISKY: i32 = 10;
/// Exit status when the agent says it's not something a command can do; the reason is on stderr.
pub const NOT_A_COMMAND: i32 = 3;

const USAGE: &str = "usage: dino ai suggest [--agent claude|codex] [--shell zsh] [--cwd DIR] [--last CMD --status N] -- <request>
       dino ai agent [--agent claude|codex] [--cwd DIR] [--last CMD --status N] -- <request>
       dino ai risky [--cwd DIR] -- <command>";

pub fn run(args: &[String]) -> anyhow::Result<()> {
    let opts = Opts::parse(args.get(1..).unwrap_or_default())?;
    match args.first().map(String::as_str) {
        Some("suggest") => {
            let command = match suggest(&opts) {
                Ok(c) => c,
                Err(Failure::NotACommand(why)) => {
                    eprintln!("{why}");
                    std::process::exit(NOT_A_COMMAND);
                }
                Err(Failure::Other(e)) => {
                    eprintln!("dino: {e}");
                    std::process::exit(1);
                }
            };
            println!("{command}");
            if let Some(why) = risky(&command, &opts.cwd) {
                eprintln!("{why}");
                std::process::exit(RISKY);
            }
            Ok(())
        }
        Some("agent") => agent(&opts),
        Some("risky") => {
            if let Some(why) = risky(&opts.words, &opts.cwd) {
                println!("{why}");
                std::process::exit(RISKY);
            }
            Ok(())
        }
        _ => {
            println!("{USAGE}");
            Ok(())
        }
    }
}

struct Opts {
    agent: Option<String>,
    shell: String,
    cwd: std::path::PathBuf,
    last: Option<String>,
    status: Option<i32>,
    words: String,
}

impl Opts {
    fn parse(args: &[String]) -> anyhow::Result<Self> {
        let mut o = Opts {
            agent: std::env::var("DINO_AI_AGENT").ok().filter(|a| !a.is_empty()),
            shell: std::env::var("SHELL").ok().and_then(|s| s.rsplit('/').next().map(String::from)).unwrap_or_else(|| "zsh".into()),
            cwd: std::env::current_dir().unwrap_or_else(|_| "/".into()),
            last: None,
            status: None,
            words: String::new(),
        };
        let mut it = args.iter();
        while let Some(a) = it.next() {
            let mut value = || it.next().cloned().ok_or_else(|| anyhow::anyhow!("{a} needs a value\n{USAGE}"));
            match a.as_str() {
                "--agent" => o.agent = Some(value()?),
                "--shell" => o.shell = value()?,
                "--cwd" => o.cwd = value()?.into(),
                "--last" => o.last = Some(value()?).filter(|l| !l.is_empty()),
                "--status" => o.status = value()?.parse().ok(),
                "--" => {
                    o.words = it.by_ref().cloned().collect::<Vec<_>>().join(" ");
                    break;
                }
                _ => o.words = [o.words.as_str(), a.as_str()].join(" ").trim().to_string(),
            }
        }
        o.words = o.words.trim().trim_start_matches('#').trim().to_string();
        Ok(o)
    }
}

enum Failure {
    NotACommand(String),
    Other(anyhow::Error),
}

impl<E: Into<anyhow::Error>> From<E> for Failure {
    fn from(e: E) -> Self {
        Failure::Other(e.into())
    }
}

/// Which agent answers: `--agent`/`DINO_AI_AGENT`, else the default in Settings, else whichever
/// of Claude Code and Codex is installed.
fn which_agent(asked: Option<&str>) -> anyhow::Result<&'static str> {
    let pick = |a: &str| if a.starts_with("codex") { "codex" } else { "claude" };
    if let Some(a) = asked {
        return Ok(pick(a));
    }
    let default = Settings::load().policies.default_agent.filter(|a| a.starts_with("claude") || a.starts_with("codex"));
    let wanted = default.as_deref().map(pick).unwrap_or("claude");
    let other = if wanted == "claude" { "codex" } else { "claude" };
    [wanted, other].into_iter().find(|a| installed(a)).ok_or_else(|| anyhow::anyhow!("neither Claude Code nor Codex is installed"))
}

fn installed(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(program).is_file()))
}

/// Model and effort from Settings → Agents, as the agent's own flags; its defaults otherwise.
fn control_args(agent: &str) -> Vec<String> {
    let chosen = Settings::load().agents.get(agent).cloned().unwrap_or_default();
    let c = Controls { mode: None, ..chosen };
    let catalog = match agent {
        "codex" => models::codex_from_files(),
        _ => models::claude_from_files(None),
    };
    controls::args(agent, &c, &controls::knobs(agent, false, catalog.as_ref()))
}

fn instructions(shell: &str) -> String {
    let os = if cfg!(target_os = "macos") { "macOS" } else { std::env::consts::OS };
    format!(
        "You turn a request typed at a {shell} prompt on {os} into one {shell} command. Reply with the command only, \
         on one line: no explanation, no markdown, no code fences, no leading $. Prefer common, safe commands. \
         If no command does it, reply with # and a short reason instead."
    )
}

fn request_text(o: &Opts, output: Option<&str>) -> String {
    let mut t = format!("Folder: {}\n", o.cwd.display());
    if let Some(last) = &o.last {
        t.push_str(&format!("Last command: {}{}\n", hide_secrets(last), o.status.map(|s| format!(" (exit {s})")).unwrap_or_default()));
    }
    if let Some(output) = output {
        t.push_str(&format!("Its output (last lines):\n{output}\n"));
    }
    t.push_str(&format!("Request: {}", o.words));
    t
}

/// Lines of output kept for a suggestion: enough to see an error, cheap to send.
const SUGGEST_OUTPUT_LINES: usize = 60;

/// What this Dino shell's last command printed, from its shell integration (via dinod), with
/// lines that look like secrets taken out. None outside a Dino shell, if it printed nothing, or
/// with `DINO_AI_OUTPUT=0`.
fn shell_output() -> Option<(String, Option<i32>)> {
    if std::env::var("DINO_AI_OUTPUT").is_ok_and(|v| v == "0") {
        return None;
    }
    let id = std::env::var("DINO_SESSION").ok().filter(|s| !s.is_empty())?;
    match crate::client::request(&Request::ShellOutput { id }).ok()? {
        Response::ShellOutput { output: Some(text), exit } => Some((hide_secrets(&text), exit)),
        _ => None,
    }
}

/// `text` with any line that looks like it holds a secret (a key, a token, a password, a private
/// key) replaced by a note: output goes to a model, and a leaked credential can't be taken back.
pub fn hide_secrets(text: &str) -> String {
    const MARKERS: [&str; 16] = [
        "sk-", "sk_live_", "rk_live_", "ghp_", "gho_", "ghu_", "ghs_", "github_pat_", "xoxb-", "xoxp-", "akia", "nvapi-", "aiza", "-----begin", "eyjhbgci", "glpat-",
    ];
    const NAMES: [&str; 8] = ["password", "passwd", "secret", "token", "api_key", "apikey", "authorization", "private_key"];
    let looks_secret = |line: &str| {
        let lower = line.to_ascii_lowercase();
        if MARKERS.iter().any(|m| lower.contains(m)) {
            return true;
        }
        // name=value or name: value
        if NAMES.iter().any(|n| lower.find(n).is_some_and(|i| lower[i + n.len()..].trim_start().starts_with(['=', ':']))) {
            return true;
        }
        // A long run of letters and digits mixed: a key or a hash of one.
        line.split(|c: char| !(c.is_ascii_alphanumeric() || "+/_=-".contains(c))).any(|w| {
            w.len() >= 32 && w.chars().any(|c| c.is_ascii_digit()) && w.chars().any(|c| c.is_ascii_alphabetic()) && !w.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
        })
    };
    text.lines().map(|l| if looks_secret(l) { "[line hidden: it looks like a secret]" } else { l }).collect::<Vec<_>>().join("\n")
}

/// The environment of a Claude Code the user started: this may run inside one, and a nested
/// `claude -p` that inherits its markers behaves as a child of it.
fn scrub(cmd: &mut Command) {
    for (k, _) in std::env::vars_os() {
        let k = k.to_string_lossy().to_string();
        if k == "CLAUDECODE" || k == "CLAUDE_PID" || k == "CLAUDE_EFFORT" || (k.starts_with("CLAUDE_CODE_") && !k.starts_with("CLAUDE_CODE_USE_")) {
            cmd.env_remove(k);
        }
    }
}

fn suggest(o: &Opts) -> Result<String, Failure> {
    if o.words.is_empty() {
        return Err(Failure::NotACommand("type what you want to do first".into()));
    }
    let agent = which_agent(o.agent.as_deref())?;
    // After a failed command its error is usually what the request is about.
    let output = shell_output()
        .filter(|(_, exit)| exit.is_some_and(|e| e != 0))
        .map(|(text, _)| text.lines().rev().take(SUGGEST_OUTPUT_LINES).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n"));
    let out_file = std::env::temp_dir().join(format!("dino-ai-{}.txt", std::process::id()));
    let mut cmd = Command::new(agent);
    match agent {
        "codex" => {
            // Read-only sandbox, never asks, keeps no session: it can look but not act.
            cmd.args(["exec", "--skip-git-repo-check", "--ephemeral", "-s", "read-only", "--color", "never", "-o"]).arg(&out_file);
            cmd.args(control_args(agent));
            cmd.arg(format!("{}\n\n{}", instructions(&o.shell), request_text(o, output.as_deref())));
        }
        _ => {
            // No tools at all, no MCP servers, nothing saved: a plain answer. In a folder Claude
            // doesn't trust, not the project's settings either: their hooks would run.
            cmd.args(trust::claude_headless_args(trust::claude_trusts(&o.cwd)));
            cmd.args(["-p", "--tools", "", "--strict-mcp-config", "--no-session-persistence", "--output-format", "text", "--system-prompt"]);
            cmd.arg(instructions(&o.shell));
            cmd.args(control_args(agent));
            cmd.arg(request_text(o, output.as_deref()));
        }
    }
    scrub(&mut cmd);
    cmd.current_dir(&o.cwd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let text = run_for(cmd, timeout(), agent)?;
    let text = if agent == "codex" { std::fs::read_to_string(&out_file).unwrap_or(text) } else { text };
    let _ = std::fs::remove_file(&out_file);
    parse(&text)
}

fn timeout() -> Duration {
    Duration::from_secs(std::env::var("DINO_AI_TIMEOUT").ok().and_then(|t| t.parse().ok()).unwrap_or(60))
}

/// Its stdout; an error with its stderr when it fails, or when it's still going after `limit`.
fn run_for(mut cmd: Command, limit: Duration, name: &str) -> anyhow::Result<String> {
    let mut child = cmd.spawn().map_err(|e| anyhow::anyhow!("couldn't start {name}: {e}"))?;
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let out = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        s
    });
    let err = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if start.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("{name} didn't answer within {}s", limit.as_secs());
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    let (out, err) = (out.join().unwrap_or_default(), err.join().unwrap_or_default());
    if !status.success() {
        let why = err.lines().chain(out.lines()).map(str::trim).filter(|l| !l.is_empty()).last().unwrap_or("no output");
        anyhow::bail!("{name} failed: {why}");
    }
    Ok(out)
}

/// The command in an agent's reply, without fences or a prompt sign.
fn parse(text: &str) -> Result<String, Failure> {
    let lines: Vec<&str> = text.lines().map(str::trim_end).filter(|l| !l.trim().starts_with("```") && !l.trim().is_empty()).collect();
    let command = lines.join("\n");
    let command = command.trim().trim_start_matches("$ ").trim().to_string();
    if command.is_empty() {
        return Err(Failure::Other(anyhow::anyhow!("the agent gave no command")));
    }
    if let Some(why) = command.strip_prefix('#') {
        return Err(Failure::NotACommand(why.trim().to_string()));
    }
    Ok(command)
}

/// Why `command` could destroy something, if it could: it then needs a second Enter.
pub fn risky(command: &str, cwd: &Path) -> Option<&'static str> {
    for segment in command.split(['\n', ';', '|', '&']) {
        let words: Vec<&str> = segment.split_whitespace().collect();
        let Some(&first) = words.first() else { continue };
        let has = |flag: &str| words.iter().any(|w| *w == flag);
        // Short flags, bundled or not: `-rf` has r.
        let short = |c: char| words[1..].iter().any(|w| w.starts_with('-') && !w.starts_with("--") && w.contains(c));
        let why = match first {
            "sudo" | "doas" => Some("runs as root"),
            "rm" if short('r') || short('R') || has("--recursive") => Some("deletes folders"),
            "dd" => Some("writes raw to a disk or file"),
            f if f.starts_with("mkfs") => Some("formats a disk"),
            "chmod" | "chown" | "chgrp" if short('R') || has("--recursive") => Some("changes permissions throughout a folder"),
            "git" => match words.get(1).copied() {
                Some("push") if short('f') || has("--force") || words.iter().any(|w| w.starts_with("--force-with-lease") || w.starts_with('+')) => Some("rewrites the remote's history"),
                Some("reset") if has("--hard") => Some("throws away uncommitted changes"),
                Some("clean") if short('f') => Some("deletes untracked files"),
                _ => None,
            },
            "find" if has("-delete") => Some("deletes what it finds"),
            _ => None,
        };
        if why.is_some() {
            return why;
        }
        // `> file` over one that's there (not >>, not 2>&1).
        for (i, w) in words.iter().enumerate() {
            let target = match w.strip_prefix('>').or_else(|| w.strip_prefix("1>")) {
                Some("") => words.get(i + 1).copied(),
                Some(t) if !t.starts_with('>') && !t.starts_with('&') => Some(t),
                _ => None,
            };
            if let Some(t) = target.map(|t| t.trim_matches(['"', '\''])).filter(|t| *t != "/dev/null") {
                let t = t.strip_prefix("~/").map(|rest| std::env::var("HOME").map(|h| format!("{h}/{rest}")).unwrap_or_default()).unwrap_or_else(|| t.to_string());
                if cwd.join(&t).exists() {
                    return Some("overwrites a file that's there");
                }
            }
        }
    }
    None
}

/// Hand the request to the default agent as a new session: beside this shell in Dino, or
/// attached here in any other terminal.
fn agent(o: &Opts) -> anyhow::Result<()> {
    anyhow::ensure!(!o.words.is_empty(), "type what you want the agent to do first");
    let agent = which_agent(o.agent.as_deref())?;
    // In a Dino shell the app shows the new session under it; elsewhere it takes this terminal.
    let by = std::env::var("DINO_SESSION").ok().filter(|s| !s.is_empty());
    let (cols, rows) = crossterm::terminal::size().unwrap_or((120, 40));
    // What the shell's last command printed goes along, so "fix this" has something to fix.
    let context = by.as_ref().and_then(|_| shell_output()).map(|(text, exit)| {
        let what = match o.last.as_deref().map(hide_secrets) {
            Some(c) if c.starts_with("[line hidden") => "the last command (not shown: it looks like it holds a secret)".into(),
            Some(c) => format!("`{c}`"),
            None => "the last command".to_string(),
        };
        let how = exit.map(|e| format!(", which exited with {e},")).unwrap_or_default();
        format!("\n\nFor context: in my shell, {what}{how} printed:\n```\n{text}\n```")
    });
    let req = Request::New {
        launcher: agent.into(),
        args: vec![],
        cwd: Some(o.cwd.display().to_string()),
        cols,
        rows,
        worktree: false,
        controls: Default::default(),
        host: None,
        prompt: Some(format!("{}{}", o.words, context.unwrap_or_default())),
        by: by.clone(),
        route: None,
    };
    match crate::client::request(&req)? {
        Response::Created { id } if by.is_some() => {
            println!("{id}");
            Ok(())
        }
        Response::Created { id } => crate::client::attach_raw(&id),
        Response::Error { message } => anyhow::bail!(message),
        other => anyhow::bail!("unexpected reply from dinod: {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replies_become_one_command() {
        assert_eq!(parse("```bash\nls -la\n```\n").ok(), Some("ls -la".into()));
        assert_eq!(parse("$ du -sh .\n").ok(), Some("du -sh .".into()));
        assert!(matches!(parse("# needs a browser"), Err(Failure::NotACommand(w)) if w == "needs a browser"));
        assert!(parse("\n\n").is_err());
    }

    #[test]
    fn destructive_commands_are_flagged() {
        let dir = std::env::temp_dir().join(format!("dino-risky-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("notes.txt"), "x").unwrap();
        for c in ["rm -rf build", "rm -r x", "sudo ls", "dd if=/dev/zero of=x", "mkfs.ext4 /dev/sda", "git push --force", "git push -f origin main", "git reset --hard HEAD~1", "chmod -R 777 .", "ls && rm -Rf /tmp/x", "echo hi > notes.txt", "find . -name '*.o' -delete", "git clean -fd"] {
            assert!(risky(c, &dir).is_some(), "{c}");
        }
        for c in ["ls -la", "rm notes.txt", "echo hi >> notes.txt", "echo hi > new.txt", "cmd 2>&1 | less", "cmd > /dev/null", "git push", "git reset HEAD", "grep -r foo ."] {
            assert!(risky(c, &dir).is_none(), "{c}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn secrets_stay_out_of_the_context() {
        let text = "error: build failed\nexport OPENAI_API_KEY=sk-proj-abc123\nPASSWORD: hunter2\ntoken = 9f8e7d\nkey 3kF9aB2mQ7xL0pZ4rT8vW1yC6nH5jD2e\nsee https://example.com/docs\ncommit 4f2a9c1e8b7d6a5f4e3d2c1b0a9f8e7d6c5b4a39";
        let shown = hide_secrets(text);
        assert_eq!(shown.lines().filter(|l| l.starts_with("[line hidden")).count(), 4, "{shown}");
        assert!(shown.contains("error: build failed") && shown.contains("https://example.com/docs") && shown.contains("commit 4f2a9c1e"));
    }

    #[test]
    fn options_and_words() {
        let o = Opts::parse(&["--shell".into(), "bash".into(), "--last".into(), "make".into(), "--status".into(), "2".into(), "--".into(), "#".into(), "list".into(), "big files".into()]).unwrap();
        assert_eq!((o.shell.as_str(), o.last.as_deref(), o.status, o.words.as_str()), ("bash", Some("make"), Some(2), "list big files"));
    }
}
