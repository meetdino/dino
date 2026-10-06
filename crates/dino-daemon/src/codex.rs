//! Codex's own record of where it is, read without adding anything to Codex: the rollout it
//! writes each turn to (`~/.codex/sessions/…/rollout-<time>-<id>.jsonl`), named exactly by the
//! file its process has open, and the notices (OSC 9) it puts on its terminal when it waits on
//! the user. Both hold with routing off, when no model call passes through dino. Codex 0.160.1
//! runs its conversations in a background server its terminals share, and the process in the
//! terminal has no rollout open: its rollout is then the one of the conversation dino knows it's on,
//! else the one begun for it as it started (see `Agent::new_conversation`).

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use dino_core::agent::codex::open_rollout;
use dino_core::history;
use dino_proxy::Activity;

use super::{Daemon, Session};

/// How Codex words an approval notice.
const APPROVAL: &str = "Approval requested: ";

/// A notice that isn't an approval counts once the turn goes on this long without ending:
/// Codex notices a finished turn just before writing it down.
const SETTLE: Duration = Duration::from_millis(1500);

/// How often to look again at which rollout the process has open: `/new` and `/resume` switch it.
const RELOOK: Duration = Duration::from_secs(3);

/// What a Codex session's rollout has said so far.
#[derive(Default)]
pub(crate) struct Rollout {
    pub(crate) path: Option<PathBuf>,
    looked: Option<Instant>,
    /// Read up to here.
    offset: u64,
    /// On a turn: started and not yet complete or aborted.
    turn: bool,
    /// Notices seen on the terminal, and the last one not yet counted.
    notices: u64,
    notice: Option<(Instant, String)>,
    /// What it waits on the user for, while it does.
    needs: Option<String>,
    /// That's an approval, in a dialog on its screen.
    approval: bool,
    reported: Option<Activity>,
    /// Its calls that reach the Mac or a browser and haven't answered yet: (call id, tool).
    calls: Vec<(String, String)>,
    /// The file's modification time when its context was last read, and what it said.
    read: Option<(SystemTime, Option<(u64, u64)>)>,
    /// What its conversation is called, as Codex's records say: the name Codex gave it, else its
    /// first prompt. Its terminal's title is only its folder.
    pub(crate) title: Option<String>,
}

/// Look at every Codex session on this Mac (a shell's typed there among them): which conversation
/// it's on, and where its turn is.
pub(crate) fn watch(d: &Daemon) {
    let sessions: Vec<_> = d.sessions.lock().unwrap().iter().filter(|s| super::watched(s) && !s.pane.is_exited()).cloned().collect();
    let claimed: Vec<(String, String)> = sessions.iter().filter_map(|s| Some((s.id.clone(), s.agent_session.lock().unwrap().clone()?))).collect();
    for s in sessions {
        let others: Vec<String> = claimed.iter().filter(|(id, _)| *id != s.id).map(|(_, c)| c.clone()).collect();
        // Another conversation: followed once its rollout's lock is let go (a fork made in Codex
        // starts a session for the original).
        if let Some((known, now)) = track(d, &s, &others) {
            super::fork::moved(d, &s, known, now);
        }
    }
}

/// The rollout Codex process `pid` of session `s` is on: the one it has open, else (its
/// conversations in Codex's shared server) the one of the conversation dino knows it's on, or
/// the one begun for it as it started in its folder, but `claimed`. Only a rollout it has open says
/// it moved to another.
fn rollout_of(a: &dyn dino_core::agent::Agent, s: &Session, pid: u32, followed: bool, claimed: &[String]) -> Option<PathBuf> {
    if let Some(open) = open_rollout(pid) {
        return Some(open);
    }
    if followed {
        return None;
    }
    let known = s.agent_session.lock().unwrap().clone();
    let id = known.or_else(|| a.new_conversation(&s.agent_cwd(), dino_core::procinfo::started(pid)?, claimed))?;
    dino_core::agent::codex::rollout_path(&id)
}

