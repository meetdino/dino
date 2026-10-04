//! Where an agent's turn is, from its own server: an agent that serves its API (OpenCode's
//! terminal UI, given `--port`) streams what happens in it as server-sent events, and dinod
//! follows that stream for each of its sessions: a conversation busy or idle, a permission or a
//! question it waits on, how full its context is. dinod picks the port and a password for each
//! session (`Agent::serve`), on this Mac only; nothing is added to the agent, and it holds with
//! routing off.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::sync::Arc;
use std::time::Duration;

use dino_core::agent::{Agent, ServerEvent, agent};
use dino_proxy::computer::Phase;
use dino_proxy::{Activity, ReportedContext, Stats};

use super::Session;

/// Where a session's agent serves: a port on 127.0.0.1, and the password it takes.
#[derive(Clone)]
pub(crate) struct Address {
    pub(crate) port: u16,
    pub(crate) password: String,
}

/// A free port and a fresh password for a session's agent to serve on.
pub(crate) fn address() -> Option<Address> {
    let port = std::net::TcpListener::bind("127.0.0.1:0").ok()?.local_addr().ok()?.port();
    Some(Address { port, password: dino_core::new_uuid().replace('-', "") })
}

/// How long to wait between tries while its server starts, or after the stream drops.
const RETRY: Duration = Duration::from_millis(250);

/// Longer than its server ever goes without a word (OpenCode's heartbeat comes every 10 s).
const QUIET: Duration = Duration::from_secs(30);

/// Follow session `s`'s agent's server, on a thread of its own, while the agent runs.
pub(crate) fn follow(stats: Arc<Stats>, s: Arc<Session>) {
    let (Some(a), Some(addr)) = (agent(&s.agent_id), s.server.clone()) else { return };
    let name = format!("agentserver-{}", s.id);
    let _ = std::thread::Builder::new().name(name).spawn(move || run(a, &stats, &s, &addr));
}

fn run(a: &'static dyn Agent, stats: &Stats, s: &Session, addr: &Address) {
    // The stream on a client of its own: a request on the same blocking client waits behind a
    // response still being read. Its timeout bounds each read: the server says something at
    // least every `QUIET`, so a stream that stalls or dies is opened again.
    let connect = || reqwest::blocking::Client::builder().connect_timeout(Duration::from_secs(1));
    let (Ok(streams), Ok(client)) = (connect().timeout(QUIET).build(), connect().timeout(Duration::from_secs(5)).build()) else { return };
    let base = format!("http://127.0.0.1:{}", addr.port);
    let on = |c: &reqwest::blocking::Client, path: &str| c.get(format!("{base}{path}")).basic_auth(a.server_user(), Some(&addr.password)).send().ok().filter(|r| r.status().is_success());
    let get = |path: &str| on(&client, path);
    let mut st = State::default();
    while !s.pane.is_exited() {
        // Opened while its server is still starting, the stream can hang without a word: first
        // see that it answers.
        let ready = a.server_snapshot().first().is_none_or(|p| get(p).is_some());
        let Some(events) = ready.then(|| on(&streams, "/event")).flatten() else {
            std::thread::sleep(RETRY);
            continue;
        };
        // Connected: how things stand now, then each change.
        st.reset();
        for path in a.server_snapshot() {
            let answer = get(path).and_then(|r| r.json::<serde_json::Value>().ok()).unwrap_or_default();
            for e in a.server_snapshot_events(path, &answer) {
                st.take(a, s, e);
            }
        }
        st.report(stats, s);
        for line in BufReader::new(events).lines() {
            let Ok(line) = line else { break };
            if !worth_reading(&line) {
                continue;
            }
            let Some(v) = line.strip_prefix("data:").and_then(|d| serde_json::from_str::<serde_json::Value>(d.trim()).ok()) else { continue };
            match a.server_event(&v) {
                ServerEvent::Tool { call, name, done } => st.tool(stats, s, call, name, done),
                ServerEvent::Context { session, model, used } => {
                    if s.agent_session.lock().unwrap().as_deref() != Some(session.as_str()) {
                        continue;
                    }
                    let window = *st.windows.entry(model.clone()).or_insert_with(|| {
                        let providers = a.server_providers().and_then(|p| get(p)).and_then(|r| r.json::<serde_json::Value>().ok())?;
                        a.server_context_window(&providers, &model)
                    });
                    if let Some(window) = window.filter(|_| !s.pane.is_exited()) {
                        stats.report_context(&s.id, ReportedContext { used: Some(used), window });
                    }
                }
                e => {
                    st.take(a, s, e);
                    st.report(stats, s);
                }
            }
        }
        std::thread::sleep(RETRY);
    }
}

