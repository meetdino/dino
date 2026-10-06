//! The permission mode and model a session's agent is really on, and switching its mode in place.
//!
//! What dino asked for when it started the agent isn't always what it's in: Claude switches mode
//! itself (Shift+Tab, leaving plan mode), and a switch chosen mid-turn waits. So the mode shown is
//! the one the agent's own screen shows, else the one it last showed there, else the one its hooks
//! last reported. An agent with a key that steps through its modes (Claude's Shift+Tab) is
//! switched with that key, its screen read after each step, rather than restarted. The model is
//! the one the agent last said it's on (`/model` in it included): Claude's hooks, Codex's rollout,
//! other agents' own records; else the one it was started with.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use dino_core::agent::agent;
use dino_core::controls::{self, Controls};
use dino_proxy::Activity;

use super::{Daemon, Session};

/// What a session's agent has shown of its mode since it started.
#[derive(Default)]
pub(crate) struct Seen {
    /// The mode its screen last showed, or the one dino last switched it to.
    now: Option<String>,
    /// Where one step of its mode key went from each mode, as dino saw it press it.
    steps: HashMap<String, String>,
    /// Modes its key went all the way round without reaching: they take a restart.
    unreachable: Vec<String>,
}

/// How much a mode lets through without asking: plan least, bypass most.
fn latitude(mode: &str) -> u8 {
    match mode {
        "plan" => 0,
        "ask" => 1,
        "edits" => 2,
        "auto" => 3,
        _ => 4,
    }
}

/// How much of the bottom of its screen an agent's mode is shown in (see `Agent::screen_mode`).
const FOOTER_LINES: usize = 3;

/// The mode `s`'s agent shows on its screen now, noted as the one it's in. It reads the screen:
/// never call it holding a lock of the session's, which the pane's reader takes holding the screen.
fn on_screen(s: &Session) -> Option<String> {
    if s.host.is_some() || s.pane.is_exited() {
        return None;
    }
    let mode = agent(&s.agent_id)?.screen_mode(&s.pane.last_lines(FOOTER_LINES))?;
    s.mode_seen.lock().unwrap().now = Some(mode.clone());
    Some(mode)
}

/// The mode `s`'s agent is in, as far as dino can tell: its screen now, else what it last showed
/// there, else what its hooks last said (`hooked`, in the agent's words). `None` when nothing has
/// said: it's in whatever it was started with.
pub(crate) fn now(s: &Session, hooked: Option<&str>) -> Option<String> {
    on_screen(s)
        .or_else(|| s.mode_seen.lock().unwrap().now.clone())
        .or_else(|| hooked.and_then(|m| controls::reported_mode(&s.agent_id, m)))
}

/// The model `s`'s agent says it's on (`said`: `SessionStats::agent_model`), once it has said
/// since it started; `None` until then, when it's on the one it was started with, and for an agent
/// whose model dino picks for each turn (the free tier).
pub(crate) fn model_now(s: &Session, said: Option<&str>) -> Option<String> {
    said.filter(|_| agent(&s.agent_id).is_some_and(|a| a.picks_model())).map(String::from)
}

/// `s`'s controls as its agent runs with them now: its mode and model as it says they are (see
/// `now`, `model_now`), over the ones it was started with. What it restarts with, so a restart
/// doesn't undo a switch.
pub(crate) fn current(d: &Daemon, s: &Session) -> Controls {
    let st = d.proxy.stats.session(&s.id);
    Controls {
        mode: now(s, st.agent_mode.as_deref()).or_else(|| s.controls.mode.clone()),
        model: model_now(s, st.agent_model.as_deref()).or_else(|| s.controls.model.clone()),
        ..s.controls.clone()
    }
}

/// Whether a switch of `s` to `want` is worth trying in place: its agent has a key for it, and
/// that key is known to reach `want` from how it was started.
pub(crate) fn switchable(s: &Session, want: &str) -> bool {
    if s.host.is_some() || s.pane.is_exited() {
        return false;
    }
    let Some((_, order)) = agent(&s.agent_id).and_then(|a| a.mode_cycle(&launched(s))) else { return false };
    let seen = s.mode_seen.lock().unwrap();
    (order.contains(&want) || seen.steps.values().any(|m| m == want)) && !seen.unreachable.iter().any(|m| m == want)
}

/// The arguments `s`'s agent was started with, its mode flag and what put others in reach of its
/// mode key included.
fn launched(s: &Session) -> Vec<String> {
    let mut args = s.args.clone();
    if let (Some(a), Some(m)) = (agent(&s.agent_id), s.controls.mode.as_deref()) {
        args.extend(a.mode_args(m));
    }
    args.extend(s.reach.iter().cloned());
    args
}

