//! Asleep to save memory, as a browser puts tabs it doesn't show to sleep. An agent idle a while
//! and on no screen (Settings → Sessions; sooner, and on screen too, when macOS says memory is
//! short) has its process stopped as a restart stops it, and its session stays where it was, as it
//! was: its name, pin, last screen (its pane frozen on it), usage, account, mode and model. Shown in
//! a client (`Request::Viewing`, or an attach that isn't a background surface) or sent input, it
//! wakes: its agent starts again on its conversation in the same session, as `restart_with` starts
//! one, and what was typed or sent meanwhile is held until the agent takes input (as `schedule`
//! tells a starting agent does), then given to it in order. It stays asleep across dinod restarts.
//!
//! Only what a stop cuts nothing short of sleeps: an agent dino started here and can start again
//! on its conversation, between turns, asking nothing, and with nothing of its own still running
//! (subagents, background commands, monitors, servers, scheduled prompts, a tmux client, a preview,
//! anything it left running outside its terminal), not pinned (unless asked by name), not followed
//! by an automation's run.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use dino_core::agent::agent;
use dino_core::procinfo;
use dino_core::settings::Settings;
use dino_proxy::Activity;

use crate::{Daemon, LIVE_HISTORY, Session, bump, claude_setup, conversation_of, mode, now_secs, procs, restart_with, save, send_input, stamp, stats, tmux};

/// How often the checker looks: memory pressure is one sysctl, each session a few locks.
const LOOK: Duration = Duration::from_secs(30);
/// While memory is short, how long an agent has to have been idle to go to sleep.
const PRESSED_IDLE: Duration = Duration::from_secs(5 * 60);
/// The most a woken agent is waited for before what was held for it goes in anyway.
const READY_WITHIN: Duration = Duration::from_secs(60);
/// How often a waking agent is looked at.
const READY_LOOK: Duration = Duration::from_millis(50);
/// How long a sleeping agent's processes get to be gone before it counts as asleep anyway.
const GONE_WITHIN: Duration = Duration::from_secs(6);

/// Input for a session that's asleep or waking, held until its agent takes it.
pub(crate) enum Held {
    /// Keys, as a client typed them.
    Keys(Vec<u8>),
    /// A paste, with Return after it when `submit` (see `send_input`).
    Text { text: String, submit: bool },
}

/// How short of memory macOS says it is (`kern.memorystatus_vm_pressure_level`: 1, 2, 4). Linux
/// reads none (see `pressure`): there only tests make the others.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) enum Pressure {
    Normal,
    Warning,
    Critical,
}

/// The memory pressure now: one sysctl.
#[cfg(target_os = "macos")]
pub(crate) fn pressure() -> Pressure {
    let mut level: libc::c_int = 0;
    let mut size = std::mem::size_of::<libc::c_int>();
    // SAFETY: an int sysctl read into an int, its size given.
    let read = unsafe { libc::sysctlbyname(c"kern.memorystatus_vm_pressure_level".as_ptr(), (&raw mut level).cast(), &mut size, std::ptr::null_mut(), 0) };
    match (read, level) {
        (0, 4) => Pressure::Critical,
        (0, 2) => Pressure::Warning,
        _ => Pressure::Normal,
    }
}

/// Linux has no levels, only how long tasks stalled for memory (/proc/pressure/memory), which
/// would take thresholds of dino's own: idle agents sleep after their time only.
#[cfg(not(target_os = "macos"))]
pub(crate) fn pressure() -> Pressure {
    Pressure::Normal
}

/// Look every `LOOK` for agents to put to sleep.
pub(crate) fn start(d: &Arc<Daemon>) {
    let d = d.clone();
    let _ = std::thread::Builder::new().name("sleep".into()).spawn(move || {
        loop {
            std::thread::sleep(LOOK);
            let minutes = Settings::load().machine.sleep_idle_agents;
            if minutes > 0 {
                tick(&d, Duration::from_secs(u64::from(minutes) * 60), &pressure);
            }
        }
    });
}

