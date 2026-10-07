//! CodeWhale (`codewhale`, formerly DeepSeek-TUI, whose older installs are `deepseek-tui`). It
//! keeps each conversation as one JSON document, `$CODEWHALE_HOME/sessions/<id>.json` (default
//! `~/.codewhale`, with the old `~/.deepseek` still read), rewritten whole as it goes, and while a
//! turn runs a crash-recovery copy of it in `sessions/checkpoints/<id>.json`, dropped when the turn
//! ends however it ends. dino asks those files where a turn is; an approval it waits on is only on
//! its screen. Its hooks live in its own config.toml, which dino leaves alone. It can't be given a
//! conversation id up front, so dino claims the first one it starts in that folder; `--resume`
//! continues one, in the model it was saved with whatever `--model` says, and without `--yolo`
//! (`--approval-policy` holds): so a running session's model and Full Access can't be changed
//! from dino, and one resumed is shown on its own model, asking. Its models are what
//! `codewhale models` lists for its provider, offline.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::Deserialize;
use serde_json::Value;

use super::{Agent, ControlKind, StatusSource, Wiring, strings};
use crate::controls::Controls;
use crate::found::{self, FoundSession, Source};
use crate::history::{self, Meta, Page, Turn, one_line, turn};
use crate::models::{Catalog, ModelInfo};
use crate::providers::Format;

pub(crate) struct CodeWhale;

/// Its process names: the command (`codew` is its short name), and the old one.
const NAMES: &[&str] = &["codewhale", "codew", "deepseek-tui"];

fn home_dir() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

/// Where it keeps its files: `$CODEWHALE_HOME`, else `~/.codewhale`.
fn codewhale_home() -> PathBuf {
    std::env::var_os("CODEWHALE_HOME").map(PathBuf::from).unwrap_or_else(|| home_dir().join(".codewhale"))
}

/// Its conversation folders, newest name first: the old `~/.deepseek` is still read, unless
/// `$CODEWHALE_HOME` says where to look.
fn sessions_dirs() -> Vec<PathBuf> {
    let mut out = vec![codewhale_home().join("sessions")];
    if std::env::var_os("CODEWHALE_HOME").is_none() {
        out.push(home_dir().join(".deepseek/sessions"));
    }
    out
}

/// It has run on this Mac: asking it anything before then makes its folder.
fn has_run() -> bool {
    codewhale_home().exists() || (std::env::var_os("CODEWHALE_HOME").is_none() && home_dir().join(".deepseek").exists())
}

/// Every conversation file, the newer folder's copy of one in both.
fn documents() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = vec![];
    for dir in sessions_dirs() {
        for p in std::fs::read_dir(&dir).into_iter().flatten().flatten().map(|e| e.path()) {
            let is_doc = p.extension().is_some_and(|x| x == "json") && p.file_stem().is_some_and(|s| s.to_string_lossy().contains('-'));
            if is_doc && !out.iter().any(|o| o.file_name() == p.file_name()) {
                out.push(p);
            }
        }
    }
    out
}

fn document(id: &str) -> Option<PathBuf> {
    if id.is_empty() || id.contains('/') {
        return None;
    }
    sessions_dirs().into_iter().map(|d| d.join(format!("{id}.json"))).find(|p| p.is_file())
}

/// The copy it keeps while a turn runs.
fn checkpoint(id: &str) -> Option<PathBuf> {
    if id.is_empty() || id.contains('/') {
        return None;
    }
    sessions_dirs().into_iter().map(|d| d.join("checkpoints").join(format!("{id}.json"))).find(|p| p.is_file())
}

/// The part of a conversation file that says what it is.
#[derive(Deserialize, Default, Clone)]
#[serde(default)]
struct Head {
    id: String,
    title: String,
    /// The model it runs on, and will again when resumed.
    model: String,
    created_at: String,
    workspace: String,
    archived: bool,
    /// A sub-agent's conversation, not one someone had.
    spawn_depth: u32,
}

#[derive(Deserialize)]
struct Doc {
    metadata: Head,
}

fn head_in(text: &str) -> Option<Head> {
    serde_json::from_str::<Doc>(text).ok().map(|d| d.metadata).filter(|h| !h.id.is_empty())
}

/// It writes `metadata` first, before the conversation: read from the start of the file, not all
/// of a long one.
fn head_at_start(start: &str) -> Option<Head> {
    let key = start.find("\"metadata\"")?;
    let value = start[key + "\"metadata\"".len()..].trim_start().strip_prefix(':')?;
    let mut de = serde_json::Deserializer::from_str(value);
    Head::deserialize(&mut de).ok().filter(|h| !h.id.is_empty())
}

