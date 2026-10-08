//! Automations (see `dino_core::schedule`): a trigger fires, the conditions are checked, the
//! action runs, and the run is followed to its end, when its result is kept (the agent's last
//! message, what it changed, its PR), passed on (a PR comment, the automations that come after
//! it) and, if it failed, tried again.
//!
//! One thread does the deciding: it wakes every 30 seconds for the schedules (a time missed while
//! the Mac slept or dinod wasn't running is caught up once, when it's back: only the latest one,
//! and only if it's under a week old), every few seconds while a run is going or files changed,
//! and at once when told. GitHub and git are looked at by a second thread (see `triggers`), and
//! files are watched by FSEvents, so an idle automation costs nothing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use dino_core::ipc::LauncherInfo;
use dino_core::schedule::{
    ActionKind, Event, Frequency, MAX_HISTORY, MAX_QUEUE, MAX_SEEN, Retry, ScheduledRun, ScheduledTask, TriggerKind, TriggerState, fill, fill_command, shorten,
    split_args,
};
use dino_core::controls::Controls;
use dino_core::settings::{AgentSwitch, Settings};
use dino_core::worktree;
use dino_proxy::Activity;

use crate::{Daemon, Launch, fallbacks, finished, idle, new_uuid, now_secs, save, send_input, spawn, spawn_in_worktree, triggers, work_dir};

/// Later than this after its time, a run counts as a catch-up.
const LATE: u64 = 120;

/// How long a shell gets to start before its task's command is typed in.
const SHELL_READY: Duration = Duration::from_secs(30);

/// How long an agent waiting for its first message has to keep waiting while its screen moves.
const AGENT_STEADY: Duration = Duration::from_secs(3);

/// How often the deciding thread looks, with nothing going on (schedules are to the minute).
const IDLE_LOOK: Duration = Duration::from_secs(30);
/// … while a run is going or files changed,
const BUSY_LOOK: Duration = Duration::from_secs(2);
/// … and while a session is followed for an automation that comes after its turn.
const FOLLOW_LOOK: Duration = Duration::from_secs(5);

/// A run's agent that is still idle this long after it was started, without having called its
/// model once, never got going (a prompt it was waiting on, a model that isn't there).
const NO_START: Duration = Duration::from_secs(180);

/// A command gets this long.
const COMMAND_TIME: Duration = Duration::from_secs(30 * 60);
/// What's kept of what a command printed.
const OUTPUT_KEEP: usize = 4000;
/// What's kept of an agent's last message as the run's summary; the comment gets more.
const SUMMARY_KEEP: usize = 600;
const COMMENT_KEEP: usize = 6000;

/// Files that changed settle this long before the automation fires, so one save (or a branch
/// switch) is one run.
pub(crate) const SETTLE: Duration = Duration::from_secs(3);

#[derive(Default)]
pub(crate) struct Scheduler {
    tasks: Mutex<Vec<ScheduledTask>>,
    /// One decision at a time (a schedule, an event, a run's end), so nothing runs twice.
    ticking: Mutex<()>,
    /// Wakes the deciding thread: set by a change to an automation, a file change, an event.
    wake: Arc<(Mutex<bool>, Condvar)>,
    /// Runs being followed, by run id.
    live: Mutex<HashMap<String, Live>>,
    /// Sessions followed for "after a session finishes" triggers: whether it was busy, since when
    /// it has looked done, and its model calls when its last turn ended.
    turns: Mutex<HashMap<String, (bool, Option<Instant>, u64)>>,
    pub(crate) triggers: triggers::Triggers,
}

/// A run that hasn't finished.
struct Live {
    task: String,
    /// The prompt still to send, when the session it continues was busy.
    deliver: Option<String>,
    /// Model calls each session had made when the run began: more since means it went to work.
    baseline: HashMap<String, u64>,
    since: Instant,
    /// When it first looked done (two looks apart before it counts).
    done_since: Option<Instant>,
    /// Followed again after dinod restarted: the call counts started over.
    restored: bool,
    /// A command's result, once it's in.
    command: Option<Arc<Mutex<Option<(Option<i32>, String)>>>>,
    event: Option<Event>,
}

