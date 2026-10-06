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
pub(crate) fn read_range(p: &Path, from: u64, to: u64) -> Option<String> {
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

/// `~/.claude/projects/<dir>/<uuid>.jsonl`, not subagents' (`<uuid>/subagents/…`), in each of
/// Claude's config folders.
pub(crate) fn claude_transcripts() -> Vec<PathBuf> {
    crate::transcript::claude_projects()
        .flat_map(|d| std::fs::read_dir(d).into_iter().flatten().flatten())
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

// ---- Usage: what agents' own records say each answer used (see `usage`) ----

/// Ms since the epoch from a time as records keep it: RFC 3339, or a number of seconds or ms.
pub(crate) fn ms_of(v: &Value) -> Option<i64> {
    if let Some(n) = v.as_f64() {
        return Some(if n > 100_000_000_000.0 { n as i64 } else { (n * 1000.0) as i64 });
    }
    crate::usage::parse_time(v.as_str()?)
}

/// A token count, 0 when absent.
pub(crate) fn count(v: &Value) -> u64 {
    v.as_u64().or_else(|| v.as_f64().map(|f| f.max(0.0) as u64)).unwrap_or(0)
}

/// The first line of `p`: a record's header, read without the rest of it.
pub(crate) fn first_line(p: &Path) -> Option<String> {
    use std::io::BufRead;
    let mut line = String::new();
    std::io::BufReader::new(std::fs::File::open(p).ok()?).take(PEEK).read_line(&mut line).ok()?;
    Some(line)
}

/// The lines of `text`, which starts at byte `from` of its file, each with where it starts there.
pub(crate) fn lines_at(text: &str, from: u64) -> impl Iterator<Item = (u64, &str)> {
    let mut at = from;
    text.split_inclusive('\n').map(move |l| {
        let start = at;
        at += l.len() as u64;
        (start, l.trim_end_matches('\n'))
    })
}

/// Claude's transcripts and its subagents' (`<project>/<uuid>/subagents/*.jsonl`), whose
/// `sessionId` is the conversation that started them.
pub(crate) fn claude_usage_files() -> Vec<PathBuf> {
    let mut out = vec![];
    for project in crate::transcript::claude_projects() {
        for e in std::fs::read_dir(project).into_iter().flatten().flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "jsonl") {
                out.push(p);
            } else if p.is_dir() {
                let subs = std::fs::read_dir(p.join("subagents")).into_iter().flatten().flatten().map(|e| e.path());
                out.extend(subs.filter(|p| p.extension().is_some_and(|x| x == "jsonl")));
            }
        }
    }
    out
}

/// The answers in Claude transcript lines. One answer is written as a line per content block,
/// each with its usage: kept once, by its message and request ids (which also count a
/// conversation copied into another file once), with the most each count reached.
pub(crate) fn claude_usage_in(jsonl: &str) -> Vec<crate::usage::Used> {
    let mut out: Vec<crate::usage::Used> = vec![];
    // Answer id → where it is in `out`: a transcript has thousands.
    let mut at: HashMap<String, usize> = HashMap::new();
    for line in jsonl.lines().filter(|l| l.contains("\"assistant\"")) {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let m = &v["message"];
        let u = &m["usage"];
        if v["type"] != "assistant" || !u.is_object() || m["model"] == "<synthetic>" {
            continue;
        }
        let Some(mid) = m["id"].as_str() else { continue };
        let id = match v["requestId"].as_str() {
            Some(r) => format!("{mid}:{r}"),
            None => mid.to_string(),
        };
        let next = crate::usage::Used { undated: false,
            id,
            at_ms: ms_of(&v["timestamp"]).unwrap_or(0),
            conversation: v["sessionId"].as_str().unwrap_or_default().to_string(),
            cwd: v["cwd"].as_str().map(String::from),
            model: m["model"].as_str().map(String::from),
            input: count(&u["input_tokens"]),
            cache_read: count(&u["cache_read_input_tokens"]),
            cache_write: count(&u["cache_creation_input_tokens"]),
            output: count(&u["output_tokens"]),
        };
        match at.get(&next.id).map(|i| &mut out[*i]) {
            Some(o) => {
                o.input = o.input.max(next.input);
                o.cache_read = o.cache_read.max(next.cache_read);
                o.cache_write = o.cache_write.max(next.cache_write);
                o.output = o.output.max(next.output);
            }
            None if next.at_ms > 0 && !next.conversation.is_empty() => {
                at.insert(next.id.clone(), out.len());
                out.push(next);
            }
            None => {}
        }
    }
    out
}

