//! Agent sessions that exist outside dino: running in another terminal, recent on disk, or in the
//! cloud. dino can continue any of them (see dinod's `Adopt`).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// A live process in some other terminal.
    Running,
    /// A conversation on disk that nothing is running.
    Recent,
    /// Lives in a provider's cloud.
    Cloud,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FoundSession {
    pub source: Source,
    /// "claude" or "codex".
    pub agent: String,
    /// The agent's own conversation id (or cloud task id). Empty for "pick in the agent" entries.
    pub session_id: String,
    pub title: String,
    pub cwd: Option<String>,
    pub updated_at: u64,
    /// Running: process, its state ("busy"/"idle" when known), the app it runs in, launch flags.
    pub pid: Option<u32>,
    pub status: Option<String>,
    pub terminal: Option<String>,
    pub args: Vec<String>,
    pub url: Option<String>,
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

fn mtime(p: &Path) -> u64 {
    p.metadata().ok().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs())
}

fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn alive(pid: u32) -> bool {
    run("ps", &["-p", &pid.to_string(), "-o", "pid="]).is_some_and(|s| !s.trim().is_empty())
}

/// The GUI app (or multiplexer) a process lives in, found by walking up its parents.
pub fn terminal_of(pid: u32) -> Option<String> {
    let mut p = pid;
    for _ in 0..12 {
        let line = run("ps", &["-o", "ppid=,comm=", "-p", &p.to_string()])?;
        let line = line.trim();
        let (ppid, comm) = line.split_once(' ')?;
        let comm = comm.trim();
        if let Some(i) = comm.find(".app/") {
            let app = comm[..i].rsplit('/').next().unwrap_or(comm);
            return Some(match app {
                "iTerm" | "iTerm2" => "iTerm2".into(),
                "Code" | "Visual Studio Code" => "VS Code".into(),
                other => other.to_string(),
            });
        }
        if comm.ends_with("tmux") || comm.contains("tmux: server") {
            return Some("tmux".into());
        }
        p = ppid.trim().parse().ok()?;
        if p <= 1 {
            return None;
        }
    }
    None
}

fn args_of(pid: u32) -> Vec<String> {
    run("ps", &["-o", "args=", "-p", &pid.to_string()]).map(|s| s.split_whitespace().skip(1).map(String::from).collect()).unwrap_or_default()
}

/// Launch flags worth carrying over when continuing a session (permissions, model, extra dirs),
/// minus the ones that pick or create a session.
pub fn portable_flags(agent: &str, args: &[String]) -> Vec<String> {
    let (drop_with_value, drop_alone): (&[&str], &[&str]) = match agent {
        "claude" => (
            &["--resume", "-r", "--session-id", "--settings", "--teleport", "--from-pr", "--output-format", "--input-format"],
            &["--continue", "-c", "--fork-session", "--print", "-p"],
        ),
        _ => (&["-c", "--config"], &["resume", "--last"]),
    };
    let mut out = vec![];
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        let key = a.split('=').next().unwrap_or(a);
        if drop_with_value.contains(&key) {
            // `--resume` may be bare (picker); only skip a value that isn't another flag.
            if !a.contains('=') && args.get(i + 1).is_some_and(|v| !v.starts_with('-')) {
                i += 1;
            }
        } else if !drop_alone.contains(&key) && (a.starts_with('-') || out.last().is_some_and(|p: &String| p.starts_with('-'))) {
            out.push(a.clone());
        }
        i += 1;
    }
    out
}

/// Claude Code writes `~/.claude/sessions/<pid>.json` for every live process.
fn running_claude() -> Vec<FoundSession> {
    let dir = home().join(".claude/sessions");
    let mut out = vec![];
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let Some(v) = std::fs::read_to_string(&p).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()) else { continue };
        let (Some(pid), Some(sid)) = (v["pid"].as_u64(), v["sessionId"].as_str()) else { continue };
        let pid = pid as u32;
        if v["kind"].as_str().is_some_and(|k| k != "interactive") || !alive(pid) {
            continue;
        }
        let cwd = v["cwd"].as_str().map(String::from);
        let title = crate::history::claude_title(sid).or_else(|| v["name"].as_str().map(String::from)).unwrap_or_else(|| "Claude Code session".into());
        out.push(FoundSession {
            source: Source::Running,
            agent: "claude".into(),
            session_id: sid.into(),
            title,
            cwd,
            updated_at: v["updatedAt"].as_u64().map_or(0, |ms| ms / 1000),
            pid: Some(pid),
            status: v["status"].as_str().map(String::from),
            terminal: terminal_of(pid),
            args: portable_flags("claude", &args_of(pid)),
            url: None,
        });
    }
    out
}