/// Put to sleep the agents idle `after` or longer and on no screen; while `pressure` says memory
/// is short, those idle `PRESSED_IDLE` (and, when it's critical, on screen too), the longest idle
/// first, one at a time, the pressure read again after each. The ids put to sleep, in order.
pub(crate) fn tick(d: &Daemon, after: Duration, pressure: &dyn Fn() -> Pressure) -> Vec<String> {
    let sessions = d.sessions.lock().unwrap().clone();
    let shown = shown(d);
    let mut idle: Vec<(Duration, Arc<Session>)> = sessions.into_iter().filter(|s| !s.asleep() && !s.pane.is_exited()).map(|s| (idle_for(&s), s)).collect();
    idle.sort_by(|a, b| b.0.cmp(&a.0));
    let mut slept = vec![];
    let mut now = pressure();
    for (idle, s) in idle {
        let due = if now > Pressure::Normal { after.min(PRESSED_IDLE) } else { after };
        if idle < due || (shown.contains(&s.id) && now < Pressure::Critical) {
            continue;
        }
        // Refused (see `why_not`), it's looked at again next time, unsaid: most idle sessions are
        // refused every time, for their pin or what they still run.
        if sleep(d, &s.id, false).is_ok() {
            eprintln!("{} dinod: session {} ({}) asleep after {} min idle{}", stamp(), s.id, s.name, idle.as_secs() / 60, if now > Pressure::Normal { ", memory short" } else { "" });
            slept.push(s.id.clone());
            if now > Pressure::Normal {
                now = pressure();
            }
        }
    }
    slept
}

/// How long `s` has been idle: since its agent last printed something the user didn't cause or
/// the user last typed into it, whichever came last; since it started here, with neither yet.
pub(crate) fn idle_for(s: &Session) -> Duration {
    let last = [*s.last_output.lock().unwrap(), *s.typed.lock().unwrap()].into_iter().flatten().max();
    last.unwrap_or(s.spawned).elapsed()
}

/// The sessions some client shows: in what one said it shows (`Viewing`), or with one attached
/// that's looking at it rather than keeping a surface.
fn shown(d: &Daemon) -> HashSet<String> {
    let mut out: HashSet<String> = d.viewing.lock().unwrap().values().flatten().cloned().collect();
    out.extend(d.sessions.lock().unwrap().iter().filter(|s| s.looking.load(Ordering::Relaxed) > 0).map(|s| s.id.clone()));
    out
}