/// How much of a file its metadata is looked for in.
const HEAD: u64 = 256 << 10;

fn head(p: &Path) -> Option<Head> {
    use std::io::Read;
    let mut buf = vec![];
    std::fs::File::open(p).ok()?.take(HEAD).read_to_end(&mut buf).ok()?;
    head_at_start(&String::from_utf8_lossy(&buf)).or_else(|| head_in(&std::fs::read_to_string(p).ok()?))
}

/// Remembered until the file changes: title, folder, and hidden when it's archived or a
/// sub-agent's.
fn meta(p: &Path) -> Meta {
    history::cached(p, |p| match head(p) {
        Some(h) => Meta { title: one_line(&h.title), cwd: (!h.workspace.is_empty()).then(|| h.workspace.clone()), hidden: h.archived || h.spawn_depth > 0 },
        None => Meta { hidden: true, ..Meta::default() },
    })
}

/// Seconds since the epoch of an RFC 3339 time in UTC, as it writes them
/// ("2026-10-04T03:54:22.806675Z").
fn epoch(t: &str) -> Option<u64> {
    let (date, time) = t.split_once('T')?;
    let mut d = date.splitn(3, '-').map(|x| x.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let time = time.trim_end_matches('Z');
    let time = time.split(['+', '-']).next()?;
    let mut hms = time.splitn(3, ':');
    let (h, min) = (hms.next()?.parse::<i64>().ok()?, hms.next()?.parse::<i64>().ok()?);
    let s = hms.next()?.split('.').next()?.parse::<i64>().ok()?;
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + h * 3600 + min * 60 + s).ok()
}

fn mtime(p: &Path) -> Option<u64> {
    p.metadata().ok()?.modified().ok()?.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs())
}

/// Whether conversation `id` is on a turn in a process started at `since`: its turn's copy exists,
/// written since then. One left by a process that was killed mid-turn is older.
fn on_turn(id: &str, since: u64) -> bool {
    checkpoint(id).is_some_and(|p| written_since(&p, since))
}

fn written_since(p: &Path, since: u64) -> bool {
    mtime(p).is_some_and(|t| t >= since)
}

/// The model the newest of `files` written since `since` says the conversation is on.
fn model_written(files: &[PathBuf], since: u64) -> Option<String> {
    let newest = files.iter().filter(|p| written_since(p, since)).max_by_key(|p| mtime(p))?;
    head(newest).map(|h| h.model).filter(|m| !m.is_empty())
}

/// Folders compared as the same folder, whatever links lead to them.
fn same_dir(a: &str, b: &Path) -> bool {
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    canon(Path::new(a)) == canon(b)
}

/// The conversation begun in `cwd` since `since`, the first not in `claimed`. Only files written
/// since are read.
fn begun(cwd: &Path, since: u64, claimed: &[String]) -> Option<(String, PathBuf)> {
    documents()
        .into_iter()
        .filter(|p| mtime(p).is_some_and(|t| t + 1 >= since))
        .filter_map(|p| Some((head(&p)?, p)))
        .filter(|(h, _)| !claimed.contains(&h.id) && h.spawn_depth == 0 && same_dir(&h.workspace, cwd))
        .filter_map(|(h, p)| Some((epoch(&h.created_at)?, h.id, p)))
        .filter(|(created, ..)| created + 1 >= since)
        .min_by_key(|(created, ..)| *created)
        .map(|(_, id, p)| (id, p))
}

/// The conversation `--resume` (`-r`, `--session-id`, or the `resume` command) names in `args`.
fn resumed_in(args: &[String]) -> Option<&str> {
    let i = args.iter().position(|a| matches!(a.as_str(), "--resume" | "-r" | "--session-id" | "resume"))?;
    args.get(i + 1).map(String::as_str).filter(|v| !v.starts_with('-'))
}

/// The conversation live process `pid` (run with `args`) is on: the one it was told to resume, by
/// its id or the start of it, else the one it began in its folder since it started.
fn conversation_in(pid: u32, args: &[String]) -> Option<(String, PathBuf)> {
    if let Some(id) = resumed_in(args) {
        if let Some(p) = document(id) {
            return Some((id.to_string(), p));
        }
        let mut found = documents().into_iter().filter(|p| p.file_stem().is_some_and(|s| s.to_string_lossy().starts_with(id)));
        return match (found.next(), found.next()) {
            (Some(p), None) => Some((p.file_stem()?.to_string_lossy().into_owned(), p)),
            _ => None,
        };
    }
    let cwd = crate::procinfo::cwd_of(pid)?;
    begun(Path::new(&cwd), crate::procinfo::started(pid).unwrap_or(0), &[])
}

