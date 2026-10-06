//! Agent sessions that exist outside dino: running in another terminal, recent on disk, or in the
//! cloud. dino can continue any of them (see dinod's `Adopt`).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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

/// Process `p` lives and is the one a record written at `started_ms` (milliseconds since the
/// epoch) speaks of: it had started by then. A record left behind by one that crashed names a pid
/// the system may since have given another process, started later. No time: alive is enough.
pub(crate) fn started_before(p: Option<&crate::procinfo::Proc>, started_ms: &Value) -> bool {
    let Some(p) = p else { return false };
    started_ms.as_u64().is_none_or(|ms| p.started_us / 1000 <= ms + 2000)
}

/// A process's terminal and launch flags never change: read once per process (its pid and
/// start), kept while it lives.
static PROCS: Mutex<Option<HashMap<u32, (Option<u64>, Option<String>, Vec<String>)>>> = Mutex::new(None);

pub(crate) fn terminal_and_flags(agent: &dyn Agent, pid: u32) -> (Option<String>, Vec<String>) {
    let started = crate::procinfo::process(pid).map(|p| p.started_us);
    if let Some((at, terminal, flags)) = PROCS.lock().unwrap().get_or_insert_default().get(&pid) {
        if *at == started {
            return (terminal.clone(), flags.clone());
        }
    }
    let found = (terminal_of(pid), agent.portable_flags(&args_of(pid)));
    PROCS.lock().unwrap().get_or_insert_default().insert(pid, (started, found.0.clone(), found.1.clone()));
    found
}

/// The GUI app (or multiplexer) a process lives in, found by walking up its parents.
pub fn terminal_of(pid: u32) -> Option<String> {
    let mut p = pid;
    for _ in 0..12 {
        // Another user's program (`login`'s) can't be read: passed on the way up.
        let path = crate::procinfo::exe_of(p).unwrap_or_default();
        if let Some(i) = path.find(".app/") {
            let app = path[..i].rsplit('/').next().unwrap_or(&path);
            return Some(match app {
                "iTerm" | "iTerm2" => "iTerm2".into(),
                "Code" | "Visual Studio Code" => "VS Code".into(),
                other => other.to_string(),
            });
        }
        if path.rsplit('/').next() == Some("tmux") {
            return Some("tmux".into());
        }
        p = crate::procinfo::parent_of(p)?;
        if p <= 1 {
            return None;
        }
    }
    None
}

/// The arguments a process was started with, after its program.
pub(crate) fn args_of(pid: u32) -> Vec<String> {
    crate::procinfo::args_and_env(pid).map(|(args, _)| args.into_iter().skip(1).collect()).unwrap_or_default()
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

/// Cloud work of the agents `program` finds a CLI for: Claude Code web sessions (picked via
/// `--teleport`), Codex cloud tasks.
pub fn cloud(program: &dyn Fn(&str) -> Option<PathBuf>) -> Vec<FoundSession> {
    crate::agent::all().into_iter().filter_map(|a| Some(a.cloud(&program(a.id())?))).flatten().collect()
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
    // Each process, parents first, with its command as `ps -o comm` names it (its first argument,
    // or the kernel's name for one this user can't read) and its arguments: asked of the kernel,
    // microseconds each. A `ps` of every process cost tens of milliseconds at each new foreground.
    let procs: Vec<(u32, String, Vec<String>)> = crate::procinfo::tree(fg)
        .into_iter()
        .take(64)
        .map(|(pid, _)| match crate::procinfo::args_and_env(pid) {
            Some((mut args, _)) if !args.is_empty() => (pid, args.remove(0).trim_end().to_string(), args),
            _ => (pid, crate::procinfo::name(pid).unwrap_or_default(), vec![]),
        })
        .collect();
    for (pid, comm, args) in &procs {
        if let Some(s) = crate::agent::all().into_iter().find_map(|a| a.inside(*pid, comm, &|| args.clone())) {
            return Some(s);
        }
    }
    procs.iter().find_map(|(pid, comm, _)| {
        let a = crate::agent::all().into_iter().find(|a| a.may_be(comm))?;
        Some(FoundSession { status: Some("starting".into()), ..by_hand(a.id(), *pid) })
    })
}

/// How old an agent's process must be to be listed: one just started may be gone in a moment
/// (a one-shot run, a helper), and listing it would make the list flicker.
pub const MIN_AGE: Duration = Duration::from_secs(3);
/// How long a listed agent stays listed once a scan no longer finds it while its process lives
/// on (a conversation file caught mid-write, a moment without its file open): no blinking out.
pub const LINGER: Duration = Duration::from_secs(3);
/// How long an agent with no conversation yet counts as starting.
const STARTING_FOR: Duration = Duration::from_secs(120);

/// An agent process scans have found, by pid.
struct Tracked {
    started_us: u64,
    /// The last scan that found it, and how many scans in a row did.
    last_scan: u64,
    streak: u32,
    /// What was listed for it, and when it was last found.
    shown: Option<(FoundSession, Instant)>,
}

#[derive(Default)]
struct Scanner {
    scans: u64,
    tracked: HashMap<u32, Tracked>,
}

/// Held for a whole scan: one at a time, so each sees the one before it.
static SCANNER: Mutex<Option<Scanner>> = Mutex::new(None);

/// Interpreters an agent may run under (Pi, Qwen, Cursor are Node programs): the process's
/// title, if it set one, says which program it is.
fn interpreter(name: &str) -> bool {
    ["node", "bun", "deno", "ruby"].contains(&name) || name.starts_with("python")
}

/// The agents' processes among `procs`, worked out once per scan.
struct Kinds<'a> {
    procs: &'a HashMap<u32, crate::procinfo::Proc>,
    agents: [&'static dyn Agent; 11],
    memo: HashMap<u32, Option<&'static dyn Agent>>,
}

impl Kinds<'_> {
    /// The agent process `pid` is, by its program, or the title a Node program gave itself.
    fn of(&mut self, pid: u32) -> Option<&'static dyn Agent> {
        if let Some(k) = self.memo.get(&pid) {
            return *k;
        }
        let name = self.procs.get(&pid).map(|p| p.name.clone()).unwrap_or_default();
        let path = crate::procinfo::exe_of(pid).unwrap_or(name);
        let file = path.rsplit('/').next().unwrap_or(&path);
        let kind = if interpreter(file) {
            let title = crate::procinfo::args_and_env(pid).and_then(|(a, _)| a.into_iter().next()).unwrap_or_default();
            let title = title.trim_end().rsplit('/').next().unwrap_or_default().to_string();
            if interpreter(&title) { None } else { self.agents.into_iter().find(|a| a.may_be(&title)) }
        } else {
            self.agents.into_iter().find(|a| a.may_be(&path))
        };
        self.memo.insert(pid, kind);
        kind
    }

    /// `pid`'s parents, nearest first, up to launchd.
    fn ancestors(&self, pid: u32) -> Vec<u32> {
        let mut out = vec![];
        let mut at = self.procs.get(&pid).map(|p| p.parent);
        while let Some(p) = at.filter(|&p| p > 1 && out.len() < 64 && !out.contains(&p)) {
            out.push(p);
            at = self.procs.get(&p).map(|p| p.parent);
        }
        out
    }
}

