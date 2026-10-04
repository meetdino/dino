//! Amp (`amp`, ampcode.com). It signs in to the user's Amp account, and its agent runs on Amp's
//! servers: the CLI runs the tools and talks to Amp, never to a model provider, so dino can't
//! route or meter it. Its threads live in Amp's cloud (`T-<uuid>`), listed by `amp threads list`
//! and continued with `amp threads continue`; nothing on disk records them as it goes, so dino goes
//! on its output and what its screen asks. Its mode (`-m low|medium|high|ultra`) picks the model,
//! as its help lists them.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

use super::{Agent, ControlKind, StatusSource, Wiring, strings};
use crate::found::{self, FoundSession, Source};
use crate::history::{Turn, one_line};
use crate::models::{Catalog, ModelInfo};

pub(crate) struct Amp;

/// Its modes, from its help's `-m, --mode` entry: "(low, medium, high, ultra, or a plugin mode …)".
fn modes_in(help: &str) -> Vec<String> {
    let mut lines = help.lines().skip_while(|l| !l.trim_start().starts_with("-m, --mode"));
    let Some(entry) = lines.nth(1) else { return vec![] };
    let Some((_, list)) = entry.split_once('(') else { return vec![] };
    list.split(',').map(str::trim).take_while(|m| !m.starts_with("or ") && !m.contains(' ') && !m.contains(')')).map(String::from).collect()
}

fn capitalized(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

/// `program args`'s output, or `None` when it fails or takes longer than `wait`.
fn output(program: &Path, args: &[&str], wait: Duration) -> Option<String> {
    let (program, args): (PathBuf, Vec<String>) = (program.into(), args.iter().map(|a| a.to_string()).collect());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let out = std::process::Command::new(&program).args(&args).env("NO_COLOR", "1").stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).output();
        let _ = tx.send(out);
    });
    let out = rx.recv_timeout(wait).ok()?.ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// It has an account to use: a key in its environment, or one `amp login` saved. Asking needs no
/// network, and never starts a sign-in.
pub(crate) fn signed_in(program: &Path) -> Option<bool> {
    if std::env::var("AMP_API_KEY").is_ok_and(|k| !k.is_empty()) {
        return Some(true);
    }
    let out = output(program, &["account", "list"], Duration::from_secs(3))?;
    Some(!out.trim().is_empty() && !out.contains("No saved accounts"))
}

/// Seconds since the epoch, from a number (seconds or milliseconds) or an ISO 8601 time.
fn when(v: &Value) -> u64 {
    if let Some(n) = v.as_u64() {
        return if n > 100_000_000_000 { n / 1000 } else { n };
    }
    let Some(s) = v.as_str() else { return 0 };
    let (Some(date), Some(time)) = (s.get(..10), s.get(11..19)) else { return 0 };
    let d: Vec<i64> = date.split('-').filter_map(|x| x.parse().ok()).collect();
    let t: Vec<i64> = time.split(':').filter_map(|x| x.parse().ok()).collect();
    if d.len() != 3 || t.len() != 3 {
        return 0;
    }
    // Days from the civil date (Howard Hinnant's algorithm); times are UTC.
    let (y, m) = if d[1] <= 2 { (d[0] - 1, d[1] + 9) } else { (d[0], d[1] - 3) };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + d[2] - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    (days * 86_400 + t[0] * 3600 + t[1] * 60 + t[2]).max(0) as u64
}

/// `amp threads list --json`: its threads, as a list or under `threads`.
fn threads_in(json: &str) -> Vec<FoundSession> {
    let Ok(v) = serde_json::from_str::<Value>(json) else { return vec![] };
    let list = v.as_array().or_else(|| v["threads"].as_array()).cloned().unwrap_or_default();
    list.iter()
        .filter_map(|t| {
            let id = t["id"].as_str().filter(|id| id.starts_with("T-"))?;
            let title = ["title", "name"].iter().find_map(|k| t[*k].as_str().and_then(one_line)).unwrap_or_else(|| "Amp thread".into());
            let updated = ["updated", "updatedAt", "lastModified", "lastUpdated", "created"].iter().map(|k| when(&t[*k])).find(|&w| w > 0).unwrap_or(0);
            Some(FoundSession {
                source: Source::Cloud,
                agent: "amp".into(),
                session_id: id.into(),
                title,
                cwd: None,
                updated_at: updated,
                pid: None,
                status: None,
                terminal: None,
                args: vec![],
                url: Some(t["url"].as_str().map(String::from).unwrap_or_else(|| format!("https://ampcode.com/threads/{id}"))),
                tmux: None,
            })
        })
        .collect()
}

