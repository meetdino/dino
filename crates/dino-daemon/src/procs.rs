//! A session's processes, wherever they went, and what's left of sessions that are gone.
//!
//! Its terminal holds only some of them: agents start background commands in terminal sessions of
//! their own (Node's `detached`), which launchd takes in once the agent that started them exits,
//! and nothing walking down from the agent finds them then. So every process a session starts
//! carries its tag in its environment ([`TAG`]), which the kernel keeps as it started and dinod
//! reads back ([`Look`]). macOS hides the environment of its own programs (`/bin/zsh`, `/bin/sh`;
//! with System Integrity Protection off it shows them too), so with each tagged process goes the
//! rest of its terminal session and everything under it.
//!
//! A session's processes stop with it (closed, deleted, archived, or not resumed after dinod
//! restarts), as its terminal's do: hangup, then SIGTERM, then SIGKILL. While it lives they're
//! its own, whatever its agent did: a dev server keeps serving. When its agent's process is
//! replaced (dinod restarted, new controls), the builds the old one started are stopped: their
//! output was for a process that's gone, and the agent builds again when it needs to (a Cargo
//! build left running would hold the lock the new one waits on). Anything else stays with it.
//!
//! What dinod never stops for a session, though it carries the tag: a tmux (or screen…) server and
//! what runs in it, which outlives a terminal tab by design; an app launched from it; and dinod.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use dino_core::ipc::Leftover;
use dino_core::procinfo::{self, Procs};

/// The variable that says which session of which dino a process is: `<session id> <dino's folder>`.
pub(crate) const TAG: &str = "DINO_SESSION_TAG";

/// The tag of session `id` of the dino whose folder is `home` (its dinod's `Daemon::home`).
pub(crate) fn tag(home: &Path, id: &str) -> String {
    format!("{id} {}", home.display())
}

/// Programs that build: a process with one of these names is a build at work.
const BUILD_TOOLS: &[&str] = &[
    "cargo",
    "rustc",
    "rustdoc",
    "clippy-driver",
    "swift-build",
    "swift-test",
    "swift-frontend",
    "swift-driver",
    "swiftc",
    "xcodebuild",
    "make",
    "gmake",
    "ninja",
    "cmake",
    "clang",
    "clang++",
    "cc",
    "c++",
    "gcc",
    "g++",
    "ld",
    "go",
    "gradle",
    "bazel",
    "javac",
];

pub(crate) fn is_build(name: &str) -> bool {
    BUILD_TOOLS.contains(&name)
}

/// Terminal multiplexers: their server outlives the terminal that started it, on purpose.
const MULTIPLEXERS: &[&str] = &["tmux", "screen", "zellij", "tmate", "abduco", "dtach"];

/// What a process's environment says about it.
#[derive(Clone, Debug, PartialEq)]
enum Env {
    /// macOS doesn't show it (a program of its own), or it couldn't be read.
    Hidden,
    Untagged,
    Tagged(String),
}

/// Environments read so far, by pid, start time and name: an environment never changes, and a
/// process that execs another program has a new name, and maybe one that can be read.
static READ: Mutex<Option<HashMap<EnvKey, Env>>> = Mutex::new(None);
/// A process's pid, start time and name.
type EnvKey = (u32, u64, String);

/// Every process this user can see, and what each one's environment says, for the dino whose
/// folder is `home`: its sessions are those whose tags name it.
pub(crate) struct Look {
    pub procs: Procs,
    home: String,
    env: HashMap<u32, Env>,
    sids: HashMap<u32, u32>,
}

