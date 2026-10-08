//! Usage statistics (see `dino_core::usage`): the proxy's calls written down in batches, off its
//! path, and agents' own records read when someone asks or a session ends, never on a timer.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};

use dino_core::usage::{self, Call, Range, Report, RouteWindow, Status, Store};
use dino_proxy::CallStatus;

use crate::{Daemon, Session, conversation_of};

/// `stats.db`, opened on first use. `None` inside while it can't be opened (said once).
fn store() -> &'static Mutex<Option<Store>> {
    static STORE: OnceLock<Mutex<Option<Store>>> = OnceLock::new();
    STORE.get_or_init(|| {
        Mutex::new(match Store::open() {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!("dinod: usage statistics are off, {} can't be opened: {e}", usage::path().display());
                None
            }
        })
    })
}

/// Who a dino session's calls are from: its agent (or the one typed in its shell), its
/// conversation, its folder.
fn describe(s: &Session) -> (String, Option<String>, Option<String>) {
    let cwd = s.host.is_none().then(|| s.cwd.display().to_string());
    if s.agent_id == "shell" {
        if let Some(f) = s.typed() {
            let conversation = s.agent_session.lock().unwrap().clone().or_else(|| Some(f.session_id.clone()).filter(|c| !c.is_empty()));
            return (f.agent, conversation, f.cwd.or(cwd));
        }
        return ("shell".into(), None, cwd);
    }
    // The free tier's variants are their agent's.
    let agent = s.agent_id.strip_suffix("-free").unwrap_or(&s.agent_id).to_string();
    (agent, s.agent_session.lock().unwrap().clone(), cwd)
}

/// A session carried over from before (dinod restarted, or it was unarchived as `before`): what
/// the proxy already carried for it goes back into its meter, so its tokens and its budget don't
/// start again from zero.
pub(crate) fn seed(d: &Daemon, id: &str, before: Option<&str>, started_at: u64, conversation: Option<&str>) {
    let guard = store().lock().unwrap();
    let Some(store) = guard.as_ref() else { return };
    let since_ms = (started_at as i64).saturating_mul(1000);
    let conversation = conversation.filter(|c| !c.is_empty());
    let usage = |[input, cache_read, cache_write, output]: [u64; 4]| dino_proxy::Usage { input, cache_read, cache_write, output };
    match store.session_routes(id, before, since_ms, conversation).and_then(|r| Ok((r, store.session_subagents(id, before, since_ms, conversation)?))) {
        Ok((routes, subagents)) => {
            let routes: Vec<(String, dino_proxy::Usage)> = routes.into_iter().map(|(route, t)| (route, usage(t))).collect();
            if !routes.is_empty() {
                d.proxy.stats.seed(id, &routes, &usage(subagents));
            }
        }
        Err(e) => eprintln!("dinod: couldn't read {id}'s usage so far: {e}"),
    }
}

/// Write the proxy's finished calls down; cheap when there are none. Called every few seconds by
/// dinod's save loop, and before a report.
pub(crate) fn flush(d: &Daemon) {
    let calls = d.proxy.stats.take_calls();
    if calls.is_empty() {
        return;
    }
    let sessions: Vec<Arc<Session>> = d.sessions.lock().unwrap().clone();
    let archived = d.archived.lock().unwrap();
    let rows: Vec<Call> = calls
        .into_iter()
        .map(|c| {
            let (agent, conversation, cwd) = match sessions.iter().find(|s| s.id == c.session) {
                Some(s) => describe(s),
                // Ended and archived since: what it saved.
                None => match archived.iter().find(|a| a.saved.id == c.session) {
                    Some(a) => (a.saved.launcher.strip_suffix("-free").unwrap_or(&a.saved.launcher).to_string(), a.saved.agent_session.clone(), Some(a.saved.cwd.clone())),
                    None => ("unknown".into(), None, None),
                },
            };
            Call {
                at_ms: c.at_ms,
                session: c.session,
                agent,
                // The call's own when it says (a background job or a `claude -p` in the session
                // is another conversation), else the session's.
                conversation: c.conversation.or(conversation),
                cwd,
                route: c.route,
                model: c.model,
                input: c.usage.input,
                cache_read: c.usage.cache_read,
                cache_write: c.usage.cache_write,
                output: c.usage.output,
                ttft_ms: c.ttft_ms,
                duration_ms: c.duration_ms,
                status: match c.status {
                    CallStatus::Ok => Status::Ok,
                    CallStatus::Error => Status::Error,
                    CallStatus::Limit => Status::Limit,
                },
                fallback: c.fallback,
                cost: c.cost,
                subagent: c.subagent.is_some(),
                subagent_id: c.subagent,
                answer: c.answer,
            }
        })
        .collect();
    drop(archived);
    if let Some(store) = store().lock().unwrap().as_mut()
        && let Err(e) = store.add_calls(&rows)
    {
        eprintln!("dinod: couldn't write usage statistics: {e}");
    }
}