impl Scheduler {
    pub(crate) fn load() -> Self {
        let mut tasks: Vec<ScheduledTask> = std::fs::read(path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        // Runs from before automations had no ids: give them one, so they can be followed.
        for t in &mut tasks {
            for r in &mut t.history {
                if r.id.is_empty() {
                    r.id = new_uuid()[..8].to_string();
                }
                if r.sessions.is_empty() {
                    r.sessions.extend(r.session.clone());
                }
            }
            if t.state.since == 0 {
                t.state.since = t.created_at;
            }
        }
        Self { tasks: Mutex::new(tasks), ..Default::default() }
    }

    fn poke(&self) {
        let (m, c) = &*self.wake;
        *m.lock().unwrap() = true;
        c.notify_all();
    }

    pub(crate) fn waker(&self) -> Arc<(Mutex<bool>, Condvar)> {
        self.wake.clone()
    }

    pub(crate) fn tasks(&self) -> Vec<ScheduledTask> {
        self.tasks.lock().unwrap().clone()
    }

    /// Change the task `id` in place and keep it, if it's still there (on disk only if it changed).
    pub(crate) fn update<R>(&self, id: &str, f: impl FnOnce(&mut ScheduledTask) -> R) -> Option<R> {
        let mut tasks = self.tasks.lock().unwrap();
        let t = tasks.iter_mut().find(|t| t.id == id)?;
        let before = t.clone();
        let r = f(t);
        if *t != before {
            store(&tasks);
        }
        Some(r)
    }

    pub(crate) fn busy(&self, id: &str) -> bool {
        self.live.lock().unwrap().values().any(|l| l.task == id)
    }
}

fn path() -> PathBuf {
    dino_core::config_dir().join("schedule.json")
}

fn store(tasks: &[ScheduledTask]) {
    let _ = crate::write_private(&path(), &serde_json::to_vec_pretty(tasks).unwrap_or_default());
}

/// Decide now, and whenever there's reason to after.
pub(crate) fn start(d: &Arc<Daemon>) {
    restore_live(d);
    triggers::start(d);
    let d = d.clone();
    std::thread::Builder::new()
        .name("dinod-automations".into())
        .spawn(move || {
            let mut awake_checked: Option<Instant> = None;
            loop {
                let now = now_secs();
                // Runs that ended first, so what comes next doesn't wait on them.
                follow(d.as_ref());
                tick(d.as_ref(), now);
                triggers::files(d.as_ref());
                owed(d.as_ref(), now);
                // It reads the settings: at the idle pace, however often runs are looked at.
                if awake_checked.is_none_or(|t| t.elapsed() >= IDLE_LOOK) {
                    keep_awake(d.as_ref());
                    awake_checked = Some(Instant::now());
                }
                let wait = next_look(d.as_ref());
                let (m, c) = &*d.schedule.wake;
                let mut woken = m.lock().unwrap();
                if !*woken {
                    woken = c.wait_timeout(woken, wait).unwrap().0;
                }
                *woken = false;
            }
        })
        .expect("dinod: no thread for automations");
}

fn next_look(d: &Daemon) -> Duration {
    let busy = !d.schedule.live.lock().unwrap().is_empty() || d.schedule.triggers.files_pending();
    let following = !d.schedule.turns.lock().unwrap().is_empty();
    let now = now_secs();
    let retry = d.schedule.tasks.lock().unwrap().iter().filter_map(|t| t.state.retry.as_ref().map(|r| r.at)).min();
    let mut wait = if busy {
        BUSY_LOOK
    } else if following {
        FOLLOW_LOOK
    } else {
        IDLE_LOOK
    };
    if let Some(at) = retry {
        wait = wait.min(Duration::from_secs(at.saturating_sub(now).max(1)));
    }
    wait
}

/// The tasks, with when each runs next and what keeps a trigger from looking.
pub(crate) fn list(d: &Daemon) -> Vec<ScheduledTask> {
    let now = now_secs();
    let mut tasks = d.schedule.tasks.lock().unwrap().clone();
    let problems = d.schedule.triggers.problems();
    for t in &mut tasks {
        t.next_run = if t.enabled && t.scheduled() { next_due(t.frequency, now) } else { None };
        t.problem = problems.get(&t.id).cloned();
        // Only dinod needs to know which events it has seen.
        t.state.seen.clear();
    }
    tasks
}

/// Add or replace a task. Only times after this count: an edit or a resume owes no runs from
/// before. A changed trigger starts over: only events from now on count.
pub(crate) fn put(d: &Daemon, mut t: ScheduledTask) -> anyhow::Result<ScheduledTask> {
    t.name = t.name.trim().to_string();
    anyhow::ensure!(!t.name.is_empty(), "give the automation a name");
    check(d, &mut t)?;
    let now = now_secs();
    let mut tasks = d.schedule.tasks.lock().unwrap();
    anyhow::ensure!(
        !tasks.iter().any(|o| o.id != t.id && o.name.eq_ignore_ascii_case(&t.name)),
        "there's already an automation called {}",
        t.name
    );
    if t.trigger.on == TriggerKind::After {
        let other = tasks.iter().find(|o| o.id == t.trigger.after || o.name.eq_ignore_ascii_case(t.trigger.after.trim()));
        if let Some(o) = other {
            anyhow::ensure!(o.id != t.id, "an automation can't come after itself");
            // Stored by id: a rename keeps the chain.
            t.trigger.after = o.id.clone();
            // A chain that comes round again would never stop.
            let mut at = o.clone();
            for _ in 0..tasks.len() {
                if at.trigger.on != TriggerKind::After {
                    break;
                }
                anyhow::ensure!(at.trigger.after != t.id || t.id.is_empty(), "that would make a loop: {} already comes after {}", o.name, t.name);
                match tasks.iter().find(|x| x.id == at.trigger.after) {
                    Some(n) => at = n.clone(),
                    None => break,
                }
            }
        } else {
            let sessions = d.sessions.lock().unwrap();
            let s = sessions.iter().find(|s| s.id == t.trigger.after || s.name == t.trigger.after.trim());
            let s = s.ok_or_else(|| anyhow::anyhow!("no automation or session {}", t.trigger.after))?;
            t.trigger.after = s.id.clone();
            anyhow::ensure!(
                !(t.action.kind == ActionKind::Continue && (t.action.session == s.id || t.action.session == s.name)),
                "continuing the session it comes after would run forever"
            );
        }
    }
    t.last_due = prev_due(t.frequency, now);
    t.next_run = None;
    t.problem = None;
    let old = tasks.iter().find(|o| !t.id.is_empty() && o.id == t.id).cloned();
    t.state = match &old {
        // Same trigger, same place: what it has seen still counts.
        Some(o) if o.trigger == t.trigger && o.cwd == t.cwd => TriggerState { runs: if t.enabled && !o.enabled { 0 } else { o.state.runs }, ..o.state.clone() },
        _ => TriggerState { since: now, ..Default::default() },
    };
    match old {
        Some(o) => {
            t.created_at = o.created_at;
            t.history = o.history;
            if let Some(slot) = tasks.iter_mut().find(|x| x.id == t.id) {
                *slot = t.clone();
            }
        }
        None => {
            t.id = new_uuid()[..8].to_string();
            t.created_at = now;
            t.history = vec![];
            tasks.push(t.clone());
        }
    }
    store(&tasks);
    drop(tasks);
    keep_awake(d);
    d.schedule.triggers.changed();
    d.schedule.poke();
    Ok(t)
}

/// Whether `t` can run as set up, filling in what was left to dinod.
fn check(d: &Daemon, t: &mut ScheduledTask) -> anyhow::Result<()> {
    let dir = work_dir(Some(&t.cwd));
    anyhow::ensure!(dir.is_dir(), "{} isn't a folder", t.cwd);
    let agent = match t.action.kind {
        ActionKind::Agent => true,
        ActionKind::Command => {
            anyhow::ensure!(!t.action.command.trim().is_empty(), "give the command to run");
            anyhow::ensure!(matches!(t.action.then_agent.as_str(), "" | "never" | "failure" | "always"), "then_agent is never, failure or always");
            matches!(t.action.then_agent.as_str(), "failure" | "always")
        }
        ActionKind::Continue => {
            anyhow::ensure!(!t.prompt.trim().is_empty(), "give the prompt to send");
            let sessions = d.sessions.lock().unwrap();
            let s = sessions.iter().find(|s| s.id == t.action.session || s.name == t.action.session.trim());
            let s = s.ok_or_else(|| anyhow::anyhow!("no session {} to continue", t.action.session))?;
            anyhow::ensure!(s.agent_id != "shell", "{} is a shell: continue an agent's session", s.name);
            t.action.session = s.id.clone();
            false
        }
        ActionKind::Agents => {
            anyhow::ensure!(!t.prompt.trim().is_empty(), "give the prompt for the agents");
            anyhow::ensure!(!t.action.agents.is_empty(), "pick the agents to start");
            worktree::repo_root(&dir).map_err(|_| anyhow::anyhow!("{} isn't in a git repository: each agent gets a worktree of it", dir.display()))?;
            for a in &t.action.agents {
                let l = d.allowed_launcher(a)?;
                anyhow::ensure!(l.agent_id != "shell", "a shell can't take a prompt");
            }
            false
        }
    };
    if agent {
        let l = d.allowed_launcher(&t.launcher)?;
        let needs_prompt = t.action.kind == ActionKind::Agent && l.agent_id != "shell";
        anyhow::ensure!(!needs_prompt || !t.prompt.trim().is_empty(), "give the automation a prompt");
        if t.worktree {
            worktree::repo_root(&dir).map_err(|_| anyhow::anyhow!("{} isn't in a git repository, so runs can't have their own worktree", dir.display()))?;
        }
        if let Some(r) = &t.route {
            crate::provider_route(&l, r.clone())?;
        }
    }
    if let Frequency::Hourly { minute } | Frequency::Daily { minute, .. } | Frequency::Weekdays { minute, .. } | Frequency::Weekly { minute, .. } = t.frequency {
        anyhow::ensure!(minute < 60, "no such minute: {minute}");
    }
    if let Frequency::Daily { hour, .. } | Frequency::Weekdays { hour, .. } | Frequency::Weekly { hour, .. } = t.frequency {
        anyhow::ensure!(hour < 24, "no such hour: {hour}");
    }
    if let Frequency::Weekly { weekday, .. } = t.frequency {
        anyhow::ensure!(weekday < 7, "no such weekday: {weekday}");
    }
    let tr = &mut t.trigger;
    tr.repo = tr.repo.trim().trim_start_matches("https://github.com/").trim_end_matches('/').to_string();
    match tr.on {
        TriggerKind::Schedule | TriggerKind::After => {}
        k if k.github() => {
            if !tr.repo.is_empty() {
                anyhow::ensure!(tr.repo.split('/').count() == 2 && !tr.repo.contains(' '), "a GitHub repo is owner/name, not {}", tr.repo);
            } else if k != TriggerKind::ReviewRequested {
                tr.repo = crate::github::repo_of(&dir).ok_or_else(|| anyhow::anyhow!("{} isn't a clone of a GitHub repo: name the repo (owner/name)", dir.display()))?;
            }
            anyhow::ensure!(k != TriggerKind::IssueLabeled || !tr.label.trim().is_empty(), "name the label");
            anyhow::ensure!(k != TriggerKind::Comment || !tr.phrase.trim().is_empty(), "give the words a comment has to say");
        }
        k if k.git() => {
            worktree::repo_root(&dir).map_err(|_| anyhow::anyhow!("{} isn't in a git repository", dir.display()))?;
        }
        TriggerKind::Files => {
            let folder = if tr.path.trim().is_empty() { dir.clone() } else { dir.join(tr.path.trim()) };
            anyhow::ensure!(folder.is_dir(), "{} isn't a folder", folder.display());
        }
        _ => {}
    }
    if !matches!(t.trigger.when.as_str(), "" | "any" | "success" | "failure") {
        anyhow::bail!("after a run's success, failure or any end, not {}", t.trigger.when);
    }
    anyhow::ensure!(matches!(t.conditions.on_limit.as_str(), "" | "skip" | "fallback" | "run"), "at a limit: skip, fallback or run");
    Ok(())
}

pub(crate) fn delete(d: &Daemon, id: &str) -> anyhow::Result<()> {
    let mut tasks = d.schedule.tasks.lock().unwrap();
    let before = tasks.len();
    tasks.retain(|t| t.id != id);
    anyhow::ensure!(tasks.len() < before, "no automation {id}");
    store(&tasks);
    drop(tasks);
    d.schedule.live.lock().unwrap().retain(|_, l| l.task != id);
    keep_awake(d);
    d.schedule.triggers.changed();
    Ok(())
}

/// Run now, paused or not, whatever the conditions; the run is in the history like any other.
/// Answers with the session it started, if it started one.
pub(crate) fn run_now(d: &Daemon, id: &str) -> anyhow::Result<String> {
    let t = d.schedule.tasks.lock().unwrap().iter().find(|t| t.id == id).cloned();
    let t = t.ok_or_else(|| anyhow::anyhow!("no automation {id}"))?;
    let _one = d.schedule.ticking.lock().unwrap();
    let run = run(d, &t, Why { manual: true, ..Default::default() });
    match run {
        Some(r) if r.outcome == "failed" => Err(anyhow::anyhow!(r.reason.unwrap_or_default())),
        Some(r) => Ok(r.session.unwrap_or_default()),
        None => Ok(String::new()),
    }
}

/// Why a run starts.
#[derive(Default)]
struct Why {
    /// The scheduled time it's for.
    due: Option<u64>,
    catch_up: bool,
    event: Option<Event>,
    /// Asked for (Run now): the conditions don't apply.
    manual: bool,
    attempt: u32,
}

/// Run every task whose latest scheduled time up to `now` hasn't been dealt with.
pub(crate) fn tick(d: &Daemon, now: u64) {
    let _one = d.schedule.ticking.lock().unwrap();
    let due: Vec<(ScheduledTask, u64)> = d
        .schedule
        .tasks
        .lock()
        .unwrap()
        .iter()
        .filter(|t| t.enabled && t.scheduled())
        .filter_map(|t| {
            let due = prev_due(t.frequency, now)?;
            (due > t.last_due.unwrap_or(t.created_at)).then(|| (t.clone(), due))
        })
        .collect();
    for (t, due) in due {
        let catch_up = now.saturating_sub(due) > LATE;
        d.schedule.update(&t.id, |t| t.last_due = Some(due));
        run(d, &t, Why { due: Some(due), catch_up, ..Default::default() });
    }
}

/// An event for task `id`: run it unless it already ran for this one.
pub(crate) fn deliver(d: &Daemon, id: &str, event: Event) {
    let _one = d.schedule.ticking.lock().unwrap();
    let fresh = d.schedule.update(id, |t| {
        if !t.enabled || t.state.seen.contains(&event.key) {
            return None;
        }
        t.state.seen.push(event.key.clone());
        let extra = t.state.seen.len().saturating_sub(MAX_SEEN);
        t.state.seen.drain(..extra);
        Some(t.clone())
    });
    if let Some(Some(t)) = fresh {
        run(d, &t, Why { event: Some(event), ..Default::default() });
    }
}

/// Retries that are due, and events that waited for the run before them.
fn owed(d: &Daemon, now: u64) {
    let _one = d.schedule.ticking.lock().unwrap();
    for t in d.schedule.tasks() {
        if let Some(r) = t.state.retry.clone().filter(|r| r.at <= now) {
            d.schedule.update(&t.id, |t| t.state.retry = None);
            if t.enabled {
                run(d, &t, Why { event: r.event, attempt: r.attempt, ..Default::default() });
            }
            continue;
        }
        if t.enabled && !t.state.queue.is_empty() && (t.conditions.parallel || !d.schedule.busy(&t.id)) {
            let next = d.schedule.update(&t.id, |t| (!t.state.queue.is_empty()).then(|| t.state.queue.remove(0))).flatten();
            if let Some(e) = next {
                run(d, &t, Why { event: Some(e), ..Default::default() });
            }
        }
    }
}

/// Start a run of `t`, conditions permitting, and keep it in the history; none when it waits
/// for the run before it.
fn run(d: &Daemon, t: &ScheduledTask, why: Why) -> Option<ScheduledRun> {
    let base = ScheduledRun {
        id: new_uuid()[..8].to_string(),
        at: now_secs(),
        due: why.due,
        catch_up: why.catch_up,
        event: why.event.clone(),
        attempt: why.attempt,
        ..Default::default()
    };
    let mut launcher = t.launcher.clone();
    if !why.manual {
        match hold(d, t, &why) {
            Ok(Some(other)) => launcher = other,
            Ok(None) => {}
            Err(Hold::Wait) => {
                let e = why.event?;
                d.schedule.update(&t.id, |t| {
                    t.state.queue.push(e);
                    let extra = t.state.queue.len().saturating_sub(MAX_QUEUE);
                    t.state.queue.drain(..extra);
                });
                return None;
            }
            Err(Hold::Skip(reason)) => {
                let run = ScheduledRun { outcome: "skipped".into(), reason: Some(reason), ..base };
                record(d, &t.id, run.clone());
                return Some(run);
            }
        }
    }
    let fingerprint = if t.conditions.if_changed { fingerprint(&work_dir(Some(&t.cwd))) } else { None };
    let run = match fire(d, t, &launcher, why.event.as_ref(), &base.id) {
        Ok(started) => {
            let run = ScheduledRun { session: started.sessions.first().cloned(), sessions: started.sessions.clone(), outcome: "started".into(), ..base };
            let baseline = started.sessions.iter().map(|s| (s.clone(), d.proxy.stats.session(s).requests)).collect();
            d.schedule.live.lock().unwrap().insert(
                run.id.clone(),
                Live { task: t.id.clone(), deliver: started.deliver, baseline, since: Instant::now(), done_since: None, restored: false, command: started.command, event: why.event.clone() },
            );
            // Followed from now on, every few seconds rather than the idle half minute.
            d.schedule.poke();
            run
        }
        Err(e) => {
            let run = ScheduledRun { outcome: "failed".into(), reason: Some(e.to_string()), ..base };
            retry_later(d, t, &run);
            run
        }
    };
    let started = run.outcome == "started";
    d.schedule.update(&t.id, |t| {
        if started {
            t.state.runs += 1;
            t.state.fingerprint = fingerprint;
            // Ran its last: paused, the way a person would.
            if !why.manual && t.conditions.max_runs > 0 && t.state.runs >= t.conditions.max_runs {
                t.enabled = false;
            }
        }
    });
    record(d, &t.id, run.clone());
    Some(run)
}

enum Hold {
    /// Not now: after the run before it.
    Wait,
    Skip(String),
}

/// Whether `t` may run now: `Ok(Some(agent))` to run on another agent than its own.
fn hold(d: &Daemon, t: &ScheduledTask, why: &Why) -> Result<Option<String>, Hold> {
    let c = &t.conditions;
    if c.max_runs > 0 && t.state.runs >= c.max_runs {
        return Err(Hold::Skip(format!("it already ran {} times", t.state.runs)));
    }
    if c.ac_power && on_battery() {
        return Err(Hold::Skip("the Mac was on battery".into()));
    }
    if c.lid_open && lid_closed() {
        return Err(Hold::Skip("the lid was closed".into()));
    }
    if !c.parallel && let Some(prev) = still_going(d, t) {
        // An event is owed a run; a scheduled time just comes round again.
        return if why.event.is_some() && why.attempt == 0 { Err(Hold::Wait) } else { Err(Hold::Skip(format!("the previous run ({prev}) was still going"))) };
    }
    if c.if_changed && why.attempt == 0 && t.state.runs > 0 {
        let now = fingerprint(&work_dir(Some(&t.cwd)));
        if now.is_some() && now == t.state.fingerprint {
            return Err(Hold::Skip("nothing changed since the last run".into()));
        }
    }
    let uses_agent = matches!(t.action.kind, ActionKind::Agent) || (t.action.kind == ActionKind::Command && t.action.then_agent == "always");
    if uses_agent && t.route.is_none() && let Some(l) = d.launcher(&t.launcher) && let Some(limit) = at_limit(d, &l) {
        return match c.on_limit.as_str() {
            "run" => Ok(None),
            "fallback" => match fallback(d, &l) {
                Some((other, _)) => Ok(Some(other.short)),
                None => Err(Hold::Skip(format!("{limit}, and it has no fallback agent"))),
            },
            _ => Err(Hold::Skip(limit)),
        };
    }
    Ok(None)
}

/// The agent to use when `l`'s account is at its limit, and its model: the one its new sessions
/// start with meanwhile (Settings → Agents → When it hits a limit).
pub(crate) fn fallback(d: &Daemon, l: &LauncherInfo) -> Option<(LauncherInfo, AgentSwitch)> {
    fallbacks::switch_to(d, &Settings::load(), &l.agent_id)
}

/// Why `l` can't take a run now: a route its own account uses is known spent (see `fallbacks`), or
/// its subscription's window is used up (as the provider last said). Claude Code with more of the
/// user's Claude accounts, its calls through dino: only once every one of them is, as each last
/// found it, for dino's proxy signs its calls with one that isn't (see `dino_proxy::accounts`).
pub(crate) fn at_limit(d: &Daemon, l: &LauncherInfo) -> Option<String> {
    at_limit_routed(d, l, || Settings::load().routing.proxy)
}

/// `at_limit`, `routed` saying whether its new sessions talk through dino (Settings → routing).
pub(crate) fn at_limit_routed(d: &Daemon, l: &LauncherInfo, routed: impl FnOnce() -> bool) -> Option<String> {
    let accounts = (l.agent_id == "claude").then(|| d.proxy.claude_accounts_now()).flatten().filter(|_| routed());
    if let Some(accounts) = &accounts
        && let Some(first) = dino_proxy::accounts::all_spent(accounts.iter().map(|(_, l, q)| (l.as_ref(), q.as_ref())), now_secs())
    {
        let until = first.resets_at.map(|r| format!(" until {}, when the first resets", clock(r))).unwrap_or_default();
        let all = if accounts.len() == 2 { "both".to_string() } else { format!("all {}", accounts.len()) };
        return Some(format!("{} is at its limit on {all} Claude accounts{until}", l.label));
    }
    if let Some(spent) = fallbacks::limits(d).into_iter().find(|s| s.agent_id == l.agent_id) {
        let until = spent.resets_at.map(|r| format!(" until {}", clock(r))).unwrap_or_default();
        return Some(format!("{} is at its {} ({}){until}", l.label, spent.reason, spent.name));
    }
    let provider = match l.agent_id.as_str() {
        // Its own windows say nothing while another account has room.
        "claude" if accounts.is_some() => return None,
        "claude" => "anthropic",
        "codex" => "chatgpt",
        _ => return None,
    };
    let q = d.proxy.stats.quota(provider)?;
    let now = now_secs();
    let (name, w) = q.windows.iter().find(|(_, w)| w.utilization >= 1.0 && w.resets_at.is_none_or(|r| r > now))?;
    let until = w.resets_at.map(|r| format!(" until {}", clock(r))).unwrap_or_default();
    Some(format!("{} is at its {name} limit{until}", l.label))
}

pub(crate) fn clock(t: u64) -> String {
    let tm = local(t);
    format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
}

/// The Mac is running on its battery (`pmset -g batt`).
#[cfg(target_os = "macos")]
fn on_battery() -> bool {
    let out = Command::new(dino_core::power::PMSET).args(["-g", "batt"]).stdin(Stdio::null()).stderr(Stdio::null()).output();
    out.is_ok_and(|o| dino_core::power::battery(&String::from_utf8_lossy(&o.stdout)).0 == Some(false))
}

/// Linux: on battery when the machine has a power adapter (a `Mains` supply in sysfs) and none is
/// online. A server or a container has none, so it's never on battery.
#[cfg(target_os = "linux")]
fn on_battery() -> bool {
    let Ok(dir) = std::fs::read_dir("/sys/class/power_supply") else { return false };
    let read = |p: std::path::PathBuf| std::fs::read_to_string(p).unwrap_or_default().trim().to_string();
    let adapters: Vec<String> = dir.flatten().map(|e| e.path()).filter(|p| read(p.join("type")) == "Mains").map(|p| read(p.join("online"))).collect();
    !adapters.is_empty() && adapters.iter().all(|o| o == "0")
}

/// Linux: the lid is closed, as ACPI reports it (`/proc/acpi/button/lid/*/state`). No lid, no.
#[cfg(target_os = "linux")]
fn lid_closed() -> bool {
    let Ok(dir) = std::fs::read_dir("/proc/acpi/button/lid") else { return false };
    dir.flatten().any(|e| std::fs::read_to_string(e.path().join("state")).is_ok_and(|s| s.contains("closed")))
}

/// The lid is closed (its clamshell state in the I/O registry).
#[cfg(target_os = "macos")]
fn lid_closed() -> bool {
    let out = Command::new("/usr/sbin/ioreg").args(["-r", "-k", "AppleClamshellState", "-d", "1"]).stdin(Stdio::null()).stderr(Stdio::null()).output();
    out.is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("\"AppleClamshellState\" = Yes"))
}