/// The answers in Codex rollout `id`'s lines: each `token_count` event's last call. It repeats an
/// event now and then, the running total unchanged; that total names the call, so it counts
/// once. `model` and `cwd` carry what earlier lines said (`turn_context`) from one read to the next.
pub(crate) fn codex_usage_in(jsonl: &str, id: &str, model: &mut Option<String>, cwd: &mut Option<String>) -> Vec<crate::usage::Used> {
    let mut out = vec![];
    let mut seen = std::collections::HashSet::new();
    for line in jsonl.lines() {
        let context = line.contains("\"turn_context\"") || line.contains("\"session_meta\"");
        if !context && !line.contains("\"token_count\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let p = &v["payload"];
        match (v["type"].as_str(), p["type"].as_str()) {
            (Some("turn_context"), _) => {
                *model = p["model"].as_str().map(String::from).or(model.take());
                *cwd = p["cwd"].as_str().map(String::from).or(cwd.take());
            }
            (Some("session_meta"), _) => *cwd = p["cwd"].as_str().map(String::from).or(cwd.take()),
            (_, Some("token_count")) => {
                let (last, total) = (&p["info"]["last_token_usage"], &p["info"]["total_token_usage"]["total_tokens"]);
                let Some(total) = total.as_u64().filter(|t| *t > 0 && last.is_object()) else { continue };
                // OpenAI counts cached input inside input.
                let call = format!("{id}:{total}");
                if !seen.insert(call.clone()) {
                    continue;
                }
                let cached = count(&last["cached_input_tokens"]);
                out.push(crate::usage::Used { undated: false,
                    id: call,
                    at_ms: ms_of(&v["timestamp"]).unwrap_or(0),
                    conversation: id.to_string(),
                    cwd: cwd.clone(),
                    model: model.clone(),
                    input: count(&last["input_tokens"]).saturating_sub(cached),
                    cache_read: cached,
                    cache_write: count(&last["cache_write_input_tokens"]),
                    output: count(&last["output_tokens"]),
                });
            }
            _ => {}
        }
    }
    out
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

/// The name Codex gave the conversation in rollout `p`, from the `session_index.jsonl` of the
/// Codex home it's in (beside its `sessions`, whichever `CODEX_HOME` that Codex ran with).
pub fn codex_thread_name(p: &Path) -> Option<String> {
    let home = p.ancestors().find(|a| a.file_name().is_some_and(|n| n == "sessions"))?.parent()?;
    let text = std::fs::read_to_string(home.join("session_index.jsonl")).ok()?;
    codex_titles_in(&text).remove(&rollout_id(p)?)
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

/// The conversation a Claude transcript's first lines say it was forked from (`forkedFrom`, which
/// `/branch` puts on every entry it copies).
pub(crate) fn claude_forked_from(jsonl: &str) -> Option<String> {
    jsonl
        .lines()
        .take(50)
        .filter(|l| l.contains("\"forkedFrom\""))
        .find_map(|l| serde_json::from_str::<Value>(l).ok()?["forkedFrom"]["sessionId"].as_str().map(String::from))
}

/// The conversation a Codex rollout was forked from (`forked_from_id` in its `session_meta`).
pub fn codex_forked_from(rollout: &Path) -> Option<String> {
    codex_forked_from_in(&read_range(rollout, 0, PEEK)?)
}

fn codex_forked_from_in(jsonl: &str) -> Option<String> {
    let meta: Value = serde_json::from_str(jsonl.lines().next()?).ok()?;
    (meta["type"] == "session_meta").then(|| meta["payload"]["forked_from_id"].as_str().map(String::from)).flatten()
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
    let arg = ["command", "cmd", "file_path", "filePath", "path", "pattern", "description", "url", "query", "prompt"].iter().find_map(|k| match &input[*k] {
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

    /// As Claude Code 2.1.289's `/branch` and Codex 0.160's `/fork` write them (from real ones).
    #[test]
    fn a_fork_names_the_conversation_it_came_from() {
        let branch = concat!(
            r#"{"parentUuid":null,"type":"user","uuid":"9fa5","sessionId":"d26b","forkedFrom":{"sessionId":"0a87","messageUuid":"9fa5"}}"#,
            "\n",
            r#"{"type":"assistant","uuid":"1fa0","sessionId":"d26b"}"#,
            "\n"
        );
        assert_eq!(claude_forked_from(branch).as_deref(), Some("0a87"));
        // `--fork-session` copies the entries without saying where from; a plain one never says.
        assert_eq!(claude_forked_from(r#"{"type":"user","uuid":"9d08","sessionId":"e46f"}"#), None);
        let rollout = r#"{"type":"session_meta","payload":{"id":"01a1-7bd0","forked_from_id":"01a1-5eb6","forked_from_ordinal_exclusive":13,"source":"cli"}}
{"type":"event_msg","payload":{"type":"task_started"}}"#;
        assert_eq!(codex_forked_from_in(rollout).as_deref(), Some("01a1-5eb6"));
        assert_eq!(codex_forked_from_in(r#"{"type":"session_meta","payload":{"id":"01a1"}}"#), None);
    }

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

    #[test]
    fn claude_answers_count_once_with_their_final_usage() {
        let jsonl = r#"{"type":"user","sessionId":"s1","timestamp":"2026-10-03T23:04:30.000Z","message":{"role":"user","content":"hi"}}
{"type":"assistant","sessionId":"s1","cwd":"/r","timestamp":"2026-10-03T23:04:33.255Z","requestId":"req_1","message":{"id":"msg_1","model":"claude-x","usage":{"input_tokens":2,"cache_creation_input_tokens":640,"cache_read_input_tokens":0,"output_tokens":1}}}
{"type":"assistant","sessionId":"s1","cwd":"/r","timestamp":"2026-10-03T23:04:34.424Z","requestId":"req_1","message":{"id":"msg_1","model":"claude-x","usage":{"input_tokens":2,"cache_creation_input_tokens":640,"cache_read_input_tokens":0,"output_tokens":182}}}
{"type":"assistant","sessionId":"s1","cwd":"/r","timestamp":"2026-10-03T23:05:00.000Z","message":{"id":"x","model":"<synthetic>","usage":{"input_tokens":0,"output_tokens":0}}}
{"type":"assistant","sessionId":"s1","cwd":"/r","timestamp":"2026-10-03T23:06:00.000Z","requestId":"req_2","message":{"id":"msg_2","model":"claude-x","usage":{"input_tokens":5,"cache_read_input_tokens":640,"output_tokens":7}}}"#;
        let used = claude_usage_in(jsonl);
        assert_eq!(used.len(), 2, "one per answer, not per line; nothing for a synthetic one");
        assert_eq!((used[0].id.as_str(), used[0].conversation.as_str(), used[0].cwd.as_deref()), ("msg_1:req_1", "s1", Some("/r")));
        assert_eq!((used[0].input, used[0].cache_write, used[0].output), (2, 640, 182));
        assert_eq!(used[0].at_ms, crate::usage::parse_time("2026-10-03T23:04:33.255Z").unwrap());
        assert_eq!((used[1].cache_read, used[1].output, used[1].model.as_deref()), (640, 7, Some("claude-x")));
    }

    #[test]
    fn codex_calls_come_from_token_counts_once_each() {
        let head = r#"{"timestamp":"2026-10-04T00:46:19.760Z","type":"session_meta","payload":{"id":"r1","cwd":"/r"}}
{"timestamp":"2026-10-04T00:46:20.019Z","type":"turn_context","payload":{"model":"gpt-x","cwd":"/r"}}
{"timestamp":"2026-10-04T00:46:25.145Z","type":"event_msg","payload":{"type":"token_count","info":null}}
{"timestamp":"2026-10-04T00:46:25.145Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"total_tokens":1006},"last_token_usage":{"input_tokens":1000,"cached_input_tokens":400,"output_tokens":6}}}}
{"timestamp":"2026-10-04T00:46:25.300Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"total_tokens":1006},"last_token_usage":{"input_tokens":1000,"cached_input_tokens":400,"output_tokens":6}}}}"#;
        let (mut model, mut cwd) = (None, None);
        let used = codex_usage_in(head, "r1", &mut model, &mut cwd);
        assert_eq!(used.len(), 1, "a repeated count is the same call");
        assert_eq!((used[0].input, used[0].cache_read, used[0].output), (600, 400, 6), "cached input is inside input");
        assert_eq!((used[0].id.as_str(), used[0].model.as_deref(), used[0].cwd.as_deref()), ("r1:1006", Some("gpt-x"), Some("/r")));
        // The next read has only new lines: the model carries over.
        let more = r#"{"timestamp":"2026-10-04T00:47:00.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"total_tokens":2100},"last_token_usage":{"input_tokens":1090,"output_tokens":4}}}}"#;
        let used = codex_usage_in(more, "r1", &mut model, &mut cwd);
        assert_eq!((used[0].id.as_str(), used[0].model.as_deref(), used[0].input), ("r1:2100", Some("gpt-x"), 1090));
    }

    #[test]
    fn record_times_read_in_any_form() {
        assert_eq!(ms_of(&serde_json::json!(1790739705969u64)), Some(1790739705969));
        assert_eq!(ms_of(&serde_json::json!(101.5)), Some(101500));
        assert_eq!(ms_of(&serde_json::json!("1970-01-01T00:00:01Z")), Some(1000));
        assert_eq!(lines_at("a\nbc\nd", 10).collect::<Vec<_>>(), [(10, "a"), (12, "bc"), (15, "d")]);
    }
}
