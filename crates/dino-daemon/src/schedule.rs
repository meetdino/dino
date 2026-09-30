//! Scheduled tasks (see `dino_core::schedule`): each run is a new session, started with the task's
//! prompt. Checked every 30 seconds. Like Claude Desktop, a time missed while the Mac slept or dinod
//! wasn't running is caught up once, when it's back: only the latest one, and only if it's under a
//! week old (the latest always is: every frequency comes round within a week).

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dino_core::schedule::{Frequency, MAX_HISTORY, ScheduledRun, ScheduledTask, split_args};
use dino_core::ipc::LauncherInfo;
use dino_core::settings::Settings;
use dino_core::worktree;

use crate::{Daemon, Launch, finished, new_uuid, now_secs, save, send_input, spawn, spawn_in_worktree, work_dir};

/// Later than this after its time, a run counts as a catch-up.
const LATE: u64 = 120;

/// How long a shell gets to start before its task's command is typed in.
const SHELL_READY: Duration = Duration::from_secs(30);

#[derive(Default)]
pub(crate) struct Scheduler {
    tasks: Mutex<Vec<ScheduledTask>>,
    /// `caffeinate`, while it keeps the Mac awake for the tasks.
    awake: Mutex<Option<Child>>,
    /// One check at a time, so no time is run twice.
    ticking: Mutex<()>,
}