/// Why `s` can't go to sleep now, if it can't: what stopping it would cut short, or that dino
/// couldn't start it again as it was. `asked` by name, a pin doesn't keep it awake. Called with
/// the waking lock held (see `sleep`), so the caller knows it isn't waking.
pub(crate) fn why_not(d: &Daemon, s: &Session, asked: bool) -> Option<String> {
    if s.agent_id == "shell" {
        return Some("it's a shell (an agent typed into one stays awake too)".into());
    }
    if let Some(host) = &s.host {
        return Some(format!("it runs on {host}"));
    }
    if agent(&s.agent_id).is_none() {
        return Some(format!("dino can't start {} again on its conversation", s.agent_id));
    }
    if s.asleep() {
        return Some("it's asleep already".into());
    }
    if s.pane.is_exited() {
        return Some("it has ended".into());
    }
    if s.replaced.load(Ordering::Relaxed) || s.pending.lock().unwrap().is_some() {
        return Some("it's restarting".into());
    }
    if !asked && s.pinned.load(Ordering::Relaxed) {
        return Some("it's pinned".into());
    }
    if s.moving_to.lock().unwrap().is_some() {
        return Some("its agent is moving to another conversation".into());
    }
    let st = stats(d, s);
    match &st.activity {
        Some(Activity::NeedsPermission(what)) => return Some(format!("it's waiting on you ({what})")),
        Some(Activity::Working) => return Some("it's working".into()),
        _ => {}
    }
    if st.in_flight > 0 {
        return Some("it's working".into());
    }
    let screen = s.pane.text(0);
    if let Some(what) = claude_setup(s, st.activity.as_ref()).map(String::from).or_else(|| s.adapter().and_then(|a| a.asking(&screen))) {
        return Some(format!("it's waiting on you ({what})"));
    }
    if s.last_output.lock().unwrap().is_some_and(|t| t.elapsed() < crate::TURN_OVER_QUIET) {
        return Some("it's printing".into());
    }
    if s.typed.lock().unwrap().is_some_and(|t| t.elapsed() < crate::TYPED_QUIET) {
        return Some("you're typing in it".into());
    }
    if st.subagents.iter().any(|a| a.running) {
        return Some("a subagent of its is running".into());
    }
    if let Some(b) = st.background.iter().find(|b| b.running) {
        let what = if b.kind == "monitor" { "a monitor" } else { "a background command" };
        return Some(format!("{what} of its is running{}", b.description.as_deref().or(b.command.as_deref()).map(|c| format!(" ({c})")).unwrap_or_default()));
    }
    if let Some(x) = s.servers.lock().unwrap().first() {
        return Some(format!("it serves on port {}", x.ports.iter().map(u16::to_string).collect::<Vec<_>>().join(", ")));
    }
    if !st.crons.is_empty() {
        return Some("it has prompts scheduled for itself (CronCreate, /loop)".into());
    }
    if d.previews.lock().unwrap().iter().any(|p| p.session == s.id && p.running()) {
        return Some("its preview's server is running".into());
    }
    if d.schedule.follows(&s.id) {
        return Some("an automation's run is following it".into());
    }
    // Its conversation, as its agent names it now (after `/clear`, `/resume`): what it wakes on.
    let conversation = {
        let mut known = s.agent_session.lock().unwrap();
        if let Some(now) = conversation_of(s) {
            *known = Some(now);
        }
        known.clone()
    };
    if conversation.is_none() {
        return Some("dino doesn't know its conversation yet, to start it again on".into());
    }
    let pid = s.pane.pid()?;
    if procinfo::tree(pid).iter().any(|(p, _)| *p != pid && tmux::is_tmux_process(*p)) {
        return Some("a tmux client runs in it".into());
    }
    // Last, a look at every process: what it started that left its terminal (a server or build
    // in the background dino doesn't follow) would stop with it.
    let own = procinfo::session_of(pid);
    let left: Vec<String> = procs::of_session(&d.home, &s.id).into_iter().filter(|&p| procinfo::session_of(p) != own).filter_map(procinfo::name).collect();
    if !left.is_empty() {
        return Some(format!("it left {} running", left.join(", ")));
    }
    None
}

/// Put session `id` to sleep now, unless `why_not` says it can't (a pin doesn't when `asked`):
/// its agent and everything it started stopped, its pane frozen on its last screen. Answers once
/// they're gone.
pub(crate) fn sleep(d: &Daemon, id: &str, asked: bool) -> anyhow::Result<()> {
    let s = d.sessions.lock().unwrap().iter().find(|s| s.id == id).cloned().ok_or_else(|| anyhow::anyhow!("no session {id}"))?;
    // Decided under the waking lock: input that comes meanwhile waits for it (and then wakes it),
    // and no wake starts the agent's replacement before the look at what it runs
    // (`procs::stop_session`) is taken.
    let stopping = {
        let waking = d.waking.lock().unwrap();
        if waking.contains_key(id) {
            anyhow::bail!("{} is waking up", s.name);
        }
        if s.asleep() {
            return Ok(());
        }
        // Input on its way to its program now (taken before the waking lock was let go, see
        // `input`) lands first, and then counts as typing.
        let Ok(_giving) = s.giving.try_lock() else { anyhow::bail!("{} stays awake: you're typing in it", s.name) };
        if let Some(why) = why_not(d, &s, asked) {
            anyhow::bail!("{} stays awake: {why}", s.name);
        }
        s.asleep.store(now_secs().max(1), Ordering::SeqCst);
        s.pane.freeze();
        let terminal = s.pane.kill();
        let rest = procs::stop_session(&d.home, id, false);
        (terminal, rest)
    };
    eprintln!("{} dinod: session {id} ({}) going to sleep", stamp(), s.name);
    bump(d);
    // Gone before it's saved and answered: its memory is free then.
    let since = Instant::now();
    for h in [stopping.0, stopping.1].into_iter().flatten() {
        let _ = h.join();
    }
    while !s.pane.is_exited() && since.elapsed() < GONE_WITHIN {
        std::thread::sleep(Duration::from_millis(20));
    }
    save(d);
    bump(d);
    Ok(())
}

