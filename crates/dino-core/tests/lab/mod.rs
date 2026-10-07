//! A lab of stand-in agent processes for the found-scan tests: each a copy of fake_agent.c's
//! program under an agent's name, in a terminal of its own (a pty) or none, detached from the
//! test (its parent is launchd, as in a terminal app) or under a parent of the test's choosing,
//! with the conversation records the agent would write, in a HOME of its own.
//!
//! Every process it starts is killed when it's dropped, by its pid, and with its children.
#![allow(dead_code)]

use std::collections::HashMap;
use std::os::fd::{FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dino_core::found::{self, FoundSession};
use dino_core::procinfo;

/// The names the program is copied under: the agents', and the parents they run under.
pub const NAMES: &[&str] = &["claude", "codex", "kimi-code", "copilot", "opencode", "codewhale", "pi", "cursor-agent", "amp", "dino", "tmux", "zsh"];

/// One lab at a time: the scan's state and HOME are the process's.
static ONE: Mutex<()> = Mutex::new(());

pub struct Lab {
    _one: MutexGuard<'static, ()>,
    pub root: PathBuf,
    pub home: PathBuf,
    bin: PathBuf,
    /// The terminals' other ends: closed, their processes would get a hangup.
    masters: Vec<OwnedFd>,
    pub pids: Vec<u32>,
    pub node: Option<PathBuf>,
    /// This lab's alone, in the conversation ids it makes (`uuid`).
    run: u64,
}

/// What `spawn` starts.
#[derive(Default, Clone)]
pub struct Fake<'a> {
    pub name: &'a str,
    pub args: Vec<String>,
    pub tty: bool,
    pub cwd: Option<PathBuf>,
    pub open: Option<PathBuf>,
    pub life_ms: Option<u64>,
    /// Its child, started by it (sharing its terminal unless it has its own).
    pub child: Option<Box<Fake<'a>>>,
    /// Becomes this program (argv given whole), after its setup: a Node program.
    pub exec: Option<(PathBuf, Vec<String>)>,
}

pub fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
}