/// The text a person typed in a message, without what it adds itself (`<turn_meta>`).
fn typed_in(content: &Value) -> Option<String> {
    let parts: Vec<&str> = match content {
        Value::String(s) => vec![s.as_str()],
        Value::Array(parts) => parts.iter().filter(|p| p["type"] == "text").filter_map(|p| p["text"].as_str()).filter_map(history::typed).collect(),
        _ => vec![],
    };
    (!parts.is_empty()).then(|| parts.join("\n"))
}

/// The tools its last message calls (an assistant's `tool_use` parts), while their results
/// aren't in yet: the calls out now.
fn tools_in(text: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<Value>(text) else { return vec![] };
    let Some(last) = v["messages"].as_array().and_then(|m| m.last()) else { return vec![] };
    if last["role"] != "assistant" {
        return vec![];
    }
    last["content"].as_array().into_iter().flatten().filter(|p| p["type"] == "tool_use").filter_map(|p| p["name"].as_str().map(String::from)).collect()
}

/// `tools_in` of the turn's copy, read again only when it changes: it's looked at twice a second.
fn tools_out(id: &str) -> Vec<String> {
    static LAST: std::sync::Mutex<Option<(PathBuf, std::time::SystemTime, u64, Vec<String>)>> = std::sync::Mutex::new(None);
    let Some(p) = checkpoint(id) else { return vec![] };
    let Some((modified, len)) = p.metadata().ok().and_then(|m| Some((m.modified().ok()?, m.len()))) else { return vec![] };
    let mut last = LAST.lock().unwrap();
    if let Some((_, _, _, tools)) = last.as_ref().filter(|(q, m, l, _)| *q == p && *m == modified && *l == len) {
        return tools.clone();
    }
    let tools = std::fs::read_to_string(&p).map(|t| tools_in(&t)).unwrap_or_default();
    *last = Some((p, modified, len, tools.clone()));
    tools
}

/// A conversation document's messages as turns.
fn turns_in(text: &str) -> Vec<Turn> {
    let Ok(v) = serde_json::from_str::<Value>(text) else { return vec![] };
    let mut out = vec![];
    for m in v["messages"].as_array().into_iter().flatten() {
        let content = &m["content"];
        match m["role"].as_str() {
            Some("user") => out.extend(typed_in(content).map(|t| turn("user", t))),
            Some("assistant") => {
                let text: Vec<&str> = content.as_array().into_iter().flatten().filter(|p| p["type"] == "text").filter_map(|p| p["text"].as_str()).collect();
                let text = text.join("\n");
                if !text.trim().is_empty() {
                    out.push(turn("assistant", text.trim()));
                }
                for call in content.as_array().into_iter().flatten().filter(|p| p["type"] == "tool_use") {
                    out.push(turn("tool", format!("{}{}", call["name"].as_str().unwrap_or("tool"), history::hint(&call["input"]))));
                }
            }
            _ => {}
        }
    }
    out
}

/// Its approval dialog, as it draws it: what it asks to do, in a few words.
fn question_in(screen: &str) -> Option<String> {
    let lines: Vec<&str> = screen.lines().map(str::trim).collect();
    let at = lines.iter().rposition(|l| l.starts_with("APPROVAL"))?;
    let rest = &lines[at..];
    // Its way out: answering it is the only thing left to do on that screen.
    if !rest.iter().any(|l| l.contains("Abort the turn") || l.contains("Deny this call")) {
        return None;
    }
    let tool = rest[0].trim_start_matches("APPROVAL").trim();
    let command = rest.iter().position(|l| *l == "Command:").and_then(|i| rest.get(i + 1)).filter(|c| !c.is_empty());
    Some(match command {
        Some(c) => format!("Run: {}?", history::short(c)),
        None if !tool.is_empty() => format!("Use {tool}?"),
        None => "Approve a tool call?".into(),
    })
}

/// `codewhale models`: its provider's catalog, the default marked `*`.
fn catalog_in(list: &str) -> Option<Catalog> {
    let mut models = vec![];
    let mut default_model = None;
    for line in list.lines() {
        let (mark, id) = match (line.strip_prefix("* "), line.strip_prefix("  ")) {
            (Some(id), _) => (true, id),
            (_, Some(id)) => (false, id),
            _ => continue,
        };
        let id = id.trim();
        if id.is_empty() || id.contains(char::is_whitespace) {
            continue;
        }
        if mark {
            default_model = Some(id.to_string());
        }
        models.push(ModelInfo { id: id.into(), label: id.into(), efforts: vec![], default_effort: None, group: None, aliases: vec![] });
    }
    (!models.is_empty()).then_some(Catalog { models, default_model, ..Catalog::default() })
}