/// The repo at `dir` as it is: its commit and its uncommitted changes. None outside a repo.
fn fingerprint(dir: &Path) -> Option<String> {
    use sha2::Digest;
    let root = worktree::repo_root(dir).ok()?;
    let git = |args: &[&str]| Command::new("git").arg("-C").arg(&root).args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok().filter(|o| o.status.success()).map(|o| o.stdout);
    let head = git(&["rev-parse", "HEAD"]).unwrap_or_default();
    let status = git(&["status", "--porcelain=v1", "-z", "--untracked-files=normal"])?;
    // Edits to a file already changed don't change its status line: its content counts too.
    let diff = git(&["diff", "HEAD", "--no-ext-diff", "--binary"]).unwrap_or_default();
    let mut h = sha2::Sha256::new();
    h.update(&head);
    h.update(&status);
    h.update(&diff);
    Some(hex::encode(&h.finalize()[..12]))
}

fn record(d: &Daemon, id: &str, run: ScheduledRun) {
    d.schedule.update(id, |t| {
        match t.history.iter_mut().find(|r| r.id == run.id) {
            Some(r) => *r = run,
            None => t.history.push(run),
        }
        let extra = t.history.len().saturating_sub(MAX_HISTORY);
        t.history.drain(..extra);
    });
}

