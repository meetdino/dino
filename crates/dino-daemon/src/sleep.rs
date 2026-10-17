//! Asleep to save memory, as a browser puts tabs it doesn't show to sleep. An agent idle a while
//! and on no screen (Settings → Power → Memory; sooner, and on screen too, when macOS says memory is
//! short) has its process stopped as a restart stops it, and its session stays where it was, as it
//! was: its name, pin, last screen (its pane frozen on it), usage, account, mode and model. Shown in
//! a client (`Request::Viewing`, or an attach that isn't a background surface) or sent input, it
//! wakes: its agent starts again on its conversation in the same session, as `restart_with` starts
//! one, and what was typed or sent meanwhile is held until the agent can take it (see `settle`),
//! then given to it in order. It stays asleep across dinod restarts.
//!
//! Only what a stop cuts nothing short of sleeps: an agent dino started here and can start again
//! on its conversation, between turns, asking nothing, and with nothing of its own still running
//! (subagents, background commands, monitors, servers, scheduled prompts, a tmux client, a preview,
//! anything it left running outside its terminal), nothing typed into it and not sent, not pinned
//! (unless asked by name), not followed by an automation's run.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::{Arc, MutexGuard};
use std::time::{Duration, Instant};

use dino_core::agent::agent;
use dino_core::procinfo;
use dino_core::settings::Settings;
use dino_proxy::Activity;

use crate::{Daemon, LIVE_HISTORY, Session, attachable, bump, claude_setup, conversation_of, mode, now_secs, procs, restart_with, save, send_input, stamp, stats, tmux};

/// How often the checker looks: memory pressure is one sysctl, each session a few locks.
const LOOK: Duration = Duration::from_secs(30);
/// While memory is short, how long an agent has to have been idle to go to sleep.
const PRESSED_IDLE: Duration = Duration::from_secs(5 * 60);
/// The most a woken agent is waited for, and what was held for it, before what's left isn't given
/// (see `settle`). Shorter in tests, which wait it out.
const READY_WITHIN: Duration = Duration::from_secs(if cfg!(test) { 8 } else { 60 });
/// How often a waking agent is looked at.
const READY_LOOK: Duration = Duration::from_millis(50);
/// How long a sleeping agent's processes get to be gone before it counts as asleep anyway.
const GONE_WITHIN: Duration = Duration::from_secs(6);

/// Input for a session that's asleep or waking, held until its agent can take it.
pub(crate) enum Held {
    /// Keys, as a client typed them.
    Keys(Vec<u8>),
    /// A paste of the user's (the app's composer, `send_input`), with Return after it when `submit`.
    Text { text: String, submit: bool },
    /// A prompt from elsewhere: another session's agent, an automation, a PR's failed checks. Given
    /// only while the agent waits for one and nobody's typing (see `peers::message`).
    Message(String),
}