/// The conversation it was on and the one it's on now, when it has moved to another.
fn track(d: &Daemon, s: &Session, claimed: &[String]) -> Option<(Option<String>, String)> {
    let mut moved = None;
    let a = s.adapter()?;
    let pid = s.agent_pid()?;
    // The model it says it's on, as of the lines read now.
    let mut model = None;
    let mut r = s.rollout.lock().unwrap();
    // Until its first prompt makes one, look every poll: a short first turn is over in seconds.
    let relook = r.path.is_none() || r.looked.is_none_or(|t| t.elapsed() >= RELOOK);
    if relook {
        r.looked = Some(Instant::now());
        let followed = r.path.is_some();
        if let Some(path) = rollout_of(a, s, pid, followed, claimed).filter(|p| r.path.as_ref() != Some(p)) {
            // Its first prompt, or another conversation: pick up where that one is.
            let known = s.agent_session.lock().unwrap().clone();
            moved = history::rollout_id(&path).filter(|now| known.as_ref() != Some(now)).map(|now| (known, now));
            r.turn = history::codex_status(&path).as_deref() == Some("busy");
            r.offset = path.metadata().map_or(0, |m| m.len());
            // One begun since it started (its first prompt, `/new`, a fork) says what it's been
            // on so far: a `/model` before that first prompt included. One it resumed says what an
            // earlier run was on.
            if dino_core::procinfo::started(pid).is_some_and(|since| born(&path) + 1 >= since) {
                let mut from = r.offset.saturating_sub(MODEL_PEEK);
                model = new_events(&path, &mut from).iter().filter_map(|v| a.log_model(v)).last();
            }
            r.needs = None;
            r.notice = None;
            r.read = None;
            r.calls.clear();
            r.path = Some(path);
        }
    }
    let Some(path) = r.path.clone() else { return moved };
    if relook {
        r.title = history::codex_thread_name(&path).or_else(|| history::codex_meta(&path).title);
    }
    for v in new_events(&path, &mut r.offset) {
        model = a.log_model(&v).or(model);
        if let Some(call) = tool_call(&v) {
            count(&mut r, call, |tool, phase| d.proxy.stats.tool_call(&s.id, tool, phase));
        }
        let Some(kind) = event_kind(&v) else { continue };
        match kind.as_str() {
            "task_started" => {
                r.turn = true;
                r.needs = None;
            }
            "task_complete" | "turn_aborted" => {
                r.turn = false;
                r.needs = None;
                r.notice = None;
                // An interrupted call never writes its output.
                r.calls.clear();
            }
            // Bookkeeping, written whether or not the user answered.
            "token_count" | "token_usage_record" | "world_state" | "turn_context" => {}
            // Anything else it does means it's no longer waiting.
            _ => r.needs = None,
        }
    }
    let seen = s.pane.shared.notices.load(std::sync::atomic::Ordering::Relaxed);
    if seen > r.notices {
        r.notices = seen;
        r.notice = s.pane.shared.notice.lock().unwrap().clone().map(|text| (Instant::now(), text));
    }
    if let Some((at, text)) = r.notice.clone() {
        if !r.turn {
            r.notice = None;
        } else if let Some(command) = text.strip_prefix(APPROVAL) {
            r.needs = Some(command.to_string());
            r.approval = true;
            r.notice = None;
        } else if at.elapsed() >= SETTLE {
            r.needs = Some(text);
            r.notice = None;
        }
    }
    // Answering its approval dialog writes nothing to the rollout until the command is done: the
    // dialog gone from its screen says it (see `question_answered`).
    if r.needs.is_none() {
        r.approval = false;
    } else if r.approval && let Some(what) = r.needs.clone() {
        let quiet = s.last_write.lock().unwrap().is_none_or(|t| t.elapsed() > super::TURN_OVER_QUIET);
        if super::question_answered(s, "codex", &what, r.notices, quiet) != super::Answer::Waiting {
            r.needs = None;
            r.approval = false;
        }
    }
    let now = match (&r.needs, r.turn) {
        (Some(what), _) => Activity::NeedsPermission(what.clone()),
        (None, true) => Activity::Working,
        (None, false) => Activity::Done,
    };
    if r.reported.as_ref() != Some(&now) {
        d.proxy.stats.report(&s.id, now.clone());
        r.reported = Some(now);
    }
    if let Some(m) = model {
        d.proxy.stats.report_model(&s.id, m);
    }
    moved
}