/// Most of what it streams is the answer being written, a few characters at a time: only lines
/// that may say something about the turn are parsed.
fn worth_reading(line: &str) -> bool {
    ["\"session.", "\"permission.", "\"question.", "\"message.updated\""].iter().any(|k| line.contains(k))
        // A part that's a tool call, not each of the answer's.
        || (line.contains("\"message.part.updated\"") && line.contains("\"type\":\"tool\""))
}

/// What its server has said so far.
#[derive(Default)]
struct State {
    /// Conversations on a turn, its subagents' among them.
    busy: HashSet<String>,
    /// What it waits on the user for: (request, conversation, what).
    asked: Vec<(String, String, String)>,
    reported: Option<Activity>,
    /// Each model's context window, as its server says; `None` when it doesn't know.
    windows: HashMap<String, Option<u64>>,
    /// Tool calls out: (call, tool).
    calls: HashMap<String, String>,
}

impl State {
    fn reset(&mut self) {
        self.busy.clear();
        self.asked.clear();
    }

    /// A tool call it says is out or has ended, told once each to what notices the Mac or a
    /// browser in use (see `dino_proxy::computer`).
    fn tool(&mut self, stats: &Stats, s: &Session, call: String, name: String, done: bool) {
        let phase = match (done, self.calls.contains_key(&call)) {
            (false, false) => Phase::Started,
            (false, true) => return,
            (true, true) => Phase::Ended,
            // Ended before it was seen out (a stream opened again meanwhile): a call made.
            (true, false) => Phase::Called,
        };
        if done {
            self.calls.remove(&call);
        } else {
            self.calls.insert(call, name.clone());
        }
        stats.tool_call(&s.id, &name, phase);
    }

    fn take(&mut self, a: &dyn Agent, s: &Session, e: ServerEvent) {
        match e {
            ServerEvent::Busy(id) => {
                // The conversation it's on now: `/new` and picking another one in it switch it.
                if !self.busy.contains(&id) && s.agent_session.lock().unwrap().as_deref() != Some(id.as_str()) && a.is_conversation(&id) {
                    *s.agent_session.lock().unwrap() = Some(id.clone());
                }
                self.busy.insert(id);
            }
            ServerEvent::Idle(id) => {
                self.busy.remove(&id);
                self.asked.retain(|(_, session, _)| *session != id);
            }
            ServerEvent::Asked { id, session, what } => {
                self.asked.retain(|(r, ..)| *r != id);
                self.asked.push((id, session, what));
            }
            ServerEvent::Answered(id) => self.asked.retain(|(r, ..)| *r != id),
            ServerEvent::Context { .. } | ServerEvent::Tool { .. } | ServerEvent::Other => {}
        }
    }

    /// Say where its turn is, when that changed.
    fn report(&mut self, stats: &Stats, s: &Session) {
        let now = match (self.asked.last(), self.busy.is_empty()) {
            (Some((.., what)), _) => Activity::NeedsPermission(what.clone()),
            (None, false) => Activity::Working,
            (None, true) => Activity::Done,
        };
        // Nothing yet: say nothing, as for an agent that hasn't been sent a prompt.
        if self.reported.is_none() && now == Activity::Done {
            return;
        }
        // Gone (restarted, quit): its replacement reports for the session now.
        if self.reported.as_ref() != Some(&now) && !s.pane.is_exited() {
            stats.report(&s.id, now.clone());
            self.reported = Some(now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_what_may_say_something_is_read() {
        assert!(worth_reading(r#"data: {"id":"e1","type":"session.status","properties":{"sessionID":"s","status":{"type":"busy"}}}"#));
        assert!(worth_reading(r#"data: {"type":"permission.asked","properties":{}}"#));
        assert!(worth_reading(r#"data: {"type":"message.updated","properties":{}}"#));
        assert!(!worth_reading(r#"data: {"type":"message.part.delta","properties":{"delta":"hel"}}"#));
        assert!(!worth_reading(r#"data: {"type":"server.heartbeat","properties":{}}"#));
        assert!(worth_reading(r#"data: {"type":"message.part.updated","properties":{"part":{"type":"tool","tool":"open-computer-use_list_apps","callID":"c","state":{"status":"running"}}}}"#));
        assert!(!worth_reading(r#"data: {"type":"message.part.updated","properties":{"part":{"type":"text","text":"hi"}}}"#));
    }

    #[test]
    fn a_free_port_and_a_password() {
        let a = address().unwrap();
        assert!(a.port > 0);
        assert_eq!(a.password.len(), 32);
        assert_ne!(a.password, address().unwrap().password);
    }
}