/// A session waking (see `Daemon::waking`).
pub(crate) struct Wake {
    /// What was typed or sent while it was asleep or waking, in order.
    held: Vec<Held>,
    /// Its agent is up, its screen drawn: keys typed from now on are typed seeing it.
    drawn: bool,
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
    let mut idle: Vec<(Duration, Arc<Session>)> = sessions.into_iter().filter(|s| !s.asleep() && !s.pane.is_exited()).map(|s| (idle_for(&s), s)).collect();
    idle.sort_by(|a, b| b.0.cmp(&a.0));
    let mut slept = vec![];
    let mut now = pressure();
    for (idle, s) in idle {
        let due = if now > Pressure::Normal { after.min(PRESSED_IDLE) } else { after };
        if idle < due {
            continue;
        }
        // Refused (see `why_not`, and on screen: `sleep`), it's looked at again next time, unsaid:
        // most idle sessions are refused every time, for their pin or what they still run.
        if sleep(d, &s.id, By::Checker { critical: now == Pressure::Critical }).is_ok() {
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
    // Claude's phone and web clients can't wake it. (Turned on inside it, `/remote-control`, dino
    // can't tell.)
    if s.args.iter().any(|a| a == "--remote-control" || a.starts_with("--remote-control=")) {
        return Some("Remote Control is on in it".into());
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
    // Its agent would forget it: Claude Code doesn't keep an unsent prompt across a resume.
    if unsent(s) {
        return Some("something typed in it isn't sent".into());
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

/// Who puts a session to sleep, which decides what besides `why_not` keeps it awake.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum By {
    /// Someone, by its name (`dino sleep`, Sleep Now): neither its pin nor a client showing it
    /// keeps it awake.
    Name,
    /// The checker (`tick`): a pin keeps it awake, and so does a client showing it, unless memory
    /// is critically short.
    Checker { critical: bool },
}

/// Put session `id` to sleep now, unless `why_not` (or, for the checker, a client showing it right
/// now) says it can't: its agent and everything it started stopped, its pane frozen on its last
/// screen. Answers once they're gone.
pub(crate) fn sleep(d: &Daemon, id: &str, by: By) -> anyhow::Result<()> {
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
        if by == (By::Checker { critical: false }) && shown(d).contains(id) {
            anyhow::bail!("{} stays awake: it's on screen", s.name);
        }
        if let Some(why) = why_not(d, &s, by == By::Name) {
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

/// Session `id` is waking: its agent starting, or up with what was held for it not all given yet.
pub(crate) fn waking(d: &Daemon, id: &str) -> bool {
    d.waking.lock().unwrap().contains_key(id)
}

/// Session `id`'s agent is starting, its screen not up yet: what clients show as waking.
pub(crate) fn starting(d: &Daemon, id: &str) -> bool {
    d.waking.lock().unwrap().get(id).is_some_and(|w| !w.drawn)
}

/// How much is held for session `id`, waking.
#[cfg(test)]
pub(crate) fn held(d: &Daemon, id: &str) -> usize {
    d.waking.lock().unwrap().get(id).map_or(0, |w| w.held.len())
}

/// Every session `starting`.
pub(crate) fn starting_ids(d: &Daemon) -> HashSet<String> {
    d.waking.lock().unwrap().iter().filter(|(_, w)| !w.drawn).map(|(id, _)| id.clone()).collect()
}

/// Wake session `id` if it's asleep (one tried and failed before is tried again), and answer once
/// its agent takes input; one waking already is waited for. One agent starts, however many ask.
pub(crate) fn wake(d: &Daemon, id: &str) -> anyhow::Result<()> {
    {
        let waking = d.waking.lock().unwrap();
        let s = attachable(d, id).ok_or_else(|| anyhow::anyhow!("no session {id}"))?;
        if !waking.contains_key(id) {
            if !s.asleep() {
                return Ok(());
            }
            start_wake(d, waking, &s, vec![]);
        }
    }
    let since = Instant::now();
    while starting(d, id) && since.elapsed() < READY_WITHIN + GONE_WITHIN {
        std::thread::sleep(READY_LOOK);
    }
    let s = attachable(d, id).ok_or_else(|| anyhow::anyhow!("{id} was closed as it woke"))?;
    if let Some((why, _)) = s.wake_failed.lock().unwrap().as_ref() {
        anyhow::bail!("{} didn't wake: {why}", s.name);
    }
    anyhow::ensure!(!s.ended(), "{} ended as it woke", s.name);
    Ok(())
}

/// Wake session `id` if it's asleep, off the caller's wait: a client came to look at it.
/// `retry`: one whose wake failed is tried again (shown anew); a client reattaching doesn't.
pub(crate) fn wake_soon(d: &Daemon, id: &str, retry: bool) {
    let waking = d.waking.lock().unwrap();
    let Some(s) = attachable(d, id) else { return };
    if waking.contains_key(id) || !s.asleep() || (!retry && s.wake_failed.lock().unwrap().is_some()) {
        return;
    }
    start_wake(d, waking, &s, vec![]);
}

/// Input for session `id` (a client's keys, a paste, a message), whichever of its programs the
/// caller saw last: to its agent, through `giving`, unless it's asleep or waking. Asleep, it's
/// held and the session wakes; waking, it's held until the agent can take it (see `settle`), but
/// for keys typed once its screen is up, which go straight to it, and the client terminal's answers
/// to the starting agent's queries (see `split_replies`), which go at once.
pub(crate) fn input(d: &Daemon, id: &str, what: Held) {
    let mut waking = d.waking.lock().unwrap();
    let Some(s) = attachable(d, id) else { return };
    if let Some(w) = waking.get_mut(id) {
        let what = match what {
            Held::Keys(bytes) => {
                let (replies, keys) = split_replies(&bytes);
                if !replies.is_empty() && !s.asleep() {
                    s.pane.write(replies);
                }
                if keys.is_empty() {
                    return;
                }
                Held::Keys(keys)
            }
            other => other,
        };
        // Typed seeing its screen: the user's, straight to it. Messages wait their turn.
        if !w.drawn || matches!(what, Held::Message(_)) {
            w.held.push(what);
            return;
        }
        return give_through(waking, &s, what);
    }
    if s.asleep() {
        // A wake that failed isn't tried again for every key: what's typed is kept with it.
        if let Some((_, kept)) = s.wake_failed.lock().unwrap().as_mut() {
            kept.push(what);
            return;
        }
        start_wake(d, waking, &s, vec![what]);
        return;
    }
    give_through(waking, &s, what);
}

/// Give `what` to awake `s` through its `giving` gate, taken before the waking lock (`waking`) goes
/// so a sleep decided next waits for it to land; or, with input on its way already, after it, never
/// waited for under that lock (a program that doesn't read its input holds a write up): that input
/// counts as typing, which keeps it awake meanwhile.
fn give_through(waking: MutexGuard<'_, HashMap<String, Wake>>, s: &Session, what: Held) {
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
}

/// The one way a wake starts: asleep `s`, `waking` (the caller's guard, with no entry for it) gets
/// one with `held` behind what was kept from a wake that failed; its agent is started in its place
/// on this thread, so a client attaching next finds it (see `attachable`), and waited for off it.
fn start_wake(d: &Daemon, mut waking: MutexGuard<'_, HashMap<String, Wake>>, s: &Session, mut held: Vec<Held>) {
    if let Some((_, kept)) = s.wake_failed.lock().unwrap().take() {
        held.splice(0..0, kept);
    }
    waking.insert(s.id.clone(), Wake { held, drawn: false });
    drop(waking);
    let woken = begin(d, &s.id);
    match (woken, d.me.upgrade()) {
        (Some(woken), Some(me)) => {
            let _ = std::thread::Builder::new().name("wake".into()).spawn(move || settle(&me, &woken));
        }
        (Some(woken), None) => settle(d, &woken),
        (None, _) => {}
    }
}

/// Give `s`'s agent what was held for it, as it would have had it: keys typed, a paste, a message
/// sent. What's typed and not sent yet keeps it awake (see `unsent`).
fn give(s: &Session, what: Held) {
    let now = Instant::now();
    *s.typed.lock().unwrap() = Some(now);
    match what {
        Held::Keys(keys) => {
            match draft(&keys) {
                Some(true) => *s.drafted.lock().unwrap() = Some(now),
                Some(false) => *s.submitted.lock().unwrap() = Some(now),
                None => {}
            }
            s.pane.write(keys);
        }
        Held::Text { text, submit } => {
            *(if submit { &s.submitted } else { &s.drafted }).lock().unwrap() = Some(now);
            send_input(s, &text, submit);
        }
        Held::Message(text) => {
            *s.submitted.lock().unwrap() = Some(now);
            send_input(s, &text, true);
        }
    }
}

/// Something was typed into `s`'s agent and not sent: its last key that added to the prompt line
/// came after the last that sent or cleared it. Asleep, the agent would forget it (Claude Code
/// doesn't keep a prompt it wasn't sent across a resume).
pub(crate) fn unsent(s: &Session) -> bool {
    let drafted = *s.drafted.lock().unwrap();
    drafted.is_some_and(|d| s.submitted.lock().unwrap().is_none_or(|sent| d > sent))
}

/// What keys do to an agent's prompt line, the last of them that does anything: add to it
/// (`Some(true)`: text, a paste), send or clear it (`Some(false)`: Return, Ctrl+C, Ctrl+U), or
/// neither (`None`: arrows, Escape, a mode key). A key the kitty keyboard protocol encodes
/// (`CSI <code>;<mods> u`) counts as the key it is; Shift+Return adds a line.
fn draft(keys: &[u8]) -> Option<bool> {
    let mut out = None;
    let mut paste = false;
    let mut i = 0;
    while i < keys.len() {
        match (keys[i], keys.get(i + 1)) {
            (0x1b, Some(b'[')) => {
                let from = i + 2;
                let Some(n) = keys[from..].iter().position(|b| (0x40..=0x7e).contains(b)) else { break };
                let params = std::str::from_utf8(&keys[from..from + n]).unwrap_or("");
                match (keys[from + n], params) {
                    (b'~', "200") => paste = true,
                    (b'~', "201") => paste = false,
                    (b'u', p) => {
                        let mut fields = p.split(';');
                        let code: u32 = fields.next().and_then(|c| c.split(':').next()?.parse().ok()).unwrap_or(0);
                        let mods = fields.next().and_then(|m| m.split(':').next()?.parse::<u32>().ok()).unwrap_or(1).saturating_sub(1);
                        let (shift, ctrl) = (mods & 1 != 0, mods & 0b1110 != 0);
                        out = match code {
                            13 if !shift && !ctrl => Some(false),
                            13 => Some(true),
                            99 | 117 if mods & 4 != 0 => Some(false),
                            c if c >= 32 && c != 127 && !ctrl => Some(true),
                            _ => out,
                        };
                    }
                    _ => {}
                }
                i = from + n + 1;
                continue;
            }
            // SS3 (an arrow in application mode) or Alt+key: no text.
            (0x1b, Some(b'O')) => i += 3,
            (0x1b, _) => i += 2,
            (b'\r' | b'\n', _) if !paste => {
                out = Some(false);
                i += 1;
            }
            (0x03 | 0x15, _) => {
                out = Some(false);
                i += 1;
            }
            (b, _) => {
                if paste || (b >= 0x20 && b != 0x7f) {
                    out = Some(true);
                }
                i += 1;
            }
        }
    }
    out
}

/// Start session `id`'s agent again in its place, its entry in `waking` made: on its conversation,
/// with the controls it slept with (or any chosen meanwhile) and its last screen above. The
/// session now in its place. Failing, it stays asleep and says why in its pane, what was held for
/// it kept with the failure (see `start_wake`) and not tried again until it's asked for by name or
/// shown anew; closed meanwhile, there's nothing to keep.
fn begin(d: &Daemon, id: &str) -> Option<Arc<Session>> {
    let asleep = attachable(d, id);
    let started = (|| {
        let s = asleep.clone().ok_or_else(|| anyhow::anyhow!("no session {id}"))?;
        let screen = s.pane.replay(LIVE_HISTORY);
        let controls = s.pending.lock().unwrap().clone().unwrap_or_else(|| mode::current(d, &s));
        restart_with(d, id, Some(screen), |saved| {
            if saved.controls.model != controls.model {
                d.proxy.stats.reset_context(id);
            }
            saved.controls = controls;
        })?;
        attachable(d, id).filter(|s| !s.asleep()).ok_or_else(|| anyhow::anyhow!("it was closed as it woke"))
    })();
    bump(d);
    match started {
        Ok(s) => {
            eprintln!("{} dinod: session {id} waking", stamp());
            Some(s)
        }
        Err(e) => {
            let held = d.waking.lock().unwrap().remove(id).map(|w| w.held).unwrap_or_default();
            eprintln!("{} dinod: session {id} didn't wake: {e}{}", stamp(), if held.is_empty() { String::new() } else { format!(" ({} inputs kept for it)", held.len()) });
            if let Some(s) = asleep.filter(|s| s.asleep() && attachable(d, id).is_some_and(|now| Arc::ptr_eq(&now, s))) {
                say_asleep(&s, &format!("couldn't wake it: {e}. What you type is kept for it; Wake (or `dino wake {id}`) tries again."));
                *s.wake_failed.lock().unwrap() = Some((e.to_string(), held));
            }
            None
        }
    }
}

/// A line from dino in asleep `s`'s pane, as `say_in` writes one in a running session's: into its
/// frozen screen, and to the clients showing it.
fn say_asleep(s: &Session, text: &str) {
    let note = format!("\r\n\x1b[38;2;117;179;64m▲▲ dino\x1b[0m  {text}\r\n").into_bytes();
    s.pane.feed(&note);
    s.subscribers.lock().unwrap().retain(|(_, tx)| tx.send(note.clone()).is_ok());
}

/// Wait until the woken agent `s` is up (it reads keys and shows its screen, as a starting agent is
/// told to: `schedule::agent_ready`); from then on keys typed seeing it go straight to it. Then give
/// it what was held, front to back, each once it may have it (`may_give`): none of it while it asks
/// something on its screen, a dialog whoever typed blind never saw; a message only while it waits
/// for a prompt and nobody's typing one. What's still held after `READY_WITHIN` isn't given, and its
/// pane says so; dropped if the agent ends first.
fn settle(d: &Daemon, s: &Arc<Session>) {
    let since = Instant::now();
    let mut steady = None;
    while !s.pane.is_exited() && since.elapsed() < READY_WITHIN && !crate::schedule::agent_ready(s, &mut steady, false) {
        std::thread::sleep(READY_LOOK);
    }
    if let Some(w) = d.waking.lock().unwrap().get_mut(&s.id) {
        w.drawn = true;
    }
    eprintln!("{} dinod: session {} awake in {} ms", stamp(), s.id, since.elapsed().as_millis());
    bump(d);
    loop {
        if s.pane.is_exited() {
            let lost = d.waking.lock().unwrap().remove(&s.id).map_or(0, |w| w.held.len());
            if lost > 0 {
                eprintln!("{} dinod: session {} ended as it woke; {lost} inputs held for it dropped", stamp(), s.id);
            }
            break;
        }
        let next = {
            let mut waking = d.waking.lock().unwrap();
            let Some(w) = waking.get_mut(&s.id) else { break };
            match w.held.first() {
                None => {
                    waking.remove(&s.id);
                    break;
                }
                Some(h) if may_give(d, s, h) => Some(w.held.remove(0)),
                Some(_) if since.elapsed() >= READY_WITHIN => {
                    let left = waking.remove(&s.id).map(|w| w.held).unwrap_or_default();
                    drop(waking);
                    not_given(d, s, &left);
                    break;
                }
                Some(_) => None,
            }
        };
        match next {
            Some(h) => {
                let _giving = s.giving.lock().unwrap();
                give(s, h);
            }
            None => std::thread::sleep(READY_LOOK),
        }
    }
    bump(d);
}

/// Whether held `h` may go to woken `s` now (see `settle`).
fn may_give(d: &Daemon, s: &Session, h: &Held) -> bool {
    if asking(s) {
        return false;
    }
    match h {
        Held::Message(_) => crate::idle(d, s) && !typing(s),
        _ => true,
    }
}

/// `s`'s agent asks something on its screen: a question, a dialog, its sign-in or folder trust.
fn asking(s: &Session) -> bool {
    let screen = s.pane.text(0);
    s.adapter().is_some_and(|a| a.asking(&screen).is_some()) || (s.agent_now() == "claude" && crate::setup_prompt(&screen).is_some())
}

/// Someone's typing in `s`: a key lately, or a prompt typed and not sent.
fn typing(s: &Session) -> bool {
    s.typed.lock().unwrap().is_some_and(|t| t.elapsed() < crate::peers::USER_TYPING) || unsent(s)
}

/// What was held for woken `s` and not given in time, said in its pane: what it was, and why.
fn not_given(d: &Daemon, s: &Session, left: &[Held]) {
    let why = if asking(s) {
        "it's still asking something"
    } else if left.first().is_some_and(|h| matches!(h, Held::Message(_))) && typing(s) {
        "you were typing in it"
    } else if left.first().is_some_and(|h| matches!(h, Held::Message(_))) && !crate::idle(d, s) {
        "it was busy"
    } else {
        "it didn't take input"
    };
    let what: Vec<String> = left
        .iter()
        .map(|h| match h {
            Held::Keys(k) => format!("“{}”", printable(&String::from_utf8_lossy(&split_replies(k).1))),
            Held::Text { text, .. } => format!("“{}”", printable(text)),
            Held::Message(text) => format!("a message, “{}”", printable(text)),
        })
        .collect();
    eprintln!("{} dinod: session {}: {} inputs held as it woke not given: {why}", stamp(), s.id, left.len());
    crate::say_in(s, &format!("held while it woke and not sent, as {why}: {}", what.join(", ")));
}

/// `text` shortened to one line, without control characters (an escape sequence's ESC among them).
fn printable(text: &str) -> String {
    let t: String = text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.chars().count() > 80 { t.chars().take(79).collect::<String>() + "…" } else { t }
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
        let _ = std::thread::Builder::new().name("wake".into()).spawn(move || wake_soon(&d, &id, true));
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

    #[test]
    fn keys_that_add_to_a_prompt_are_told_from_those_that_send_it() {
        assert_eq!(draft(b"half a prompt"), Some(true));
        assert_eq!(draft(b"done\r"), Some(false));
        assert_eq!(draft(b"\r"), Some(false));
        assert_eq!(draft(b"\x03"), Some(false), "Ctrl+C clears it");
        assert_eq!(draft(b"\x15"), Some(false), "so does Ctrl+U");
        // Arrows, in either mode, Escape, Backspace, Shift+Tab: nothing.
        assert_eq!(draft(b"\x1b[A\x1bOB\x1b\x7f\x1b[Z"), None);
        // A paste is text, its lines and all.
        assert_eq!(draft(b"\x1b[200~a\rb\x1b[201~"), Some(true));
        // Kitty-encoded: a letter, Return, Shift+Return (a new line), Ctrl+C.
        assert_eq!(draft(b"\x1b[97u"), Some(true));
        assert_eq!(draft(b"\x1b[13u"), Some(false));
        assert_eq!(draft(b"\x1b[13;2u"), Some(true));
        assert_eq!(draft(b"\x1b[99;5u"), Some(false));
        assert_eq!(draft(b"x\x1b[99;5u"), Some(false), "the last that does anything");
    }
}
