//! Codex CLI. It names its conversation only once it writes one: by the rollout file its process
//! has open (`~/.codex/sessions/…/rollout-<time>-<id>.jsonl`, under the Codex home it runs with:
//! `$CODEX_HOME`); one its shared background server runs (Codex 0.160.1) has none open, and is
//! told by what that server has loaded (see `attached`).

pub mod attached;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::{Agent, ControlKind, StatusSource, Wiring, strings};
use attached::Attached;
use crate::found::{self, FoundSession, Source};
use crate::history::{self, Turn};
use crate::models::{self, Catalog};
use crate::providers::Format;
use crate::procinfo;

pub(crate) struct Codex;

impl Codex {
    /// Codex `pid`, on the conversation `rollout` is of, as found running.
    fn on(&self, pid: u32, rollout: &Path, titles: &std::collections::HashMap<String, String>) -> Option<FoundSession> {
        let sid = history::rollout_id(rollout)?;
        let (terminal, args) = found::terminal_and_flags(self, pid);
        Some(FoundSession {
            source: Source::Running,
            agent: "codex".into(),
            title: titles.get(&sid).cloned().or_else(|| history::codex_meta(rollout).title).unwrap_or_else(|| "Codex session".into()),
            session_id: sid,
            cwd: procinfo::cwd_of(pid),
            updated_at: history::modified(rollout),
            pid: Some(pid),
            status: history::codex_status(rollout),
            terminal,
            args,
            url: None,
            tmux: None,
            unsure: None,
        })
    }
}

/// Flags that make Codex say on its terminal when it waits on the user (an approval, a
/// question), focused or not. It notices a finished turn too, which its rollout says anyway.
pub const NOTICE_ARGS: [&str; 6] =
    ["-c", "tui.notifications=true", "-c", "tui.notification_method=\"osc9\"", "-c", "tui.notification_condition=\"always\""];

/// The conversation Codex process `pid` is on: the rollout it has open. A subagent's is open too
/// while it runs; the session's own is the one that isn't a subagent's.
/// After `/fork` it keeps the original's open too (Codex 0.160): the one it's on is the one written
/// last, which the fork is from the moment it's made.
pub fn open_rollout(pid: u32) -> Option<PathBuf> {
    procinfo::open_files(pid)
        .into_iter()
        .map(PathBuf::from)
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("rollout-") && n.ends_with(".jsonl")))
        .filter(|p| !history::codex_meta(p).hidden)
        .max_by_key(|p| p.metadata().and_then(|m| m.modified()).ok())
}

/// Conversation `id`'s rollout in Codex home `home`, once it's written: looked for in the day's
/// folder its id says it began on (Codex's ids are UUIDv7s), else everywhere.
pub fn rollout_path(home: &Path, id: &str) -> Option<PathBuf> {
    match attached::begun_ms(id) {
        Some(at) => rollout_in(&home.join("sessions"), id, at / 1000),
        None => crate::transcript::codex_path_in(&home.join("sessions"), id),
    }
}

/// The Codex home process `pid` runs with (see [`home_in`]), as the kernel names its files.
pub fn home_of(pid: u32) -> Option<PathBuf> {
    let (_, env) = procinfo::args_and_env(pid)?;
    Some(home_in(&env, || procinfo::cwd_of(pid).map(PathBuf::from)))
}

/// The Codex home of a process with environment `env` (`NAME=value`), as Codex finds it: its
/// `CODEX_HOME` (one relative to its folder, `cwd`), else `.codex` in its home folder; links
/// resolved, as the kernel names the files it has open.
fn home_in(env: &[String], cwd: impl FnOnce() -> Option<PathBuf>) -> PathBuf {
    let var = |name: &str| env.iter().find_map(|kv| kv.strip_prefix(name)?.strip_prefix('=')).filter(|v| !v.is_empty()).map(PathBuf::from);
    let home = match var("CODEX_HOME") {
        Some(h) if h.is_relative() => cwd().unwrap_or_default().join(h),
        Some(h) => h,
        None => var("HOME").or_else(|| std::env::var_os("HOME").map(PathBuf::from)).unwrap_or_default().join(".codex"),
    };
    attached::resolved(&home)
}

/// `rollout_path` of conversation `id`, begun at `at`, with Codex's rollouts kept in `root`.
fn rollout_in(root: &Path, id: &str, at: u64) -> Option<PathBuf> {
    let suffix = format!("-{id}.jsonl");
    // Its folder is the day it began on this Mac's clock; a clock changed since, the day either side.
    let mut days: Vec<PathBuf> = [at, at.saturating_sub(86_400), at + 86_400].into_iter().filter_map(|t| day_dir(root, t)).collect();
    days.dedup();
    days.iter()
        .flat_map(|d| std::fs::read_dir(d).into_iter().flatten().flatten())
        .map(|e| e.path())
        .find(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("rollout-") && n.ends_with(&suffix)))
}