/// Session `id` is waking: its agent starting, what's sent to it held.
pub(crate) fn waking(d: &Daemon, id: &str) -> bool {
    d.waking.lock().unwrap().contains_key(id)
}

/// Wake session `id` if it's asleep, and answer once its agent takes input. One waking already
/// is waited for; one agent starts, however many ask.
pub(crate) fn wake(d: &Daemon, id: &str) -> anyhow::Result<()> {
    let s = d.sessions.lock().unwrap().iter().find(|s| s.id == id).cloned().ok_or_else(|| anyhow::anyhow!("no session {id}"))?;
    let first = {
        let mut waking = d.waking.lock().unwrap();
        let first = !waking.contains_key(id);
        if first {
            if !s.asleep() {
                return Ok(());
            }
            waking.insert(id.to_string(), vec![]);
        }
        first
    };
    if first {
        let woken = begin(d, id)?;
        settle(d, &woken);
        return Ok(());
    }
    // Waking already: answered when it's done.
    let since = Instant::now();
    while waking(d, id) && since.elapsed() < READY_WITHIN + GONE_WITHIN {
        std::thread::sleep(READY_LOOK);
    }
    Ok(())
}

/// Wake session `id` if it's asleep (or give it `held` if it's waking, or awake already): its
/// agent started in its place now, and given what's held once it takes input, off this thread.
pub(crate) fn wake_soon(d: &Arc<Daemon>, id: &str, held: Vec<Held>) {
    let start = {
        let mut waking = d.waking.lock().unwrap();
        if let Some(q) = waking.get_mut(id) {
            q.extend(held);
            return;
        }
        let now = d.sessions.lock().unwrap().iter().find(|s| s.id == id).cloned();
        match now {
            Some(s) if s.asleep() => {
                waking.insert(id.to_string(), held);
                true
            }
            Some(s) => {
                drop(waking);
                let _giving = s.giving.lock().unwrap();
                for h in held {
                    give(&s, h);
                }
                false
            }
            None => false,
        }
    };
    if !start {
        return;
    }
    // Started here, so a client attaching next finds the agent taking its place (see `attachable`).
    let Ok(woken) = begin(d, id) else { return };
    let d = d.clone();
    let _ = std::thread::Builder::new().name("wake".into()).spawn(move || settle(&d, &woken));
}

/// Input for session `s`: to its agent, unless it's asleep or waking, when it's held for the agent
/// (waking it). `s` may be one a wake replaced that a client still sends to: what it sends goes to
/// the session in its place. While it wakes, the client terminal's answers to the starting agent's
/// queries go straight to it (see `split_replies`).
pub(crate) fn input(d: &Arc<Daemon>, s: &Arc<Session>, what: Held) {
    let mut waking = d.waking.lock().unwrap();
    if let Some(q) = waking.get_mut(&s.id) {
        let what = match what {
            Held::Keys(bytes) => {
                let (replies, keys) = split_replies(&bytes);
                if !replies.is_empty() {
                    let now = if s.asleep() { d.sessions.lock().unwrap().iter().find(|o| o.id == s.id).cloned() } else { Some(s.clone()) };
                    if let Some(now) = now.filter(|n| !n.asleep()) {
                        now.pane.write(replies);
                    }
                }
                if keys.is_empty() {
                    return;
                }
                Held::Keys(keys)
            }
            text => text,
        };
        q.push(what);
        return;
    }
    if !s.asleep() {
        // Taken before the waking lock goes, so a sleep decided next waits for this to land; or,
        // with input on its way already, after it, never waited for under that lock (a program
        // that doesn't read its input holds a write up): that input counts as typing, which keeps
        // it awake meanwhile.
        let giving = match s.giving.try_lock() {
            Ok(g) => {
                drop(waking);
                g
            }
            Err(_) => {
                drop(waking);
                s.giving.lock().unwrap()
            }
        };
        give(s, what);
        drop(giving);
        return;
    }
    // Asleep: held, and its agent started off this thread (a client's reader), so what the client
    // sends next is read and held behind it at once.
    let now = d.sessions.lock().unwrap().iter().find(|o| o.id == s.id).cloned();
    match now {
        Some(now) if now.asleep() => {
            waking.insert(s.id.clone(), vec![what]);
            drop(waking);
            let (d, id) = (d.clone(), s.id.clone());
            let _ = std::thread::Builder::new().name("wake".into()).spawn(move || {
                if let Ok(woken) = begin(&d, &id) {
                    settle(&d, &woken);
                }
            });
        }
        // Woken since this client last looked: to the one in its place.
        Some(now) => {
            drop(waking);
            let _giving = now.giving.lock().unwrap();
            give(&now, what);
        }
        None => {}
    }
}