/// The task's last run, by session name, while it's still going.
fn still_going(d: &Daemon, t: &ScheduledTask) -> Option<String> {
    let live = d.schedule.live.lock().unwrap();
    let run = t.history.iter().rev().find(|r| live.contains_key(&r.id))?;
    let sessions = d.sessions.lock().unwrap();
    Some(match run.session.as_ref() {
        Some(id) => sessions.iter().find(|s| &s.id == id).map_or_else(|| id.clone(), |s| s.name.clone()),
        None => "a command".into(),
    })
}

/// What a run started.
#[derive(Default)]
struct Started {
    sessions: Vec<String>,
    deliver: Option<String>,
    command: Option<Arc<Mutex<Option<(Option<i32>, String)>>>>,
}

/// The placeholders a run fills: its event's, and a few of its own.
fn fields(t: &ScheduledTask, event: Option<&Event>) -> std::collections::BTreeMap<String, String> {
    let mut f = event.map(|e| e.fields.clone()).unwrap_or_default();
    f.insert("automation".into(), t.name.clone());
    if let Some(e) = event {
        f.insert("event".into(), e.title.clone());
        f.entry("event.url".into()).or_insert_with(|| e.url.clone().unwrap_or_default());
    }
    f
}

/// Do what `t` does.
fn fire(d: &Daemon, t: &ScheduledTask, launcher: &str, event: Option<&Event>, run_id: &str) -> anyhow::Result<Started> {
    let prompt = fill(t.prompt.trim(), &fields(t, event));
    match t.action.kind {
        ActionKind::Agent => Ok(Started { sessions: vec![start_agent(d, t, launcher, &prompt)?], ..Default::default() }),
        ActionKind::Continue => {
            let s = d.sessions.lock().unwrap().iter().find(|s| s.id == t.action.session).cloned();
            let s = s.ok_or_else(|| anyhow::anyhow!("the session it continues is gone"))?;
            if s.pane.is_exited() {
                crate::resume(d, &s.id)?;
            }
            Ok(Started { sessions: vec![s.id.clone()], deliver: Some(prompt), ..Default::default() })
        }
        ActionKind::Agents => {
            // Each a session of its own in a worktree of its own, like a one-agent run's.
            let mut sessions = vec![];
            for a in &t.action.agents {
                let l = d.allowed_launcher(a)?;
                let each = ScheduledTask {
                    launcher: l.short.clone(),
                    worktree: true,
                    args: String::new(),
                    // A provider's model for the agents that can take it, the rest on their own accounts.
                    route: t.route.clone().filter(|r| crate::provider_route(&l, r.clone()).is_ok()),
                    ..t.clone()
                };
                match start_agent(d, &each, &l.short, &prompt) {
                    Ok(id) => sessions.push(id),
                    // The ones started already run on: the run is theirs.
                    Err(e) if sessions.is_empty() => return Err(e),
                    Err(e) => eprintln!("dinod: {} didn't start {}: {e}", t.name, l.short),
                }
            }
            Ok(Started { sessions, ..Default::default() })
        }
        ActionKind::Command => {
            let dir = work_dir(Some(&t.cwd));
            anyhow::ensure!(dir.is_dir(), "{} doesn't exist any more", t.cwd);
            let command = fill_command(t.action.command.trim(), &fields(t, event));
            let slot = Arc::new(Mutex::new(None));
            let (out, wake, name) = (slot.clone(), d.schedule.waker(), format!("dinod-command-{run_id}"));
            std::thread::Builder::new().name(name).spawn(move || {
                let result = run_command(&command, &dir);
                *out.lock().unwrap() = Some(result);
                let (m, c) = &*wake;
                *m.lock().unwrap() = true;
                c.notify_all();
            })?;
            Ok(Started { command: Some(slot), ..Default::default() })
        }
    }
}

/// `command` with `sh -c` in `dir`: its exit status (none if it was stopped) and the end of what
/// it printed, both streams together.
fn run_command(command: &str, dir: &Path) -> (Option<i32>, String) {
    use std::io::Read;
    let child = Command::new("/bin/sh")
        .arg("-c")
        .arg(format!("{{ {command}\n}} 2>&1"))
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => return (None, format!("couldn't start it: {e}")),
    };
    let mut pipe = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut all = Vec::new();
        let mut buf = [0u8; 8192];
        while let Some(Ok(n)) = pipe.as_mut().map(|p| p.read(&mut buf)) {
            if n == 0 {
                break;
            }
            all.extend_from_slice(&buf[..n]);
            // The end is what matters: keep a little more than is shown.
            if all.len() > 4 * OUTPUT_KEEP {
                all.drain(..all.len() - 2 * OUTPUT_KEEP);
            }
        }
        all
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s.code(),
            Ok(None) if start.elapsed() > COMMAND_TIME => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(200)),
            Err(_) => break None,
        }
    };
    let out = String::from_utf8_lossy(&reader.join().unwrap_or_default()).into_owned();
    let tail: String = { let n = out.chars().count(); out.chars().skip(n.saturating_sub(OUTPUT_KEEP)).collect() };
    (status, if status.is_none() && start.elapsed() > COMMAND_TIME { format!("{tail}\n(stopped after {} minutes)", COMMAND_TIME.as_secs() / 60) } else { tail })
}

