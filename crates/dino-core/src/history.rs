//! Every agent conversation on this Mac: Claude transcripts and Codex rollouts, titled the way
//! the agent titles them, and readable as a conversation. Files can be hundreds of megabytes, so
//! only their ends are read, and what's learned is kept until the file changes.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::found::{FoundSession, Source};

/// How much of each end of a file is read for its title and folder.
const PEEK: u64 = 512 << 10;
/// How much of a conversation is read per page.
const PAGE: u64 = 2 << 20;

/// What a conversation file says about itself.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Meta {
    pub title: Option<String>,
    pub cwd: Option<String>,
    /// Not a conversation someone had: a subagent's, a headless run's (`claude -p`, `codex exec`),
    /// or one that never got a prompt.
    pub hidden: bool,
}

struct Cached {
    mtime: u64,
    size: u64,
    meta: Meta,
}

static CACHE: LazyLock<Mutex<HashMap<PathBuf, Cached>>> = LazyLock::new(Default::default);

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

fn stat(p: &Path) -> Option<(u64, u64)> {
    let m = p.metadata().ok()?;
    let mtime = m.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs();
    Some((mtime, m.len()))
}

/// `parse(p)`, remembered until `p` changes.
pub(crate) fn cached(p: &Path, parse: fn(&Path) -> Meta) -> Meta {
    let Some((mtime, size)) = stat(p) else { return Meta::default() };
    if let Some(c) = CACHE.lock().unwrap().get(p).filter(|c| c.mtime == mtime && c.size == size) {
        return c.meta.clone();
    }
    let meta = parse(p);
    CACHE.lock().unwrap().insert(p.to_path_buf(), Cached { mtime, size, meta: meta.clone() });
    meta
}

/// The whole lines in bytes `from..to` of `p`. A range starting mid-line drops that fragment.
fn read_range(p: &Path, from: u64, to: u64) -> Option<String> {
    let mut f = std::fs::File::open(p).ok()?;
    f.seek(SeekFrom::Start(from)).ok()?;
    let mut buf = vec![];
    f.take(to.saturating_sub(from)).read_to_end(&mut buf).ok()?;
    let mut text = String::from_utf8_lossy(&buf).into_owned();
    if from > 0 {
        text = text.split_once('\n').map_or(String::new(), |(_, rest)| rest.to_string());
    }
    if to < p.metadata().ok()?.len() {
        // Ends mid-line: drop the fragment.
        text.truncate(text.rfind('\n').map_or(0, |i| i + 1));
    }
    Some(text)
}

/// The start and end of `p` (all of it when small), in file order.
pub(crate) fn peek(p: &Path) -> String {
    let len = p.metadata().map_or(0, |m| m.len());
    if len <= 2 * PEEK {
        return read_range(p, 0, len).unwrap_or_default();
    }
    format!("{}\n{}", read_range(p, 0, PEEK).unwrap_or_default(), read_range(p, len - PEEK, len).unwrap_or_default())
}

/// One line, at most 80 characters.
pub(crate) fn one_line(s: &str) -> Option<String> {
    let line = s.lines().map(str::trim).find(|l| !l.is_empty())?;
    let short: String = line.chars().take(80).collect();
    Some(if short.len() < line.len() { format!("{short}…") } else { short })
}

/// What a person typed, not a slash command's expansion or an injected wrapper.
pub(crate) fn typed(text: &str) -> Option<&str> {
    let t = text.trim();
    let wrapper = [
        "<",
        "[Request interrupted",
        "[Image",
        "# AGENTS.md",
        "Caveat:",
        "You've inherited the conversation context",
        "Your response above was cut off",
    ]
    .iter()
    .any(|w| t.starts_with(w));
    (!t.is_empty() && !wrapper).then_some(t)
}

// ---- Claude ----

/// `~/.claude/projects/<dir>/<uuid>.jsonl`, not subagents' (`<uuid>/subagents/…`).
pub(crate) fn claude_transcripts() -> Vec<PathBuf> {
    std::fs::read_dir(home().join(".claude/projects"))
        .into_iter()
        .flatten()
        .flatten()
        .flat_map(|d| std::fs::read_dir(d.path()).into_iter().flatten().flatten())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .collect()
}

