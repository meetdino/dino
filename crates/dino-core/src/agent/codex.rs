//! Codex CLI. It names its conversation only once it writes one: by the rollout file its process
//! has open (`~/.codex/sessions/…/rollout-<time>-<id>.jsonl`).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::{Agent, ControlKind, StatusSource, Wiring, strings};
use crate::found::{self, FoundSession, Source};
use crate::history::{self, Turn};
use crate::models::{self, Catalog};
use crate::providers::Format;
use crate::procinfo;

pub(crate) struct Codex;

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

/// Each Codex process's open conversation: when it started, the descriptor, the file.
static HELD: std::sync::Mutex<Option<std::collections::HashMap<u32, (u64, i32, String)>>> = std::sync::Mutex::new(None);

/// The provider's header that carries the proxy's secret, from the environment (`keyed_urls`).
fn key_header() -> String {
    format!(r#"model_providers.dino.env_http_headers={{"{}"="{}"}}"#, super::KEY_HEADER, super::KEY_ENV)
}

/// `"chatgpt"` or `"apikey"`, from `~/.codex/auth.json`.
fn auth_mode() -> Option<String> {
    let home = std::env::var_os("HOME")?;
    let auth = std::fs::read_to_string(Path::new(&home).join(".codex/auth.json")).ok()?;
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
        // So it says when it waits on the user (see dinod's `codex`).
        after.extend(NOTICE_ARGS.map(String::from));
        (before, after)
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

    /// Codex keeps its rollout file open; that names the session, the process's cwd the folder.
    fn running(&self, procs: &crate::procinfo::Procs) -> Vec<FoundSession> {
        let pids = procinfo::named_in(procs, "codex");
        if pids.is_empty() {
            return vec![];
        }
        let titles = history::codex_titles();
        let mut out = vec![];
        let mut held = HELD.lock().unwrap();
        let held = held.get_or_insert_default();
        held.retain(|pid, _| pids.contains(pid));
        for pid in pids {
            // The descriptor it had its conversation open as last time, if it still does: one
            // look instead of one per file it has open.
            let started = procs.get(&pid).map_or(0, |p| p.started_us);
            let known = held.get(&pid).filter(|(at, fd, path)| *at == started && procinfo::open_file(pid, *fd).as_deref() == Some(path.as_str()));
            let rollout = match known {
                Some((_, _, path)) => path.clone(),
                None => {
                    held.remove(&pid);
                    let Some((fd, path)) = procinfo::open_fds(pid).into_iter().find(|(_, f)| f.contains("/.codex/sessions/") && f.ends_with(".jsonl")) else { continue };
                    held.insert(pid, (started, fd, path.clone()));
                    path
                }
            };
            let rollout = Path::new(&rollout);
            let Some(sid) = history::rollout_id(rollout) else { continue };
            let (terminal, args) = found::terminal_and_flags(self, pid);
            out.push(FoundSession {
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
            });
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
        if let Some(rollout) = names.iter().find(|f| f.contains("/.codex/sessions/") && f.ends_with(".jsonl")) {
            s.session_id = history::rollout_id(Path::new(rollout)).unwrap_or_default();
            s.updated_at = history::modified(Path::new(rollout));
            if let Some(n) = history::codex_titles().remove(&s.session_id) {
                s.title = n;
            }
        }
        s.cwd = found::run("lsof", &["-a", "-p", &pid.to_string(), "-d", "cwd", "-Fn"]).and_then(|t| t.lines().find_map(|l| l.strip_prefix('n').map(String::from)));
        s.args = self.portable_flags(&args());
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