/// A new session named after the task. Claude isn't started where it would first ask whether to
/// trust the folder: nobody is there to answer, and dino won't answer for the user.
fn start_agent(d: &Daemon, t: &ScheduledTask, launcher: &str, prompt: &str) -> anyhow::Result<String> {
    let l = d.allowed_launcher(launcher)?;
    let dir = work_dir(Some(&t.cwd));
    anyhow::ensure!(dir.is_dir(), "{} doesn't exist any more", t.cwd);
    check_trust(&l, &dir, t.worktree)?;
    let name = {
        let sessions = d.sessions.lock().unwrap();
        let taken = |n: &str| sessions.iter().any(|s| s.name == n);
        std::iter::once(t.name.clone()).chain((2..).map(|n| format!("{}-{n}", t.name))).find(|n| !taken(n)).unwrap()
    };
    // Agents take the prompt as an argument (see `spawn`); a shell gets it typed in, as a command.
    let shell = l.agent_id == "shell";
    // Instead of the task's own agent, at its limit: with the model its fallback names.
    let model = (l.short != t.launcher).then(|| d.launcher(&t.launcher).and_then(|own| fallback(d, &own))).flatten().filter(|(o, _)| o.short == l.short).and_then(|(_, s)| s.model);
    let launch = Launch {
        name: Some(name),
        scheduled: Some(t.name.clone()),
        prompt: (!shell && !prompt.is_empty()).then(|| prompt.to_string()),
        // The fallback agent runs on its own account.
        route: t.route.clone().filter(|_| l.short == t.launcher),
        controls: Controls { model, ..Default::default() },
        // The task's conditions said what to run at a limit (`hold`): spawn doesn't decide again.
        stay: true,
        ..Launch::new(&l.short, if l.short == t.launcher { split_args(&t.args) } else { vec![] }, Some(dir.display().to_string()))
    };
    let id = if t.worktree { spawn_in_worktree(d, launch)? } else { spawn(d, launch)? };
    // Started by dinod, not by a request: the tree learns of it here.
    super::reshaped(d);
    save(d);
    if shell && !prompt.is_empty() {
        type_when_ready(d, &id, prompt);
    }
    Ok(id)
}

/// An agent that asks whether to trust a folder it hasn't seen would wait there with nobody to
/// answer: refuse up front.
pub(crate) fn check_trust(l: &LauncherInfo, dir: &Path, worktree: bool) -> anyhow::Result<()> {
    if let Some(a) = dino_core::agent::agent(&l.agent_id).filter(|a| a.asks_trust()) {
        let root = worktree::repo_root(dir).unwrap_or_else(|_| dir.to_path_buf());
        let trusted = a.trusted_in(dir, &root);
        anyhow::ensure!(trusted.is_some(), "Claude doesn't trust {} yet. Start Claude there once and accept its trust prompt", dir.display());
        // The repo's own trust carries into a worktree by itself; a folder inside it only through dino.
        anyhow::ensure!(
            !worktree || trusted.is_some_and(|r| r.as_os_str().is_empty()) || Settings::load().policies.worktree_trust,
            "Claude would ask to trust the new worktree. Turn on worktree trust in Settings → Workspaces → Worktrees, or run without a worktree"
        );
    }
    Ok(())
}

