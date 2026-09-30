//! Pi (`@earendil-works/pi-coding-agent`), and Pi on dino's free tier (`free`). It never asks
//! before running a tool, so it has no permission modes. Its model is `--model provider/id`, its
//! effort `--thinking`, both as Pi lists them. It takes its conversation id up front
//! (`--session-id`, created if missing) and keeps each conversation at
//! `~/.pi/agent/sessions/--<cwd>--/<time>_<id>.jsonl`, which dino follows for status. On the free
//! tier dino gives it one extension for that session, which only moves Anthropic's address to dino
//! (`registerProvider`, no tools), and a placeholder key; Pi keeps its own models.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{Agent, ControlKind, LogEvent, StatusSource, Wiring, strings};
use crate::found::{self, FoundSession};
use crate::history::{self, Meta, Turn, one_line, turn};
use crate::models::{Catalog, ModelInfo};

pub(crate) struct Pi {
    pub(crate) free: bool,
}

/// Where Pi keeps its own files: `$PI_CODING_AGENT_DIR`, else `~/.pi/agent`.
fn pi_dir() -> PathBuf {
    std::env::var_os("PI_CODING_AGENT_DIR").map(PathBuf::from).unwrap_or_else(|| std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(".pi/agent"))
}

fn sessions_dir() -> PathBuf {
    std::env::var_os("PI_CODING_AGENT_SESSION_DIR").map(PathBuf::from).unwrap_or_else(|| pi_dir().join("sessions"))
}

/// The folder a working directory's conversations are in: its path without the leading
/// separator, separators and colons as dashes, between `--`.
fn folder_of(cwd: &str) -> PathBuf {
    let path: String = cwd.trim_start_matches('/').chars().map(|c| if matches!(c, '/' | '\\' | ':') { '-' } else { c }).collect();
    sessions_dir().join(format!("--{path}--"))
}

/// Every conversation file.
fn transcripts() -> Vec<PathBuf> {
    std::fs::read_dir(sessions_dir())
        .into_iter()
        .flatten()
        .flatten()
        .flat_map(|d| std::fs::read_dir(d.path()).into_iter().flatten().flatten())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .collect()
}

/// A conversation's id, from its file name: `<time>_<id>.jsonl`.
fn id_of(p: &Path) -> Option<String> {
    p.file_stem()?.to_str()?.split_once('_').map(|(_, id)| id.to_string())
}

/// The text of a message's content: a string, or its text blocks.
fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts.iter().filter(|p| p["type"] == "text").filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

/// Title as Pi shows it: the name given (`/name`), else the first prompt. Folder from its header.
fn meta_in(jsonl: &str) -> Meta {
    let (mut name, mut first, mut cwd) = (None, None, None);
    for v in jsonl.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
        match v["type"].as_str() {
            Some("session") => cwd = v["cwd"].as_str().map(String::from),
            Some("session_info") => name = v["name"].as_str().and_then(one_line).or(name),
            Some("message") if first.is_none() && v["message"]["role"] == "user" => first = history::typed(&text_of(&v["message"]["content"])).and_then(one_line),
            _ => {}
        }
    }
    Meta { hidden: first.is_none(), title: name.or(first), cwd }
}

fn meta(p: &Path) -> Meta {
    history::cached(p, |p| meta_in(&history::peek(p)))
}

fn turns_in(jsonl: &str) -> Vec<Turn> {
    let mut out = vec![];
    for v in jsonl.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
        if v["type"] != "message" {
            continue;
        }
        let m = &v["message"];
        match m["role"].as_str() {
            Some("user") => out.extend(history::typed(&text_of(&m["content"])).map(|t| turn("user", t))),
            Some("assistant") => {
                let text = text_of(&m["content"]);
                if !text.trim().is_empty() {
                    out.push(turn("assistant", text.trim()));
                }
                for call in m["content"].as_array().into_iter().flatten().filter(|p| p["type"] == "toolCall") {
                    out.push(turn("tool", format!("{}{}", call["name"].as_str().unwrap_or("tool"), history::hint(&call["arguments"]))));
                }
            }
            _ => {}
        }
    }
    out
}