/// Give `s`'s agent what was held for it, as it would have had it: keys typed, a message pasted.
/// Either counts as typing, so it isn't put to sleep with it on its way.
fn give(s: &Session, what: Held) {
    *s.typed.lock().unwrap() = Some(Instant::now());
    match what {
        Held::Keys(keys) => s.pane.write(keys),
        Held::Text { text, submit } => send_input(s, &text, submit),
    }
}

/// Start session `id`'s agent again in its place, its entry in `waking` made: on its conversation,
/// with the controls it slept with (or any chosen meanwhile) and its last screen above. The
/// session now in its place. Failing, it stays asleep, and what was held for it is dropped.
fn begin(d: &Daemon, id: &str) -> anyhow::Result<Arc<Session>> {
    let started = (|| {
        let s = d.sessions.lock().unwrap().iter().find(|s| s.id == id).cloned().ok_or_else(|| anyhow::anyhow!("no session {id}"))?;
        let screen = s.pane.replay(LIVE_HISTORY);
        let controls = s.pending.lock().unwrap().clone().unwrap_or_else(|| mode::current(d, &s));
        restart_with(d, id, Some(screen), |saved| {
            if saved.controls.model != controls.model {
                d.proxy.stats.reset_context(id);
            }
            saved.controls = controls;
        })?;
        d.sessions.lock().unwrap().iter().find(|s| s.id == id).cloned().ok_or_else(|| anyhow::anyhow!("session {id} is gone"))
    })();
    match &started {
        Ok(_) => eprintln!("{} dinod: session {id} waking", stamp()),
        Err(e) => {
            let lost = d.waking.lock().unwrap().remove(id).map_or(0, |h| h.len());
            let lost = if lost > 0 { format!("; {lost} inputs held for it dropped") } else { String::new() };
            eprintln!("{} dinod: session {id} didn't wake: {e}{lost}", stamp());
        }
    }
    bump(d);
    started
}

/// Wait until the woken agent `s` takes input, as a starting agent is told to (see
/// `schedule::agent_ready`; what it asks on its screen, the user answers with what they typed),
/// then give it what was held, in order, and let input go straight to it. Given up on after
/// `READY_WITHIN`, when what's held goes in anyway; dropped if it ends first.
fn settle(d: &Daemon, s: &Arc<Session>) {
    let since = Instant::now();
    let mut steady = None;
    while !s.pane.is_exited() && since.elapsed() < READY_WITHIN && !crate::schedule::agent_ready(s, &mut steady, false) {
        std::thread::sleep(READY_LOOK);
    }
    if s.pane.is_exited() {
        let lost = d.waking.lock().unwrap().remove(&s.id).map_or(0, |h| h.len());
        eprintln!("{} dinod: session {} ended as it woke{}", stamp(), s.id, if lost > 0 { format!("; {lost} inputs held for it dropped") } else { String::new() });
        bump(d);
        return;
    }
    // One at a time from the front, what comes meanwhile joining the back, until none is left:
    // then input goes straight to it. Given without the lock, which a write held up would hold.
    loop {
        let next = {
            let mut waking = d.waking.lock().unwrap();
            match waking.get_mut(&s.id) {
                Some(q) if !q.is_empty() => q.remove(0),
                _ => {
                    waking.remove(&s.id);
                    break;
                }
            }
        };
        give(s, next);
    }
    eprintln!("{} dinod: session {} awake in {} ms", stamp(), s.id, since.elapsed().as_millis());
    bump(d);
}