pub fn claude_meta(p: &Path) -> Meta {
    cached(p, |p| claude_meta_in(&peek(p)))
}

/// Title as Claude Code shows it: a name given with `/rename`, else its own title, else the
/// last prompt. Later lines win.
fn claude_meta_in(jsonl: &str) -> Meta {
    let (mut custom, mut ai, mut summary, mut last, mut first) = (None, None, None, None, None);
    let (mut cwd, mut headless) = (None, false);
    for line in jsonl.lines() {
        let field = |key: &str| serde_json::from_str::<Value>(line).ok().and_then(|v| v[key].as_str().and_then(one_line));
        if line.contains("\"type\":\"custom-title\"") {
            custom = field("customTitle").or(custom);
        } else if line.contains("\"type\":\"ai-title\"") {
            ai = field("aiTitle").or(ai);
        } else if line.contains("\"type\":\"summary\"") {
            summary = field("summary").or(summary);
        } else if line.contains("\"type\":\"last-prompt\"") {
            last = serde_json::from_str::<Value>(line).ok().and_then(|v| v["lastPrompt"].as_str().and_then(typed).and_then(one_line)).or(last);
        } else if (cwd.is_none() || first.is_none()) && line.contains("\"cwd\":") {
            let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
            if cwd.is_none() {
                cwd = v["cwd"].as_str().map(String::from);
                headless = v["entrypoint"].as_str().is_some_and(|e| e == "sdk-cli");
            }
            if first.is_none() && v["type"] == "user" && v["isMeta"] != true && v["isSidechain"] != true {
                first = claude_text(&v["message"]["content"]).as_deref().and_then(typed).and_then(one_line);
            }
        }
    }
    let prompted = first.is_some() || last.is_some();
    Meta { title: custom.or(ai).or(summary).or(last).or(first), cwd, hidden: headless || !prompted }
}

/// A message's text, whether a plain string or text blocks.
fn claude_text(content: &Value) -> Option<String> {
    if let Some(s) = content.as_str() {
        return Some(s.to_string());
    }
    let parts: Vec<&str> = content.as_array()?.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect();
    (!parts.is_empty()).then(|| parts.join("\n"))
}

/// The title of Claude conversation `session_id`, from its transcript.
pub fn claude_title(session_id: &str) -> Option<String> {
    claude_meta(&crate::transcript::claude_path(session_id)?).title
}

// ---- Codex ----

pub(crate) fn codex_rollouts() -> Vec<PathBuf> {
    let mut out = vec![];
    let mut stack = vec![home().join(".codex/sessions")];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("rollout-") && n.ends_with(".jsonl")) {
                out.push(p);
            }
        }
    }
    out
}

/// Session id from a rollout filename: `rollout-<timestamp>-<uuid>.jsonl`.
pub fn rollout_id(p: &Path) -> Option<String> {
    let stem = p.file_stem()?.to_str()?;
    let id = stem.get(stem.len().checked_sub(36)?..)?;
    (id.len() == 36 && id.chars().filter(|&c| c == '-').count() == 4).then(|| id.to_string())
}

/// Thread names from `~/.codex/session_index.jsonl` (what Codex lists); later lines win.
pub fn codex_titles() -> HashMap<String, String> {
    let text = std::fs::read_to_string(home().join(".codex/session_index.jsonl")).unwrap_or_default();
    codex_titles_in(&text)
}

fn codex_titles_in(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for v in text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
        if let (Some(id), Some(name)) = (v["id"].as_str(), v["thread_name"].as_str().and_then(one_line)) {
            out.insert(id.to_string(), name);
        }
    }
    out
}

pub fn codex_meta(p: &Path) -> Meta {
    cached(p, |p| {
        let len = p.metadata().map_or(0, |m| m.len());
        codex_meta_in(&read_range(p, 0, len.min(PEEK)).unwrap_or_default())
    })
}