/// Type `text` into a new session once it waits for it: a shell's command, once the shell has drawn
/// its prompt and gone quiet; the first message of an agent that takes none on its command line,
/// once it reads keys (see `Pane::reads_keys`: starting up, it can go quiet for a while, and what's
/// typed then is lost), shows something, and has gone quiet or stayed so for `AGENT_STEADY` (one
/// whose screen keeps moving, as Amp's welcome does). Never while it asks something on its screen
/// (whether to trust the folder, to sign in), which the text would answer: the user answers that,
/// and the text goes in after.
pub(crate) fn type_when_ready(d: &Daemon, id: &str, text: &str) {
    let Some(s) = d.sessions.lock().unwrap().iter().find(|s| s.id == id).cloned() else { return };
    let text = text.to_string();
    std::thread::spawn(move || {
        let start = Instant::now();
        let shell = s.agent_id == "shell";
        let asks = |screen: &str| s.adapter().is_some_and(|a| a.asking(screen).is_some());
        // Since when the agent has read keys, shown something and asked nothing.
        let mut steady: Option<Instant> = None;
        loop {
            if s.pane.is_exited() {
                return;
            }
            let quiet = s.last_write.lock().unwrap().is_some_and(|t| t.elapsed() > Duration::from_secs(1));
            let ready = if shell {
                (quiet || start.elapsed() > SHELL_READY) && !asks(&s.pane.text(0))
            } else {
                let waits = s.pane.reads_keys() && {
                    let screen = s.pane.text(0);
                    !screen.trim().is_empty() && !asks(&screen)
                };
                steady = steady.filter(|_| waits).or_else(|| waits.then(Instant::now));
                waits && (quiet || steady.is_some_and(|t| t.elapsed() > AGENT_STEADY))
            };
            if ready {
                break;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        send_input(&s, &text, true);
    });
}

// ---- Following a run to its end ----

/// Runs that were going when dinod stopped: followed again, now that their sessions are back.
/// One whose sessions are all gone ended while nobody watched: when, nobody knows.
fn restore_live(d: &Daemon) {
    let sessions: Vec<String> = d.sessions.lock().unwrap().iter().map(|s| s.id.clone()).collect();
    let mut live = d.schedule.live.lock().unwrap();
    let mut tasks = d.schedule.tasks.lock().unwrap();
    let mut changed = false;
    for t in tasks.iter_mut() {
        for r in t.history.iter_mut().filter(|r| r.outcome == "started" && r.finished_at.is_none()) {
            if !r.sessions.is_empty() && !r.sessions.iter().any(|s| sessions.contains(s)) {
                r.finished_at = Some(r.at);
                changed = true;
                continue;
            }
            let baseline = r.sessions.iter().map(|s| (s.clone(), 0)).collect();
            live.insert(r.id.clone(), Live { task: t.id.clone(), deliver: None, baseline, since: Instant::now(), done_since: None, restored: true, command: None, event: r.event.clone() });
        }
    }
    if changed {
        store(&tasks);
    }
}

/// How a run's session is: `None` while it's going, else whether it went well.
fn session_done(d: &Daemon, id: &str, baseline: u64, live: &Live) -> Option<bool> {
    let Some(s) = d.sessions.lock().unwrap().iter().find(|s| s.id == id).cloned() else {
        // Closed or archived: over, as far as anyone can tell.
        return Some(true);
    };
    if s.pane.is_exited() {
        return Some(s.pane.exit_code().unwrap_or(0) == 0);
    }
    // A shell's command calls no model: done once it has been quiet a while.
    if s.agent_id == "shell" {
        let quiet = s.last_write.lock().unwrap().is_none_or(|w| w.elapsed() > Duration::from_secs(5));
        return (quiet && live.since.elapsed() > Duration::from_secs(10)).then_some(true);
    }
    if !finished(d, &s) {
        return None;
    }
    let st = d.proxy.stats.session(id);
    let ok = st.last_error.is_none() && st.limit_error.is_none();
    // A new session's first turn may end without a call through dino (an agent that reports its
    // turns); a session continued has to call its model again, or its last turn's Done counts.
    let worked = st.requests > baseline || (baseline == 0 && st.activity == Some(Activity::Done));
    if worked || live.restored {
        Some(ok)
    } else if live.since.elapsed() > NO_START {
        Some(false)
    } else {
        None
    }
}

/// Look at every run that's going: send what waits to be sent, start the agent after a
/// command, and finish the ones that are done.
fn follow(d: &Daemon) {
    let _one = d.schedule.ticking.lock().unwrap();
    let ids: Vec<String> = d.schedule.live.lock().unwrap().keys().cloned().collect();
    for run_id in ids {
        let Some(task) = d.schedule.live.lock().unwrap().get(&run_id).map(|l| l.task.clone()) else { continue };
        let Some(t) = d.schedule.tasks().into_iter().find(|t| t.id == task) else {
            d.schedule.live.lock().unwrap().remove(&run_id);
            continue;
        };
        // A prompt for a session that was busy: now it isn't.
        let deliver = {
            let live = d.schedule.live.lock().unwrap();
            live.get(&run_id).and_then(|l| l.deliver.clone().map(|p| (p, l.baseline.keys().next().cloned())))
        };
        if let Some((prompt, Some(sid))) = deliver {
            let s = d.sessions.lock().unwrap().iter().find(|s| s.id == sid).cloned();
            match s {
                Some(s) if !s.pane.is_exited() && idle(d, &s) && s.last_write.lock().unwrap().is_some_and(|w| w.elapsed() > Duration::from_secs(2)) => {
                    send_input(&s, &prompt, true);
                    let mut live = d.schedule.live.lock().unwrap();
                    if let Some(l) = live.get_mut(&run_id) {
                        l.deliver = None;
                        l.baseline.insert(sid.clone(), d.proxy.stats.session(&sid).requests);
                        l.since = Instant::now();
                    }
                }
                Some(_) => {}
                None => finish(d, &t, &run_id, false, Some("the session it continues is gone".into())),
            }
            continue;
        }
        // A command that's done: an agent with its output, or the end.
        let command = d.schedule.live.lock().unwrap().get(&run_id).and_then(|l| l.command.clone());
        if let Some(slot) = command {
            let Some((exit, output)) = slot.lock().unwrap().take() else { continue };
            let failed = exit != Some(0);
            record_command(d, &t.id, &run_id, exit, &output);
            let agent = match t.action.then_agent.as_str() {
                "always" => true,
                "failure" => failed,
                _ => false,
            };
            if !agent {
                finish(d, &t, &run_id, !failed, None);
                continue;
            }
            let event = d.schedule.live.lock().unwrap().get(&run_id).and_then(|l| l.event.clone());
            let mut f = fields(&t, event.as_ref());
            f.insert("cmd.exit".into(), exit.map_or("none (stopped)".into(), |c| c.to_string()));
            f.insert("cmd.output".into(), output.clone());
            f.insert("cmd.command".into(), t.action.command.clone());
            let prompt = if t.prompt.trim().is_empty() { default_fix_prompt(&t, exit, &output) } else { fill(t.prompt.trim(), &f) };
            match start_agent(d, &t, &t.launcher, &prompt) {
                Ok(sid) => {
                    let requests = d.proxy.stats.session(&sid).requests;
                    if let Some(l) = d.schedule.live.lock().unwrap().get_mut(&run_id) {
                        l.command = None;
                        l.baseline = [(sid.clone(), requests)].into();
                        l.since = Instant::now();
                    }
                    d.schedule.update(&t.id, |t| {
                        if let Some(r) = t.history.iter_mut().find(|r| r.id == run_id) {
                            r.session = Some(sid.clone());
                            r.sessions = vec![sid.clone()];
                        }
                    });
                }
                Err(e) => finish(d, &t, &run_id, false, Some(format!("the command failed, and the agent couldn't start: {e}"))),
            }
            continue;
        }
        // Sessions: done when every one is, two looks apart (a pause between tools isn't the end).
        let (baseline, done) = {
            let live = d.schedule.live.lock().unwrap();
            let Some(l) = live.get(&run_id) else { continue };
            let results: Vec<Option<bool>> = l.baseline.iter().map(|(s, b)| session_done(d, s, *b, l)).collect();
            (l.baseline.len(), results.iter().all(Option::is_some).then(|| results.iter().all(|r| *r == Some(true))))
        };
        if baseline == 0 {
            let restored = d.schedule.live.lock().unwrap().get(&run_id).is_some_and(|l| l.restored);
            // A command whose result was lost when dinod stopped.
            let why = restored.then(|| "dino's background service stopped while it ran".to_string());
            finish(d, &t, &run_id, !restored, why);
            continue;
        }
        let mut live = d.schedule.live.lock().unwrap();
        let Some(l) = live.get_mut(&run_id) else { continue };
        match done {
            None => l.done_since = None,
            Some(ok) => match l.done_since {
                None => l.done_since = Some(Instant::now()),
                Some(at) if at.elapsed() >= Duration::from_secs(4) => {
                    drop(live);
                    finish(d, &t, &run_id, ok, None);
                }
                Some(_) => {}
            },
        }
    }
    follow_sessions(d);
}

fn default_fix_prompt(t: &ScheduledTask, exit: Option<i32>, output: &str) -> String {
    let how = exit.map_or("was stopped".to_string(), |c| format!("exited with status {c}"));
    format!("This command {how}:\n\n    {}\n\nWhat it printed (the end of it):\n\n```\n{}\n```\n\nFind out why, and fix it.", t.action.command.trim(), output.trim_end())
}

fn record_command(d: &Daemon, id: &str, run_id: &str, exit: Option<i32>, output: &str) {
    d.schedule.update(id, |t| {
        if let Some(r) = t.history.iter_mut().find(|r| r.id == run_id) {
            r.exit = exit;
            r.output = Some(output.to_string());
        }
    });
}

/// A run is over: keep what it came to, pass it on, and try again if it failed.
fn finish(d: &Daemon, t: &ScheduledTask, run_id: &str, ok: bool, reason: Option<String>) {
    let live = d.schedule.live.lock().unwrap().remove(run_id);
    let Some(mut run) = t.history.iter().find(|r| r.id == run_id).cloned() else { return };
    // The task as it is now: a command's output was recorded since `t` was read.
    if let Some(now) = d.schedule.tasks().into_iter().find(|x| x.id == t.id).and_then(|x| x.history.into_iter().find(|r| r.id == run_id)) {
        run = now;
    }
    let mut messages = vec![];
    for sid in &run.sessions {
        let s = d.sessions.lock().unwrap().iter().find(|s| &s.id == sid).cloned();
        let Some(s) = s else { continue };
        if let Some(m) = last_message(&s).or_else(|| (s.agent_id == "shell").then(|| screen_tail(&s, &t.prompt)).flatten()) {
            messages.push(if run.sessions.len() > 1 { format!("{}: {m}", s.launcher) } else { m });
        }
        if let Ok(Ok((dir, base, _))) = crate::changes_base(d, sid)
            && let Ok(stat) = worktree::stat(&dir, &base)
        {
            let c = run.changes.get_or_insert_default();
            c.files += stat.files;
            c.added += stat.added;
            c.removed += stat.removed;
        }
        if run.pr.is_none() {
            run.pr = d.prs.lock().unwrap().get(sid).map(|p| p.url.clone());
        }
    }
    if run.pr.is_none() {
        run.pr = run.event.as_ref().and_then(|e| e.fields.get("pr.url")).filter(|u| !u.is_empty()).cloned();
    }
    let full = if !messages.is_empty() {
        Some(messages.join("\n\n"))
    } else {
        run.output.as_ref().map(|o| {
            let last: Vec<&str> = o.lines().filter(|l| !l.trim().is_empty()).collect();
            let tail = last[last.len().saturating_sub(3)..].join("\n");
            match run.exit {
                Some(0) => format!("Exited 0. {tail}"),
                Some(c) => format!("Exited {c}. {tail}"),
                None => format!("Stopped. {tail}"),
            }
        })
    };
    run.summary = full.as_deref().map(|m| shorten(m, SUMMARY_KEEP));
    run.finished_at = Some(now_secs());
    run.result = Some(if ok { "success" } else { "failure" }.into());
    if reason.is_some() {
        run.reason = reason;
    }
    record(d, &t.id, run.clone());
    if t.output.pr_comment && let Some(text) = full.clone() {
        post_comment(d, t, &run, text);
    }
    if !ok {
        retry_later(d, t, &run);
    }
    // What comes after it.
    let event = Event {
        on: TriggerKind::After,
        key: format!("after:{}", run.id),
        title: format!("{} {}", t.name, if ok { "succeeded" } else { "failed" }),
        url: run.pr.clone(),
        fields: [
            ("after.name".to_string(), t.name.clone()),
            ("after.outcome".into(), if ok { "success" } else { "failure" }.into()),
            ("after.summary".into(), full.unwrap_or_default()),
            ("after.session".into(), run.session.clone().unwrap_or_default()),
        ]
        .into_iter()
        .chain(run.session.as_deref().map(|s| place(d, s)).unwrap_or_default())
        .collect(),
    };
    chain(d, &t.id, ok, &event);
    drop(live);
    d.schedule.poke();
}

/// Where session `id` worked and what its changes are measured from, for what comes after it to
/// look at: `git -C {after.dir} diff {after.base}`.
fn place(d: &Daemon, id: &str) -> Vec<(String, String)> {
    match crate::changes_base(d, id) {
        Ok(Ok((dir, base, _))) => vec![("after.dir".into(), dir.display().to_string()), ("after.base".into(), base)],
        _ => vec![],
    }
}

/// Fire the automations that come after `id` (an automation, or a session) on this outcome.
fn chain(d: &Daemon, id: &str, ok: bool, event: &Event) {
    let next: Vec<String> = d
        .schedule
        .tasks()
        .iter()
        .filter(|n| n.enabled && n.trigger.on == TriggerKind::After && n.trigger.after == id)
        .filter(|n| match n.trigger.when.as_str() {
            "success" => ok,
            "failure" => !ok,
            _ => true,
        })
        .map(|n| n.id.clone())
        .collect();
    // The deciding lock is held already: `deliver` would wait on it.
    for n in next {
        deliver_held(d, &n, event.clone());
    }
}

/// `deliver`, for a caller that already holds the deciding lock.
fn deliver_held(d: &Daemon, id: &str, event: Event) {
    let fresh = d.schedule.update(id, |t| {
        if !t.enabled || t.state.seen.contains(&event.key) {
            return None;
        }
        t.state.seen.push(event.key.clone());
        let extra = t.state.seen.len().saturating_sub(MAX_SEEN);
        t.state.seen.drain(..extra);
        Some(t.clone())
    });
    if let Some(Some(t)) = fresh {
        run(d, &t, Why { event: Some(event), ..Default::default() });
    }
}

/// Sessions that "after a session finishes" automations wait on: a turn that ends fires them.
fn follow_sessions(d: &Daemon) {
    let watched: Vec<String> = d
        .schedule
        .tasks()
        .iter()
        .filter(|t| t.enabled && t.trigger.on == TriggerKind::After)
        .map(|t| t.trigger.after.clone())
        .filter(|id| d.sessions.lock().unwrap().iter().any(|s| &s.id == id))
        .collect();
    let mut turns = d.schedule.turns.lock().unwrap();
    turns.retain(|id, _| watched.contains(id));
    let mut ended = vec![];
    for id in watched {
        let Some(s) = d.sessions.lock().unwrap().iter().find(|s| s.id == id).cloned() else { continue };
        let requests = d.proxy.stats.session(&id).requests;
        let entry = turns.entry(id.clone()).or_insert((false, None, requests));
        let done = s.pane.is_exited() || finished(d, &s);
        if !done {
            *entry = (true, None, entry.2);
            continue;
        }
        // Two looks apart, as for runs; only after it was seen working, and only if it called its
        // model meanwhile (an agent redrawing as it resumes is no turn).
        match entry {
            (true, None, _) => entry.1 = Some(Instant::now()),
            (true, Some(at), before) if at.elapsed() >= Duration::from_secs(4) => {
                let worked = requests > *before;
                *entry = (false, None, requests);
                if !worked {
                    continue;
                }
                let st = d.proxy.stats.session(&id);
                let ok = if s.pane.is_exited() { s.pane.exit_code().unwrap_or(0) == 0 } else { st.last_error.is_none() && st.limit_error.is_none() };
                ended.push((s.clone(), ok, st.requests));
            }
            _ => {}
        }
    }
    drop(turns);
    for (s, ok, requests) in ended {
        let event = Event {
            on: TriggerKind::After,
            key: format!("turn:{}:{requests}:{}", s.id, now_secs()),
            title: format!("{} finished its turn", s.name),
            url: None,
            fields: [
                ("after.name".to_string(), s.name.clone()),
                ("after.outcome".into(), if ok { "success" } else { "failure" }.into()),
                ("after.summary".into(), last_message(&s).unwrap_or_default()),
                ("after.session".into(), s.id.clone()),
            ]
            .into_iter()
            .chain(place(d, &s.id))
            .collect(),
        };
        chain(d, &s.id, ok, &event);
    }
}

/// What a shell printed for the command typed into it: the lines after the command, without the
/// prompt it came back to; else its last lines on screen.
fn screen_tail(s: &crate::Session, command: &str) -> Option<String> {
    let text = s.pane.text(200);
    let lines: Vec<&str> = text.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()).collect();
    let first = command.lines().next().unwrap_or_default().trim();
    let after = (!first.is_empty()).then(|| lines.iter().rposition(|l| l.ends_with(first))).flatten();
    let out: Vec<&str> = match after {
        Some(i) if i + 2 < lines.len() => lines[i + 1..lines.len() - 1].to_vec(),
        Some(_) => vec![],
        None => lines[lines.len().saturating_sub(4)..].to_vec(),
    };
    let out = &out[out.len().saturating_sub(12)..];
    (!out.is_empty()).then(|| out.join("\n"))
}

/// What the agent of `s` said last, from its own record of the conversation.
fn last_message(s: &crate::Session) -> Option<String> {
    let id = s.agent_session.lock().unwrap().clone().or_else(|| crate::conversation_of(s))?;
    let page = dino_core::history::conversation(&s.agent_id, &id, None)?;
    page.turns.iter().rev().find(|t| t.role == "assistant" && !t.text.trim().is_empty()).map(|t| shorten(&t.text, COMMENT_KEEP))
}

/// Try a failed run again later, if the automation says so.
fn retry_later(d: &Daemon, t: &ScheduledTask, run: &ScheduledRun) {
    if run.attempt >= t.conditions.retries {
        return;
    }
    let backoff = u64::from(t.conditions.backoff.max(1)) << run.attempt.min(10);
    let at = now_secs() + backoff;
    d.schedule.update(&t.id, |t| t.state.retry = Some(Retry { at, attempt: run.attempt + 1, event: run.event.clone() }));
}

/// The summary, as a comment on the PR that started the run, or else the run's own PR.
fn post_comment(d: &Daemon, t: &ScheduledTask, run: &ScheduledRun, text: String) {
    let target = run
        .event
        .as_ref()
        .and_then(|e| Some((e.fields.get("repo")?.clone(), e.fields.get("pr.number").or_else(|| e.fields.get("issue.number"))?.clone())))
        .filter(|(r, n)| !r.is_empty() && !n.is_empty())
        .or_else(|| {
            let sid = run.session.as_ref()?;
            let number = d.prs.lock().unwrap().get(sid)?.number;
            let cwd = d.sessions.lock().unwrap().iter().find(|s| &s.id == sid)?.cwd.clone();
            Some((crate::github::repo_of(&cwd)?, number.to_string()))
        });
    let Some((repo, number)) = target else {
        let note = "no PR to comment on".to_string();
        d.schedule.update(&t.id, |t| {
            if let Some(r) = t.history.iter_mut().find(|r| r.id == run.id) {
                r.commented = None;
                r.reason.get_or_insert(note);
            }
        });
        return;
    };
    let outcome = if run.result.as_deref() == Some("success") { "" } else { " (failed)" };
    let body = format!("**{}**{outcome}\n\n{}\n\n{}", t.name, shorten(&text, COMMENT_KEEP), dino_core::triggers::MARK);
    let posted = d.schedule.triggers.github.post(&format!("/repos/{repo}/issues/{number}/comments"), &serde_json::json!({ "body": body }));
    d.schedule.update(&t.id, |t| {
        if let Some(r) = t.history.iter_mut().find(|r| r.id == run.id) {
            match &posted {
                Ok(v) => r.commented = v["html_url"].as_str().map(String::from),
                Err(e) => r.reason = Some(format!("couldn't comment on the PR: {e}")),
            }
        }
    });
}

/// While the setting is on and any task runs on a schedule, dinod's own power assertion keeps the
/// Mac from idle sleep (see [`crate::awake`], which also holds it for working agents and the lid).
pub(crate) fn keep_awake(d: &Daemon) {
    let settings = Settings::load();
    let scheduled = settings.machine.keep_awake && d.schedule.tasks.lock().unwrap().iter().any(|t| t.enabled && t.scheduled() && t.frequency != Frequency::Manual);
    d.awake.set_scheduled(scheduled);
    crate::awake::apply(d, &settings);
}

// ---- When: local time, through libc, so daylight saving time is the system's. ----

fn local(t: u64) -> libc::tm {
    let t = t as libc::time_t;
    // SAFETY: localtime_r only writes the tm it's given.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        tm
    }
}