/// What one entry says about its turn: a prompt starts one; an answer that isn't asking for a
/// tool ends it. It never waits on the user mid-turn.
fn event_of(v: &Value) -> LogEvent {
    if v["type"] != "message" {
        return LogEvent::Bookkeeping;
    }
    match (v["message"]["role"].as_str(), v["message"]["stopReason"].as_str()) {
        (Some("user"), _) => LogEvent::TurnStarted,
        (Some("assistant"), Some(r)) if r != "toolUse" => LogEvent::TurnEnded,
        _ => LogEvent::Other,
    }
}

fn busy_in(jsonl: &str) -> bool {
    let mut busy = false;
    for v in jsonl.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
        match event_of(&v) {
            LogEvent::TurnStarted => busy = true,
            LogEvent::TurnEnded => busy = false,
            _ => {}
        }
    }
    busy
}

/// Its thinking levels, as its help lists them for `--thinking`.
fn levels_in(help: &str) -> Vec<String> {
    let Some(line) = help.lines().find(|l| l.trim_start().starts_with("--thinking")) else { return vec![] };
    let Some((_, list)) = line.split_once(':') else { return vec![] };
    list.split(',').map(|l| l.trim().to_string()).filter(|l| !l.is_empty() && !l.contains(' ')).collect()
}

/// The models `pi --list-models` lists (for providers it can use), with levels for those that think.
fn catalog_in(list: &str, levels: &[String], settings: &Value) -> Option<Catalog> {
    let mut rows = list.lines().map(|l| l.split_whitespace().collect::<Vec<_>>());
    let header = rows.next()?;
    let col = |name: &str| header.iter().position(|h| *h == name);
    let (provider, model, thinking) = (col("provider")?, col("model")?, col("thinking"));
    let models: Vec<ModelInfo> = rows
        .filter(|r| r.len() > provider.max(model))
        .map(|r| {
            let thinks = thinking.and_then(|t| r.get(t)).is_some_and(|t| *t == "yes");
            // "off" is no effort at all: the effort menu is for how much.
            let efforts: Vec<String> = if thinks { levels.iter().filter(|l| *l != "off").cloned().collect() } else { vec![] };
            ModelInfo { id: format!("{}/{}", r[provider], r[model]), label: r[model].to_string(), efforts, default_effort: None, group: None, aliases: vec![r[model].to_string()] }
        })
        .collect();
    let default_model = match (settings["defaultProvider"].as_str(), settings["defaultModel"].as_str()) {
        (Some(p), Some(m)) => Some(format!("{p}/{m}")),
        _ => None,
    };
    let default_effort = settings["defaultThinkingLevel"].as_str().map(String::from);
    let models = models.into_iter().map(|mut m| {
        m.default_effort = default_effort.clone().filter(|e| m.efforts.contains(e));
        m
    });
    let models: Vec<ModelInfo> = models.collect();
    (!models.is_empty()).then_some(Catalog { models, default_model })
}

/// The extension that points Anthropic's API at dino for one free-tier session, written where
/// dino keeps its own files.
fn route_extension(base: &str) -> Option<PathBuf> {
    let dir = crate::config_dir().join("pi");
    std::fs::create_dir_all(&dir).ok()?;
    let session = base.split("/s/").nth(1).and_then(|r| r.split('/').next()).unwrap_or("session");
    let name: String = session.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let path = dir.join(format!("free-{name}.js"));
    let url = serde_json::to_string(base).ok()?;
    let text = format!("// Written by dino: this Pi session's Anthropic requests go to dino's free tier. No tools.\nexport default function (pi) {{\n  pi.registerProvider(\"anthropic\", {{ baseUrl: {url} }});\n}}\n");
    std::fs::write(&path, text).ok()?;
    Some(path)
}

/// When a file was created, in seconds.
fn born(p: &Path) -> u64 {
    p.metadata().and_then(|m| m.created()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs())
}

/// The conversation a live process is on: the one it began (created in its folder since it
/// started). Nothing before then, and nothing for one it continued: its process title overwrites
/// its arguments, so which one can't be told from another Pi's in the same folder.
fn conversation_in(pid: u32) -> Option<PathBuf> {
    let cwd = crate::procinfo::cwd_of(pid)?;
    let since = crate::procinfo::started(pid).unwrap_or(0);
    std::fs::read_dir(folder_of(&cwd))
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl") && born(p) + 1 >= since)
        .min_by_key(|p| born(p))
}

