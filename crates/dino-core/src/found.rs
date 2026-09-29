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

/// Every process as (pid, parent, command path).
fn process_table() -> Vec<(u32, u32, String)> {
    let text = run("ps", &["-A", "-o", "pid=,ppid=,comm="]).unwrap_or_default();
    text.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let pid = it.next()?.parse().ok()?;
            let ppid = it.next()?.parse().ok()?;
            Some((pid, ppid, it.collect::<Vec<_>>().join(" ")))
        })
        .collect()
}

/// `root` and everything under it, parents before children.
fn subtree(table: &[(u32, u32, String)], root: u32) -> Vec<u32> {
    let mut out = vec![root];
    let mut i = 0;
    while i < out.len() && out.len() < 64 {
        let parent = out[i];
        out.extend(table.iter().filter(|(pid, ppid, _)| *ppid == parent && *pid != parent).map(|(pid, ..)| *pid));
        i += 1;
    }
    out
}

/// Which agent CLI a process is, from its command and arguments. Claude is found by its session
/// file instead: its native binary is named after its version.
fn agent_of(comm: &str, args: &[String]) -> Option<&'static str> {
    let base = |s: &str| s.rsplit('/').next().unwrap_or(s).to_string();
    let name = base(comm);
    if name == "codex" || name.starts_with("codex-") {
        return Some("codex");
    }
    if name == "gemini" || (name == "node" && args.first().is_some_and(|a| base(a) == "gemini" || a.contains("gemini-cli"))) {
        return Some("gemini");
    }
    None
}

/// The live `~/.claude/sessions/<pid>.json` of an interactive Claude, if `pid` is one.
fn claude_live(pid: u32) -> Option<Value> {
    let text = std::fs::read_to_string(home().join(format!(".claude/sessions/{pid}.json"))).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    (v["pid"].as_u64() == Some(pid as u64) && v["kind"].as_str().is_none_or(|k| k == "interactive")).then_some(v)
}

/// A Claude transcript's title from its last `max` bytes: the latest `ai-title`, else the last
/// prompt. Cheap on long conversations, where the title is rewritten as they go.
fn tail_title(path: &Path, max: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(max))).ok()?;
    let mut buf = vec![];
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    let (mut title, mut prompt) = (None, None);
    for line in text.lines() {
        if line.contains("\"type\":\"ai-title\"") {
            title = serde_json::from_str::<Value>(line).ok().and_then(|v| v["aiTitle"].as_str().map(String::from)).or(title);
        } else if line.contains("\"type\":\"last-prompt\"") {
            prompt = serde_json::from_str::<Value>(line).ok().and_then(|v| v["lastPrompt"].as_str().map(String::from)).or(prompt);
        }
    }
    title.or(prompt.map(|p| p.chars().take(60).collect()))
}