/// Look at every process now, for the dino whose folder is `home`. A few milliseconds: only
/// processes not seen before are read.
pub(crate) fn look(home: &Path) -> Look {
    let procs = procinfo::processes();
    let mut read = READ.lock().unwrap();
    let read = read.get_or_insert_with(HashMap::new);
    let mut env = HashMap::with_capacity(procs.len());
    for p in procs.values() {
        let key = (p.pid, p.started_us, p.name.clone());
        let e = read.entry(key).or_insert_with(|| env_of(p.pid));
        env.insert(p.pid, e.clone());
    }
    read.retain(|(pid, started, name), _| procs.get(pid).is_some_and(|p| p.started_us == *started && p.name == *name));
    Look { procs, home: home.display().to_string(), env, sids: HashMap::new() }
}

fn env_of(pid: u32) -> Env {
    match procinfo::args_and_env(pid) {
        Some((_, env)) if !env.is_empty() => env.iter().find_map(|e| e.strip_prefix(TAG).and_then(|r| r.strip_prefix('='))).map_or(Env::Untagged, |t| Env::Tagged(t.to_string())),
        _ => Env::Hidden,
    }
}

impl Look {
    fn tag_of(&self, pid: u32) -> Option<(&str, &str)> {
        match self.env.get(&pid)? {
            Env::Tagged(t) => t.split_once(' '),
            _ => None,
        }
    }

    fn sid(&mut self, pid: u32) -> Option<u32> {
        if let std::collections::hash_map::Entry::Vacant(e) = self.sids.entry(pid) {
            let sid = procinfo::session_of(pid)?;
            e.insert(sid);
        }
        self.sids.get(&pid).copied()
    }

    fn children(&self) -> HashMap<u32, Vec<u32>> {
        let mut kids: HashMap<u32, Vec<u32>> = HashMap::new();
        for p in self.procs.values() {
            kids.entry(p.parent).or_default().push(p.pid);
        }
        kids
    }

    /// Never stopped for a session, though it may carry its tag (see the module's notes): dinod
    /// itself, another dinod and what runs under it (a test dino an agent started is stopped,
    /// with what it runs), what runs in a terminal multiplexer, and an app launched from it.
    fn kept_apart(&self, pid: u32) -> bool {
        let me = std::process::id();
        let mut at = pid;
        for depth in 0..64 {
            let Some(p) = self.procs.get(&at) else { return false };
            if at == me {
                // Under this dinod: one of its sessions' own.
                return depth == 0;
            }
            if MULTIPLEXERS.contains(&p.name.as_str()) {
                return true;
            }
            if procinfo::is_dinod(at, &p.name) && (depth > 0 || dinod_home(at).is_none_or(|h| h == self.home)) {
                return true;
            }
            if p.parent == 1 && procinfo::exe_of(at).is_some_and(|e| is_app(&e)) {
                return true;
            }
            if p.parent <= 1 {
                return false;
            }
            at = p.parent;
        }
        false
    }

    /// Whether `p` may be session `id`'s, as far as its environment says: it carries its tag, or
    /// macOS hides what it carries. One whose environment shows no tag dropped it (`env -u`), to
    /// outlive the session, or never came from it.
    fn may_be(&self, p: u32, id: &str) -> bool {
        match self.env.get(&p) {
            Some(Env::Tagged(t)) => t.split_once(' ') == Some((id, self.home.as_str())),
            Some(Env::Untagged) => false,
            Some(Env::Hidden) | None => true,
        }
    }

    /// The processes of each of this dino's sessions that carry its tag, with what else is in
    /// their terminal sessions and everything under them, by session id.
    pub(crate) fn sessions(&mut self) -> HashMap<String, Vec<u32>> {
        let mut seeds: HashMap<String, Vec<u32>> = HashMap::new();
        for &pid in self.procs.keys() {
            if let Some((id, dir)) = self.tag_of(pid)
                && dir == self.home
                && !self.kept_apart(pid)
            {
                seeds.entry(id.to_string()).or_default().push(pid);
            }
        }
        if seeds.is_empty() {
            return HashMap::new();
        }
        let own_sid = procinfo::session_of(std::process::id());
        let kids = self.children();
        let all: Vec<u32> = self.procs.keys().copied().collect();
        let mut out = HashMap::new();
        for (id, mut set) in seeds {
            let sids: HashSet<u32> = set.iter().filter_map(|&p| self.sid(p)).filter(|&s| s > 1 && Some(s) != own_sid).collect();
            for &p in &all {
                if !set.contains(&p) && self.sid(p).is_some_and(|s| sids.contains(&s)) && self.may_be(p, &id) && !self.kept_apart(p) {
                    set.push(p);
                }
            }
            let mut i = 0;
            while i < set.len() {
                for &k in kids.get(&set[i]).map(Vec::as_slice).unwrap_or_default() {
                    if !set.contains(&k) && self.may_be(k, &id) && !self.kept_apart(k) {
                        set.push(k);
                    }
                }
                i += 1;
            }
            out.insert(id, set);
        }
        out
    }