impl Agent for Amp {
    fn id(&self) -> &'static str {
        "amp"
    }

    // It asks only for what the user's permissions say; `--dangerously-allow-all` never asks.
    fn modes(&self) -> &'static [&'static str] {
        &["ask", "bypass"]
    }

    fn mode_label(&self, mode: &str) -> Option<&'static str> {
        Some(match mode {
            "ask" => "Default",
            "bypass" => "Allow all",
            _ => return None,
        })
    }

    fn mode_args(&self, mode: &str) -> Vec<String> {
        if mode == "bypass" { strings(&["--dangerously-allow-all"]) } else { vec![] }
    }

    // Its mode is its model: Amp picks the model for each.
    fn model_args(&self, model: &str) -> Vec<String> {
        strings(&["-m", model])
    }

    fn effort_args(&self, _effort: &str) -> Vec<String> {
        vec![]
    }

    fn value_flags(&self) -> &'static [&'static str] {
        &["-m", "--mode", "--visibility", "--settings-file", "--log-level", "--log-file", "--mcp-config", "--features", "--executor", "--runner-dir", "-x", "--execute", "--attach", "--project", "--orb-size", "--title", "-l", "--label"]
    }

    fn control_of(&self, name: &str, _value: Option<&str>) -> Option<ControlKind> {
        match name {
            "--dangerously-allow-all" => Some(ControlKind::Mode),
            "-m" | "--mode" => Some(ControlKind::Model),
            _ => None,
        }
    }

    fn read_mode(&self, flags: &[(&str, Option<&str>)]) -> Option<String> {
        flags.last().map(|_| "bypass".into())
    }

    // Its modes as its help lists them; it keeps no files to watch.
    fn catalog(&self, program: &str) -> Option<Catalog> {
        let help = output(Path::new(program), &["--help"], Duration::from_secs(5))?;
        let models: Vec<ModelInfo> = modes_in(&help).into_iter().map(|m| ModelInfo { label: capitalized(&m), id: m, ..ModelInfo::default() }).collect();
        (!models.is_empty()).then_some(Catalog { models, default_model: None })
    }

    // Its agent runs on Amp's servers, on the user's Amp account; dino leaves it be.
    fn wiring(&self, _route: bool, _base: &dyn Fn(&str) -> String, _status_line: Option<String>) -> Wiring {
        (vec![], vec![])
    }

    // Its terminal takes no prompt to start on (one given runs once and exits).
    fn prompt_args(&self, _prompt: String) -> Vec<String> {
        vec![]
    }

    // A thread it was given (continued, or taken over); otherwise, after a restart, the one it last
    // had in this folder, as Amp remembers (it names a new thread only to its own servers).
    fn session_args(&self, session: &mut Option<String>, restoring: bool) -> (Vec<String>, Vec<String>) {
        match session {
            Some(id) => (strings(&["threads", "continue"]), vec![id.clone()]),
            None if restoring => (strings(&["threads", "continue", "--last"]), vec![]),
            None => (vec![], vec![]),
        }
    }

    fn status_source(&self) -> StatusSource {
        StatusSource::Screen
    }

    fn asks_on_screen(&self) -> bool {
        true
    }

    fn asking(&self, screen: &str) -> Option<String> {
        if screen.contains("Approval Required") || screen.contains("Guarded File Modification") || screen.contains("Waiting for Approval") {
            Some("Amp asks for approval".into())
        } else if screen.contains("Would you like to log in to Amp?") {
            Some("Sign in to Amp".into())
        } else {
            None
        }
    }

    fn portable_flags(&self, args: &[String]) -> Vec<String> {
        let args: Vec<String> = match args.first().map(String::as_str) {
            // `threads continue <ids>`, `last`: what follows are its options.
            Some("threads" | "t" | "thread") => args.iter().skip(2).filter(|a| !a.starts_with("T-") && !a.contains("/threads/T-")).cloned().collect(),
            Some("last" | "l") => args[1..].to_vec(),
            _ => args.to_vec(),
        };
        found::drop_flags(&args, &["-x", "--execute", "--title", "-l", "--label", "--attach", "--log-file"], &["--last", "--pick", "-ox", "--orb-execute", "--stream-json", "--stream-json-thinking", "--stream-json-input"])
    }

    fn may_be(&self, comm: &str) -> bool {
        comm.rsplit('/').next() == Some("amp")
    }

    // Its threads aren't on this Mac: running ones can't be told which they are.
    fn running(&self) -> Vec<FoundSession> {
        vec![]
    }

    fn inside(&self, pid: u32, comm: &str, args: &dyn Fn() -> Vec<String>) -> Option<FoundSession> {
        if !self.may_be(comm) {
            return None;
        }
        let args = args();
        let mut s = found::by_hand("amp", pid);
        s.cwd = crate::procinfo::cwd_of(pid);
        s.title = "Amp".into();
        // The thread it was told to continue, when it was.
        let continued = matches!(args.first().map(String::as_str), Some("threads" | "t" | "thread")) && matches!(args.get(1).map(String::as_str), Some("continue" | "c"));
        if let Some(id) = args.iter().skip(2).find(|a| a.starts_with("T-")).filter(|_| continued) {
            s.session_id = id.rsplit('/').next().unwrap_or(id).to_string();
        }
        s.args = self.portable_flags(&args);
        Some(s)
    }

    fn recent(&self, _running: &dyn Fn(&str) -> bool) -> Vec<FoundSession> {
        vec![]
    }

    // Its threads, from Amp's servers; nothing when signed out (it isn't asked).
    fn cloud(&self, program: &Path) -> Vec<FoundSession> {
        if signed_in(program) != Some(true) {
            return vec![];
        }
        output(program, &["threads", "list", "--json", "--limit", "50"], Duration::from_secs(10)).map(|j| threads_in(&j)).unwrap_or_default()
    }

    fn cloud_args(&self, session_id: &str) -> Vec<String> {
        if session_id.is_empty() { strings(&["threads", "continue"]) } else { vec!["threads".into(), "continue".into(), session_id.into()] }
    }

    // Its conversations are on Amp's servers, read there.
    fn transcript(&self, _session_id: &str) -> Option<PathBuf> {
        None
    }

    fn turns(&self, _text: &str, _path: &Path, _start: u64) -> Vec<Turn> {
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn its_modes_from_its_help() {
        // Amp 0.0.1791074829's help.
        let help = "  --mcp-config <value>\n      JSON configuration or file path for MCP servers to merge with existing settings\n  -m, --mode <value>\n      Set the agent mode (low, medium, high, ultra, or a plugin mode by key or label, case-insensitive) — controls the\n      model, system prompt, and tool selection\n";
        assert_eq!(modes_in(help), ["low", "medium", "high", "ultra"]);
        assert!(modes_in("Usage: amp").is_empty());
        assert_eq!(capitalized("ultra"), "Ultra");
    }

    #[test]
    fn continued_by_its_thread_or_the_last_one_here() {
        let mut none = None;
        assert_eq!(Amp.session_args(&mut none, false), (vec![], vec![]), "a new thread");
        assert_eq!(Amp.session_args(&mut none, true).0, ["threads", "continue", "--last"]);
        let mut id = Some("T-7f3a".to_string());
        assert_eq!(Amp.session_args(&mut id, true), (strings(&["threads", "continue"]), strings(&["T-7f3a"])));
        assert_eq!(Amp.cloud_args("T-7f3a"), ["threads", "continue", "T-7f3a"]);
        let args = strings(&["threads", "continue", "T-7f3a", "-m", "high", "--dangerously-allow-all"]);
        assert_eq!(Amp.portable_flags(&args), ["-m", "high", "--dangerously-allow-all"]);
        assert_eq!(Amp.portable_flags(&strings(&["-m", "low", "-x", "say hi"])), ["-m", "low"]);
        assert_eq!(Amp.read_mode(&[("--dangerously-allow-all", None)]).as_deref(), Some("bypass"));
    }

    #[test]
    fn its_threads_listed() {
        // Its list's fields aren't documented: the id, a title or name, and when it changed.
        let list = r#"[{"id":"T-1b2c3d4e-0000-4000-8000-000000000001","title":"Fix the flaky test","updated":"2026-10-03T21:04:05.000Z","url":"https://ampcode.com/threads/T-1b2c3d4e-0000-4000-8000-000000000001"},{"id":"not-a-thread"}]"#;
        let found = threads_in(list);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].title.as_str(), found[0].updated_at), ("Fix the flaky test", 1_791_061_445));
        let wrapped = r#"{"threads":[{"id":"T-2","name":"Refactor","lastModified":1791061445000}]}"#;
        let found = threads_in(wrapped);
        assert_eq!((found[0].title.as_str(), found[0].updated_at), ("Refactor", 1_791_061_445));
        assert_eq!(found[0].url.as_deref(), Some("https://ampcode.com/threads/T-2"));
        assert!(threads_in("No threads found.").is_empty());
    }

    #[test]
    fn its_dialogs_on_screen() {
        assert!(Amp.asking(" Approval Required\n Allow Once  Reject").is_some());
        assert_eq!(Amp.asking("Would you like to log in to Amp? [(y)es, (n)o]:").as_deref(), Some("Sign in to Amp"));
        assert_eq!(Amp.asking("> hello").as_deref(), None);
    }
}