/// Of what a client sent while its session wakes, the answers its terminal gave the agent's
/// queries (device attributes, modes, kitty keyboard flags, colours, its version), which go to the
/// agent at once, and the rest, keys, which are held. An answer is a control string (OSC, DCS, APC,
/// PM) or a CSI with a private marker (`?`, `>`, `=`), which no key sends.
fn split_replies(bytes: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let (mut replies, mut keys) = (vec![], vec![]);
    let mut i = 0;
    while i < bytes.len() {
        let end = match (bytes[i], bytes.get(i + 1), bytes.get(i + 2)) {
            // Up to BEL or ST.
            (0x1b, Some(b']' | b'P' | b'_' | b'^'), _) => {
                let rest = &bytes[i + 2..];
                rest.iter().position(|&b| b == 0x07).map(|j| i + 2 + j + 1).or_else(|| rest.windows(2).position(|w| w == b"\x1b\\").map(|j| i + 2 + j + 2))
            }
            // Up to its final byte.
            (0x1b, Some(b'['), Some(b'?' | b'>' | b'=')) => bytes[i + 3..].iter().position(|b| (0x40..=0x7e).contains(b)).map(|j| i + 3 + j + 1),
            _ => None,
        };
        match end {
            Some(end) => {
                replies.extend_from_slice(&bytes[i..end]);
                i = end;
            }
            None => {
                keys.push(bytes[i]);
                i += 1;
            }
        }
    }
    (replies, keys)
}

/// What one client shows (`Request::Viewing`), forgotten when its connection closes.
pub(crate) struct Viewer<'a> {
    d: &'a Daemon,
    pub conn: u64,
}

impl<'a> Viewer<'a> {
    pub(crate) fn new(d: &'a Daemon) -> Self {
        Self { d, conn: d.next_sub.fetch_add(1, Ordering::Relaxed) }
    }
}

impl Drop for Viewer<'_> {
    fn drop(&mut self) {
        self.d.viewing.lock().unwrap().remove(&self.conn);
    }
}

/// Client `conn` shows `ids` now: those asleep that weren't shown before wake.
pub(crate) fn viewing(d: &Arc<Daemon>, conn: u64, ids: Vec<String>) {
    let now: HashSet<String> = ids.into_iter().collect();
    let came = {
        let mut viewing = d.viewing.lock().unwrap();
        let before = viewing.insert(conn, now.clone()).unwrap_or_default();
        now.difference(&before).cloned().collect::<Vec<_>>()
    };
    let asleep: Vec<String> = {
        let sessions = d.sessions.lock().unwrap();
        came.into_iter().filter(|id| sessions.iter().any(|s| &s.id == id && s.asleep())).collect()
    };
    for id in asleep {
        let d = d.clone();
        let _ = std::thread::Builder::new().name("wake".into()).spawn(move || wake_soon(&d, &id, vec![]));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_terminals_answers_are_told_from_keys() {
        // Device attributes, a mode report, kitty flags, a colour (BEL and ST), the version (DCS).
        let replies = b"\x1b[?62;22c\x1b[?2026;2$y\x1b[?1u\x1b]11;rgb:1616/1616/1a1a\x07\x1b]10;rgb:d8d8/d8d8/d8d8\x1b\\\x1bP>|ghostty 1.2\x1b\\\x1b[>1;10;0c";
        assert_eq!(split_replies(replies), (replies.to_vec(), vec![]));
        // Keys: letters, Enter, arrows, Alt+P, a kitty key event, an SGR mouse report, Escape.
        let keys = b"hi\r\x1b[A\x1bP\x1b[97;5u\x1b[<0;10;5M\x1b";
        assert_eq!(split_replies(keys), (vec![], keys.to_vec()));
        let (r, k) = split_replies(b"a\x1b[?1ub\x1b]11;x\x07c");
        assert_eq!((r.as_slice(), k.as_slice()), (b"\x1b[?1u\x1b]11;x\x07".as_slice(), b"abc".as_slice()));
        // Cut short: keys, not a reply that swallows them.
        assert_eq!(split_replies(b"\x1b[?12"), (vec![], b"\x1b[?12".to_vec()));
    }
}