impl Scheduler {
    pub(crate) fn load() -> Self {
        let tasks = std::fs::read(path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Self { tasks: Mutex::new(tasks), ..Default::default() }
    }
}

fn path() -> PathBuf {
    dino_core::config_dir().join("schedule.json")
}

fn store(tasks: &[ScheduledTask]) {
    let _ = crate::write_private(&path(), &serde_json::to_vec_pretty(tasks).unwrap_or_default());
}

/// Check for due tasks now and every 30 seconds after.
pub(crate) fn start(d: &Arc<Daemon>) {
    let d = d.clone();
    std::thread::spawn(move || {
        loop {
            tick(&d, now_secs());
            keep_awake(&d);
            std::thread::sleep(Duration::from_secs(30));
        }
    });
}

/// The tasks, with when each runs next.
pub(crate) fn list(d: &Daemon) -> Vec<ScheduledTask> {
    let now = now_secs();
    let mut tasks = d.schedule.tasks.lock().unwrap().clone();
    for t in &mut tasks {
        t.next_run = if t.enabled { next_due(t.frequency, now) } else { None };
    }
    tasks
}

/// Add or replace a task. Only times after this count: an edit or a resume owes no runs from before.
pub(crate) fn put(d: &Daemon, mut t: ScheduledTask) -> anyhow::Result<ScheduledTask> {
    t.name = t.name.trim().to_string();
    anyhow::ensure!(!t.name.is_empty(), "give the task a name");
    let l = d.allowed_launcher(&t.launcher)?;
    anyhow::ensure!(!t.prompt.trim().is_empty() || l.agent_id == "shell", "give the task a prompt");
    let dir = work_dir(Some(&t.cwd));
    anyhow::ensure!(dir.is_dir(), "{} isn't a folder", t.cwd);
    if t.worktree {
        worktree::repo_root(&dir).map_err(|_| anyhow::anyhow!("{} isn't in a git repository, so runs can't have their own worktree", dir.display()))?;
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
    let now = now_secs();
    let mut tasks = d.schedule.tasks.lock().unwrap();
    anyhow::ensure!(
        !tasks.iter().any(|o| o.id != t.id && o.name.eq_ignore_ascii_case(&t.name)),
        "there's already a task called {}",
        t.name
    );
    t.last_due = prev_due(t.frequency, now);
    t.next_run = None;
    match tasks.iter_mut().find(|o| !t.id.is_empty() && o.id == t.id) {
        Some(old) => {
            t.created_at = old.created_at;
            t.history = std::mem::take(&mut old.history);
            *old = t.clone();
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
    Ok(t)
}

pub(crate) fn delete(d: &Daemon, id: &str) -> anyhow::Result<()> {
    let mut tasks = d.schedule.tasks.lock().unwrap();
    let before = tasks.len();
    tasks.retain(|t| t.id != id);
    anyhow::ensure!(tasks.len() < before, "no scheduled task {id}");
    store(&tasks);
    drop(tasks);
    keep_awake(d);
    Ok(())
}

/// Run now, paused or not; the run is in the history like any other.
pub(crate) fn run_now(d: &Daemon, id: &str) -> anyhow::Result<String> {
    let t = d.schedule.tasks.lock().unwrap().iter().find(|t| t.id == id).cloned();
    let t = t.ok_or_else(|| anyhow::anyhow!("no scheduled task {id}"))?;
    let result = fire(d, &t);
    let run = match &result {
        Ok(session) => ScheduledRun { at: now_secs(), session: Some(session.clone()), outcome: "started".into(), ..Default::default() },
        Err(e) => ScheduledRun { at: now_secs(), outcome: "failed".into(), reason: Some(e.to_string()), ..Default::default() },
    };
    record(d, id, run, None);
    result
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
        .filter(|t| t.enabled)
        .filter_map(|t| {
            let due = prev_due(t.frequency, now)?;
            (due > t.last_due.unwrap_or(t.created_at)).then(|| (t.clone(), due))
        })
        .collect();
    for (t, due) in due {
        let catch_up = now.saturating_sub(due) > LATE;
        let run = if let Some(prev) = still_going(d, &t) {
            ScheduledRun { outcome: "skipped".into(), reason: Some(format!("the previous run ({prev}) was still going")), ..Default::default() }
        } else {
            match fire(d, &t) {
                Ok(session) => ScheduledRun { session: Some(session), outcome: "started".into(), ..Default::default() },
                Err(e) => ScheduledRun { outcome: "failed".into(), reason: Some(e.to_string()), ..Default::default() },
            }
        };
        record(d, &t.id, ScheduledRun { at: now, due: Some(due), catch_up, ..run }, Some(due));
    }
}

fn record(d: &Daemon, id: &str, run: ScheduledRun, due: Option<u64>) {
    let mut tasks = d.schedule.tasks.lock().unwrap();
    // Deleted while it ran: nothing to keep.
    let Some(t) = tasks.iter_mut().find(|t| t.id == id) else { return };
    if let Some(due) = due {
        t.last_due = Some(due);
    }
    t.history.push(run);
    let extra = t.history.len().saturating_sub(MAX_HISTORY);
    t.history.drain(..extra);
    store(&tasks);
}

/// The task's last run, by session name, while it's still working, waiting on the user, or
/// waiting on work its turn left running.
fn still_going(d: &Daemon, t: &ScheduledTask) -> Option<String> {
    let id = t.history.iter().rev().find_map(|r| r.session.clone())?;
    let s = d.sessions.lock().unwrap().iter().find(|s| s.id == id).cloned()?;
    (!s.pane.is_exited() && !finished(d, &s)).then(|| s.name.clone())
}

/// Start a run: a new session named after the task. Claude isn't started where it would first ask
/// whether to trust the folder: nobody is there to answer, and dino won't answer for the user.
fn fire(d: &Daemon, t: &ScheduledTask) -> anyhow::Result<String> {
    let l = d.allowed_launcher(&t.launcher)?;
    let dir = work_dir(Some(&t.cwd));
    anyhow::ensure!(dir.is_dir(), "{} doesn't exist any more", t.cwd);
    check_trust(&l, &dir, t.worktree)?;
    let name = {
        let sessions = d.sessions.lock().unwrap();
        let taken = |n: &str| sessions.iter().any(|s| s.name == n);
        std::iter::once(t.name.clone()).chain((2..).map(|n| format!("{}-{n}", t.name))).find(|n| !taken(n)).unwrap()
    };
    let prompt = t.prompt.trim();
    // Agents take the prompt as an argument (see `spawn`); a shell gets it typed in, as a command.
    let shell = l.agent_id == "shell";
    let launch = Launch {
        name: Some(name),
        scheduled: Some(t.name.clone()),
        prompt: (!shell && !prompt.is_empty()).then(|| prompt.to_string()),
        ..Launch::new(&l.short, split_args(&t.args), Some(dir.display().to_string()))
    };
    let id = if t.worktree { spawn_in_worktree(d, launch)? } else { spawn(d, launch)? };
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
        anyhow::ensure!(
            a.trusted_in(dir, &root).is_some(),
            "Claude doesn't trust {} yet. Start Claude there once and accept its trust prompt",
            dir.display()
        );
        anyhow::ensure!(
            !worktree || Settings::load().policies.worktree_trust,
            "Claude would ask to trust the new worktree. Turn on worktree trust in Settings → Policies, or run without a worktree"
        );
    }
    Ok(())
}

/// Type `text` into a new session once it has drawn its prompt and gone quiet.
pub(crate) fn type_when_ready(d: &Daemon, id: &str, text: &str) {
    let Some(s) = d.sessions.lock().unwrap().iter().find(|s| s.id == id).cloned() else { return };
    let text = text.to_string();
    std::thread::spawn(move || {
        let start = Instant::now();
        while start.elapsed() < SHELL_READY && !s.pane.is_exited() {
            if s.last_write.lock().unwrap().is_some_and(|t| t.elapsed() > Duration::from_secs(1)) {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        if !s.pane.is_exited() {
            send_input(&s, &text, true);
        }
    });
}

/// While the setting is on and any task runs on a schedule, `caffeinate -i` holds an IOPM
/// assertion against idle sleep. It exits with dinod (`-w`).
pub(crate) fn keep_awake(d: &Daemon) {
    let want = Settings::load().machine.keep_awake && d.schedule.tasks.lock().unwrap().iter().any(|t| t.enabled && t.frequency != Frequency::Manual);
    let mut awake = d.schedule.awake.lock().unwrap();
    if let Some(c) = awake.as_mut()
        && !matches!(c.try_wait(), Ok(None))
    {
        *awake = None;
    }
    if want && awake.is_none() {
        let pid = std::process::id().to_string();
        *awake = Command::new("/usr/bin/caffeinate")
            .args(["-i", "-w", &pid])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .inspect_err(|e| eprintln!("dinod: couldn't keep the Mac awake: {e}"))
            .ok();
    } else if !want && let Some(mut c) = awake.take() {
        let _ = c.kill();
        let _ = c.wait();
    }
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
}