impl CodeWhale {
    fn found(&self, pid: u32, args: &[String]) -> FoundSession {
        let mut s = found::by_hand("codewhale", pid);
        s.cwd = crate::procinfo::cwd_of(pid);
        s.title = "CodeWhale".into();
        if let Some((id, p)) = conversation_in(pid, args) {
            s.title = meta(&p).title.unwrap_or_else(|| "CodeWhale".into());
            s.updated_at = history::modified(&p);
            s.status = Some(if on_turn(&id, crate::procinfo::started(pid).unwrap_or(0)) { "busy" } else { "idle" }.into());
            s.session_id = id;
        }
        s
    }
}

impl Agent for CodeWhale {
    fn id(&self) -> &'static str {
        "codewhale"
    }

    // Its permission postures. Plan mode has no flag: it's picked inside it (Tab).
    fn modes(&self) -> &'static [&'static str] {
        &["ask", "auto", "bypass"]
    }

    fn mode_label(&self, mode: &str) -> Option<&'static str> {
        Some(match mode {
            "ask" => "Ask",
            "auto" => "Auto-Review",
            "bypass" => "Full Access",
            _ => return None,
        })
    }

    fn mode_args(&self, mode: &str) -> Vec<String> {
        match mode {
            "auto" => strings(&["--approval-policy", "auto"]),
            "bypass" => strings(&["--yolo"]),
            // Said, so a posture saved in its own settings doesn't stand in for it.
            _ => strings(&["--approval-policy", "on-request"]),
        }
    }

    fn model_args(&self, model: &str) -> Vec<String> {
        strings(&["--model", model])
    }

    // Thinking is set in its config (`reasoning_effort`) or inside it (Ctrl+T), not on its
    // command line.
    fn effort_args(&self, _effort: &str) -> Vec<String> {
        vec![]
    }

    fn value_flags(&self) -> &'static [&'static str] {
        &[
            "--config", "--profile", "--provider", "--model", "--output-mode", "--verbosity", "--log-level", "--telemetry", "--approval-policy", "--sandbox-mode", "--api-key",
            "--base-url", "-C", "--workspace", "-w", "-r", "--resume", "--session-id", "-p", "--prompt", "--set", "--max-subagents",
        ]
    }

    fn control_of(&self, name: &str, _value: Option<&str>) -> Option<ControlKind> {
        match name {
            "--yolo" | "--approval-policy" => Some(ControlKind::Mode),
            "--model" => Some(ControlKind::Model),
            _ => None,
        }
    }

    fn read_mode(&self, flags: &[(&str, Option<&str>)]) -> Option<String> {
        let mode = match flags.last()? {
            ("--yolo", _) => "bypass",
            (_, Some("auto")) => "auto",
            (_, Some("on-request" | "untrusted" | "suggest")) => "ask",
            _ => return None,
        };
        Some(mode.into())
    }

    fn catalog_sources(&self) -> Vec<PathBuf> {
        vec![codewhale_home().join("config.toml")]
    }

    fn catalog(&self, program: &str) -> Option<Catalog> {
        if !has_run() {
            return None;
        }
        let out = std::process::Command::new(program).arg("models").stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).output().ok()?;
        catalog_in(&String::from_utf8_lossy(&out.stdout))
    }

    // On its own, nothing: which provider it talks to is its own setting, and dino reads its
    // files for status.
    fn wiring(&self, _route: bool, _base: &dyn Fn(&str) -> String, _status_line: Option<String>) -> Wiring {
        (vec![], vec![])
    }

    fn provider_formats(&self) -> &'static [Format] {
        &[Format::Chat, Format::Anthropic]
    }

    // Its OpenAI-compatible or Anthropic provider, pointed at dino for this process; its config,
    // and the keys it keeps, stay as they are (it sends a saved key only to that key's own
    // endpoint, and none to a local address).
    fn provider_wiring(&self, url: &str, format: Format, model: &str) -> Option<Wiring> {
        let (provider, var, base) = match format {
            Format::Chat => ("openai", "OPENAI_BASE_URL", format!("{url}/v1")),
            Format::Anthropic => ("anthropic", "ANTHROPIC_BASE_URL", url.to_string()),
            Format::Responses => return None,
        };
        let env = [("CODEWHALE_PROVIDER", provider.to_string()), ("CODEWHALE_BASE_URL", base.clone()), (var, base), ("CODEWHALE_MODEL", model.to_string())];
        Some((env.map(|(k, v)| (k.to_string(), v)).into(), vec![]))
    }

    fn session_args(&self, session: &mut Option<String>, restoring: bool) -> (Vec<String>, Vec<String>) {
        match session {
            Some(id) if restoring => (vec!["--resume".into(), id.clone()], vec![]),
            _ => (vec![], vec![]),
        }
    }

    fn resume_keeps(&self) -> &'static [&'static str] {
        &["model", "bypass"]
    }

    // Its own model, and no Full Access: `--yolo` is dropped on resume. Asking, said, so a
    // posture in its settings doesn't stand in for it.
    fn resumed(&self, session: &str, c: Controls) -> Controls {
        let saved = document(session).and_then(|p| head(&p)).map(|h| h.model).filter(|m| !m.is_empty());
        let mode = if c.mode.as_deref() == Some("bypass") { Some("ask".to_string()) } else { c.mode };
        Controls { model: saved.or(c.model), mode, ..c }
    }

    fn status_source(&self) -> StatusSource {
        StatusSource::Polled
    }

    fn turn_now(&self, session: &str, since: u64) -> Option<bool> {
        document(session).or_else(|| checkpoint(session))?;
        Some(on_turn(session, since))
    }

    fn asking(&self, screen: &str) -> Option<String> {
        question_in(screen)
    }

    fn asks_on_screen(&self) -> bool {
        true
    }

    fn tools_now(&self, session: &str) -> Vec<String> {
        tools_out(session)
    }

    // The copy it saves as a prompt goes out, then the conversation as the turn ends: a model
    // picked in it shows from its next prompt. What it resumes with, too (see `resumed`).
    fn model_now(&self, session: &str, since: u64) -> Option<String> {
        model_written(&[checkpoint(session), document(session)].into_iter().flatten().collect::<Vec<_>>(), since)
    }

    fn new_conversation(&self, cwd: &Path, since: u64, claimed: &[String]) -> Option<String> {
        begun(cwd, since, claimed).map(|(id, _)| id)
    }

    fn busy(&self, pid: u32) -> Option<bool> {
        let (id, _) = conversation_in(pid, &found::args_of(pid))?;
        Some(on_turn(&id, crate::procinfo::started(pid).unwrap_or(0)))
    }

    fn portable_flags(&self, args: &[String]) -> Vec<String> {
        found::drop_flags(args, &["-r", "--resume", "--session-id", "-p", "--prompt", "--api-key", "--base-url"], &["-c", "--continue", "--fresh"])
    }

    fn headless(&self, args: &[String]) -> bool {
        super::runs_with(args, &[], &["exec", "web", "update", "completion", "doctor"])
    }

    fn may_be(&self, comm: &str) -> bool {
        comm.rsplit('/').next().is_some_and(|n| NAMES.contains(&n))
    }

    fn running(&self, procs: &crate::procinfo::Procs) -> Vec<FoundSession> {
        let mut out = vec![];
        for pid in NAMES.iter().flat_map(|n| crate::procinfo::named_in(procs, n)) {
            let mut s = self.found(pid, &found::args_of(pid));
            if s.session_id.is_empty() || out.iter().any(|o: &FoundSession| o.session_id == s.session_id) {
                continue;
            }
            let (terminal, flags) = found::terminal_and_flags(self, pid);
            s.terminal = terminal;
            s.args = flags;
            out.push(s);
        }
        out
    }

    fn inside(&self, pid: u32, comm: &str, args: &dyn Fn() -> Vec<String>) -> Option<FoundSession> {
        if !self.may_be(comm) {
            return None;
        }
        let args = args();
        let mut s = self.found(pid, &args);
        s.source = Source::Running;
        s.args = self.portable_flags(&args);
        Some(s)
    }

    fn recent(&self, leave_out: &dyn Fn(&str, u64) -> bool) -> Vec<FoundSession> {
        let mut out = vec![];
        for p in documents() {
            let Some(id) = p.file_stem().map(|s| s.to_string_lossy().into_owned()) else { continue };
            let updated = history::modified(&p);
            if leave_out(&id, updated) {
                continue;
            }
            let meta = meta(&p);
            if meta.hidden {
                continue;
            }
            let Some(title) = meta.title else { continue };
            out.push(history::recent("codewhale", id, title, meta.cwd, updated));
        }
        out
    }

    // No cloud work of its own.
    fn cloud_args(&self, _session_id: &str) -> Vec<String> {
        vec![]
    }

    fn transcript(&self, session_id: &str) -> Option<PathBuf> {
        document(session_id)
    }

    // A whole document; a part of one isn't JSON.
    // Its document keeps no answer's usage nor time, only the conversation's: each new answer
    // counts as a call to its model, at the time the document was last saved, with no tokens.
    fn usage(&self, seen: &mut crate::usage::Seen) -> Vec<crate::usage::Used> {
        let mut out = vec![];
        for p in documents() {
            if !seen.changed(&p) {
                continue;
            }
            let Some(id) = p.file_stem().map(|s| s.to_string_lossy().into_owned()) else { continue };
            let text = std::fs::read_to_string(&p).unwrap_or_default();
            seen.remember(&p);
            let key = format!("codewhale:{id}");
            let before: usize = seen.mark(&key).and_then(|m| m.parse().ok()).unwrap_or(0);
            let (found, answers) = usage_in(&text, before);
            out.extend(found);
            seen.set_mark(&key, answers.to_string());
        }
        out
    }

    fn turns(&self, text: &str, _path: &Path, _start: u64) -> Vec<Turn> {
        turns_in(text)
    }

    // All of it at once, from the copy of a turn still running when there is one: it's newer.
    fn page(&self, session_id: &str, _before: Option<u64>) -> Option<Page> {
        let doc = document(session_id);
        let newest = match (checkpoint(session_id), &doc) {
            (Some(c), Some(d)) if mtime(&c) >= mtime(d) => Some(c),
            (Some(c), None) => Some(c),
            _ => doc.clone(),
        }?;
        let turns = turns_in(&std::fs::read_to_string(&newest).ok()?);
        Some(Page { turns, start: 0, path: doc.map(|d| d.display().to_string()) })
    }
}