/// Folder and kind from the first line (`session_meta`); a fallback title from the first prompt.
fn codex_meta_in(jsonl: &str) -> Meta {
    let mut lines = jsonl.lines();
    let meta = lines.next().and_then(|l| serde_json::from_str::<Value>(l).ok()).unwrap_or_default();
    let payload = &meta["payload"];
    // Subagents are `{"subagent": …}`; `exec` is headless.
    let hidden = meta["type"] != "session_meta" || payload["source"].is_object() || payload["source"] == "exec";
    let first = lines.filter(|l| l.contains("\"user_message\"") || l.contains("\"input_text\"")).find_map(|l| {
        let v = serde_json::from_str::<Value>(l).ok()?;
        let p = &v["payload"];
        let text = match p["type"].as_str()? {
            "user_message" => p["message"].as_str()?.to_string(),
            "message" if p["role"] == "user" => p["content"].as_array()?.iter().filter_map(|c| c["text"].as_str()).find_map(typed)?.to_string(),
            _ => return None,
        };
        typed(&text).and_then(one_line)
    });
    Meta { title: first, cwd: payload["cwd"].as_str().map(String::from), hidden }
}

/// "busy" while Codex is on a turn, else "idle", from the rollout's last turn event.
pub fn codex_status(rollout: &Path) -> Option<String> {
    let len = rollout.metadata().ok()?.len();
    codex_status_in(&read_range(rollout, len.saturating_sub(PEEK), len)?)
}

fn codex_status_in(jsonl: &str) -> Option<String> {
    let line = jsonl.lines().rev().find(|l| ["\"task_started\"", "\"task_complete\"", "\"turn_aborted\""].iter().any(|k| l.contains(k)))?;
    Some(if line.contains("\"task_started\"") { "busy" } else { "idle" }.into())
}

// ---- Listing ----

/// When `p` last changed, in seconds; 0 when it can't be read.
pub(crate) fn modified(p: &Path) -> u64 {
    stat(p).map_or(0, |s| s.0)
}

/// A conversation on disk that nothing is running.
pub(crate) fn recent(agent: &str, session_id: String, title: String, cwd: Option<String>, updated_at: u64) -> FoundSession {
    FoundSession {
        source: Source::Recent,
        agent: agent.into(),
        session_id,
        title,
        cwd,
        updated_at,
        pid: None,
        status: None,
        terminal: None,
        args: vec![],
        url: None,
        tmux: None,
    }
}

/// Every conversation on disk that isn't running (those are in `running`), newest first.
pub fn finished(running: &[FoundSession]) -> Vec<FoundSession> {
    let is_running = |id: &str| running.iter().any(|r| r.session_id == id);
    let mut out: Vec<FoundSession> = crate::agent::all().into_iter().flat_map(|a| a.recent(&is_running)).collect();
    out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    out
}

// ---- Reading ----

/// One entry of a conversation: `role` is "task" (what a subagent was asked), "user",
/// "assistant", "tool" (a call, in brief) or "note" (something that happened, like an
/// interruption).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Turn {
    pub role: String,
    pub text: String,
}

/// Part of a conversation, oldest first. `start` is where it begins in the file: pass it as
/// `before` for the part before it; 0 means this is the beginning.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct Page {
    pub turns: Vec<Turn>,
    pub start: u64,
    pub path: Option<String>,
}

/// The conversation of `agent`'s session `session_id`, the part ending at byte `before` (default:
/// the end). A Claude subagent's id reads its own transcript.
pub fn conversation(agent: &str, session_id: &str, before: Option<u64>) -> Option<Page> {
    let a = crate::agent::agent(agent)?;
    if let Some(p) = a.page(session_id, before) {
        return Some(p);
    }
    page(a, &a.transcript(session_id)?, before)
}

/// The part of `agent`'s transcript at `path` ending at byte `before` (default: the end).
pub fn page(agent: &dyn crate::agent::Agent, path: &Path, before: Option<u64>) -> Option<Page> {
    let len = path.metadata().ok()?.len();
    let end = before.unwrap_or(len).min(len);
    let start = end.saturating_sub(PAGE);
    let text = read_range(path, start, end)?;
    // The fragment dropped at the start belongs to the page before.
    let start = if start == 0 { 0 } else { end - text.len() as u64 };
    let turns = agent.turns(&text, path, start);
    Some(Page { turns, start, path: Some(path.display().to_string()) })
}