    /// What's in terminal sessions `sids` now and may be session `id`'s.
    fn in_sessions(&mut self, sids: &HashSet<u32>, id: &str) -> Vec<u32> {
        let all: Vec<u32> = self.procs.keys().copied().collect();
        all.into_iter().filter(|&p| self.sid(p).is_some_and(|s| sids.contains(&s)) && self.may_be(p, id) && !self.kept_apart(p)).collect()
    }

    /// `pids` in groups that run together: by terminal session.
    fn groups(&mut self, pids: &[u32]) -> Vec<Vec<u32>> {
        let mut by: HashMap<u32, Vec<u32>> = HashMap::new();
        for &p in pids {
            by.entry(self.sid(p).unwrap_or(p)).or_default().push(p);
        }
        by.into_values().collect()
    }

    /// Of `pids`, the groups that are building something; `or_terminal`, and those still in a
    /// terminal (one an earlier dinod had, which nobody can type into any more).
    fn builds(&mut self, pids: &[u32], or_terminal: bool) -> Vec<u32> {
        let groups = self.groups(pids);
        groups.into_iter().filter(|g| g.iter().any(|p| self.procs.get(p).is_some_and(|p| is_build(&p.name) || (or_terminal && p.tty)))).flatten().collect()
    }
}

/// `exe` is an app's own program: the one in its bundle's Contents/MacOS, not a helper some
/// bundle carries elsewhere (Xcode's Python.app, deep in Xcode.app, is a command-line Python).
fn is_app(exe: &str) -> bool {
    exe.split_once(".app/").is_some_and(|(_, rest)| rest.starts_with("Contents/MacOS/"))
}

/// Where dinod `pid` keeps its config: its `DINO_HOME`, else the default under its `HOME`.
fn dinod_home(pid: u32) -> Option<String> {
    let (_, env) = procinfo::args_and_env(pid)?;
    let var = |k: &str| env.iter().find_map(|e| e.strip_prefix(k).and_then(|r| r.strip_prefix('=')).map(str::to_string));
    var("DINO_HOME").or_else(|| var("HOME").map(|h| format!("{h}/.config/dino")))
}

/// How long processes get to leave after the hangup, then after SIGTERM: as a terminal's (see
/// `Pane::kill`).
const HANGUP_GRACE: Duration = Duration::from_secs(1);
const TERM_GRACE: Duration = Duration::from_secs(2);