fn make(mut tm: libc::tm) -> u64 {
    // SAFETY: mktime reads and normalizes the tm it's given.
    unsafe { libc::mktime(&mut tm).max(0) as u64 }
}

/// `hour:minute` local time, `days` after the day `base` is on.
fn on_day(base: &libc::tm, days: i32, hour: u8, minute: u8) -> u64 {
    let mut tm = *base;
    tm.tm_mday += days;
    tm.tm_hour = hour.into();
    tm.tm_min = minute.into();
    tm.tm_sec = 0;
    // Whatever's in effect that day.
    tm.tm_isdst = -1;
    make(tm)
}

/// `minute` past the hour `now` is in.
fn in_hour(now: u64, minute: u8) -> u64 {
    let mut tm = local(now);
    tm.tm_min = minute.into();
    tm.tm_sec = 0;
    make(tm)
}

fn runs_on(f: Frequency, t: u64) -> bool {
    let day = local(t).tm_wday;
    match f {
        Frequency::Weekdays { .. } => (1..=5).contains(&day),
        Frequency::Weekly { weekday, .. } => day == i32::from(weekday),
        _ => true,
    }
}

/// The latest scheduled time at or before `now`; none for manual tasks.
pub(crate) fn prev_due(f: Frequency, now: u64) -> Option<u64> {
    let (hour, minute) = match f {
        Frequency::Manual => return None,
        Frequency::Hourly { minute } => {
            let t = in_hour(now, minute);
            return Some(if t > now { t - 3600 } else { t });
        }
        Frequency::Daily { hour, minute } | Frequency::Weekdays { hour, minute } | Frequency::Weekly { hour, minute, .. } => (hour, minute),
    };
    let base = local(now);
    (0..=7).map(|back| on_day(&base, -back, hour, minute)).find(|&t| t <= now && runs_on(f, t))
}

/// The first scheduled time after `now`; none for manual tasks.
pub(crate) fn next_due(f: Frequency, now: u64) -> Option<u64> {
    let (hour, minute) = match f {
        Frequency::Manual => return None,
        Frequency::Hourly { minute } => {
            let t = in_hour(now, minute);
            return Some(if t <= now { t + 3600 } else { t });
        }
        Frequency::Daily { hour, minute } | Frequency::Weekdays { hour, minute } | Frequency::Weekly { hour, minute, .. } => (hour, minute),
    };
    let base = local(now);
    (0..=7).map(|ahead| on_day(&base, ahead, hour, minute)).find(|&t| t > now && runs_on(f, t))
}