/// What the Claude subagent whose transcript is at `path` was asked, from the start of the file.
pub fn subagent_task(path: &Path) -> Option<String> {
    let len = path.metadata().ok()?.len();
    let head = read_range(path, 0, len.min(1 << 20))?;
    claude_turns(&head, Some(true)).into_iter().find(|t| t.role == "task").map(|t| t.text)
}

/// `~/.claude/projects/<dir>/<parent>/subagents/agent-<id>.jsonl`.
pub(crate) fn is_subagent(path: &Path) -> bool {
    path.parent().and_then(Path::file_name).is_some_and(|d| d == "subagents")
}

pub(crate) fn turn(role: &str, text: impl Into<String>) -> Turn {
    Turn { role: role.into(), text: text.into() }
}

/// Longer lines are tool results carrying images or files, never something shown.
const LONGEST_SHOWN_LINE: usize = 256 << 10;

/// A Claude transcript as turns. What Claude Code adds on its side (reminders, notes about its
/// setup, screenshots, tool results) is left out, and so are subagents' turns in their parent's
/// file. `subagent`: this is a subagent's own file; `Some(true)` when read from its start, where
/// its first message is its task (a fork's comes after the boilerplate that says it's a fork).
pub(crate) fn claude_turns(jsonl: &str, subagent: Option<bool>) -> Vec<Turn> {
    let mut out = vec![];
    let mut tasked = subagent != Some(true);
    for line in jsonl.lines().filter(|l| l.len() <= LONGEST_SHOWN_LINE) {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if v["isMeta"] == true || (subagent.is_none() && v["isSidechain"] == true) {
            continue;
        }
        let content = &v["message"]["content"];
        match v["type"].as_str() {
            Some("user") => {
                let blocks = content.as_array().into_iter().flatten().filter(|b| b["type"] == "text");
                for text in content.as_str().into_iter().chain(blocks.filter_map(|b| b["text"].as_str())) {
                    let text = text.trim();
                    if let Some((_, rest)) = text.split_once("</fork-boilerplate>") {
                        let rest = rest.trim();
                        out.push(turn("task", rest.strip_prefix("Your directive:").unwrap_or(rest).trim()));
                        tasked = true;
                    } else if text.starts_with("This session is being continued") {
                        out.push(turn("note", "Context compacted"));
                    } else if text.starts_with("[Request interrupted") {
                        out.push(turn("note", "Interrupted"));
                    } else if let Some(msg) = text.strip_prefix("The coordinator sent a message while you were working:") {
                        out.push(turn("user", msg.trim()));
                    } else if let Some(t) = typed(text) {
                        out.push(turn(if tasked { "user" } else { "task" }, t));
                        tasked = true;
                    }
                }
            }
            // Before its task, a fork's transcript repeats the call that started it.
            Some("assistant") if tasked => {
                for b in content.as_array().into_iter().flatten() {
                    match b["type"].as_str() {
                        Some("text") => {
                            if let Some(t) = b["text"].as_str().map(str::trim).filter(|t| !t.is_empty()) {
                                out.push(turn("assistant", t));
                            }
                        }
                        Some("tool_use") => out.push(turn("tool", format!("{}{}", b["name"].as_str().unwrap_or("tool"), hint(&b["input"])))),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    out.retain(|t| !t.text.is_empty());
    out
}

/// Codex writes each message twice: as an event (what its UI shows) and as a model item. Events
/// are used where the rollout has them.
pub(crate) fn codex_turns(jsonl: &str) -> Vec<Turn> {
    let values: Vec<Value> = jsonl.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    let events = values.iter().any(|v| v["type"] == "event_msg" && v["payload"]["type"] == "user_message");
    let mut out = vec![];
    for v in &values {
        let p = &v["payload"];
        match (v["type"].as_str(), p["type"].as_str()) {
            (Some("event_msg"), Some("user_message")) if events => {
                if let Some(t) = p["message"].as_str().and_then(typed) {
                    out.push(turn("user", t));
                }
            }
            (Some("event_msg"), Some("agent_message")) if events => {
                if let Some(t) = p["message"].as_str().map(str::trim).filter(|t| !t.is_empty()) {
                    out.push(turn("assistant", t));
                }
            }
            (Some("response_item"), Some("message")) if !events => {
                let role = match p["role"].as_str() {
                    Some("user") => "user",
                    Some("assistant") => "assistant",
                    _ => continue,
                };
                let text: Vec<&str> = p["content"].as_array().into_iter().flatten().filter_map(|c| c["text"].as_str()).filter_map(typed).collect();
                if !text.is_empty() {
                    out.push(turn(role, text.join("\n")));
                }
            }
            (Some("response_item"), Some("function_call")) => {
                let args = p["arguments"].as_str().and_then(|a| serde_json::from_str::<Value>(a).ok()).unwrap_or_default();
                out.push(turn("tool", format!("{}{}", p["name"].as_str().unwrap_or("tool"), hint(&args))));
            }
            (Some("response_item"), Some("custom_tool_call")) => {
                let input = p["input"].as_str().unwrap_or("");
                // apply_patch: the files it touches.
                let files: Vec<&str> = input.lines().filter_map(|l| l.strip_prefix("*** ")).filter(|l| l.contains(" File: ")).collect();
                let detail = if files.is_empty() { input.lines().next().unwrap_or("").to_string() } else { files.join(", ") };
                out.push(turn("tool", format!("{} {}", p["name"].as_str().unwrap_or("tool"), short(&detail))));
            }
            (Some("response_item"), Some("web_search_call")) => out.push(turn("tool", "web search")),
            _ => {}
        }
    }
    out
}

/// The telling argument of a tool call: the command, the file, the pattern.
pub(crate) fn hint(input: &Value) -> String {
    let arg = ["command", "cmd", "file_path", "path", "pattern", "description", "url", "query", "prompt"].iter().find_map(|k| match &input[*k] {
        Value::String(s) => Some(s.clone()),
        Value::Array(a) => Some(a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" ")),
        _ => None,
    });
    arg.map(|a| format!(" {}", short(&a))).unwrap_or_default()
}

pub(crate) fn short(s: &str) -> String {
    let line = s.lines().next().unwrap_or("");
    let cut: String = line.chars().take(120).collect();
    if cut.len() < line.len() { format!("{cut}…") } else { cut }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_titles_prefer_a_rename_then_its_own_title() {
        let head = [
            r#"{"type":"mode","mode":"normal","sessionId":"s"}"#,
            r#"{"type":"user","cwd":"/r","entrypoint":"cli","message":{"role":"user","content":"<command-name>/model</command-name>"},"isMeta":false}"#,
            r#"{"type":"user","cwd":"/r","entrypoint":"cli","message":{"role":"user","content":[{"type":"text","text":"fix the flaky test\nplease"}]}}"#,
        ];
        let meta = claude_meta_in(&head.join("\n"));
        assert_eq!(meta, Meta { title: Some("fix the flaky test".into()), cwd: Some("/r".into()), hidden: false });

        let more = [r#"{"type":"last-prompt","lastPrompt":"and the other one"}"#, r#"{"type":"ai-title","aiTitle":"Fix flaky tests"}"#];
        let jsonl = [&head[..], &more[..]].concat().join("\n");
        assert_eq!(claude_meta_in(&jsonl).title.as_deref(), Some("Fix flaky tests"));
        let renamed = format!("{jsonl}\n{}\n{}", r#"{"type":"custom-title","customTitle":"Flakes"}"#, r#"{"type":"ai-title","aiTitle":"Later"}"#);
        assert_eq!(claude_meta_in(&renamed).title.as_deref(), Some("Flakes"));
    }

    #[test]
    fn claude_hides_headless_and_empty_transcripts() {
        let print = r#"{"type":"user","cwd":"/r","entrypoint":"sdk-cli","message":{"content":"say hi"}}"#;
        assert!(claude_meta_in(print).hidden);
        let empty = r#"{"type":"mode","mode":"normal","sessionId":"s"}"#;
        assert!(claude_meta_in(empty).hidden);
    }

    #[test]
    fn codex_meta_skips_subagents_and_wrappers() {
        let sub = r#"{"type":"session_meta","payload":{"cwd":"/r","source":{"subagent":{"thread_spawn":{"parent_thread_id":"p"}}}}}"#;
        assert!(codex_meta_in(sub).hidden);
        assert!(codex_meta_in(r#"{"type":"session_meta","payload":{"cwd":"/r","source":"exec"}}"#).hidden);
        let jsonl = [
            r#"{"type":"session_meta","payload":{"cwd":"/r","source":"vscode"}}"#,
            r##"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"# AGENTS.md instructions for /r"}]}}"##,
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>x</environment_context>"}]}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"add a dark mode"}]}}"#,
        ]
        .join("\n");
        assert_eq!(codex_meta_in(&jsonl), Meta { title: Some("add a dark mode".into()), cwd: Some("/r".into()), hidden: false });
    }

    #[test]
    fn rollout_ids() {
        let p = Path::new("/x/rollout-2026-09-28T14-16-05-01a0e93b-2fcf-7a20-8efb-916be31ad524.jsonl");
        assert_eq!(rollout_id(p).as_deref(), Some("01a0e93b-2fcf-7a20-8efb-916be31ad524"));
    }

    #[test]
    fn codex_titles_later_lines_win() {
        let index = "{\"id\":\"a\",\"thread_name\":\"First\"}\n{\"id\":\"b\",\"thread_name\":\"Other\"}\n{\"id\":\"a\",\"thread_name\":\"Renamed\"}";
        let t = codex_titles_in(index);
        assert_eq!((t["a"].as_str(), t["b"].as_str()), ("Renamed", "Other"));
    }

    #[test]
    fn codex_busy_until_the_turn_ends() {
        let started = r#"{"type":"event_msg","payload":{"type":"task_started","turn_id":"t"}}"#;
        let done = r#"{"type":"event_msg","payload":{"type":"task_complete","turn_id":"t"}}"#;
        let aborted = r#"{"type":"event_msg","payload":{"type":"turn_aborted","reason":"interrupted"}}"#;
        let tokens = r#"{"type":"event_msg","payload":{"type":"token_count"}}"#;
        assert_eq!(codex_status_in(&[started, tokens].join("\n")).as_deref(), Some("busy"));
        assert_eq!(codex_status_in(&[started, done, tokens].join("\n")).as_deref(), Some("idle"));
        assert_eq!(codex_status_in(&[done, started, aborted].join("\n")).as_deref(), Some("idle"));
        assert_eq!(codex_status_in(tokens), None);
    }

    #[test]
    fn claude_conversation_reads_as_turns() {
        let jsonl = [
            r#"{"type":"user","message":{"role":"user","content":"fix the build"}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"hm"},{"type":"text","text":"Looking."},{"type":"tool_use","name":"Bash","input":{"command":"cargo build\nmore"}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"ok"}]}}"#,
            r#"{"type":"assistant","isSidechain":true,"message":{"content":[{"type":"text","text":"subagent"}]}}"#,
            r#"{"type":"user","isMeta":true,"message":{"content":"<local-command-caveat>"}}"#,
            r#"{"type":"user","message":{"content":[{"type":"text","text":"[Request interrupted by user for tool use]"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Fixed."}]}}"#,
        ]
        .join("\n");
        let turns: Vec<(String, String)> = claude_turns(&jsonl, None).into_iter().map(|t| (t.role, t.text)).collect();
        let want = [("user", "fix the build"), ("assistant", "Looking."), ("tool", "Bash cargo build"), ("note", "Interrupted"), ("assistant", "Fixed.")];
        assert_eq!(turns, want.map(|(r, t)| (r.to_string(), t.to_string())));
    }

    #[test]
    fn a_subagents_conversation() {
        let fork = [
            r#"{"type":"fork-context-ref","agentId":"a1"}"#,
            r#"{"type":"assistant","isSidechain":true,"message":{"content":[{"type":"tool_use","name":"Agent","input":{"description":"Polish"}}]}}"#,
            r#"{"type":"user","isSidechain":true,"message":{"content":[{"type":"tool_result","content":"started"},{"type":"text","text":"<fork-boilerplate>\nYou are a fork.\n</fork-boilerplate>\n\nYour directive: Fix the sidebar."}]}}"#,
            r#"{"type":"user","isSidechain":true,"message":{"content":"You've inherited the conversation context above"}}"#,
            r#"{"type":"assistant","isSidechain":true,"message":{"content":[{"type":"text","text":"On it."},{"type":"tool_use","name":"Bash","input":{"command":"swift build"}}]}}"#,
            r#"{"type":"user","isSidechain":true,"message":{"content":[{"type":"tool_result","content":"ok"}]}}"#,
            r#"{"type":"attachment","isSidechain":true}"#,
            r#"{"type":"user","isSidechain":true,"message":{"content":"[Image: original 3000x1716]"}}"#,
            r#"{"type":"user","isSidechain":true,"message":{"content":"This session is being continued from a previous conversation"}}"#,
            r#"{"type":"user","isSidechain":true,"message":{"content":"The coordinator sent a message while you were working: also the toolbar"}}"#,
            r#"{"type":"assistant","isSidechain":true,"message":{"content":[{"type":"text","text":"Done."}]}}"#,
        ]
        .join("\n");
        let all = claude_turns(&fork, Some(true));
        assert_eq!(all, vec![
            turn("task", "Fix the sidebar."),
            turn("assistant", "On it."),
            turn("tool", "Bash swift build"),
            turn("note", "Context compacted"),
            turn("user", "also the toolbar"),
            turn("assistant", "Done."),
        ]);
        // Read from somewhere in the middle, nothing is taken for the task.
        assert!(claude_turns(&fork.lines().skip(4).collect::<Vec<_>>().join("\n"), Some(false)).iter().all(|t| t.role != "task"));
        // In its parent's file, a subagent's turns aren't the conversation.
        assert!(claude_turns(&fork, None).is_empty());
        // A plain subagent's first message is its task.
        let plain = r#"{"type":"user","isSidechain":true,"message":{"content":"Find the bug"}}"#;
        assert_eq!(claude_turns(plain, Some(true)), vec![turn("task", "Find the bug")]);
    }

    #[test]
    fn codex_conversation_uses_events_once() {
        let jsonl = [
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]}}"#,
            r#"{"type":"event_msg","payload":{"type":"user_message","message":"hi"}}"#,
            r#"{"type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"pwd\"}"}}"#,
            r#"{"type":"response_item","payload":{"type":"custom_tool_call","name":"apply_patch","input":"*** Begin Patch\n*** Add File: a.md\n+x\n*** End Patch\n"}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"done"}]}}"#,
            r#"{"type":"event_msg","payload":{"type":"agent_message","message":"done"}}"#,
        ]
        .join("\n");
        let turns: Vec<(String, String)> = codex_turns(&jsonl).into_iter().map(|t| (t.role, t.text)).collect();
        let want = [("user", "hi"), ("tool", "exec_command pwd"), ("tool", "apply_patch Add File: a.md"), ("assistant", "done")];
        assert_eq!(turns, want.map(|(r, t)| (r.to_string(), t.to_string())));
    }

    #[test]
    fn pages_end_on_whole_lines() {
        let dir = std::env::temp_dir().join(format!("dino-history-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t.jsonl");
        std::fs::write(&p, "aaaa\nbbbb\ncccc\n").unwrap();
        assert_eq!(read_range(&p, 0, 15).as_deref(), Some("aaaa\nbbbb\ncccc\n"));
        assert_eq!(read_range(&p, 2, 15).as_deref(), Some("bbbb\ncccc\n"), "starts after the cut line");
        assert_eq!(read_range(&p, 0, 12).as_deref(), Some("aaaa\nbbbb\n"), "ends before the cut line");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