/// The answers in a document past the first `before`, and how many it has. One shortened since
/// (compacted) counts from where it is now.
fn usage_in(text: &str, before: usize) -> (Vec<crate::usage::Used>, usize) {
    #[derive(Deserialize)]
    struct Message {
        role: String,
    }
    #[derive(Deserialize)]
    struct Whole {
        metadata: Head,
        #[serde(default)]
        messages: Vec<Message>,
    }
    let Ok(doc) = serde_json::from_str::<Whole>(text) else { return (vec![], before) };
    let h = doc.metadata;
    let answers = doc.messages.iter().filter(|m| m.role == "assistant").count();
    let at = crate::usage::parse_time(&h.created_at).map(|c| c.max(0)).unwrap_or(0);
    let updated = serde_json::from_str::<Value>(text).ok().and_then(|v| history::ms_of(&v["metadata"]["updated_at"])).unwrap_or(at);
    let out = (before.min(answers)..answers)
        .map(|i| crate::usage::Used {
            id: format!("{}:{i}", h.id),
            at_ms: updated,
            conversation: h.id.clone(),
            cwd: (!h.workspace.is_empty()).then(|| h.workspace.clone()),
            model: (!h.model.is_empty()).then(|| h.model.clone()),
            undated: true,
            ..Default::default()
        })
        .collect();
    (out, answers)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed from CodeWhale 0.10.0's own document of a session: a prompt, an answer, then a
    /// prompt that ran a command after asking.
    const DOC: &str = r#"{"schema_version":1,"metadata":{"id":"c148a117-1281-4a4b-9549-466f0477296b","runtime_store":{"data_dir":"/h/.codewhale/sessions/ac744056/runtime"},"title":"Reply with PELICAN","created_at":"2026-10-04T03:54:22.806675Z","updated_at":"2026-10-04T03:55:35.366314Z","message_count":8,"total_tokens":48,"model":"fake-coder","model_provider":"openai","workspace":"/r/proj","mode":"agent","cost":{"session_cost_usd":0.0},"cumulative_turn_secs":0,"spawn_depth":0},
"messages":[
{"role":"user","content":[{"type":"text","text":"Reply with PELICAN"},{"type":"text","text":"<turn_meta>\nCurrent local date: 2026-10-03\nCurrent workspace: /r/proj\n</turn_meta>"}]},
{"role":"assistant","content":[{"type":"text","text":"PELICAN"}]},
{"role":"user","content":[{"type":"text","text":"TOOL CMD[touch hello.txt]"},{"type":"text","text":"<turn_meta>\nCurrent local date: 2026-10-03\n</turn_meta>"}]},
{"role":"assistant","content":[{"type":"tool_use","id":"call_1","name":"bash","input":{"command":"touch hello.txt"}}]},
{"role":"user","content":[{"type":"tool_result","tool_use_id":"call_1","content":"[approval] This tool call required approval and was approved by the user before execution.\n\n(no output)"}]},
{"role":"assistant","content":[{"type":"text","text":"DONE"}]}],
"journal":{"entries":[]},"leaf_id":"e2246fb5","system_prompt":"You are Codewhale, an agent"}"#;

    #[test]
    fn its_document_reads_as_turns() {
        let turns: Vec<(String, String)> = turns_in(DOC).into_iter().map(|t| (t.role, t.text)).collect();
        let want = [("user", "Reply with PELICAN"), ("assistant", "PELICAN"), ("user", "TOOL CMD[touch hello.txt]"), ("tool", "bash touch hello.txt"), ("assistant", "DONE")];
        assert_eq!(turns, want.map(|(r, t)| (r.to_string(), t.to_string())));
        // DeepSeek-TUI 0.8 put its note first.
        let old = r#"{"metadata":{"id":"a6e09c50","title":"t","workspace":"/r"},"messages":[{"role":"user","content":[{"type":"text","text":"<turn_meta>\nCurrent local date: 2026-10-03\n</turn_meta>"},{"type":"text","text":"Reply with PELICAN"}]}]}"#;
        assert_eq!(turns_in(old)[0].text, "Reply with PELICAN");
        assert!(turns_in(&DOC[..200]).is_empty(), "part of a document isn't one");
    }

    #[test]
    fn its_turns_copy_says_which_tools_are_out() {
        assert!(tools_in(DOC).is_empty(), "answered");
        let i = DOC.find(r#"{"role":"user","content":[{"type":"tool_result""#).unwrap();
        let mid = format!("{}]}}", DOC[..i].trim_end().trim_end_matches(','));
        assert_eq!(tools_in(&mid), ["bash"], "the call out");
    }

    #[test]
    fn what_a_document_says_about_itself() {
        let h = head_in(DOC).unwrap();
        assert_eq!((h.id.as_str(), h.title.as_str(), h.workspace.as_str()), ("c148a117-1281-4a4b-9549-466f0477296b", "Reply with PELICAN", "/r/proj"));
        assert_eq!(epoch(&h.created_at), Some(1_791_086_062));
        assert_eq!(epoch("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(epoch("2000-03-01T12:30:15+00:00"), Some(951_913_815));
        assert_eq!(epoch("garbage"), None);
        assert!(head_in(r#"{"metadata":{}}"#).is_none());
        // From the start of the file alone.
        let cut = &DOC[..DOC.find(r#""messages""#).unwrap() + 20];
        assert_eq!(head_at_start(cut).map(|h| h.title), Some("Reply with PELICAN".into()));
    }

    /// Its approval dialog, as 0.10.0 draws it, and screens that aren't one.
    #[test]
    fn its_approval_on_screen() {
        let approval = "▎ TOOL CMD[touch hello.txt]\n⣄ ▶ run running (3s) · Ctrl+B → /jobs\n────\n   APPROVAL   bash\n  Command:\n    touch hello.txt\n  Save:   1 ask rule\n    tool=exec_shell command=touch hello.txt\n  Do you want to proceed?\n  [1 / y] Allow once\n  [2 / a] Allow for this session (this kind)\n❯ [3 / d / n] Deny this call\n  [Esc] Abort the turn";
        assert_eq!(question_in(approval).as_deref(), Some("Run: touch hello.txt?"));
        assert_eq!(question_in("   APPROVAL   write\n  Do you want to proceed?\n  [Esc] Abort the turn").as_deref(), Some("Use write?"));
        assert_eq!(question_in("╭ ✓ ▶ run Done · touch hello.txt\n│ ▏ output: [approval] This tool call required approval\n● DONE"), None);
        assert_eq!(question_in("▎ what does APPROVAL mean here?\n● It's a dialog."), None);
    }

    #[test]
    fn models_from_its_catalog() {
        let list = "deepseek models (default: deepseek-flash)\nsource=provider_models\tstatus={\"state\":\"unknown\"}\tfetched_at=unknown\nBundled or configured models; availability not verified.\n* deepseek-flash\n  deepseek-v4-flash\n  deepseek-v4-pro\nCached catalog; run `codewhale models --update` to refresh configured providers.\n";
        let c = catalog_in(list).unwrap();
        assert_eq!(c.default_model.as_deref(), Some("deepseek-flash"));
        assert_eq!(c.models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["deepseek-flash", "deepseek-v4-flash", "deepseek-v4-pro"]);
        assert!(catalog_in("error: no provider\n").is_none());
    }

    #[test]
    fn modes_both_ways() {
        let c = CodeWhale;
        for m in ["ask", "auto", "bypass"] {
            let args = c.mode_args(m);
            assert_eq!(c.read_mode(&[(args[0].as_str(), args.get(1).map(String::as_str))]).as_deref(), Some(m), "{m}");
        }
        assert_eq!(c.read_mode(&[("--approval-policy", Some("never"))]), None, "a posture dino has no word for");
    }

    #[test]
    fn resumed_it_asks_and_keeps_its_model() {
        let c = Controls { mode: Some("bypass".into()), model: Some("deepseek-v4-pro".into()), effort: None };
        let r = CodeWhale.resumed("not-on-this-mac", c);
        assert_eq!((r.mode.as_deref(), r.model.as_deref()), (Some("ask"), Some("deepseek-v4-pro")), "--yolo doesn't survive a resume");
        let auto = CodeWhale.resumed("not-on-this-mac", Controls { mode: Some("auto".into()), ..Controls::default() });
        assert_eq!(auto.mode.as_deref(), Some("auto"), "--approval-policy does");
        assert_eq!(CodeWhale.resumed("x", Controls::default()), Controls::default());
        assert_eq!(CodeWhale.resume_keeps(), ["model", "bypass"]);
    }

    #[test]
    fn continuing_drops_what_picks_the_session() {
        let args: Vec<String> = ["--resume", "c148a117", "--model", "deepseek-v4-pro", "--yolo", "--fresh"].iter().map(|s| s.to_string()).collect();
        assert_eq!(CodeWhale.portable_flags(&args), ["--model", "deepseek-v4-pro", "--yolo"]);
        assert_eq!(resumed_in(&args), Some("c148a117"));
        assert_eq!(resumed_in(&["resume".into(), "abc".into()]), Some("abc"));
        assert_eq!(resumed_in(&["--model".into(), "m".into()]), None);
    }

    #[test]
    fn its_provider_for_a_session_in_its_environment() {
        let url = "http://127.0.0.1:5000/s/7/local/ollama";
        let (env, args) = CodeWhale.provider_wiring(url, Format::Chat, "qwen3:4b").unwrap();
        let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        assert_eq!(get("CODEWHALE_PROVIDER"), Some("openai"));
        assert_eq!(get("CODEWHALE_BASE_URL"), Some("http://127.0.0.1:5000/s/7/local/ollama/v1"));
        assert_eq!(get("CODEWHALE_MODEL"), Some("qwen3:4b"));
        assert!(args.is_empty(), "the URL (and the proxy's secret in it) stays off its command line");
        let (env, _) = CodeWhale.provider_wiring(url, Format::Anthropic, "m").unwrap();
        assert!(env.contains(&("ANTHROPIC_BASE_URL".into(), url.into())) && env.contains(&("CODEWHALE_PROVIDER".into(), "anthropic".into())));
        assert!(CodeWhale.provider_wiring(url, Format::Responses, "m").is_none());
    }

    #[test]
    fn a_turn_is_its_checkpoint_written_since_the_process_started() {
        let dir = std::env::temp_dir().join(format!("dino-codewhale-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("c148a117-1281-4a4b-9549-466f0477296b.json");
        std::fs::write(&p, DOC).unwrap();
        let now = std::time::SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        assert!(written_since(&p, now));
        assert!(!written_since(&p, now + 60), "left by a process killed mid-turn");
        assert!(!written_since(&dir.join("gone.json"), 0));
        let m = meta(&p);
        assert_eq!((m.title.as_deref(), m.cwd.as_deref(), m.hidden), (Some("Reply with PELICAN"), Some("/r/proj"), false));
        std::fs::write(&p, DOC.replace(r#""spawn_depth":0"#, r#""spawn_depth":10"#)).unwrap();
        assert!(meta(&p).hidden, "a sub-agent's (read again: it changed size)");
        // The model it's on: picked in it (its next prompt's copy), as written since it started.
        let copy = dir.join("checkpoint.json");
        std::fs::write(&copy, DOC.replace(r#""model":"fake-coder""#, r#""model":"deepseek-v4-pro""#)).unwrap();
        assert_eq!(model_written(&[p.clone(), copy.clone()], now).as_deref(), Some("deepseek-v4-pro"), "the newest");
        assert_eq!(model_written(std::slice::from_ref(&p), now).as_deref(), Some("fake-coder"));
        assert_eq!(model_written(&[p, copy], now + 60), None, "nothing written since: what it was started with");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn each_new_answer_counts_as_a_call() {
        let (used, n) = usage_in(DOC, 0);
        assert_eq!(n, 3);
        assert_eq!(used.len(), 3);
        assert_eq!((used[0].conversation.as_str(), used[0].model.as_deref(), used[0].cwd.as_deref()), ("c148a117-1281-4a4b-9549-466f0477296b", Some("fake-coder"), Some("/r/proj")));
        assert_eq!(used[0].at_ms, crate::usage::parse_time("2026-10-04T03:55:35.366314Z").unwrap());
        assert!(usage_in(DOC, 3).0.is_empty(), "nothing new");
    }
}