impl Pi {
    fn found(&self, pid: u32) -> FoundSession {
        let mut s = found::by_hand("pi", pid);
        s.cwd = crate::procinfo::cwd_of(pid);
        s.title = "Pi".into();
        if let Some(p) = conversation_in(pid) {
            s.session_id = id_of(&p).unwrap_or_default();
            s.title = meta(&p).title.unwrap_or_else(|| "Pi".into());
            s.updated_at = history::modified(&p);
            s.status = std::fs::read_to_string(&p).ok().map(|t| if busy_in(&t) { "busy" } else { "idle" }.into());
        }
        s
    }
}

impl Agent for Pi {
    fn id(&self) -> &'static str {
        if self.free { "pi-free" } else { "pi" }
    }

    // It never asks.
    fn modes(&self) -> &'static [&'static str] {
        &[]
    }

    fn mode_args(&self, _mode: &str) -> Vec<String> {
        vec![]
    }

    // The free tier picks the model for each turn.
    fn picks_model(&self) -> bool {
        !self.free
    }

    fn free(&self) -> bool {
        self.free
    }

    fn model_args(&self, model: &str) -> Vec<String> {
        strings(&["--model", model])
    }

    fn effort_args(&self, effort: &str) -> Vec<String> {
        strings(&["--thinking", effort])
    }

    fn value_flags(&self) -> &'static [&'static str] {
        &[
            "--provider", "--model", "--api-key", "--system-prompt", "--append-system-prompt", "--mode", "--session", "--session-id", "--fork",
            "--session-dir", "--name", "-n", "--models", "--tools", "-t", "--exclude-tools", "-xt", "--thinking", "--extension", "-e", "--skill",
            "--prompt-template", "--theme", "--use-theme", "--export", "--tui-mode",
        ]
    }

    fn control_of(&self, name: &str, _value: Option<&str>) -> Option<ControlKind> {
        match name {
            "--model" => Some(ControlKind::Model),
            "--thinking" => Some(ControlKind::Effort),
            _ => None,
        }
    }

    fn read_mode(&self, _flags: &[(&str, Option<&str>)]) -> Option<String> {
        None
    }

    fn catalog_sources(&self) -> Vec<PathBuf> {
        ["models.json", "auth.json", "settings.json"].iter().map(|f| pi_dir().join(f)).collect()
    }

    fn catalog(&self, program: &str) -> Option<Catalog> {
        // Asking Pi makes its home folder: don't, for someone who has never run it.
        if self.free || !pi_dir().exists() {
            return None;
        }
        // Offline: its own catalog, without refreshing it from the network.
        let run = |args: &[&str]| std::process::Command::new(program).args(args).env("PI_OFFLINE", "1").stderr(std::process::Stdio::null()).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
        let settings = std::fs::read_to_string(pi_dir().join("settings.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null);
        catalog_in(&run(&["--list-models"])?, &levels_in(&run(&["--help"]).unwrap_or_default()), &settings)
    }

    // On its own, nothing: its providers are its own setting, and dino follows its record for
    // status. On the free tier, Anthropic's address moved to dino for this session, and a
    // placeholder key; the proxy holds the real ones.
    fn wiring(&self, _route: bool, base: &dyn Fn(&str) -> String, _status_line: Option<String>) -> Wiring {
        if !self.free {
            return (vec![], vec![]);
        }
        let Some(ext) = route_extension(&base("free")) else { return (vec![], vec![]) };
        (vec![("ANTHROPIC_API_KEY".into(), "dino-free".into())], vec!["--provider".into(), "anthropic".into(), "-e".into(), ext.display().to_string()])
    }

    // Its first messages go last on its command line.
    fn prompt_args(&self, prompt: String) -> Vec<String> {
        vec![prompt]
    }

    // Its exact id, created if missing: the same flag starts it and continues it.
    fn session_args(&self, session: &mut Option<String>, _restoring: bool) -> (Vec<String>, Vec<String>) {
        let id = session.get_or_insert_with(crate::new_uuid).clone();
        (vec!["--session-id".into(), id], vec![])
    }

    fn status_source(&self) -> StatusSource {
        StatusSource::Log
    }

    fn log_path(&self, session: &str) -> Option<PathBuf> {
        self.transcript(session)
    }

    fn log_event(&self, line: &Value) -> LogEvent {
        event_of(line)
    }

    fn busy(&self, pid: u32) -> Option<bool> {
        Some(busy_in(&std::fs::read_to_string(conversation_in(pid)?).ok()?))
    }

    fn portable_flags(&self, args: &[String]) -> Vec<String> {
        found::drop_flags(args, &["--session", "--session-id", "--fork", "--export", "--mode", "--name", "-n"], &["-c", "--continue", "-r", "--resume", "-p", "--print", "--no-session"])
    }

    fn may_be(&self, comm: &str) -> bool {
        comm.rsplit('/').next() == Some("pi")
    }

    fn running(&self) -> Vec<FoundSession> {
        if self.free {
            return vec![];
        }
        let mut out = vec![];
        for pid in crate::procinfo::pids_named("pi") {
            let mut s = self.found(pid);
            if s.session_id.is_empty() {
                continue;
            }
            let (terminal, args) = found::terminal_and_flags(self, pid);
            s.terminal = terminal;
            s.args = args;
            out.push(s);
        }
        out
    }

    fn inside(&self, pid: u32, comm: &str, args: &dyn Fn() -> Vec<String>) -> Option<FoundSession> {
        if !self.may_be(comm) || self.free {
            return None;
        }
        let mut s = self.found(pid);
        s.args = self.portable_flags(&args());
        Some(s)
    }

    fn recent(&self, running: &dyn Fn(&str) -> bool) -> Vec<FoundSession> {
        if self.free {
            return vec![];
        }
        let mut out = vec![];
        for p in transcripts() {
            let Some(id) = id_of(&p) else { continue };
            let meta = meta(&p);
            if meta.hidden || running(&id) {
                continue;
            }
            let title = meta.title.unwrap_or_else(|| "Pi session".into());
            out.push(history::recent("pi", id, title, meta.cwd, history::modified(&p)));
        }
        out
    }

    // No cloud work of its own.
    fn cloud_args(&self, _session_id: &str) -> Vec<String> {
        vec![]
    }

    fn transcript(&self, session_id: &str) -> Option<PathBuf> {
        if session_id.is_empty() {
            return None;
        }
        let end = format!("_{session_id}.jsonl");
        transcripts().into_iter().find(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.ends_with(&end)))
    }

    fn turns(&self, text: &str, _path: &Path, _start: u64) -> Vec<Turn> {
        turns_in(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed from a Pi 0.99.1 session: a prompt that failed upstream (and was retried), then one
    /// that ran a tool.
    const SESSION: &str = r#"{"type":"session","version":3,"id":"11111111-2222-4333-8444-555555555555","timestamp":"2026-09-30T04:05:59.163Z","cwd":"/private/tmp/r"}
{"type":"model_change","id":"a1","parentId":null,"timestamp":"2026-09-30T04:05:59.200Z","provider":"anthropic","modelId":"claude-opus-4-8"}
{"type":"message","id":"a2","parentId":"a1","timestamp":"2026-09-30T04:06:01.000Z","message":{"role":"system","content":"","sections":{"preamble":"You are…"}}}
{"type":"message","id":"a3","parentId":"a2","timestamp":"2026-09-30T04:06:01.100Z","message":{"role":"user","content":[{"type":"text","text":"Remember the word PELICAN. Reply with only OK."}]}}
{"type":"message","id":"a4","parentId":"a3","timestamp":"2026-09-30T04:06:03.000Z","message":{"role":"assistant","content":[],"stopReason":"error","errorMessage":"upstream"}}
{"type":"message","id":"a5","parentId":"a4","timestamp":"2026-09-30T04:06:20.000Z","message":{"role":"user","content":[{"type":"text","text":"Run ls with your bash tool, then reply DONE."}]}}
{"type":"message","id":"a6","parentId":"a5","timestamp":"2026-09-30T04:06:22.000Z","message":{"role":"assistant","content":[{"type":"text","text":""},{"type":"toolCall","id":"c1","name":"bash","arguments":{"command":"ls","timeout":5}}],"stopReason":"toolUse"}}
{"type":"message","id":"a7","parentId":"a6","timestamp":"2026-09-30T04:06:22.500Z","message":{"role":"toolResult","toolCallId":"c1","toolName":"bash","content":[{"type":"text","text":"(no output)"}]}}
{"type":"message","id":"a8","parentId":"a7","timestamp":"2026-09-30T04:06:24.000Z","message":{"role":"assistant","content":[{"type":"text","text":"DONE"}],"stopReason":"stop"}}"#;

    #[test]
    fn its_session_reads_as_turns() {
        let turns: Vec<(String, String)> = turns_in(SESSION).into_iter().map(|t| (t.role, t.text)).collect();
        let want = [
            ("user", "Remember the word PELICAN. Reply with only OK."),
            ("user", "Run ls with your bash tool, then reply DONE."),
            ("tool", "bash ls"),
            ("assistant", "DONE"),
        ];
        assert_eq!(turns, want.map(|(r, t)| (r.to_string(), t.to_string())));
        assert_eq!(meta_in(SESSION), Meta { title: Some("Remember the word PELICAN. Reply with only OK.".into()), cwd: Some("/private/tmp/r".into()), hidden: false });
    }

    #[test]
    fn a_turn_ends_on_an_answer_that_asks_for_no_tool() {
        assert!(!busy_in(SESSION));
        let (cut, _) = SESSION.split_once(r#"{"type":"message","id":"a7""#).unwrap();
        assert!(busy_in(cut), "mid-tool");
        let events: Vec<LogEvent> = SESSION.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).map(|v| event_of(&v)).collect();
        assert!(!events.iter().any(|e| matches!(e, LogEvent::Needs(_))), "it never waits on the user");
    }

    #[test]
    fn its_folders_and_ids() {
        assert!(folder_of("/private/tmp/dino-x/repo").ends_with("--private-tmp-dino-x-repo--"));
        assert_eq!(id_of(Path::new("/s/--r--/2026-09-30T04-05-59-163Z_1111-22.jsonl")).as_deref(), Some("1111-22"));
    }

    #[test]
    fn models_and_levels_from_pi_itself() {
        let help = "  --thinking <level>             Set thinking level: off, minimal, low, medium, high, xhigh, max\n";
        let levels = levels_in(help);
        assert_eq!(levels, ["off", "minimal", "low", "medium", "high", "xhigh", "max"]);
        let list = "provider   model              context  max-out  thinking  images\nanthropic  claude-opus-4-8    1M       128K     yes       yes\nopenai     gpt-4o-mini        128K     16K      no        yes\n";
        let settings = serde_json::json!({"defaultProvider": "anthropic", "defaultModel": "claude-opus-4-8", "defaultThinkingLevel": "high"});
        let c = catalog_in(list, &levels, &settings).unwrap();
        assert_eq!(c.default_model.as_deref(), Some("anthropic/claude-opus-4-8"));
        assert_eq!(c.models[0].id, "anthropic/claude-opus-4-8");
        assert_eq!(c.models[0].efforts.first().map(String::as_str), Some("minimal"));
        assert_eq!(c.models[0].default_effort.as_deref(), Some("high"));
        assert!(c.models[1].efforts.is_empty(), "no thinking, no effort");
        assert!(catalog_in("provider model\n", &levels, &Value::Null).is_none());
    }

    #[test]
    fn the_same_flag_starts_and_continues_it() {
        let p = Pi { free: false };
        let mut id = Some("abc".to_string());
        assert_eq!(p.session_args(&mut id, true).0, ["--session-id", "abc"]);
        let mut none = None;
        let (before, _) = p.session_args(&mut none, false);
        assert_eq!(before[0], "--session-id");
        assert_eq!(none.as_deref(), Some(before[1].as_str()), "picked up front");
        assert!(p.modes().is_empty(), "it never asks");
    }
}