/// Stop `pids` for sure, off the caller's thread: hangup, SIGTERM, SIGKILL, each to what's still
/// there, and to what `more` finds in the look before it (a script's next command). Only the same
/// processes are signalled: a pid taken by another since isn't.
fn stop(pids: Vec<u32>, look: &Look, more: impl Fn(&mut Look) -> Vec<u32> + Send + 'static) -> Option<std::thread::JoinHandle<()>> {
    if pids.is_empty() {
        return None;
    }
    let mut known: HashMap<u32, u64> = pids.iter().filter_map(|p| Some((*p, look.procs.get(p)?.started_us))).collect();
    let home = PathBuf::from(&look.home);
    std::thread::Builder::new()
        .name("procs-stop".into())
        .spawn(move || {
            for (signal, grace) in [(libc::SIGHUP, HANGUP_GRACE), (libc::SIGTERM, TERM_GRACE), (libc::SIGKILL, Duration::from_secs(1))] {
                let mut now = self::look(&home);
                for p in more(&mut now) {
                    if let Some(q) = now.procs.get(&p) {
                        known.entry(p).or_insert(q.started_us);
                    }
                }
                let left: Vec<u32> = known.iter().filter(|(p, s)| procinfo::alive(**p, **s)).map(|(p, _)| *p).collect();
                if left.is_empty() {
                    return;
                }
                for p in &left {
                    // SAFETY: a signal to a process just seen to be the one it was.
                    unsafe { libc::kill(*p as libc::pid_t, signal) };
                }
                let since = Instant::now();
                while since.elapsed() < grace && known.iter().any(|(p, s)| procinfo::alive(*p, *s)) {
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        })
        .ok()
}

/// Stop what session `id` of the dino whose folder is `home` runs apart from its terminal: all of
/// it once the session is gone, or, `builds_only`, the builds its agent left as its process is
/// replaced. Looked at now, before anything else starts in it; stopped off the caller's thread
/// (join to wait).
pub(crate) fn stop_session(home: &Path, id: &str, builds_only: bool) -> Option<std::thread::JoinHandle<()>> {
    let mut look = look(home);
    let pids = look.sessions().remove(id).unwrap_or_default();
    let pids = if builds_only { look.builds(&pids, false) } else { pids };
    if pids.is_empty() {
        return None;
    }
    eprintln!("{} dinod: session {id}: stopping {} {}", crate::stamp(), describe(&look, &pids), if builds_only { "its agent left building" } else { "it left running" });
    let id = id.to_string();
    // What starts later (a script's next command) is looked for only in those terminal sessions:
    // what carries the tag elsewhere is another's, the agent taking its place when its process is
    // replaced, or a session started again under the same id before the last of the old one is
    // gone.
    let sids: HashSet<u32> = pids.iter().filter_map(|&p| look.sid(p)).filter(|&s| s > 1 && Some(s) != procinfo::session_of(std::process::id())).collect();
    stop(pids, &look, move |l| l.in_sessions(&sids, &id))
}

/// Session `id`'s processes apart from its terminal, now: of the dino whose folder is `home`.
pub(crate) fn of_session(home: &Path, id: &str) -> Vec<u32> {
    look(home).sessions().remove(id).unwrap_or_default()
}

/// "cargo, rustc ×6": what `pids` are, for dinod's log.
fn describe(look: &Look, pids: &[u32]) -> String {
    let mut names: Vec<(String, usize)> = vec![];
    for p in pids {
        let Some(n) = look.procs.get(p).map(|p| p.name.clone()) else { continue };
        match names.iter_mut().find(|(m, _)| *m == n) {
            Some(e) => e.1 += 1,
            None => names.push((n, 1)),
        }
    }
    names.iter().map(|(n, c)| if *c > 1 { format!("{n} ×{c}") } else { n.clone() }).collect::<Vec<_>>().join(", ")
}

/// What a build is doing, as a person would say it: "cargo test", "swift-build", "make".
pub(crate) fn label(pid: u32, name: &str) -> String {
    if name == "cargo" {
        let sub = procinfo::args_and_env(pid).and_then(|(args, _)| args.into_iter().skip(1).find(|a| !a.starts_with('-') && !a.starts_with('+')));
        if let Some(sub) = sub {
            return format!("cargo {sub}");
        }
    }
    name.to_string()
}

/// As dinod starts, before its `sessions` are restored: what the sessions before it left running.
/// A session that doesn't come back has its processes stopped; one that does keeps them, but for
/// the builds its old agent left (see the module's notes) and what's still in its old terminal
/// (after a crash, an agent that didn't leave on the hangup). Then the builds running for no
/// session at all.
pub(crate) fn settle(home: &Path, sessions: &[String], known: &[PathBuf]) -> Vec<(Leftover, u64)> {
    let mut look = look(home);
    for (id, pids) in look.sessions() {
        let back = sessions.contains(&id);
        let pids = if back { look.builds(&pids, true) } else { pids };
        if pids.is_empty() {
            continue;
        }
        eprintln!("{} dinod: session {id} {}: stopping {}", crate::stamp(), if back { "came back" } else { "didn't come back" }, describe(&look, &pids));
        let sids: HashSet<u32> = pids.iter().filter_map(|&p| look.sid(p)).collect();
        stop(pids, &look, move |l| l.in_sessions(&sids, &id));
    }
    leftovers(&look, known)
}

/// Builds running for no session: their parents gone (taken in by launchd), outside any terminal
/// multiplexer or app, and either another dino's, whose dinod isn't running, or in a folder this
/// dino's sessions use (`known`), left there before dino tagged what sessions run. One for each
/// tree, with when its top process started.
fn leftovers(look: &Look, known: &[PathBuf]) -> Vec<(Leftover, u64)> {
    let mut tops: HashMap<u32, u32> = HashMap::new();
    for p in look.procs.values().filter(|p| is_build(&p.name)) {
        // Up to launchd: from a terminal, a dinod or a tmux, it isn't left behind.
        let mut top = p.pid;
        let mut chain = vec![p.pid];
        let detached = loop {
            let Some(q) = look.procs.get(&top) else { break false };
            if q.parent == 1 {
                break true;
            }
            // Linux: orphans go to the nearest subreaper, which for a user's processes is their
            // `systemd --user`, not pid 1.
            #[cfg(target_os = "linux")]
            if look.procs.get(&q.parent).is_some_and(|r| r.name == "systemd") {
                break true;
            }
            if q.parent == 0 || chain.len() > 64 {
                break false;
            }
            top = q.parent;
            chain.push(top);
        };
        if !detached || chain.iter().any(|&c| look.procs.get(&c).is_some_and(|q| procinfo::is_dinod(c, &q.name))) || look.kept_apart(p.pid) {
            continue;
        }
        let ours = match look.tag_of(p.pid) {
            // This dino's: its session's, or stopped as it came back (see `settle`).
            Some((_, dir)) if dir == look.home => false,
            Some((_, dir)) => std::os::unix::net::UnixStream::connect(Path::new(dir).join(dino_core::ipc::SOCKET_NAME)).is_err(),
            None => procinfo::cwd_of(p.pid).is_some_and(|c| known.iter().any(|k| Path::new(&c).starts_with(k))),
        };
        if !ours {
            continue;
        }
        // The topmost build in it names it.
        let named = chain.iter().rev().copied().find(|c| look.procs.get(c).is_some_and(|q| is_build(&q.name))).unwrap_or(p.pid);
        tops.entry(top).or_insert(named);
    }
    let mut out: Vec<(Leftover, u64)> = tops
        .into_iter()
        .filter_map(|(top, named)| {
            let t = look.procs.get(&top)?;
            let n = look.procs.get(&named)?;
            let leftover = Leftover { pid: top, name: label(named, &n.name), cwd: procinfo::cwd_of(named).unwrap_or_default(), started: t.started_us / 1_000_000, ..Default::default() };
            Some((leftover, t.started_us))
        })
        .collect();
    out.sort_by_key(|(l, _)| l.started);
    out
}

/// Stop the leftover build under `pid` (see [`settle`]) that the dino whose folder is `home`
/// found, which started at `started_us`: that process and everything under it.
pub(crate) fn stop_leftover(home: &Path, pid: u32, started_us: u64) -> Option<std::thread::JoinHandle<()>> {
    let look = look(home);
    if look.procs.get(&pid).is_none_or(|p| p.started_us != started_us) {
        return None;
    }
    let under = |l: &Look, top: u32| {
        let kids = l.children();
        let mut set = vec![top];
        let mut i = 0;
        while i < set.len() {
            set.extend(kids.get(&set[i]).map(Vec::as_slice).unwrap_or_default());
            i += 1;
        }
        set
    };
    let pids = under(&look, pid);
    eprintln!("{} dinod: stopping a leftover build: {}", crate::stamp(), describe(&look, &pids));
    stop(pids, &look, move |l| if l.procs.get(&pid).is_some_and(|p| p.started_us == started_us) { under(l, pid) } else { vec![] })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;

    /// The folder of the dino these tests' sessions are of: one of their own, never the user's.
    fn home() -> PathBuf {
        std::env::temp_dir().join("dino-procs-test")
    }

    fn wait_for(what: &str, done: impl Fn() -> bool) {
        let since = Instant::now();
        while !done() {
            assert!(since.elapsed() < Duration::from_secs(10), "{what}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// A program whose environment macOS shows (its own `/bin` ones it hides): Python, the
    /// interpreter itself. `/usr/bin/python3` only finds it, with `xcrun`, which runs `xcodebuild`
    /// when it has nothing cached yet (a fresh CI runner): a build, to dinod.
    pub(crate) fn python() -> Option<&'static str> {
        static PYTHON: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
        PYTHON
            .get_or_init(|| {
                ["/usr/bin/python3", "/opt/homebrew/bin/python3"].into_iter().filter(|p| Path::new(p).exists()).find_map(|p| {
                    let out = std::process::Command::new(p).args(["-c", "import sys; print(sys.executable)"]).env_remove(TAG).output().ok()?;
                    let exe = String::from_utf8(out.stdout).ok()?.trim().to_string();
                    (out.status.success() && Path::new(&exe).is_file()).then_some(exe)
                })
            })
            .as_deref()
    }

    #[test]
    fn finds_what_a_session_started_in_a_terminal_session_of_its_own_and_stops_it() {
        let Some(py) = python() else { return };
        let id = format!("t{}", std::process::id());
        // As an agent's background command: a shell in a terminal session of its own (setsid),
        // macOS hiding its environment (with SIP on), running a program that shows it, and one
        // that doesn't.
        let script = format!("'{py}' -c 'import time; time.sleep(60)' & /bin/sleep 60 & wait");
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.args(["-c", &script]).env(TAG, tag(&home(), &id));
        // SAFETY: setsid in the child before exec.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let mut sh = cmd.spawn().unwrap();
        let shell = sh.id();
        // Both of its programs started: neither is the shell it forked from any more.
        let started = |found: &[u32]| {
            let forked = procinfo::name(shell);
            found.len() >= 3 && found.iter().all(|&p| p == shell || procinfo::name(p) != forked)
        };
        wait_for("its three processes", || started(&of_session(&home(), &id)));
        let found = of_session(&home(), &id);
        assert!(found.contains(&shell), "the shell, though its environment is hidden: {found:?}");
        assert!(found.iter().any(|&p| procinfo::name(p).as_deref() == Some("sleep")));
        // No build in it: kept as the agent's process is replaced.
        assert!(stop_session(&home(), &id, true).is_none());
        assert_eq!(of_session(&home(), &id).len(), found.len());
        stop_session(&home(), &id, false).unwrap().join().unwrap();
        let _ = sh.wait();
        assert!(found.iter().all(|&p| procinfo::process(p).is_none() || procinfo::name(p).as_deref() == Some("sh")), "all stopped");
        assert!(of_session(&home(), &id).is_empty());
    }

    /// A session started again under the same id while what the old one left is still being
    /// stopped isn't stopped with it.
    #[test]
    fn a_session_started_again_under_its_id_is_not_stopped_with_the_old_one() {
        let Some(py) = python() else { return };
        let id = format!("r{}", std::process::id());
        let tagged = |code: &str| {
            let mut cmd = std::process::Command::new(py);
            cmd.args(["-c", code]).env(TAG, tag(&home(), &id)).stdout(std::process::Stdio::piped());
            // SAFETY: setsid in the child before exec: each in a terminal session of its own, as
            // each session's terminal is.
            unsafe {
                cmd.pre_exec(|| {
                    libc::setsid();
                    Ok(())
                });
            }
            cmd.spawn().unwrap()
        };
        // The old one outlasts the hangup, so its stop goes on to SIGTERM a second later.
        let mut old = tagged("import signal, time; signal.signal(signal.SIGHUP, signal.SIG_IGN); print('ready', flush=True); time.sleep(60)");
        let mut ready = String::new();
        std::io::BufRead::read_line(&mut std::io::BufReader::new(old.stdout.take().unwrap()), &mut ready).unwrap();
        assert_eq!(ready.trim(), "ready");
        wait_for("the old one", || of_session(&home(), &id).contains(&old.id()));
        let stopping = stop_session(&home(), &id, false).unwrap();
        let mut new = tagged("import time; time.sleep(60)");
        wait_for("the new one", || of_session(&home(), &id).contains(&new.id()));
        stopping.join().unwrap();
        assert!(old.try_wait().unwrap().is_some(), "the old one stopped");
        assert!(new.try_wait().unwrap().is_none(), "the one started again runs on");
        let _ = new.kill();
        let _ = new.wait();
    }

    #[test]
    fn another_sessions_and_a_tmux_are_left_alone() {
        let Some(py) = python() else { return };
        let me = format!("a{}", std::process::id());
        let other = format!("b{}", std::process::id());
        let mut a = std::process::Command::new(py).args(["-c", "import time; time.sleep(60)"]).env(TAG, tag(&home(), &me)).spawn().unwrap();
        let mut b = std::process::Command::new(py).args(["-c", "import time; time.sleep(60)"]).env(TAG, tag(&home(), &other)).spawn().unwrap();
        // Another dino's (its own folder) and one that only says the session's id.
        let mut c = std::process::Command::new(py).args(["-c", "import time; time.sleep(60)"]).env(TAG, format!("{me} /elsewhere/dino")).spawn().unwrap();
        wait_for("both", || of_session(&home(), &me).contains(&a.id()) && of_session(&home(), &other).contains(&b.id()));
        let mine = of_session(&home(), &me);
        assert!(!mine.contains(&b.id()) && !mine.contains(&c.id()), "{mine:?}");
        for p in [&mut a, &mut b, &mut c] {
            let _ = p.kill();
            let _ = p.wait();
        }
        // A process under a tmux is kept apart, whatever it carries.
        let mut procs = Procs::new();
        let p = |pid, parent, name: &str| procinfo::Proc { pid, parent, started_us: 1, tty: false, name: name.into() };
        procs.insert(10, p(10, 1, "tmux"));
        procs.insert(11, p(11, 10, "zsh"));
        procs.insert(12, p(12, 11, "cargo"));
        procs.insert(13, p(13, 1, "cargo"));
        let look = Look { procs, home: home().display().to_string(), env: HashMap::new(), sids: HashMap::new() };
        assert!(look.kept_apart(12));
        assert!(!look.kept_apart(13));
    }

    #[test]
    fn builds_are_told_by_their_programs() {
        assert!(is_build("cargo") && is_build("rustc") && is_build("swift-frontend") && is_build("xcodebuild"));
        assert!(!is_build("node") && !is_build("python3") && !is_build("zsh"));
        assert_eq!(label(std::process::id(), "make"), "make");
        assert!(is_app("/Applications/Visual Studio Code.app/Contents/MacOS/Electron"));
        assert!(!is_app("/Applications/Xcode.app/Contents/Developer/Library/Frameworks/Python3.framework/Versions/3.9/Resources/Python.app/Contents/MacOS/Python"));
        assert!(!is_app("/opt/homebrew/bin/node"));
    }
}