/// Mid-turn: a tool call could come at any moment.
fn on_turn(d: &Daemon, s: &Session) -> bool {
    let st = super::stats(d, s);
    st.in_flight > 0 || matches!(st.activity, Some(Activity::Working))
}

/// How long the agent gets to redraw after a step of its mode key.
const STEP_SHOWN: Duration = Duration::from_secs(3);

/// Switch `s`'s agent to `want` with its own mode key, reading its screen after each step, as the
/// user would. False when it can't now: its screen doesn't show its mode (a dialog is up), it's
/// waiting on the user, or, mid-turn, a step on the way would pass through a mode that lets more
/// through than both the one it's in and `want` (Claude goes plan → bypass → auto → manual). It's
/// then left where it got to, which is never such a mode mid-turn.
pub(crate) fn switch(d: &Daemon, s: &Session, want: &str) -> bool {
    if !switchable(s, want) {
        return false;
    }
    let Some((key, order)) = agent(&s.agent_id).and_then(|a| a.mode_cycle(&launched(s))) else { return false };
    // Two at once (the user choosing again while one runs) would overshoot each other.
    if s.switching.swap(true, Ordering::AcqRel) {
        return false;
    }
    let done = steps(d, s, want, key, &order);
    s.switching.store(false, Ordering::Release);
    if done {
        eprintln!("{} dinod: session {}: switched to {want} in place", super::stamp(), s.id);
    }
    done
}

fn steps(d: &Daemon, s: &Session, want: &str, key: &str, order: &[&str]) -> bool {
    let Some(start) = on_screen(s) else { return false };
    let mut at = start.clone();
    for _ in 0..=order.len() {
        if at == want {
            return true;
        }
        if s.pane.is_exited() || matches!(super::stats(d, s).activity, Some(Activity::NeedsPermission(_))) {
            return false;
        }
        if on_turn(d, s) {
            let most = latitude(&start).max(latitude(want));
            let ahead = path(&s.mode_seen.lock().unwrap().steps, &at, want, order);
            if ahead.iter().any(|m| latitude(m) > most) {
                return false;
            }
        }
        s.pane.write(key.as_bytes().to_vec());
        let deadline = Instant::now() + STEP_SHOWN;
        let next = loop {
            match on_screen(s) {
                Some(m) if m != at => break Some(m),
                _ if Instant::now() > deadline => break None,
                _ => std::thread::sleep(Duration::from_millis(50)),
            }
        };
        let Some(next) = next else { return false };
        s.mode_seen.lock().unwrap().steps.insert(at.clone(), next.clone());
        at = next;
        if at == start {
            // All the way round: its key doesn't reach it in this session.
            s.mode_seen.lock().unwrap().unreachable.push(want.to_string());
            return false;
        }
    }
    at == want
}

/// The modes between `from` and `to` (not counting `from`, `to` the last), as the key would step:
/// each step as seen before, else as `order` has it.
fn path(steps: &HashMap<String, String>, from: &str, to: &str, order: &[&str]) -> Vec<String> {
    let next = |m: &str| -> String {
        if let Some(n) = steps.get(m) {
            return n.clone();
        }
        let i = order.iter().position(|o| *o == m).map_or(0, |i| i + 1);
        order[i % order.len()].to_string()
    };
    let mut out = vec![];
    let mut at = from.to_string();
    for _ in 0..order.len() {
        at = next(&at);
        out.push(at.clone());
        if at == to {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latitude_orders_modes_by_what_they_let_through() {
        let mut m = vec!["bypass", "ask", "auto", "plan", "edits"];
        m.sort_by_key(|m| latitude(m));
        assert_eq!(m, ["plan", "ask", "edits", "auto", "bypass"]);
    }

    #[test]
    fn the_way_round_is_as_seen_else_as_the_agent_orders_it() {
        let order = ["ask", "edits", "plan", "bypass", "auto"];
        let none = HashMap::new();
        assert_eq!(path(&none, "ask", "plan", &order), ["edits", "plan"]);
        assert_eq!(path(&none, "plan", "ask", &order), ["bypass", "auto", "ask"], "round the end");
        // Seen: this one has no auto (the model doesn't take it).
        let seen = HashMap::from([("bypass".to_string(), "ask".to_string())]);
        assert_eq!(path(&seen, "plan", "ask", &order), ["bypass", "ask"]);
        // Not in the cycle at all: once round, back where it began.
        assert_eq!(path(&none, "ask", "dontAsk", &order[..3]), ["edits", "plan", "ask"]);
    }
}