/// How much of the end of a rollout begun since its agent started is read for the model it's on:
/// all of one that new, but never a whole long conversation.
const MODEL_PEEK: u64 = 512 << 10;

/// When `path` was made, in seconds since the epoch; 0 when that can't be read.
fn born(path: &Path) -> u64 {
    let made = path.metadata().and_then(|m| m.created()).ok();
    made.and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs())
}

/// The whole lines written since `offset`, moving it past them.
fn new_events(path: &Path, offset: &mut u64) -> Vec<serde_json::Value> {
    let Ok(mut f) = std::fs::File::open(path) else { return vec![] };
    let len = f.metadata().map_or(0, |m| m.len());
    if len < *offset {
        // Rewritten: start over from its end.
        *offset = len;
    }
    if len == *offset || f.seek(SeekFrom::Start(*offset)).is_err() {
        return vec![];
    }
    let mut buf = Vec::new();
    if f.take(len - *offset).read_to_end(&mut buf).is_err() {
        return vec![];
    }
    // A line still being written waits for the next look.
    let Some(end) = buf.iter().rposition(|&b| b == b'\n') else { return vec![] };
    *offset += end as u64 + 1;
    buf[..end].split(|&b| b == b'\n').filter_map(|l| serde_json::from_slice::<serde_json::Value>(l).ok()).collect()
}

/// `event_msg` lines by their payload type; other lines by their own.
fn event_kind(v: &serde_json::Value) -> Option<String> {
    let t = v["type"].as_str()?;
    Some(if t == "event_msg" { v["payload"]["type"].as_str()?.to_string() } else { t.to_string() })
}

/// What the rollout last said about the context window; re-read only when the file changes.
pub(crate) fn context(s: &Session) -> Option<(u64, u64)> {
    let mut r = s.rollout.lock().unwrap();
    let path = r.path.clone()?;
    let modified = path.metadata().and_then(|m| m.modified()).ok()?;
    match r.read {
        Some((at, context)) if at == modified => context,
        _ => {
            let context = dino_core::transcript::codex_context(&path);
            r.read = Some((modified, context));
            context
        }
    }
}

/// What a Node REPL call that drives Codex's browser has in its code: the plugin's browser client
/// and the `agent.browser` API it sets up.
const BROWSER_SCRIPT: &[&str] = &["browser-client.mjs", "agent.browser"];

/// What a rollout line says about a tool call: the model asked for one (`response_item`
/// `function_call`; an MCP tool's name is under its `namespace`), or one's answer is in
/// (`function_call_output`, by call id; older Codex also wrote `mcp_tool_call_end`).
#[derive(Debug, PartialEq)]
enum Call {
    Started { id: String, name: String },
    Ended { id: String },
}

fn tool_call(v: &serde_json::Value) -> Option<Call> {
    let p = &v["payload"];
    let id = || p["call_id"].as_str().unwrap_or_default().to_string();
    match (v["type"].as_str()?, p["type"].as_str()?) {
        ("response_item", "function_call" | "custom_tool_call") => {
            let namespace = p["namespace"].as_str().unwrap_or_default();
            let name = p["name"].as_str()?;
            // Its browser plugins (in-app browser, Chrome) have no server of their own: their
            // skill drives the browser with scripts it runs in the Node REPL tool.
            let browser = namespace.trim_end_matches('_') == "mcp__node_repl" && p["arguments"].as_str().is_some_and(|a| BROWSER_SCRIPT.iter().any(|m| a.contains(m)));
            let name = match namespace {
                _ if browser => "mcp__browser-use__js".to_string(),
                "" => name.to_string(),
                // `mcp__computer_use__` (0.12x) or `mcp__open_computer_use` (0.160).
                ns => format!("{}__{name}", ns.trim_end_matches('_')),
            };
            Some(Call::Started { id: id(), name })
        }
        ("response_item", "function_call_output" | "custom_tool_call_output") | ("event_msg", "mcp_tool_call_end") => Some(Call::Ended { id: id() }),
        _ => None,
    }
}