impl Lab {
    pub fn new(tag: &str) -> Lab {
        let one = ONE.lock().unwrap_or_else(|e| e.into_inner());
        let root = std::env::temp_dir().join(format!("dino-found-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let home = root.join("home");
        let bin = root.join("bin");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(root.join("work")).unwrap();
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fake_agent.c");
        let built = root.join("fake_agent");
        let ok = Command::new("cc").args(["-O2", "-o"]).arg(&built).arg(&src).status().unwrap().success();
        assert!(ok, "cc fake_agent.c");
        for n in NAMES {
            std::fs::copy(&built, bin.join(n)).unwrap();
        }
        // Each agent reads its records under this HOME, and nowhere its variables would send it.
        unsafe {
            std::env::set_var("HOME", &home);
            for v in ["CODEX_HOME", "KIMI_CODE_HOME", "COPILOT_HOME", "XDG_DATA_HOME", "XDG_CONFIG_HOME", "PI_CODING_AGENT_DIR", "PI_CODING_AGENT_SESSION_DIR", "QWEN_HOME", "CODEWHALE_HOME", "OPENCODE_DB"] {
                std::env::remove_var(v);
            }
        }
        let node = ["/opt/homebrew/bin/node", "/usr/local/bin/node"].iter().map(PathBuf::from).find(|p| p.exists());
        let run = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() as u64;
        Lab { _one: one, root, home, bin, masters: vec![], pids: vec![], node, run }
    }

    /// Conversation `n`'s id, a UUID as Codex names one, this lab's alone. Codex is found by the
    /// file its process holds open, wherever that is, so another lab's Codex on the same id (a
    /// test run beside this one, or one killed before it could clean up, its processes living on
    /// for minutes) would be on the same conversation, and listed in this one's place as the older.
    pub fn uuid(&self, n: u16) -> String {
        format!("{:08x}-{n:04x}-7a20-8efb-{:012x}", std::process::id(), self.run & 0xffff_ffff_ffff)
    }

    pub fn dir(&self, name: &str) -> PathBuf {
        let d = self.root.join("work").join(name);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A new terminal: its device, for a process to make its own.
    fn pty(&mut self) -> String {
        let (mut m, mut s) = (0, 0);
        let mut name = [0 as libc::c_char; 128];
        let r = unsafe { libc::openpty(&mut m, &mut s, name.as_mut_ptr(), std::ptr::null_mut(), std::ptr::null_mut()) };
        assert_eq!(r, 0, "openpty: {}", std::io::Error::last_os_error());
        unsafe {
            libc::close(s);
            self.masters.push(OwnedFd::from_raw_fd(m));
        }
        unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) }.to_string_lossy().into_owned()
    }

    fn env_of(&mut self, f: &Fake, prefix: &str, out: &mut Vec<(String, String)>) {
        if f.tty {
            let t = self.pty();
            out.push((format!("{prefix}FAKE_TTY"), t));
        }
        // In the lab, not wherever the test runs.
        let cwd = f.cwd.clone().unwrap_or_else(|| self.root.join("work"));
        out.push((format!("{prefix}FAKE_CWD"), cwd.display().to_string()));
        if let Some(o) = &f.open {
            out.push((format!("{prefix}FAKE_OPEN"), o.display().to_string()));
        }
        // Always one: the program does nothing without any.
        out.push((format!("{prefix}FAKE_LIFE_MS"), f.life_ms.unwrap_or(600_000).to_string()));
        if let Some((path, argv)) = &f.exec {
            out.push((format!("{prefix}FAKE_EXEC"), path.display().to_string()));
            out.push((format!("{prefix}FAKE_EXEC_ARGV"), argv.join("\x1f")));
        }
        if let Some(c) = &f.child {
            let cexe = self.bin.join(c.name);
            let mut argv = vec![cexe.display().to_string()];
            argv.extend(c.args.iter().cloned());
            out.push((format!("{prefix}FAKE_CHILD"), cexe.display().to_string()));
            out.push((format!("{prefix}FAKE_CHILD_ARGV"), argv.join("\x1f")));
            let child_prefix = format!("CHILD_{prefix}");
            self.env_of(c, &child_prefix, out);
        }
    }

    /// Starts `f`, detached from the test (its parent is launchd); its pid.
    pub fn spawn(&mut self, f: Fake) -> u32 {
        let mut env = vec![("FAKE_DETACH".to_string(), "1".to_string())];
        self.env_of(&f, "", &mut env);
        let out = Command::new(self.bin.join(f.name)).args(&f.args).envs(env).current_dir(&self.root).output().unwrap();
        let pid: u32 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or_else(|_| panic!("{} didn't start: {out:?}", f.name));
        self.pids.push(pid);
        pid
    }

    /// Waits until `pid` has `n` children (each started by it), then their pids, oldest first.
    pub fn children(&self, pid: u32, n: usize) -> Vec<u32> {
        for _ in 0..200 {
            let mut kids: Vec<_> = procinfo::processes().into_values().filter(|p| p.parent == pid).collect();
            if kids.len() >= n {
                kids.sort_by_key(|p| (p.started_us, p.pid));
                return kids.into_iter().map(|p| p.pid).collect();
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("{pid} never had {n} children");
    }

    /// Waits until `pid` has become (or still is) `name`.
    pub fn wait_named(&self, pid: u32, name: &str) {
        for _ in 0..300 {
            if procinfo::process(pid).is_some_and(|p| p.name == name) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("{pid} never became {name}");
    }

    pub fn write(&self, path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    // ---- Each agent's record of a conversation, as it writes one ----

    pub fn claude_session(&self, pid: u32, id: &str, cwd: &Path, kind: &str, started_ms: u64) -> PathBuf {
        let p = self.home.join(format!(".claude/sessions/{pid}.json"));
        let v = serde_json::json!({"pid": pid, "sessionId": id, "cwd": cwd, "startedAt": started_ms, "kind": kind, "status": "idle", "updatedAt": started_ms});
        self.write(&p, &v.to_string());
        p
    }

    /// Where Codex keeps conversation `id` (the file it holds open).
    pub fn codex_rollout(&self, id: &str) -> PathBuf {
        let p = self.home.join(format!(".codex/sessions/2026/10/04/rollout-2026-10-04T10-00-00-{id}.jsonl"));
        self.write(&p, "");
        p
    }

    pub fn qwen_session(&self, pid: u32, id: &str, cwd: &Path) {
        let v = serde_json::json!({"pid": pid, "sessionId": id, "cwd": cwd, "startedAt": now_ms(), "kind": "tui"});
        self.write(&self.home.join(format!(".qwen/sessions/{pid}.json")), &v.to_string());
    }

    /// Pi's conversation begun in `cwd` (after its process started).
    pub fn pi_session(&self, id: &str, cwd: &Path) {
        let folder: String = cwd.display().to_string().trim_start_matches('/').chars().map(|c| if matches!(c, '/' | '\\' | ':') { '-' } else { c }).collect();
        let p = self.home.join(format!(".pi/agent/sessions/--{folder}--/2026-10-04T10-00-00-000Z_{id}.jsonl"));
        let head = serde_json::json!({"type": "session", "id": id, "cwd": cwd, "timestamp": "2026-10-04T10:00:00.000Z"});
        self.write(&p, &format!("{head}\n"));
    }

    pub fn kimi_session(&self, id: &str, cwd: &Path) {
        let dir = self.home.join(format!(".kimi-code/sessions/{id}"));
        self.write(&dir.join("state.json"), &serde_json::json!({"createdAt": now_ms(), "updatedAt": now_ms(), "title": format!("kimi {id}")}).to_string());
        let line = serde_json::json!({"sessionId": id, "sessionDir": dir, "workDir": cwd});
        let index = self.home.join(".kimi-code/session_index.jsonl");
        let old = std::fs::read_to_string(&index).unwrap_or_default();
        self.write(&index, &format!("{old}{line}\n"));
    }

    pub fn copilot_session(&self, pid: u32, id: &str) {
        let dir = self.home.join(format!(".copilot/session-state/{id}"));
        self.write(&dir.join(format!("inuse.{pid}.lock")), "");
        self.write(&dir.join("events.jsonl"), "");
    }

    pub fn codewhale_session(&self, id: &str, cwd: &Path) {
        let created = iso_now();
        let v = serde_json::json!({"metadata": {"id": id, "title": format!("whale {id}"), "created_at": created, "workspace": cwd}, "messages": []});
        self.write(&self.home.join(format!(".codewhale/sessions/{id}.json")), &v.to_string());
    }

    pub fn opencode_session(&self, id: &str, cwd: &Path) {
        let db = self.home.join(".local/share/opencode/opencode.db");
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let c = rusqlite::Connection::open(&db).unwrap();
        c.execute_batch(
            "create table if not exists session (id text primary key, parent_id text, directory text not null, title text not null,
               time_created integer not null, time_updated integer not null, time_archived integer);
             create table if not exists message (id text primary key, session_id text not null, time_created integer not null, data text not null);
             create table if not exists part (id text primary key, message_id text not null, session_id text not null, data text not null);",
        )
        .unwrap();
        let now = now_ms() as i64;
        c.execute("insert into session values (?1, null, ?2, ?3, ?4, ?4, null)", rusqlite::params![id, cwd.display().to_string(), format!("opencode {id}"), now]).unwrap();
    }

    // ---- Scans ----

    /// One scan, as dinod makes it with no sessions of its own: only this lab's processes.
    pub fn scan(&self) -> Vec<FoundSession> {
        let found = found::scan(&[], &|_| false);
        // The processes listed once for all it found: once each made a scan take a second on a
        // busy Mac, and fewer scans fit in the time a test gives them.
        let procs = procinfo::processes();
        found.into_iter().filter(|f| f.pid.is_some_and(|p| self.owns(&procs, p))).collect()
    }

    /// The lab started `pid`, or one of its processes did.
    pub fn owns(&self, procs: &procinfo::Procs, pid: u32) -> bool {
        let mut at = Some(pid);
        for _ in 0..16 {
            let Some(p) = at.filter(|&p| p > 1) else { return false };
            if self.pids.contains(&p) {
                return true;
            }
            at = procs.get(&p).map(|x| x.parent);
        }
        false
    }

    /// Kills `pid` and what it started, by pid.
    pub fn kill(&mut self, pid: u32) {
        let procs = procinfo::processes();
        let mut doomed = vec![pid];
        let mut i = 0;
        while i < doomed.len() {
            let parent = doomed[i];
            doomed.extend(procs.values().filter(|p| p.parent == parent).map(|p| p.pid));
            i += 1;
        }
        for p in doomed {
            // Only a process this lab started (a pid could have been reused): its name is one of ours.
            if procs.get(&p).is_some_and(|x| NAMES.contains(&x.name.as_str()) || x.name == "node") {
                unsafe { libc::kill(p as i32, libc::SIGKILL) };
            }
        }
        self.pids.retain(|&p| p != pid);
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        for pid in self.pids.clone() {
            self.kill(pid);
        }
        // And any other process running this lab's programs (none should be).
        for p in procinfo::processes().into_keys() {
            if procinfo::exe_of(p).is_some_and(|e| Path::new(&e).starts_with(&self.bin)) {
                unsafe { libc::kill(p as i32, libc::SIGKILL) };
            }
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Now as ISO 8601, UTC.
pub fn iso_now() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

pub fn ids(found: &[FoundSession]) -> Vec<(String, String, u32)> {
    found.iter().map(|f| (f.agent.clone(), f.session_id.clone(), f.pid.unwrap_or(0))).collect()
}

pub type Ages = HashMap<u32, u64>;

impl Lab {
    /// A scan as dinod makes it, timed whole: how many of its rows are this lab's.
    pub fn scan_all(&self) -> usize {
        let shown = found::scan(&[], &|_| false);
        let procs = procinfo::processes();
        shown.iter().filter(|f| f.pid.is_some_and(|p| self.pids.contains(&p) || procs.get(&p).is_some_and(|x| self.pids.contains(&x.parent)))).count()
    }
}

/// How many processes the lab has running (each it started, and theirs).
pub fn all_processes_of(lab: &Lab) -> usize {
    let procs = procinfo::processes();
    procs.values().filter(|p| lab.pids.contains(&p.pid) || lab.pids.contains(&p.parent)).count()
}