/// Which conversation each session is on now (some agents name theirs only as they go), so a
/// conversation's transcript isn't counted again for what the proxy saw.
fn link_sessions(d: &Daemon) {
    let sessions: Vec<Arc<Session>> = d.sessions.lock().unwrap().clone();
    let mut guard = store().lock().unwrap();
    let Some(store) = guard.as_mut() else { return };
    for s in sessions {
        let (agent, conversation, _) = describe(&s);
        let conversation = conversation.or_else(|| conversation_of(&s));
        if let Some(c) = conversation {
            let _ = store.link(&s.id, &agent, &c);
        }
    }
}

/// Held while agents' records are read, and while everything is cleared.
static SCANNING: Mutex<()> = Mutex::new(());

/// Read what's new in agents' own records, one look at a time.
fn scan() {
    let _one = SCANNING.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut seen) = store().lock().unwrap().as_ref().map(Store::seen) else { return };
    // Files are read without the store held (the proxy's calls are written meanwhile); each piece
    // is written down as it comes, so a first look at a long history is never all in memory.
    usage::scan(&mut seen, |batch| {
        if let Some(store) = store().lock().unwrap().as_mut()
            && let Err(e) = store.add_used(batch)
        {
            eprintln!("dinod: couldn't keep agents' usage: {e}");
        }
    });
    if let Some(store) = store().lock().unwrap().as_mut()
        && let Err(e) = store.save_seen(&seen)
    {
        eprintln!("dinod: couldn't keep how far agents' usage was read: {e}");
    }
    give_back();
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    /// macOS: hand memory malloc keeps free back to the system (`zone` null: every zone).
    fn malloc_zone_pressure_relief(zone: *mut libc::c_void, goal: usize) -> usize;
}

/// After a big one-off piece of work (a first look at months of history, a report over all of
/// it): what it freed goes back to the system rather than stay with dinod, which runs all day.
fn give_back() {
    // SAFETY: takes no pointers of ours; null means every malloc zone.
    #[cfg(target_os = "macos")]
    unsafe {
        malloc_zone_pressure_relief(std::ptr::null_mut(), 0)
    };
    // glibc's equivalent: free heap memory back to the system.
    // SAFETY: no arguments of ours.
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    unsafe {
        libc::malloc_trim(0)
    };
}

/// A session's agent finished: read its record while it's fresh, in the background.
pub(crate) fn session_ended() {
    std::thread::spawn(scan);
}

/// Session ids seen running, to notice the ones that end (see `watch`).
fn running() -> &'static Mutex<HashSet<String>> {
    static RUNNING: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    RUNNING.get_or_init(Default::default)
}

/// From dinod's save loop: write the proxy's calls down, and read agents' records when a session
/// ended since the last look.
pub(crate) fn tick(d: &Daemon) {
    flush(d);
    let now: HashSet<String> = d.sessions.lock().unwrap().iter().filter(|s| !s.pane.is_exited()).map(|s| s.id.clone()).collect();
    let mut was = running().lock().unwrap();
    let ended = was.iter().any(|id| !now.contains(id));
    if ended {
        link_sessions(d);
        session_ended();
    }
    *was = now;
}

/// The report for `range`, with everything up to now.
pub(crate) fn report(d: &Daemon, range: Range) -> anyhow::Result<Report> {
    flush(d);
    link_sessions(d);
    scan();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis() as i64;
    let mut r = {
        let guard = store().lock().unwrap();
        let store = guard.as_ref().ok_or_else(|| anyhow::anyhow!("usage statistics are off: {} can't be opened", usage::path().display()))?;
        usage::report(store, range, now)?
    };
    for route in &mut r.routes {
        if let Some(q) = d.proxy.stats.quota(&route.route) {
            route.windows = q.windows.into_iter().map(|(name, w)| RouteWindow { name, used: w.utilization as f64, resets_at: w.resets_at }).collect();
        }
        if let Some(id) = route.route.strip_prefix("plan/")
            && let Some(p) = dino_core::plans::presets().iter().find(|p| p.id == id)
        {
            route.label = p.name.clone();
        }
        if let Some(id) = route.route.strip_prefix("local/")
            && let Some(p) = crate::providers::find(id)
        {
            route.label = format!("{} on this Mac", p.name);
        }
        if route.route == "or"
            && let Some(a) = crate::providers::find("openrouter").and_then(|p| p.account)
        {
            route.account_spend = a.usage;
            route.account_limit = a.limit;
        }
    }
    for s in &mut r.speed {
        if let Some(route) = r.routes.iter().find(|x| x.route == s.route) {
            s.label = route.label.clone();
        }
    }
    give_back();
    Ok(r)
}

/// Forget every statistic (`dino stats --clear`, Clear Stats).
pub(crate) fn clear(d: &Daemon) -> anyhow::Result<()> {
    let _one = SCANNING.lock().unwrap_or_else(|e| e.into_inner());
    let _ = d.proxy.stats.take_calls();
    let mut guard = store().lock().unwrap();
    guard.as_mut().ok_or_else(|| anyhow::anyhow!("usage statistics are off"))?.clear()
}