/// Codex's calls that reach the Mac or a browser, by call id, as dino's stats count them: each
/// is counted out once and back once, however many lines name its end.
fn count(r: &mut Rollout, call: Call, mut note: impl FnMut(&str, dino_proxy::computer::Phase)) {
    use dino_proxy::computer::{Phase, reach_of};
    match call {
        Call::Started { id, name } => {
            if reach_of(&name, true).is_some() {
                note(&name, Phase::Started);
                r.calls.push((id, name));
            }
        }
        Call::Ended { id } => {
            if let Some(i) = r.calls.iter().position(|c| c.0 == id) {
                let (_, name) = r.calls.remove(i);
                note(&name, Phase::Ended);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_events_reads_whole_lines_once() {
        let dir = std::env::temp_dir().join(format!("dino-codex-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("rollout-x.jsonl");
        std::fs::write(&p, "{\"type\":\"session_meta\",\"payload\":{}}\n{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\"}}\n{\"type\":\"event_msg\",\"pay").unwrap();
        let mut offset = 0;
        let kinds = |vs: Vec<serde_json::Value>| vs.iter().filter_map(event_kind).collect::<Vec<_>>();
        assert_eq!(kinds(new_events(&p, &mut offset)), ["session_meta", "task_started"]);
        assert_eq!(kinds(new_events(&p, &mut offset)), Vec::<String>::new());
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        std::io::Write::write_all(&mut f, b"load\":{\"type\":\"task_complete\"}}\n").unwrap();
        assert_eq!(kinds(new_events(&p, &mut offset)), ["task_complete"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn its_computer_use_calls_are_read_as_it_writes_them() {
        use dino_proxy::computer::{Phase, Reach, reach_of};
        let line = |s: &str| serde_json::from_str::<serde_json::Value>(s).unwrap();
        let run = |lines: &[&str]| {
            let mut r = Rollout::default();
            let mut seen = vec![];
            for l in lines {
                if let Some(c) = tool_call(&line(l)) {
                    count(&mut r, c, |t, p| seen.push((t.to_string(), p)));
                }
            }
            seen
        };
        // Codex 0.160 with open-computer-use, as it wrote them: the call, then its output.
        let new = run(&[
            r#"{"type":"response_item","payload":{"type":"function_call","id":"fc_1","name":"list_apps","namespace":"mcp__open_computer_use","arguments":"{}","call_id":"c1"}}"#,
            r#"{"type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{}","call_id":"c2"}}"#,
            r#"{"type":"response_item","payload":{"type":"function_call_output","call_id":"c2","output":"ok"}}"#,
            r#"{"type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":"Google Chrome"}}"#,
        ]);
        assert_eq!(new, [("mcp__open_computer_use__list_apps".to_string(), Phase::Started), ("mcp__open_computer_use__list_apps".to_string(), Phase::Ended)]);
        assert_eq!(reach_of(&new[0].0, false), Some(Reach::Computer));
        // Codex 0.12x and its Computer Use plugin: the end said twice, counted once.
        let old = run(&[
            r#"{"type":"response_item","payload":{"type":"function_call","name":"list_apps","namespace":"mcp__computer_use__","arguments":"{}","call_id":"call_1"}}"#,
            r#"{"type":"event_msg","payload":{"type":"mcp_tool_call_end","call_id":"call_1","invocation":{"server":"computer-use","tool":"list_apps","arguments":{}}}}"#,
            r#"{"type":"response_item","payload":{"type":"function_call_output","call_id":"call_1","output":"x"}}"#,
        ]);
        assert_eq!(old.len(), 2);
        assert_eq!(reach_of(&old[0].0, false), Some(Reach::Computer));
        // Its browser, through the Node REPL (as the Codex app wrote it); other REPL work isn't.
        let browse = run(&[r#"{"type":"response_item","payload":{"type":"function_call","name":"js","namespace":"mcp__node_repl__","arguments":"{\"title\":\"Check browser tabs\",\"code\":\"const openTabs = await agent.browser.user.openTabs();\"}","call_id":"b1"}}"#]);
        assert_eq!(reach_of(&browse[0].0, false), Some(Reach::Browser));
        assert!(run(&[r#"{"type":"response_item","payload":{"type":"function_call","name":"js","namespace":"mcp__node_repl__","arguments":"{\"code\":\"1+1\"}","call_id":"b2"}}"#]).is_empty());
    }
}