/// The conversation `codex resume <id>` names among `args` (after the program), options before it.
fn resumed_in(args: &[String]) -> Option<String> {
    let at = args.iter().position(|a| a == "resume")?;
    args[at + 1..].iter().find(|a| is_id(a)).cloned()
}

/// Where Codex keeps the rollouts it began on the day (this Mac's) of `t`, under `root`: `YYYY/MM/DD`.
fn day_dir(root: &Path, t: u64) -> Option<PathBuf> {
    // SAFETY: an all-zero `tm` is valid, and localtime_r writes only into it.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&(t as libc::time_t), &mut tm) }.is_null() {
        return None;
    }
    Some(root.join(format!("{:04}/{:02}/{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday)))
}

/// What the Codex processes `pids` among `procs` have of their own: the ones in a terminal (not
/// its server, a headless run…) as `attached` sees them, and the rollout each that has one open
/// has (any of `pids`: a headless one's too).
fn in_terminals(procs: &procinfo::Procs, pids: &[u32]) -> (Vec<attached::Tui>, Vec<(u32, PathBuf)>) {
    let mut tuis = vec![];
    let mut open = vec![];
    let mut held = HELD.lock().unwrap();
    let held = held.get_or_insert_default();
    held.retain(|pid, _| pids.contains(pid));
    for &pid in pids {
        let Some(p) = procs.get(&pid) else { continue };
        let (args, env) = procinfo::args_and_env(pid).unwrap_or_default();
        let args: Vec<String> = args.into_iter().skip(1).collect();
        let home = home_in(&env, || procinfo::cwd_of(pid).map(PathBuf::from));
        let sessions = attached::resolved(&home.join("sessions"));
        // The descriptor it had its conversation open as last time, if it still does: one look
        // instead of one per file it has open.
        let known = held.get(&pid).filter(|(at, fd, path)| *at == p.started_us && procinfo::open_file(pid, *fd).as_deref() == Some(path.as_str())).map(|(_, _, path)| path.clone());
        let (rollout, lock) = match known {
            Some(path) => (Some(path), None),
            None => {
                held.remove(&pid);
                let fds = procinfo::open_fds(pid);
                let rollout = fds.iter().find(|(_, f)| Path::new(f).starts_with(&sessions) && f.ends_with(".jsonl"));
                if let Some((fd, path)) = rollout {
                    held.insert(pid, (p.started_us, *fd, path.clone()));
                }
                // Run on its own (`--no-daemon`), it holds its conversation's lock from the start.
                let lock = fds.iter().find_map(|(_, f)| f.split_once("/thread-writer-locks/")?.1.strip_suffix(".lock").map(String::from));
                (rollout.map(|(_, path)| path.clone()), lock)
            }
        };
        let rollout = rollout.map(PathBuf::from);
        if let Some(r) = &rollout {
            open.push((pid, r.clone()));
        }
        if !p.tty || Codex.headless(&args) {
            continue;
        }
        let (told, picks, dir) = how_started(&args);
        let here = procinfo::cwd_of(pid).map(PathBuf::from);
        let cwd = match (dir, &here) {
            (Some(d), Some(h)) => Some(h.join(d)),
            (Some(d), None) => Some(PathBuf::from(d)),
            (None, h) => h.clone(),
        };
        tuis.push(attached::Tui {
            pid,
            home,
            started_ms: p.started_us / 1000,
            cwd: cwd.map(|c| attached::resolved(&c)),
            open: rollout.as_deref().and_then(history::rollout_id).or(lock),
            told,
            picks,
        });
    }
    (tuis, open)
}

/// What each of `tuis` is on (see `attached`): worked out among the Codexes of each Codex home,
/// with what its server has loaded, when some of them has none of its own; `claimed` being others'.
fn attach(tuis: &[attached::Tui], claimed: &[String]) -> std::collections::HashMap<u32, Attached> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
    let mut homes: std::collections::HashMap<&Path, Vec<attached::Tui>> = std::collections::HashMap::new();
    for t in tuis {
        homes.entry(&t.home).or_default().push(t.clone());
    }
    let mut out = std::collections::HashMap::new();
    for (home, tuis) in homes {
        let (loaded, up) = match tuis.iter().all(|t| t.open.is_some() || t.told.is_some()) {
            true => (vec![], None),
            false => (attached::loaded_in(home), attached::up_in(home)),
        };
        out.extend(attached::attach(&tuis, &loaded, up, claimed, now));
    }
    out
}

/// The conversation Codex process `pid`, in a terminal, is on, as sure as dino can be, when it has
/// none open of its own (Codex 0.160.1's shared server runs it): the one only it could have begun,
/// `claimed` being dino's own sessions' (see `attached`). `None` while it has begun none dino can
/// see, or dino can't tell.
pub fn attached_to(pid: u32, claimed: &[String]) -> Option<String> {
    let procs = procinfo::processes();
    let pids = procinfo::named_in(&procs, "codex");
    let (tuis, _) = in_terminals(&procs, &pids);
    match attach(&tuis, claimed).remove(&pid) {
        Some(Attached::On(id)) => Some(id),
        _ => None,
    }
}

/// What Codex's command line (`args`, after the program) says of the conversation it started on:
/// the one `codex resume <id>` names; whether it began none as it started (started to pick one,
/// `codex resume`, or to work with another server, `--remote`); the folder `-C` points it to.
fn how_started(args: &[String]) -> (Option<String>, bool, Option<String>) {
    let told = resumed_in(args);
    let forked = args.iter().position(|a| a == "fork").is_some_and(|at| args[at + 1..].iter().any(|a| attached::begun_ms(a).is_some() || is_id(a)));
    let picks = told.is_none() && !forked && super::runs_with(args, &["--remote"], &["resume", "fork"]);
    let dir = args.iter().position(|a| a == "-C" || a == "--cd").and_then(|i| args.get(i + 1).cloned()).or_else(|| args.iter().find_map(|a| a.strip_prefix("--cd=").map(String::from)));
    (told, picks, dir)
}

/// `a` looks like a conversation's id (a UUID).
fn is_id(a: &str) -> bool {
    a.len() == 36 && a.chars().filter(|&c| c == '-').count() == 4 && a.chars().all(|c| c == '-' || c.is_ascii_hexdigit())
}

/// Each Codex process's open conversation: when it started, the descriptor, the file.
static HELD: std::sync::Mutex<Option<std::collections::HashMap<u32, (u64, i32, String)>>> = std::sync::Mutex::new(None);

/// The provider's header that carries the proxy's secret, from the environment (`keyed_urls`).
fn key_header() -> String {
    format!(r#"model_providers.dino.env_http_headers={{"{}"="{}"}}"#, super::KEY_HEADER, super::KEY_ENV)
}

/// `"chatgpt"` or `"apikey"`, from `auth.json` in the Codex home the Codexes dino starts run with.
fn auth_mode() -> Option<String> {
    let auth = std::fs::read_to_string(models::codex_home().join("auth.json")).ok()?;
    // Avoid a JSON dependency for one field: find `"auth_mode": "<value>"`.
    let rest = &auth[auth.find("\"auth_mode\"")? + 11..];
    let start = rest.find('"')? + 1;
    let len = rest[start..].find('"')?;
    Some(rest[start..start + len].to_string())
}

/// `codex cloud list --json`, bounded so a slow network never stalls discovery.
fn cloud_tasks(codex: &Path) -> Vec<Value> {
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

/// Codex's command line (`codex --help`), for finding a prompt in it.
const CLI: super::Cli = super::Cli {
    value: &[
        "-c", "--config", "--enable", "--disable", "--remote", "--remote-auth-token-env", "-m", "--model", "--local-provider", "-p", "--profile", "-s",
        "--sandbox", "-C", "--cd", "--add-dir", "-a", "--ask-for-approval",
    ],
    optional: &[],
    variadic: &["-i", "--image"],
    flags: &[
        "--strict-config", "--oss", "--approve-for-me", "--dangerously-bypass-approvals-and-sandbox", "--dangerously-bypass-hook-trust", "--worktree",
        "--search", "--no-alt-screen", "--no-daemon", "--full-auto", "--yolo", "-h", "--help", "-V", "--version",
    ],
    commands: &[
        "agents", "exec", "e", "review", "login", "logout", "mcp", "plugin", "app-server", "remote-control", "app", "completion", "update", "doctor",
        "sandbox", "debug", "apply", "a", "resume", "queue", "archive", "delete", "migrate-rollouts", "unarchive", "fork", "cloud", "exec-server",
        "features", "help", "mcp-server",
    ],
};

impl Agent for Codex {
    fn id(&self) -> &'static str {
        "codex"
    }

    // `codex [options] [prompt]`.
    fn launch_prompt(&self, args: &[String]) -> Option<(Vec<String>, String)> {
        super::positional_prompt(args, &CLI)
    }

    fn answers_once(&self) -> bool {
        true
    }

    // Read-only sandbox, never asks, keeps no session: it can look but not act. Its last message
    // goes to the file, apart from its progress.
    fn one_shot(&self, ask: &super::OneShot) -> Vec<String> {
        let mut out = strings(&["exec", "--skip-git-repo-check", "--ephemeral", "-s", "read-only", "--color", "never", "-o"]);
        out.push(ask.answer.display().to_string());
        out.extend(ask.controls.iter().cloned());
        out.push(format!("{}\n\n{}", ask.instructions, ask.request));
        out
    }

    // Codex has no plan mode.
    fn modes(&self) -> &'static [&'static str] {
        &["ask", "edits", "auto", "bypass"]
    }

    // Codex's own approval presets.
    fn mode_label(&self, mode: &str) -> Option<&'static str> {
        Some(match mode {
            "ask" => "Read only",
            "edits" => "Auto",
            "auto" => "Approve for me",
            "bypass" => "Full access",
            _ => return None,
        })
    }

    fn mode_args(&self, mode: &str) -> Vec<String> {
        match mode {
            "ask" => strings(&["-s", "read-only", "-a", "on-request"]),
            "edits" => strings(&["-s", "workspace-write", "-a", "on-request"]),
            "auto" => strings(&["--approve-for-me"]),
            _ => strings(&["--dangerously-bypass-approvals-and-sandbox"]),
        }
    }

    fn model_args(&self, model: &str) -> Vec<String> {
        strings(&["-m", model])
    }

    fn effort_args(&self, effort: &str) -> Vec<String> {
        vec!["-c".into(), format!("model_reasoning_effort=\"{effort}\"")]
    }

    fn value_flags(&self) -> &'static [&'static str] {
        &["-s", "--sandbox", "-a", "--ask-for-approval", "-m", "--model", "-c", "--config"]
    }

    fn control_of(&self, name: &str, value: Option<&str>) -> Option<ControlKind> {
        match name {
            "-s" | "--sandbox" | "-a" | "--ask-for-approval" | "--approve-for-me" | "--full-auto" | "--dangerously-bypass-approvals-and-sandbox" | "--yolo" => {
                Some(ControlKind::Mode)
            }
            "-m" | "--model" => Some(ControlKind::Model),
            "-c" | "--config" if value.is_some_and(|v| v.trim_start().starts_with("model_reasoning_effort")) => Some(ControlKind::Effort),
            _ => None,
        }
    }

    // Its sandbox and approval flags name a mode only together, the way `mode_args` writes them.
    fn read_mode(&self, flags: &[(&str, Option<&str>)]) -> Option<String> {
        let (mut sandbox, mut approval, mut mode) = (None, None, None);
        for &(name, value) in flags {
            match name {
                "-s" | "--sandbox" => sandbox = value,
                "-a" | "--ask-for-approval" => approval = value,
                "--approve-for-me" => mode = Some("auto"),
                "--full-auto" => mode = Some("edits"),
                _ => mode = Some("bypass"),
            }
        }
        if sandbox.is_some() || approval.is_some() {
            mode = match (sandbox, approval, flags.len()) {
                (Some("read-only"), Some("on-request"), 2) => Some("ask"),
                (Some("workspace-write"), Some("on-request"), 2) => Some("edits"),
                _ => None,
            };
        }
        mode.map(String::from)
    }

    fn read_effort(&self, value: &str) -> Option<String> {
        value.split_once('=').map(|(_, e)| e.trim().trim_matches('"').to_string())
    }

    fn catalog_sources(&self) -> Vec<PathBuf> {
        models::codex_sources()
    }

    // No cache yet: Codex prints the same list itself.
    fn catalog(&self, program: &str) -> Option<Catalog> {
        models::codex_from_files().or_else(|| models::codex_from_program(program))
    }

    // `-c` puts the proxy's URLs on its command line.
    /// Its approvals ("Would you like to run the following command?", "…make the following
    /// edits?", …) all offer this way out; its folder trust has its own.
    fn asking(&self, screen: &str) -> Option<String> {
        let approval = screen.contains("Would you like to ") && screen.contains("No, and tell Codex what to do differently");
        let trust = screen.contains("Trust this folder?") && screen.contains("Trust and continue");
        (approval || trust).then(|| "Codex asks".into())
    }

    fn keyed_urls(&self) -> bool {
        true
    }

    // A custom provider rather than `openai_base_url`: Codex otherwise tries WebSockets first,
    // which the proxy doesn't carry. `requires_openai_auth` keeps the user's own login.
    fn wiring(&self, route: bool, base: &dyn Fn(&str) -> String, _status_line: Option<String>) -> Wiring {
        if crate::user_set(route, "OPENAI_BASE_URL") {
            return (vec![], vec![]);
        }
        let base_url = match auth_mode().as_deref() {
            Some("chatgpt") => format!("{}/codex", base("chatgpt")),
            _ => format!("{}/v1", base("openai")),
        };
        let args = [
            "model_provider=\"dino\"".to_string(),
            "model_providers.dino.name=\"dino\"".into(),
            format!("model_providers.dino.base_url=\"{base_url}\""),
            "model_providers.dino.wire_api=\"responses\"".into(),
            "model_providers.dino.requires_openai_auth=true".into(),
            "model_providers.dino.supports_websockets=false".into(),
            key_header(),
        ];
        (vec![], args.into_iter().flat_map(|a| ["-c".to_string(), a]).collect())
    }

    fn metered(&self) -> bool {
        true
    }

    fn provider_formats(&self) -> &'static [Format] {
        &[Format::Responses]
    }

    // A provider of its own for the session, named so it can't be one of Codex's built-in ones
    // (openai, ollama, lmstudio). The key is a placeholder: dino's proxy holds the real one.
    fn provider_wiring(&self, url: &str, format: Format, model: &str) -> Option<Wiring> {
        if format != Format::Responses {
            return None;
        }
        let config = [
            "model_provider=\"dino\"".to_string(),
            "model_providers.dino.name=\"dino\"".into(),
            format!("model_providers.dino.base_url=\"{url}/v1\""),
            "model_providers.dino.wire_api=\"responses\"".into(),
            "model_providers.dino.env_key=\"DINO_PROVIDER_KEY\"".into(),
            "model_providers.dino.supports_websockets=false".into(),
            key_header(),
        ];
        let mut args: Vec<String> = config.into_iter().flat_map(|a| ["-c".to_string(), a]).collect();
        args.extend(strings(&["-m", model]));
        Some((vec![("DINO_PROVIDER_KEY".into(), "dino".into())], args))
    }

    fn session_args(&self, session: &mut Option<String>, _restoring: bool) -> (Vec<String>, Vec<String>) {
        let (before, mut after) = match session {
            Some(id) => (vec!["resume".to_string()], vec![id.clone()]),
            None => (vec![], vec![]),
        };
        // So it says when it waits on the user (see dinod's `codex`); not to one its shared
        // server has open, which it takes over there only without them.
        if !session.as_deref().is_some_and(|id| self.in_shared_server(id)) {
            after.extend(NOTICE_ARGS.map(String::from));
        }
        (before, after)
    }

    // Any `-c` makes Codex 0.160.1 run on its own, not on its shared server (`codex app-server
    // --managed-daemon`); on its own it can't take a conversation that server has open: "This
    // conversation is open in another app", until the server lets it go, a minute after the last
    // Codex on it has gone (60 s by default).
    // The server of whichever Codex home has it: the one the Codexes dino starts run with, or one
    // a Codex running now runs with (one found running keeps its `CODEX_HOME` continued in dino).
    fn in_shared_server(&self, session: &str) -> bool {
        attached::in_server(&models::codex_home(), session)
            || procinfo::pids_named("codex").into_iter().filter_map(home_of).any(|h| attached::in_server(&h, session))
    }

    // Without the notices dino asks of the sessions it starts: what a person types.
    fn resume_args(&self, session: &str) -> (Vec<String>, Vec<String>) {
        (vec!["resume".into()], vec![session.into()])
    }

    // ⌃C twice at its prompt: the first says how to quit, the second quits, saying how to come
    // back. A signal leaves its screen behind (Codex 0.160.1 draws in the terminal's own screen).
    fn quit_keys(&self) -> &'static [u8] {
        b"\x03\x03"
    }

    // `codex fork <id> [prompt]` (Codex 0.160): a new conversation, its rollout naming the original
    // as `forked_from_id`; its id is known once the rollout is open. Started anywhere but the
    // original's folder it asks which folder to work in, unless told with `-C`.
    fn fork_args(&self, parent: &str, cwd: &Path, _session: &mut Option<String>) -> Option<(Vec<String>, Vec<String>)> {
        let mut after = vec!["-C".to_string(), cwd.display().to_string(), parent.to_string()];
        after.extend(NOTICE_ARGS.map(String::from));
        Some((vec!["fork".into()], after))
    }

    fn forked_from(&self, session: &str) -> Option<String> {
        history::codex_forked_from(&crate::transcript::codex_path(session)?)
    }

    fn status_source(&self) -> StatusSource {
        StatusSource::Rollout
    }

    // Its terminal's title is its folder by default (`tui.terminal_title`: spinner, project), not
    // its conversation: dinod names it from Codex's records (see `history::codex_thread_name`).
    fn shown_title(&self, _title: &str) -> Option<String> {
        None
    }

    // What each turn runs with (`turn_context`), and a change of it between turns, written as it's
    // made (`/model`: `thread_settings_applied`, Codex 0.160).
    fn log_model(&self, line: &Value) -> Option<String> {
        let p = &line["payload"];
        let model = match (line["type"].as_str()?, p["type"].as_str()) {
            ("turn_context", _) => &p["model"],
            ("event_msg", Some("thread_settings_applied")) => &p["thread_settings"]["model"],
            _ => return None,
        };
        model.as_str().filter(|m| !m.is_empty()).map(String::from)
    }

    fn conversation_of(&self, pid: u32) -> Option<String> {
        open_rollout(pid).and_then(|p| history::rollout_id(&p))
    }

    fn portable_flags(&self, args: &[String]) -> Vec<String> {
        found::drop_flags(args, &["-c", "--config"], &["resume", "--last"])
    }

    /// Codex before its shared server (and one run with `--no-daemon`) keeps its rollout open: that
    /// names its conversation, the process's cwd the folder. One its shared server runs (Codex
    /// 0.160.1) has none open: it's on the one only it could have begun, among those its server has
    /// loaded (see `attached`). One dino can't tell is listed as such, with nothing to continue.
    fn running(&self, procs: &crate::procinfo::Procs) -> Vec<FoundSession> {
        let pids = procinfo::named_in(procs, "codex");
        if pids.is_empty() {
            return vec![];
        }
        let titles = history::codex_titles();
        let mut out = vec![];
        let (tuis, open) = in_terminals(procs, &pids);
        for (pid, rollout) in &open {
            out.extend(self.on(*pid, rollout, &titles));
        }
        let home: std::collections::HashMap<u32, &Path> = tuis.iter().map(|t| (t.pid, t.home.as_path())).collect();
        for (pid, a) in attach(&tuis, &[]) {
            match a {
                Attached::On(id) => {
                    // Its rollout is written with its first prompt: nothing to continue until then.
                    if let Some(rollout) = attached::rollout(&id).or_else(|| rollout_path(home[&pid], &id)) {
                        out.extend(self.on(pid, &rollout, &titles));
                    }
                }
                Attached::Unsure { why, maybe } if !maybe.is_empty() => {
                    let (terminal, args) = found::terminal_and_flags(self, pid);
                    let updated_at = maybe.iter().filter_map(|id| attached::rollout(id)).map(|p| history::modified(&p)).max().unwrap_or(0);
                    out.push(FoundSession {
                        title: "Codex".into(),
                        cwd: procinfo::cwd_of(pid),
                        updated_at,
                        terminal,
                        args,
                        unsure: Some(found::Unsure { why: why.into(), maybe }),
                        ..found::by_hand("codex", pid)
                    });
                }
                _ => {}
            }
        }
        out
    }

    // Every command but `resume` and `fork` (its TUI on a past session) runs headless or isn't a
    // conversation: `exec`, `review`, the app server, the sandbox…
    fn headless(&self, args: &[String]) -> bool {
        super::runs_with(
            args,
            &[],
            &[
                "agents", "exec", "e", "review", "login", "logout", "mcp", "mcp-server", "plugin", "app-server", "remote-control", "app", "completion", "update",
                "doctor", "sandbox", "debug", "apply", "a", "queue", "archive", "delete", "migrate-rollouts", "unarchive", "cloud", "exec-server", "features", "help",
                "proto",
            ],
        )
    }

    fn may_be(&self, comm: &str) -> bool {
        let name = comm.rsplit('/').next().unwrap_or(comm);
        name == "codex" || name.starts_with("codex-")
    }

    fn inside(&self, pid: u32, comm: &str, args: &dyn Fn() -> Vec<String>) -> Option<FoundSession> {
        if !self.may_be(comm) {
            return None;
        }
        let mut s = found::by_hand("codex", pid);
        s.title = "Codex".into();
        let files = found::run("lsof", &["-p", &pid.to_string(), "-Fn"]).unwrap_or_default();
        let names: Vec<&str> = files.lines().filter_map(|l| l.strip_prefix('n')).collect();
        let sessions = home_of(pid).map(|h| attached::resolved(&h.join("sessions")));
        if let Some(rollout) = names.iter().find(|f| sessions.as_ref().is_some_and(|s| Path::new(f).starts_with(s)) && f.ends_with(".jsonl")) {
            s.session_id = history::rollout_id(Path::new(rollout)).unwrap_or_default();
            s.updated_at = history::modified(Path::new(rollout));
            if let Some(n) = history::codex_titles().remove(&s.session_id) {
                s.title = n;
            }
        }
        s.cwd = found::run("lsof", &["-a", "-p", &pid.to_string(), "-d", "cwd", "-Fn"]).and_then(|t| t.lines().find_map(|l| l.strip_prefix('n').map(String::from)));
        let args = args();
        // Its conversations in Codex's shared server (0.160.1), it has no rollout open: one it
        // resumes is the one it was told.
        if s.session_id.is_empty() {
            s.session_id = resumed_in(&args).unwrap_or_default();
        }
        s.args = self.portable_flags(&args);
        Some(s)
    }

    fn recent(&self, running: &dyn Fn(&str) -> bool) -> Vec<FoundSession> {
        let titles = history::codex_titles();
        let mut rollouts: Vec<(u64, PathBuf)> = history::codex_rollouts().into_iter().map(|p| (history::modified(&p), p)).filter(|(t, _)| *t > 0).collect();
        rollouts.sort_by(|a, b| b.0.cmp(&a.0));
        let mut seen = std::collections::HashSet::new();
        let mut out = vec![];
        for (updated, p) in rollouts {
            let Some(sid) = history::rollout_id(&p) else { continue };
            if running(&sid) || !seen.insert(sid.clone()) {
                continue;
            }
            let meta = history::codex_meta(&p);
            if meta.hidden {
                continue;
            }
            let title = titles.get(&sid).cloned().or(meta.title).unwrap_or_else(|| "Codex session".into());
            out.push(history::recent("codex", sid, title, meta.cwd, updated));
        }
        out
    }

    fn cloud(&self, program: &Path) -> Vec<FoundSession> {
        cloud_tasks(program)
            .into_iter()
            .map(|t| FoundSession {
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
                tmux: None,
                unsure: None,
            })
            .collect()
    }

    // Cloud tasks open in its cloud browser.
    fn cloud_args(&self, _session_id: &str) -> Vec<String> {
        vec!["cloud".into()]
    }

    fn transcript(&self, session_id: &str) -> Option<PathBuf> {
        crate::transcript::codex_path(session_id)
    }

    fn turns(&self, text: &str, _path: &Path, _start: u64) -> Vec<Turn> {
        history::codex_turns(text)
    }

    fn usage(&self, seen: &mut crate::usage::Seen) -> Vec<crate::usage::Used> {
        let mut out = vec![];
        for p in history::codex_rollouts() {
            let Some(id) = history::rollout_id(&p) else { continue };
            let Some((text, _)) = seen.new_lines(&p, b"") else { continue };
            // What earlier reads learned of its model and folder.
            let (mk, ck) = (format!("codex:model:{}", p.display()), format!("codex:cwd:{}", p.display()));
            let mut model = seen.mark(&mk).map(String::from);
            let mut cwd = seen.mark(&ck).map(String::from);
            out.extend(history::codex_usage_in(&text, &id, &mut model, &mut cwd));
            if let Some(m) = model {
                seen.set_mark(&mk, m);
            }
            if let Some(c) = cwd {
                seen.set_mark(&ck, c);
            }
        }
        out
    }

    fn account_vars(&self) -> &'static [&'static str] {
        &["CODEX_HOME", "OPENAI_ORG_ID", "OPENAI_PROJECT_ID"]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What Codex 0.160.1's shared server has loaded: a lock file for each, named for it; one a
    /// subagent's or a headless run's isn't a terminal's. Each has its folder once its rollout is
    /// written (with its first prompt), found in the day's folder its id says it began on.
    #[test]
    fn what_its_shared_server_has_loaded() {
        let home = std::env::temp_dir().join(format!("dino-codex-loaded-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let (here, sessions, locks) = (home.join("proj"), home.join(".codex/sessions"), home.join(".codex/thread-writer-locks"));
        std::fs::create_dir_all(&here).unwrap();
        std::fs::create_dir_all(&locks).unwrap();
        let since: u64 = 1_791_318_223;
        let id = |at_ms: u64, tail: &str| format!("{:08x}-{:04x}-7{tail}", at_ms >> 16, at_ms & 0xffff);
        let write = |id: &str, source: &str| {
            let day = day_dir(&sessions, since).unwrap();
            std::fs::create_dir_all(&day).unwrap();
            let meta = format!(r#"{{"timestamp":"2026-10-06T20:23:43.400Z","type":"session_meta","payload":{{"id":"{id}","cwd":"{}","originator":"codex-tui","cli_version":"0.160.1","source":{source}}}}}"#, here.display());
            std::fs::write(day.join(format!("rollout-2026-10-06T13-23-43-{id}.jsonl")), meta + "\n").unwrap();
        };
        let its = id(since * 1000 + 600, "cd2-b4cb-58e3cc582ab7");
        let fresh = id(since * 1000 + 900, "cd2-b4cb-000000000001");
        let sub = id(since * 1000 + 700, "cd2-b4cb-000000000002");
        write(&its, r#""vscode""#);
        write(&sub, r#"{"subagent":{"thread_spawn":{}}}"#);
        for f in [format!("{its}.lock"), format!("{fresh}.lock"), format!("{sub}.lock"), ".coordination.lock".into(), "not-an-id.lock".into()] {
            std::fs::write(locks.join(f), "").unwrap();
        }
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
        let mut loaded = attached::loaded_in(&home.join(".codex"));
        loaded.sort_by(|a, b| a.id.cmp(&b.id));
        assert_eq!(loaded.iter().map(|l| (l.id.as_str(), l.cwd.clone(), l.written)).collect::<Vec<_>>(), [(its.as_str(), Some(attached::resolved(&here)), true), (fresh.as_str(), None, false)]);
        assert!(loaded.iter().all(|l| l.at_ms.abs_diff(now) < 10_000), "loaded as its lock was made, not as it began");
        assert!(attached::rollout(&its).unwrap().to_string_lossy().ends_with(&format!("{its}.jsonl")));
        // Found again by its id, in the day's folder it says it began on.
        let found = rollout_in(&sessions, &its, attached::begun_ms(&its).unwrap() / 1000).unwrap();
        assert!(found.starts_with(day_dir(&sessions, since).unwrap()));
        assert_eq!(rollout_in(&sessions, &fresh, since), None, "not written yet");
        std::fs::remove_dir_all(&home).unwrap();
    }

    /// Codexes run with two Codex homes (`CODEX_HOME`) have a server each: each is on the one its
    /// own server loaded as it started, even two started together in one folder.
    #[test]
    fn each_codex_home_has_a_server_of_its_own() {
        let root = std::env::temp_dir().join(format!("dino-codex-homes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let here = root.join("proj");
        std::fs::create_dir_all(&here).unwrap();
        let started = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64 - 60_000;
        let id = |at_ms: u64, tail: &str| format!("{:08x}-{:04x}-7{tail}", at_ms >> 16, at_ms & 0xffff);
        let homes = [root.join("a"), root.join("b")];
        let ids = [id(started + 300, "cd2-b4cb-000000000001"), id(started + 400, "cd2-b4cb-000000000002")];
        for (home, id) in homes.iter().zip(&ids) {
            std::fs::create_dir_all(home.join("thread-writer-locks")).unwrap();
            let lock = std::fs::File::create(home.join(format!("thread-writer-locks/{id}.lock"))).unwrap();
            // Loaded as it began (a file made earlier than its last change is said to be born then).
            lock.set_modified(std::time::UNIX_EPOCH + Duration::from_millis(attached::begun_ms(id).unwrap())).unwrap();
        }
        let tui = |pid: u32, home: &Path, started_ms: u64| attached::Tui { pid, home: home.into(), started_ms, cwd: Some(attached::resolved(&here)), open: None, told: None, picks: false };
        let apart = attach(&[tui(1, &homes[0], started), tui(2, &homes[1], started + 100)], &[]);
        assert_eq!((apart[&1].clone(), apart[&2].clone()), (Attached::On(ids[0].clone()), Attached::On(ids[1].clone())));
        // Of one home, they started together: either could be on the one there.
        let one = attach(&[tui(1, &homes[0], started), tui(2, &homes[0], started + 100)], &[]);
        assert!(matches!((&one[&1], &one[&2]), (Attached::Unsure { .. }, Attached::Unsure { .. })), "{one:?}");
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// The Codex home a Codex runs with, as Codex finds it, named as the kernel names its files.
    #[test]
    fn the_codex_home_it_runs_with() {
        let dir = std::env::temp_dir().join(format!("dino-codex-home-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("x")).unwrap();
        std::fs::create_dir_all(dir.join(".codex")).unwrap();
        let real = attached::resolved(&dir);
        let env = |v: &[String]| v.to_vec();
        assert_eq!(home_in(&env(&[format!("CODEX_HOME={}", dir.join("x").display())]), || None), real.join("x"), "its CODEX_HOME, links resolved");
        assert_eq!(home_in(&env(&["CODEX_HOME=x".into()]), || Some(dir.clone())), real.join("x"), "one relative to its folder");
        assert_eq!(home_in(&env(&["CODEX_HOME=".into(), format!("HOME={}", dir.display())]), || None), real.join(".codex"), "else .codex in its home folder");
        let own = attached::resolved(&PathBuf::from(std::env::var_os("HOME").unwrap()).join(".codex"));
        assert_eq!(home_in(&env(&["HOMEBREW_PREFIX=/opt/homebrew".into()]), || None), own, "dinod's home folder when it names none");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn what_its_command_line_says() {
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let id = "01a112e2-c11f-7cd2-b4cb-58e3cc582ab7";
        assert_eq!(how_started(&args(&["-m", "gpt-5.5"])), (None, false, None), "a new one, begun as it starts");
        assert_eq!(how_started(&args(&["resume", id])), (Some(id.into()), false, None));
        assert_eq!(how_started(&args(&["resume"])), (None, true, None), "its picker");
        assert_eq!(how_started(&args(&["resume", "--last"])), (None, true, None), "the last one: loaded long ago, maybe");
        assert_eq!(how_started(&args(&["fork", id, "-C", "/x"])), (None, false, Some("/x".into())), "a fork is a new one");
        assert_eq!(how_started(&args(&["fork"])), (None, true, None));
        assert_eq!(how_started(&args(&["--remote", "ws://h:1"])), (None, true, None), "another server's");
        assert_eq!(how_started(&args(&["--cd=sub"])).2.as_deref(), Some("sub"));
    }

    #[test]
    fn a_resumed_conversation_is_the_one_it_was_told() {
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let id = "01a112e2-c11f-7cd2-b4cb-58e3cc582ab7";
        assert_eq!(resumed_in(&args(&["resume", "-m", "gpt-5.5", id])).as_deref(), Some(id));
        assert_eq!(resumed_in(&args(&["-m", "gpt-5.5", "resume", id, "go on"])).as_deref(), Some(id));
        assert_eq!(resumed_in(&args(&["resume"])), None, "its picker");
        assert_eq!(resumed_in(&args(&["fork", id])), None, "a fork is a new conversation");
        // What a person types to continue it, without the notices dino asks of its own sessions.
        assert_eq!(Codex.resume_args(id), (vec!["resume".to_string()], vec![id.to_string()]));
        assert_eq!(Codex.quit_keys(), b"\x03\x03");
    }
}