fn now_us() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_micros() as u64)
}

/// The agents running on this Mac that a person could take over into dino: each in a terminal
/// (it has one: not a headless run, a server, or a background job), not a one-shot (`-p`,
/// `exec`), not some agent's helper or child, not owned by a dinod (this one's sessions list
/// themselves; another's are that one's), not under `roots`, and not what `skip` says (dino's
/// own conversations). Older than [`MIN_AGE`] and found by two scans in a row before it's
/// listed (the first scan lists what's there); once listed, it lingers [`LINGER`] after it's no
/// longer found, while its process lives. One per conversation, newest process first: an order
/// that stays put as scans repeat.
pub fn scan(roots: &[u32], skip: &dyn Fn(&FoundSession) -> bool) -> Vec<FoundSession> {
    let mut guard = SCANNER.lock().unwrap();
    let state = guard.get_or_insert_default();
    let procs = crate::procinfo::processes();
    let now = now_us();
    let mut kinds = Kinds { procs: &procs, agents: crate::agent::all(), memo: HashMap::new() };
    PROCS.lock().unwrap().get_or_insert_default().retain(|pid, _| procs.contains_key(pid));

    // Conversations each agent finds running in a terminal, by its own records: none other is
    // listed, so none other is read.
    let in_terminals: crate::procinfo::Procs = procs.iter().filter(|(_, p)| p.tty).map(|(k, p)| (*k, p.clone())).collect();
    let mut found: Vec<FoundSession> = kinds.agents.into_iter().flat_map(|a| a.running(&in_terminals)).collect();
    let talking: HashSet<u32> = found.iter().filter_map(|f| f.pid).collect();
    // Agents in a terminal that haven't written a conversation yet (at a trust prompt, before the
    // first message): starting. Running for minutes with none isn't starting (its conversation
    // is somewhere dino doesn't look, e.g. under another HOME).
    let young: Vec<u32> = procs.values().filter(|p| p.tty && !talking.contains(&p.pid) && now.saturating_sub(p.started_us) < STARTING_FOR.as_micros() as u64).map(|p| p.pid).collect();
    // A wrapper whose native child has the conversation is that one.
    let wrappers: HashSet<u32> = talking.iter().flat_map(|&t| kinds.ancestors(t)).collect();
    for pid in young {
        if wrappers.contains(&pid) {
            continue;
        }
        let Some(agent) = kinds.of(pid) else { continue };
        // Only the outermost agent process of each is one.
        if kinds.ancestors(pid).into_iter().any(|a| kinds.of(a).is_some()) {
            continue;
        }
        let (terminal, args) = terminal_and_flags(agent, pid);
        found.push(FoundSession { status: Some("starting".into()), terminal, args, ..by_hand(agent.id(), pid) });
    }

    let mut kept = vec![];
    for f in found {
        let Some(pid) = f.pid else { continue };
        let Some(p) = procs.get(&pid) else { continue };
        if !p.tty || now.saturating_sub(p.started_us) < MIN_AGE.as_micros() as u64 || skip(&f) {
            continue;
        }
        let Some(agent) = crate::agent::agent(&f.agent) else { continue };
        if agent.headless(&args_of(pid)) {
            continue;
        }
        let up = kinds.ancestors(pid);
        if up.iter().any(|a| roots.contains(a)) || crate::procinfo::under_a_dinod(&procs, pid) {
            continue;
        }
        // Some agent's helper or child (a subagent, a worker it started): that agent is the one
        // to take over. A launcher of the same agent that has no conversation of its own (a
        // Node wrapper of a native binary) is only its way in.
        let base = f.agent.trim_end_matches("-free");
        if up.iter().any(|&a| talking.contains(&a) || kinds.of(a).is_some_and(|k| k.id() != base)) {
            continue;
        }
        kept.push((p.started_us, f));
    }

    // Debounced: listed once found by two scans in a row; then kept while found.
    state.scans += 1;
    let scan_no = state.scans;
    let at = Instant::now();
    let mut out = vec![];
    for (started_us, f) in kept {
        let pid = f.pid.unwrap_or_default();
        let t = state.tracked.entry(pid).or_insert(Tracked { started_us, last_scan: 0, streak: 0, shown: None });
        if t.started_us != started_us {
            *t = Tracked { started_us, last_scan: 0, streak: 0, shown: None };
        }
        if t.last_scan == scan_no {
            continue;
        }
        t.streak = if t.last_scan + 1 == scan_no { t.streak + 1 } else { 1 };
        t.last_scan = scan_no;
        if t.streak >= 2 || scan_no == 1 || t.shown.is_some() {
            t.shown = Some((f.clone(), at));
            out.push((started_us, f));
        }
    }
    // Listed before and not found now: kept a moment while its process lives.
    state.tracked.retain(|pid, t| {
        if t.last_scan == scan_no {
            return true;
        }
        let same = procs.get(pid).is_some_and(|p| p.started_us == t.started_us);
        match &t.shown {
            Some((f, seen)) if same && seen.elapsed() < LINGER && !skip(f) => {
                out.push((t.started_us, f.clone()));
                true
            }
            _ => false,
        }
    });

    // One row per conversation: its oldest process (the one the others belong to).
    out.sort_by_key(|(started, f)| (*started, f.pid));
    let mut seen = HashSet::new();
    out.retain(|(_, f)| f.session_id.is_empty() || seen.insert((f.agent.clone(), f.session_id.clone())));
    // Newest first, and the same order every scan.
    out.sort_by_key(|(started, f)| std::cmp::Reverse((*started, f.pid)));
    out.into_iter().map(|(_, f)| f).collect()
}

