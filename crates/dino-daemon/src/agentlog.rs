//! Where an agent's turn is, read from the record it keeps of its conversation: each new whole
//! line of a file it appends to as it goes (Kimi Code's `wire.jsonl`, Pi's session file), or its
//! store asked each time (Hermes's database), as its adapter reads them. Nothing is added to the
//! agent, and it holds with routing off.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use dino_core::agent::{Agent, LogEvent, StatusSource, agent};
use dino_proxy::Activity;

use super::{Daemon, Session};

/// What a session's record has said so far.
#[derive(Default)]
pub(crate) struct Log {
    /// The conversation followed, once known.
    conversation: Option<String>,
    path: Option<PathBuf>,
    /// Read up to here.
    offset: u64,
    /// On a turn: started and not yet ended.
    turn: bool,
    /// What it waits on the user for, while it does.
    needs: Option<String>,
    reported: Option<Activity>,
}

fn followed(s: &Session) -> bool {
    s.host.is_none() && agent(&s.agent_id).is_some_and(|a| matches!(a.status_source(), StatusSource::Log | StatusSource::Polled))
}

/// Look at every session whose agent keeps such a record.
pub(crate) fn watch(d: &Daemon) {
    let sessions: Vec<_> = d.sessions.lock().unwrap().iter().filter(|s| followed(s)).cloned().collect();
    let claimed: Vec<(String, String)> =
        sessions.iter().filter_map(|s| Some((s.id.clone(), s.agent_session.lock().unwrap().clone()?))).collect();
    for s in sessions.iter().filter(|s| !s.pane.is_exited()) {
        let others: Vec<String> = claimed.iter().filter(|(id, _)| *id != s.id).map(|(_, c)| c.clone()).collect();
        track(d, s, &others);
    }
}

fn track(d: &Daemon, s: &Session, claimed: &[String]) {
    let Some(a) = agent(&s.agent_id) else { return };
    let mut l = s.log.lock().unwrap();
    let Some(since) = s.pane.pid().and_then(dino_core::procinfo::started) else { return };
    if l.conversation.is_none() {
        let known = s.agent_session.lock().unwrap().clone();
        l.conversation = match known {
            Some(id) => Some(id),
            // An agent that can't be told its conversation id starts one with its first prompt.
            None => {
                let Some(id) = a.new_conversation(&s.cwd, since, claimed) else { return };
                *s.agent_session.lock().unwrap() = Some(id.clone());
                Some(id)
            }
        };
    }
    let Some(id) = l.conversation.clone() else { return };
    if a.status_source() == StatusSource::Polled {
        l.turn = a.turn_now(&id, since).unwrap_or(false);
        // A question it asks on its screen, for agents whose store doesn't say.
        l.needs = if l.turn { a.asking(&s.pane.text(0)) } else { None };
    } else {
        read_log(a, &mut l, &id, since);
    }
    // Nothing yet: say nothing, as for an agent that hasn't been sent a prompt.
    if l.reported.is_none() && !l.turn && l.needs.is_none() {
        return;
    }
    let now = match (&l.needs, l.turn) {
        (Some(what), _) => Activity::NeedsPermission(what.clone()),
        (None, true) => Activity::Working,
        (None, false) => Activity::Done,
    };
    if l.reported.as_ref() != Some(&now) {
        d.proxy.stats.report(&s.id, now.clone());
        l.reported = Some(now);
    }
}

/// Take in what conversation `id`'s file says since it was last read.
fn read_log(a: &dyn Agent, l: &mut Log, id: &str, since: u64) {
    if l.path.is_none() {
        // Some write it only once the first prompt is sent.
        let Some(path) = a.log_path(id).filter(|p| p.exists()) else { return };
        // A record begun since this process started is read from its start, so its first turn
        // counts; one it continues, from where it is now.
        let born = path.metadata().and_then(|m| m.created()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
        l.offset = if born + 1 >= since { 0 } else { path.metadata().map_or(0, |m| m.len()) };
        l.path = Some(path);
    }
    let Some(path) = l.path.clone() else { return };
    for v in new_lines(&path, &mut l.offset) {
        match a.log_event(&v) {
            LogEvent::TurnStarted => {
                l.turn = true;
                l.needs = None;
            }
            LogEvent::TurnEnded => {
                l.turn = false;
                l.needs = None;
            }
            LogEvent::Needs(what) => l.needs = Some(what),
            LogEvent::Bookkeeping => {}
            LogEvent::Other => l.needs = None,
        }
    }
}

/// The whole lines written since `offset`, moving it past them.
fn new_lines(path: &Path, offset: &mut u64) -> Vec<serde_json::Value> {
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
    buf[..end].split(|&b| b == b'\n').filter_map(|l| serde_json::from_slice(l).ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_lines_reads_whole_lines_once() {
        let dir = std::env::temp_dir().join(format!("dino-agentlog-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("wire.jsonl");
        std::fs::write(&p, "{\"type\":\"turn.prompt\"}\n{\"type\":\"turn.en").unwrap();
        let mut offset = 0;
        assert_eq!(new_lines(&p, &mut offset).len(), 1);
        assert!(new_lines(&p, &mut offset).is_empty(), "half a line waits");
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        std::io::Write::write_all(&mut f, b"ded\"}\n").unwrap();
        assert_eq!(new_lines(&p, &mut offset)[0]["type"], "turn.ended");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
