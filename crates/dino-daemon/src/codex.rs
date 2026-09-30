//! Codex's own record of where it is, read without adding anything to Codex: the rollout it
//! writes each turn to (`~/.codex/sessions/…/rollout-<time>-<id>.jsonl`), named exactly by the
//! file its process has open, and the notices (OSC 9) it puts on its terminal when it waits on
//! the user. Both hold with routing off, when no model call passes through dino.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use dino_core::history;
use dino_proxy::Activity;

use super::{Daemon, Session};

/// Flags that make Codex say on its terminal when it waits on the user (an approval, a
/// question), focused or not. It notices a finished turn too, which its rollout says anyway.
pub(crate) const NOTICE_ARGS: [&str; 6] =
    ["-c", "tui.notifications=true", "-c", "tui.notification_method=\"osc9\"", "-c", "tui.notification_condition=\"always\""];

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
    reported: Option<Activity>,
    /// The file's modification time when its context was last read, and what it said.
    read: Option<(SystemTime, Option<(u64, u64)>)>,
}

/// Look at every Codex session on this Mac: which conversation it's on, and where its turn is.
pub(crate) fn watch(d: &Daemon) {
    let sessions: Vec<_> = d.sessions.lock().unwrap().iter().filter(|s| s.agent_id == "codex" && s.host.is_none() && !s.pane.is_exited()).cloned().collect();
    for s in sessions {
        track(d, &s);
    }
}

fn track(d: &Daemon, s: &Session) {
    let mut r = s.rollout.lock().unwrap();
    // Until its first prompt makes one, look every poll: a short first turn is over in seconds.
    if r.path.is_none() || r.looked.is_none_or(|t| t.elapsed() >= RELOOK) {
        r.looked = Some(Instant::now());
        if let Some(path) = s.pane.pid().and_then(open_rollout).filter(|p| r.path.as_ref() != Some(p)) {
            // Its first prompt, or another conversation: pick up where that one is.
            *s.agent_session.lock().unwrap() = history::rollout_id(&path);
            r.turn = history::codex_status(&path).as_deref() == Some("busy");
            r.offset = path.metadata().map_or(0, |m| m.len());
            r.needs = None;
            r.notice = None;
            r.read = None;
            r.path = Some(path);
        }
    }
    let Some(path) = r.path.clone() else { return };
    for kind in new_events(&path, &mut r.offset) {
        match kind.as_str() {
            "task_started" => {
                r.turn = true;
                r.needs = None;
            }
            "task_complete" | "turn_aborted" => {
                r.turn = false;
                r.needs = None;
                r.notice = None;
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
            r.notice = None;
        } else if at.elapsed() >= SETTLE {
            r.needs = Some(text);
            r.notice = None;
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
}

/// The conversation Codex process `pid` is on: the rollout it has open. A subagent's is open too
/// while it runs; the session's own is the one that isn't a subagent's.
fn open_rollout(pid: u32) -> Option<PathBuf> {
    dino_core::procinfo::open_files(pid)
        .into_iter()
        .map(PathBuf::from)
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("rollout-") && n.ends_with(".jsonl")))
        .find(|p| !history::codex_meta(p).hidden)
}

/// The conversation session `s` is on, looked at now: for saving it before the watcher has.
pub(crate) fn conversation(s: &Session) -> Option<String> {
    s.pane.pid().and_then(open_rollout).and_then(|p| history::rollout_id(&p))
}

/// The event types (`payload.type`) of the whole lines written since `offset`, moving it past them.
fn new_events(path: &Path, offset: &mut u64) -> Vec<String> {
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
    buf[..end].split(|&b| b == b'\n').filter_map(|l| serde_json::from_slice::<serde_json::Value>(l).ok()).filter_map(|v| event_kind(&v)).collect()
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
        assert_eq!(new_events(&p, &mut offset), ["session_meta", "task_started"]);
        assert_eq!(new_events(&p, &mut offset), Vec::<String>::new());
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        std::io::Write::write_all(&mut f, b"load\":{\"type\":\"task_complete\"}}\n").unwrap();
        assert_eq!(new_events(&p, &mut offset), ["task_complete"]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