fn claude_transcript(session_id: &str) -> Option<PathBuf> {
    std::fs::read_dir(home().join(".claude/projects"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|d| d.path().join(format!("{session_id}.jsonl")))
        .find(|p| p.exists())
}

/// An agent someone started by hand inside a dino shell: `fg` is the shell's foreground process
/// group. Its session id is empty until the agent has written one (Gemini never names it).
pub fn inside(fg: u32) -> Option<FoundSession> {
    let table = process_table();
    let found = |agent: &str, pid: u32| FoundSession {
        source: Source::Running,
        agent: agent.into(),
        session_id: String::new(),
        title: String::new(),
        cwd: None,
        updated_at: 0,
        pid: Some(pid),
        status: None,
        terminal: Some("dino".into()),
        args: vec![],
        url: None,
    };
    for pid in subtree(&table, fg) {
        if let Some(v) = claude_live(pid) {
            let mut s = found("claude", pid);
            s.session_id = v["sessionId"].as_str().unwrap_or_default().into();
            s.title = claude_transcript(&s.session_id)
                .and_then(|p| tail_title(&p, 512 * 1024))
                .or_else(|| v["name"].as_str().map(String::from))
                .unwrap_or_else(|| "Claude Code".into());
            s.cwd = v["cwd"].as_str().map(String::from);
            s.updated_at = v["updatedAt"].as_u64().map_or(0, |ms| ms / 1000);
            s.status = v["status"].as_str().map(String::from);
            s.args = portable_flags("claude", &args_of(pid));
            return Some(s);
        }
        let comm = table.iter().find(|(p, ..)| *p == pid).map(|(.., c)| c.as_str()).unwrap_or_default();
        // Arguments cost a `ps` each, so only for the processes that may need them.
        let args = if comm.ends_with("node") || agent_of(comm, &[]).is_some() { args_of(pid) } else { vec![] };
        match agent_of(comm, &args) {
            Some("codex") => {
                let mut s = found("codex", pid);
                s.title = "Codex".into();
                let files = run("lsof", &["-p", &pid.to_string(), "-Fn"]).unwrap_or_default();
                let names: Vec<&str> = files.lines().filter_map(|l| l.strip_prefix('n')).collect();
                if let Some(rollout) = names.iter().find(|f| f.contains("/.codex/sessions/") && f.ends_with(".jsonl")) {
                    s.session_id = crate::history::rollout_id(Path::new(rollout)).unwrap_or_default();
                    s.updated_at = mtime(Path::new(rollout));
                    if let Some(n) = crate::history::codex_titles().remove(&s.session_id) {
                        s.title = n;
                    }
                }
                s.cwd = run("lsof", &["-a", "-p", &pid.to_string(), "-d", "cwd", "-Fn"])
                    .and_then(|t| t.lines().find_map(|l| l.strip_prefix('n').map(String::from)));
                s.args = portable_flags("codex", &args);
                return Some(s);
            }
            Some(agent) => {
                let mut s = found(agent, pid);
                s.title = "Gemini CLI".into();
                return Some(s);
            }
            None => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subtree_walks_children_in_order() {
        let t: Vec<(u32, u32, String)> = vec![(10, 1, "zsh".into()), (11, 10, "node".into()), (12, 11, "codex".into()), (13, 1, "other".into()), (14, 12, "rg".into())];
        assert_eq!(subtree(&t, 11), [11, 12, 14]);
        assert_eq!(subtree(&t, 13), [13]);
    }

    #[test]
    fn recognizes_agent_processes() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(agent_of("/opt/homebrew/bin/codex", &[]), Some("codex"));
        assert_eq!(agent_of("/x/vendor/codex-aarch64-apple-darwin", &[]), Some("codex"));
        assert_eq!(agent_of("node", &a(&["/opt/homebrew/bin/gemini", "-m", "x"])), Some("gemini"));
        assert_eq!(agent_of("node", &a(&["/x/node_modules/@google/gemini-cli/dist/index.js"])), Some("gemini"));
        assert_eq!(agent_of("node", &a(&["server.js", "gemini"])), None);
        assert_eq!(agent_of("/bin/zsh", &[]), None);
        assert_eq!(agent_of("vim", &a(&["codex.md"])), None);
    }

    #[test]
    fn tail_title_prefers_latest_ai_title() {
        let dir = std::env::temp_dir().join(format!("dino-tail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t.jsonl");
        let filler = format!("{{\"type\":\"user\",\"x\":\"{}\"}}\n", "a".repeat(2000));
        let body = format!(
            "{{\"type\":\"ai-title\",\"aiTitle\":\"Old\"}}\n{filler}{{\"type\":\"last-prompt\",\"lastPrompt\":\"fix the build\"}}\n{{\"type\":\"ai-title\",\"aiTitle\":\"New title\"}}\n{filler}"
        );
        std::fs::write(&p, &body).unwrap();
        assert_eq!(tail_title(&p, 1 << 20).as_deref(), Some("New title"));
        // Only the tail is read: the titles are out of reach, and a cut first line is skipped.
        assert_eq!(tail_title(&p, 1500), None);
        std::fs::write(&p, "{\"type\":\"last-prompt\",\"lastPrompt\":\"fix the build\"}\n").unwrap();
        assert_eq!(tail_title(&p, 1 << 20).as_deref(), Some("fix the build"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn keeps_permission_and_model_flags_drops_session_flags() {
        let args: Vec<String> = ["--dangerously-skip-permissions", "--resume", "--model", "opus", "--settings", "{}", "-c"].iter().map(|s| s.to_string()).collect();
        assert_eq!(portable_flags("claude", &args), ["--dangerously-skip-permissions", "--model", "opus"]);
        let args: Vec<String> = ["--resume", "abc-123", "--permission-mode", "plan"].iter().map(|s| s.to_string()).collect();
        assert_eq!(portable_flags("claude", &args), ["--permission-mode", "plan"]);
    }
}