/// Every conversation an agent is running on this Mac, listed or not (dino's own, another
/// dinod's, headless ones): none of them is a finished one.
pub fn live() -> Vec<FoundSession> {
    let procs = crate::procinfo::processes();
    crate::agent::all().into_iter().flat_map(|a| a.running(&procs)).collect()
}

/// The agent's own question on screen, waiting for the user: a permission or trust dialog (its
/// `asking`); an agent dino can't read this way says nothing.
pub fn asking(agent: &str, screen: &str) -> bool {
    crate::agent::agent(agent.trim_end_matches("-free")).and_then(|a| a.asking(screen)).is_some()
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

    /// An agent under the foreground (a wrapper's child) is found by its command, asked of the
    /// kernel: here a stand-in named `amp`, started by a shell that waits for it.
    #[test]
    fn inside_finds_an_agent_under_the_foreground() {
        let dir = std::env::temp_dir().join(format!("dino-inside-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let amp = dir.join("amp");
        let _ = std::fs::remove_file(&amp);
        std::os::unix::fs::symlink("/usr/bin/tail", &amp).unwrap();
        let mut sh = Command::new("/bin/sh").arg("-c").arg(format!("{} -f /dev/null; true", amp.display())).spawn().unwrap();
        let fg = sh.id();
        let since = Instant::now();
        let mut found = None;
        while found.is_none() && since.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(20));
            found = inside(fg);
        }
        for kid in crate::procinfo::children_of(fg) {
            unsafe { libc::kill(kid as i32, libc::SIGKILL) };
        }
        let _ = sh.kill();
        let _ = sh.wait();
        let _ = std::fs::remove_dir_all(&dir);
        let f = found.expect("the stand-in is found");
        assert_eq!(f.agent, "amp");
        assert_ne!(f.pid, Some(fg), "the shell's child, not the shell");
        assert_eq!(f.args, ["-f", "/dev/null"], "its arguments, read of the kernel");
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