/// Codex keeps its rollout file open; `lsof` names the session and its cwd.
fn running_codex() -> Vec<FoundSession> {
    let Some(pids) = run("pgrep", &["-x", "codex"]) else { return vec![] };
    let titles = crate::history::codex_titles();
    let mut out = vec![];
    for pid in pids.lines().filter_map(|l| l.trim().parse::<u32>().ok()) {
        let Some(files) = run("lsof", &["-p", &pid.to_string(), "-Fn"]) else { continue };
        let Some(rollout) = files.lines().filter_map(|l| l.strip_prefix('n')).find(|f| f.contains("/.codex/sessions/") && f.ends_with(".jsonl")) else {
            continue;
        };
        let rollout = Path::new(rollout);
        let Some(sid) = crate::history::rollout_id(rollout) else { continue };
        let cwd = run("lsof", &["-a", "-p", &pid.to_string(), "-d", "cwd", "-Fn"])
            .and_then(|s| s.lines().find_map(|l| l.strip_prefix('n').map(String::from)));
        out.push(FoundSession {
            source: Source::Running,
            agent: "codex".into(),
            title: titles.get(&sid).cloned().or_else(|| crate::history::codex_meta(rollout).title).unwrap_or_else(|| "Codex session".into()),
            session_id: sid,
            cwd,
            updated_at: mtime(rollout),
            pid: Some(pid),
            status: crate::history::codex_status(rollout),
            terminal: terminal_of(pid),
            args: portable_flags("codex", &args_of(pid)),
            url: None,
        });
    }
    out
}

pub fn running() -> Vec<FoundSession> {
    let mut v = running_claude();
    v.extend(running_codex());
    v
}

/// Cloud work: Codex cloud tasks, plus Claude Code web sessions (picked via `--teleport`).
pub fn cloud(codex: Option<&Path>, claude: bool) -> Vec<FoundSession> {
    let mut out = vec![];
    if claude {
        out.push(FoundSession {
            source: Source::Cloud,
            agent: "claude".into(),
            session_id: String::new(),
            title: "Claude Code on the web".into(),
            cwd: None,
            updated_at: 0,
            pid: None,
            status: Some("pick a web session to teleport".into()),
            terminal: None,
            args: vec![],
            url: None,
        });
    }
    if let Some(codex) = codex {
        for t in codex_cloud_tasks(codex) {
            out.push(FoundSession {
                source: Source::Cloud,
                agent: "codex".into(),
                session_id: t["id"].as_str().unwrap_or_default().into(),
                title: t["title"].as_str().unwrap_or("Codex cloud task").into(),
                cwd: None,
                updated_at: 0,
                pid: None,
                status: t["status"].as_str().map(String::from),
                terminal: t["environment_label"].as_str().map(String::from),
                args: vec![],
                url: t["url"].as_str().map(String::from),
            });
        }
    }
    out
}

/// `codex cloud list --json`, bounded so a slow network never stalls discovery.
fn codex_cloud_tasks(codex: &Path) -> Vec<Value> {
    let Ok(mut child) = Command::new(codex).args(["cloud", "list", "--json"]).stdout(Stdio::piped()).stderr(Stdio::null()).stdin(Stdio::null()).spawn() else {
        return vec![];
    };
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        if let Ok(Some(_)) = child.try_wait() {
            let out = child.wait_with_output().ok();
            let v = out.and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok());
            return v.and_then(|v| v["tasks"].as_array().cloned()).unwrap_or_default();
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    vec![]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_permission_and_model_flags_drops_session_flags() {
        let args: Vec<String> = ["--dangerously-skip-permissions", "--resume", "--model", "opus", "--settings", "{}", "-c"].iter().map(|s| s.to_string()).collect();
        assert_eq!(portable_flags("claude", &args), ["--dangerously-skip-permissions", "--model", "opus"]);
        let args: Vec<String> = ["--resume", "abc-123", "--permission-mode", "plan"].iter().map(|s| s.to_string()).collect();
        assert_eq!(portable_flags("claude", &args), ["--permission-mode", "plan"]);
    }
}
