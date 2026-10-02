//! Agent sessions that exist outside dino: running in another terminal, recent on disk, or in the
//! cloud. dino can continue any of them (see dinod's `Adopt`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent::Agent;

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
    /// The agent's id ("claude", "codex"), see `agent`.
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
    /// Running in a tmux pane: where, so dino can show it there (tmux owns it; dino only watches).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmux: Option<TmuxPlace>,
}

/// The tmux pane an agent runs in.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct TmuxPlace {
    /// The server's socket.
    pub socket: String,
    /// The pane's id (`%3`): stays the same as windows and panes move.
    pub pane: String,
    /// `session:window.pane`, as it is now.
    pub target: String,
    /// `session:window name`, for people.
    pub label: String,
    /// A client is attached to the pane's session, so showing it moves a real tmux on screen.
    pub attached: bool,
}

pub(crate) fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

pub(crate) fn alive(pid: u32) -> bool {
    // Signal 0 only checks: EPERM still means it exists.
    let sent = unsafe { libc::kill(pid as libc::pid_t, 0) } == 0;
    sent || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// A process's terminal and launch flags never change, and finding them takes a `ps` per parent:
/// read once per process, kept while it lives.
static PROCS: Mutex<Option<HashMap<u32, (Option<String>, Vec<String>)>>> = Mutex::new(None);

pub(crate) fn terminal_and_flags(agent: &dyn Agent, pid: u32) -> (Option<String>, Vec<String>) {
    if let Some(known) = PROCS.lock().unwrap().get_or_insert_default().get(&pid) {
        return known.clone();
    }
    let found = (terminal_of(pid), agent.portable_flags(&args_of(pid)));
    PROCS.lock().unwrap().get_or_insert_default().insert(pid, found.clone());
    found
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

pub(crate) fn args_of(pid: u32) -> Vec<String> {
    run("ps", &["-o", "args=", "-p", &pid.to_string()]).map(|s| s.split_whitespace().skip(1).map(String::from).collect()).unwrap_or_default()
}

/// The flags in `args` (and their values), but `drop_with_value` with theirs and `drop_alone`:
/// what's worth carrying over when continuing a session (permissions, model, extra dirs).
pub(crate) fn drop_flags(args: &[String], drop_with_value: &[&str], drop_alone: &[&str]) -> Vec<String> {
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

/// Agent sessions running in other terminals.
pub fn running() -> Vec<FoundSession> {
    PROCS.lock().unwrap().get_or_insert_default().retain(|&pid, _| alive(pid));
    crate::agent::all().into_iter().flat_map(|a| a.running()).collect()
}

/// Cloud work of the agents `program` finds a CLI for: Claude Code web sessions (picked via
/// `--teleport`), Codex cloud tasks.
pub fn cloud(program: &dyn Fn(&str) -> Option<PathBuf>) -> Vec<FoundSession> {
    crate::agent::all().into_iter().filter_map(|a| Some(a.cloud(&program(a.id())?))).flatten().collect()
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

/// A Claude transcript's title from its last `max` bytes: the latest `ai-title`, else the last
/// prompt. Cheap on long conversations, where the title is rewritten as they go.
pub(crate) fn tail_title(path: &Path, max: u64) -> Option<String> {
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

/// Agent `agent` started by hand as process `pid`, to be filled in.
pub(crate) fn by_hand(agent: &str, pid: u32) -> FoundSession {
    FoundSession {
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
        tmux: None,
    }
}

/// An agent someone started by hand inside a dino shell: `fg` is the shell's foreground process
/// group. Its session id is empty until the agent has written one; until then (at its trust
/// prompt, before the first message) it's "starting".
pub fn inside(fg: u32) -> Option<FoundSession> {
    let table = process_table();
    let pids = subtree(&table, fg);
    for &pid in &pids {
        let comm = table.iter().find(|(p, ..)| *p == pid).map(|(.., c)| c.as_str()).unwrap_or_default();
        // Arguments cost a `ps` each, so only for the processes that may need them, and once.
        let read = std::cell::OnceCell::new();
        let args = || read.get_or_init(|| args_of(pid)).clone();
        if let Some(s) = crate::agent::all().into_iter().find_map(|a| a.inside(pid, comm, &args)) {
            return Some(s);
        }
    }
    pids.iter().find_map(|&pid| {
        let comm = table.iter().find(|(p, ..)| *p == pid).map(|(.., c)| c.as_str())?;
        let a = crate::agent::all().into_iter().find(|a| a.may_be(comm))?;
        Some(FoundSession { status: Some("starting".into()), ..by_hand(a.id(), pid) })
    })
}

/// Agents running in a terminal somewhere on this Mac that haven't written a conversation yet
/// (at a trust prompt, before the first message), so `running` can't list them: "starting". Not
/// under `roots` (dino's own sessions, which show themselves) nor `known` (listed already), and
/// only the outermost agent process of each (a wrapper and what it runs are one agent).
pub fn starting(roots: &[u32], known: &[u32]) -> Vec<FoundSession> {
    let text = run("ps", &["-A", "-o", "pid=,ppid=,tty=,comm="]).unwrap_or_default();
    let table: Vec<(u32, u32, bool, String)> = text
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let pid = it.next()?.parse().ok()?;
            let ppid = it.next()?.parse().ok()?;
            let tty = it.next()? != "??";
            Some((pid, ppid, tty, it.collect::<Vec<_>>().join(" ")))
        })
        .collect();
    let agents = crate::agent::all();
    let agent_of = |comm: &str| agents.iter().find(|a| a.may_be(comm));
    let parent = |pid: u32| table.iter().find(|(p, ..)| *p == pid).map(|(_, pp, ..)| *pp);
    let ancestors = |pid: u32| {
        let mut out = vec![];
        let mut at = parent(pid);
        while let Some(p) = at.filter(|&p| p > 1 && out.len() < 64) {
            out.push(p);
            at = parent(p);
        }
        out
    };
    let mut out = vec![];
    for (pid, _, tty, comm) in &table {
        let Some(agent) = agent_of(comm) else { continue };
        let up = ancestors(*pid);
        let inside_agent = up.iter().any(|a| table.iter().any(|(p, _, _, c)| p == a && agent_of(c).is_some()));
        // dino runs an agent as its session's own process, or under the session's shell.
        if !tty || known.contains(pid) || roots.contains(pid) || inside_agent || up.iter().any(|a| roots.contains(a) || known.contains(a)) {
            continue;
        }
        // An agent process another one already lists (its native child) counts as listed.
        if table.iter().any(|(p, _, _, _)| known.contains(p) && ancestors(*p).contains(pid)) {
            continue;
        }
        // Where it runs (an app, tmux) and with which flags, as for one with a conversation.
        let (terminal, args) = terminal_and_flags(*agent, *pid);
        out.push(FoundSession { status: Some("starting".into()), terminal, args, ..by_hand(agent.id(), *pid) });
    }
    out
}

/// The agent's own question on screen, waiting for the user: a permission or trust dialog.
/// Each says so in words it doesn't use otherwise; an agent dino can't read this way says nothing.
pub fn asking(agent: &str, screen: &str) -> bool {
    match agent.trim_end_matches("-free") {
        // Claude Code's permission and trust dialogs each end in "Esc to cancel".
        "claude" => screen.contains("Esc to cancel"),
        // Codex's approvals ("Would you like to run the following command?", "…make the following
        // edits?", …) all offer this way out; its folder trust has its own.
        "codex" => {
            (screen.contains("Would you like to ") && screen.contains("No, and tell Codex what to do differently"))
                || (screen.contains("Trust this folder?") && screen.contains("Trust and continue"))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialogs_waiting_on_the_user() {
        assert!(asking("claude", "Do you want to proceed?\n ❯ 1. Yes\n   2. No\n\n Esc to cancel"));
        assert!(!asking("claude", "> write a poem\n  esc to interrupt"));
        // Codex 0.159's approval and trust dialogs, as it draws them.
        let approval = "  Would you like to run the following command?\n\n  $ touch hello.txt\n\n› 1. Yes, proceed (y)\n  2. Yes, and don't ask again for this command in this session\n  3. No, and tell Codex what to do differently (esc)";
        assert!(asking("codex", approval));
        assert!(asking("codex", "  Trust this folder? Codex can read, edit, and run files here.\n› 1. Trust and continue\n  2. Back"));
        assert!(!asking("codex", "› Ask Codex to do anything\n  Would you like to know more?"));
        assert!(!asking("qwen", approval), "an agent dino can't read this way says nothing");
    }

    #[test]
    fn subtree_walks_children_in_order() {
        let t: Vec<(u32, u32, String)> = vec![(10, 1, "zsh".into()), (11, 10, "node".into()), (12, 11, "codex".into()), (13, 1, "other".into()), (14, 12, "rg".into())];
        assert_eq!(subtree(&t, 11), [11, 12, 14]);
        assert_eq!(subtree(&t, 13), [13]);
    }

    #[test]
    fn recognizes_agent_processes() {
        let codex = crate::agent::agent("codex").unwrap();
        assert!(codex.may_be("/opt/homebrew/bin/codex"));
        assert!(codex.may_be("/x/vendor/codex-aarch64-apple-darwin"));
        assert!(!codex.may_be("/bin/zsh"));
        assert!(!codex.may_be("vim"));
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
        let claude = crate::agent::agent("claude").unwrap();
        assert_eq!(claude.portable_flags(&args), ["--dangerously-skip-permissions", "--model", "opus"]);
        let args: Vec<String> = ["--resume", "abc-123", "--permission-mode", "plan"].iter().map(|s| s.to_string()).collect();
        assert_eq!(claude.portable_flags(&args), ["--permission-mode", "plan"]);
    }
}