/// The first time after `now` a 5-field cron expression (`minute hour day-of-month month
/// day-of-week`, local time) matches: what an agent scheduled for itself, read as it reads it
/// (Claude's CronCreate): `*`, values, ranges, steps and lists; Sunday is 0 or 7; a day matches
/// either day field when both are given (vixie cron). None for what it doesn't read, or nothing
/// within five years (February 30th).
pub(crate) fn cron_next(expr: &str, now: u64) -> Option<u64> {
    let fields: Vec<&str> = expr.split_whitespace().collect();
    let [minute, hour, dom, month, dow] = fields[..] else { return None };
    let minutes = cron_field(minute, 0, 59)?;
    let hours = cron_field(hour, 0, 23)?;
    let days = cron_field(dom, 1, 31)?;
    let months = cron_field(month, 1, 12)?;
    let mut weekdays = cron_field(dow, 0, 7)?;
    if weekdays & (1 << 7) != 0 {
        weekdays |= 1;
    }
    let either = !dom.starts_with('*') && !dow.starts_with('*');
    let base = local(now);
    for ahead in 0..=(5 * 366) {
        let day = local(on_day(&base, ahead, 12, 0));
        let on_dom = days & (1 << day.tm_mday) != 0;
        let on_dow = weekdays & (1 << day.tm_wday) != 0;
        let on = if either { on_dom || on_dow } else { on_dom && on_dow };
        if months & (1 << (day.tm_mon + 1)) == 0 || !on {
            continue;
        }
        for h in (0..24u8).filter(|h| hours & (1 << h) != 0) {
            for m in (0..60u8).filter(|m| minutes & (1 << m) != 0) {
                let t = on_day(&base, ahead, h, m);
                // Not a time that day has (the hour the clocks skip).
                let got = local(t);
                if t > now && got.tm_hour == i32::from(h) && got.tm_min == i32::from(m) {
                    return Some(t);
                }
            }
        }
    }
    None
}

/// The values one cron field allows, as bits.
fn cron_field(field: &str, lo: u32, hi: u32) -> Option<u64> {
    let mut bits = 0u64;
    for part in field.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, s)) => (r, s.parse::<u32>().ok().filter(|&s| s > 0)?),
            None => (part, 1),
        };
        let (from, to) = if range == "*" {
            (lo, hi)
        } else if let Some((a, b)) = range.split_once('-') {
            (a.parse().ok()?, b.parse().ok()?)
        } else {
            let a: u32 = range.parse().ok()?;
            // "5/15": from 5 to the end, every 15.
            (a, if step > 1 { hi } else { a })
        };
        if from < lo || to > hi || from > to {
            return None;
        }
        bits |= (from..=to).step_by(step as usize).fold(0, |b, v| b | 1 << v);
    }
    Some(bits)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every test here runs in New York time, which has daylight saving time.
    fn new_york() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            // SAFETY: the only environment change in these tests, made once before any time is read.
            unsafe { std::env::set_var("TZ", "America/New_York") };
            unsafe extern "C" {
                fn tzset();
            }
            unsafe { tzset() };
        });
    }

    // 2026-03-10, a Tuesday, in EDT (the clocks went forward on Sunday the 8th).
    const TUE_0800: u64 = 1773144000;
    const TUE_0900: u64 = 1773147600;
    const TUE_0915: u64 = 1773148500;
    const TUE_1000: u64 = 1773151200;
    const TUE_1014: u64 = 1773152040;
    const TUE_1015: u64 = 1773152100;
    const MON_0900: u64 = 1773061200;
    const WED_0900: u64 = 1773234000;

    #[test]
    fn daily() {
        new_york();
        let f = Frequency::Daily { hour: 9, minute: 0 };
        assert_eq!(prev_due(f, TUE_1000), Some(TUE_0900));
        assert_eq!(next_due(f, TUE_1000), Some(WED_0900));
        assert_eq!(prev_due(f, TUE_0800), Some(MON_0900));
        assert_eq!(next_due(f, TUE_0800), Some(TUE_0900));
        // Exactly on time: due now, next tomorrow.
        assert_eq!(prev_due(f, TUE_0900), Some(TUE_0900));
        assert_eq!(next_due(f, TUE_0900), Some(WED_0900));
    }

    #[test]
    fn across_daylight_saving() {
        new_york();
        let f = Frequency::Daily { hour: 9, minute: 0 };
        let sat_0900 = 1772892000; // EST
        let sat_1000 = 1772895600;
        let sun_0900 = 1772974800; // EDT: 23 hours later
        assert_eq!(next_due(f, sat_1000), Some(sun_0900));
        assert_eq!(sun_0900 - sat_0900, 23 * 3600);
        assert_eq!(prev_due(f, sun_0900 + 60), Some(sun_0900));
        assert_eq!(prev_due(f, sun_0900 - 60), Some(sat_0900));
    }

    #[test]
    fn weekdays_skip_the_weekend() {
        new_york();
        let f = Frequency::Weekdays { hour: 9, minute: 0 };
        let fri_0900 = 1773406800;
        let sat_1200 = 1773504000;
        let mon_0900 = 1773666000;
        assert_eq!(prev_due(f, sat_1200), Some(fri_0900));
        assert_eq!(next_due(f, sat_1200), Some(mon_0900));
        assert_eq!(next_due(f, TUE_1000), Some(WED_0900));
    }

    #[test]
    fn weekly() {
        new_york();
        let sunday = Frequency::Weekly { weekday: 0, hour: 9, minute: 0 };
        let sun_0308 = 1772974800;
        let sun_0315 = 1773579600;
        assert_eq!(prev_due(sunday, TUE_1000), Some(sun_0308));
        assert_eq!(next_due(sunday, TUE_1000), Some(sun_0315));
        let tuesday = Frequency::Weekly { weekday: 2, hour: 9, minute: 0 };
        assert_eq!(prev_due(tuesday, TUE_1000), Some(TUE_0900));
        assert_eq!(next_due(tuesday, TUE_1000), Some(TUE_0900 + 7 * 86400));
        // A week earlier was before the clocks went forward: an hour more than 7 days.
        let tue_0303_0900 = 1772546400;
        assert_eq!(prev_due(tuesday, TUE_0800), Some(tue_0303_0900));
    }

    #[test]
    fn hourly() {
        new_york();
        let f = Frequency::Hourly { minute: 15 };
        assert_eq!(prev_due(f, TUE_1014), Some(TUE_0915));
        assert_eq!(next_due(f, TUE_1014), Some(TUE_1015));
        assert_eq!(prev_due(f, TUE_1015), Some(TUE_1015));
        assert_eq!(next_due(f, TUE_1015), Some(TUE_1015 + 3600));
    }

    #[test]
    fn manual_never_comes_due() {
        new_york();
        assert_eq!(prev_due(Frequency::Manual, TUE_1000), None);
        assert_eq!(next_due(Frequency::Manual, TUE_1000), None);
    }

    #[test]
    fn cron_expressions_as_an_agent_gives_them() {
        new_york();
        // Every hour at :23 (the founder's example).
        assert_eq!(cron_next("23 * * * *", TUE_1000), Some(TUE_1000 + 23 * 60));
        assert_eq!(cron_next("23 * * * *", TUE_1000 + 23 * 60), Some(TUE_1000 + 83 * 60));
        assert_eq!(cron_next("*/15 * * * *", TUE_1014), Some(TUE_1015));
        assert_eq!(cron_next("0 9 * * *", TUE_1000), Some(WED_0900));
        assert_eq!(cron_next("0 9 * * 1-5", TUE_0800), Some(TUE_0900));
        // Monday 9:00 from Tuesday: six days on; Sunday as 7 is Sunday.
        assert_eq!(cron_next("0 9 * * 1", TUE_1000), Some(MON_0900 + 7 * 86400));
        assert_eq!(cron_next("0 9 * * 7", TUE_1000), cron_next("0 9 * * 0", TUE_1000));
        // A date: March 15, 2:30 PM (EDT).
        assert_eq!(cron_next("30 14 15 3 *", TUE_1000), Some(1773599400));
        // Both day fields: either one (vixie cron), so the 11th (a Wednesday) or any Monday.
        assert_eq!(cron_next("0 9 11 * 1", TUE_1000), Some(WED_0900));
        assert_eq!(cron_next("0 9 1,15 * *", TUE_1000), Some(1773579600));
        // What it doesn't read, and a day that never comes.
        for bad in ["", "* * * *", "60 * * * *", "*/0 * * * *", "0 9 * * MON", "0 9 L * *", "5-1 * * * *"] {
            assert_eq!(cron_next(bad, TUE_1000), None, "{bad}");
        }
        assert_eq!(cron_next("0 9 30 2 *", TUE_1000), None);
        // The hour the clocks skipped (2:30 AM, Sunday March 8th) isn't a time: next year's.
        let sat = TUE_1000 - 3 * 86400;
        assert_eq!(cron_next("30 2 8 3 *", sat - 86400), Some(1804491000));
    }
}
