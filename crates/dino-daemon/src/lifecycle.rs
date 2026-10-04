//! A session's life after it starts: renamed, archived (stopped but kept, its worktree put away
//! when nothing would be lost), started again, forgotten. And the worktrees dino made, with their
//! size on disk, for Settings → Worktrees → Storage.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use dino_core::ipc::{self, Request, Response};
use dino_core::{pr, trust, worktree};

use super::{
    Daemon, Launch, SavedSession, SessionWorktree, kill, now_secs, real, save, save_groups, save_worktrees,
    session_worktree, sessions_in, spawn,
};

/// A stopped session kept to start again, with the worktree it ran in.
#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct Archived {
    #[serde(flatten)]
    pub saved: SavedSession,
    pub archived_at: u64,
    /// The worktree dino made for it, if it ran in one.
    pub worktree: Option<SessionWorktree>,
    /// That worktree was put away (its branch kept) and is made again on unarchive.
    pub worktree_removed: bool,
}

fn archived_path() -> PathBuf {
    dino_core::config_dir().join("archived.json")
}

pub(crate) fn load_archived() -> Vec<Archived> {
    std::fs::read(archived_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save_archived(all: &[Archived]) {
    let _ = super::write_private(&archived_path(), &serde_json::to_vec_pretty(all).unwrap_or_default());
}

/// Answer a lifecycle request.
pub(crate) fn serve(d: &Arc<Daemon>, req: Request) -> Response {
    let done = |r: anyhow::Result<()>| match r {
        Ok(()) => Response::Ok,
        Err(e) => Response::Error { message: e.to_string() },
    };
    match req {
        Request::Rename { id, name } => done(rename(d, &id, &name)),
        Request::Pin { id, pinned } => done(pin(d, &id, pinned)),
        Request::Archive { id } => done(archive(d, &id)),
        Request::Archived => Response::Archived { sessions: list(d) },
        Request::Unarchive { id } => match unarchive(d, &id) {
            Ok(id) => Response::Created { id },
            Err(e) => Response::Error { message: e.to_string() },
        },
        Request::DeleteArchived { id } => done(delete(d, &id)),
        Request::Delete { id, dry_run } => match delete_session(d, &id, dry_run) {
            Ok(deletion) => Response::Deletion { deletion },
            Err(e) => Response::Error { message: e.to_string() },
        },
        Request::Storage => Response::Storage { worktrees: storage(d) },
        Request::RemoveStored { path } => done(remove_stored(d, &path)),
        Request::FreeUpSpace => {
            let (removed, bytes) = free_up_space(d);
            Response::Freed { removed, bytes }
        }
        other => Response::Error { message: format!("not a lifecycle request: {other:?}") },
    }
}

fn rename(d: &Daemon, id: &str, name: &str) -> anyhow::Result<()> {
    let s = d.sessions.lock().unwrap().iter().find(|s| s.id == id).cloned().ok_or_else(|| anyhow::anyhow!("no session {id}"))?;
    let name: String = name.trim().chars().filter(|c| !c.is_control()).take(80).collect();
    *s.label.lock().unwrap() = (!name.is_empty()).then_some(name);
    save(d);
    Ok(())
}

fn pin(d: &Daemon, id: &str, pinned: bool) -> anyhow::Result<()> {
    let s = d.sessions.lock().unwrap().iter().find(|s| s.id == id).cloned().ok_or_else(|| anyhow::anyhow!("no session {id}"))?;
    s.pinned.store(pinned, Ordering::Relaxed);
    save(d);
    Ok(())
}

/// Stop the session and keep what it takes to start it again. Its worktree is put away when no
/// other session runs there and nothing in it would be lost: clean, and pushed or merged.
pub(crate) fn archive(d: &Daemon, id: &str) -> anyhow::Result<()> {
    archive_as(d, id, true)
}

/// Archive it; `put_away` false keeps its worktree on disk whatever state it's in.
fn archive_as(d: &Daemon, id: &str, put_away: bool) -> anyhow::Result<()> {
    let s = d.sessions.lock().unwrap().iter().find(|s| s.id == id).cloned().ok_or_else(|| anyhow::anyhow!("no session {id}"))?;
    anyhow::ensure!(super::group_of(d, id).is_none(), "{} is part of a fan-out: keep or discard it instead", s.name);
    let agent_session = s.agent_session.lock().unwrap().clone().or_else(|| super::conversation_of(&s));
    let saved = SavedSession {
        id: s.id.clone(),
        name: s.name.clone(),
        label: s.label.lock().unwrap().clone(),
        pinned: s.pinned.load(Ordering::Relaxed),
        launcher: s.launcher.clone(),
        args: s.args.clone(),
        cwd: s.cwd.display().to_string(),
        started_at: s.started_at,
        agent_session,
        auto: s.auto.lock().unwrap().clone(),
        controls: s.controls.clone(),
        scheduled: s.scheduled.clone(),
        started_by: s.started_by.clone(),
        messaged_by: s.messaged_by.lock().unwrap().clone(),
        host: s.host.clone(),
        ended: false,
        exit_code: None,
        route: s.route.clone(),
        instead_of: s.instead_of.clone(),
    };
    let w = if s.host.is_some() { None } else { session_worktree(d, &s.cwd) };
    kill(d, id);
    d.closing.lock().unwrap().remove(id);
    d.prs.lock().unwrap().remove(id);
    save(d);

    let mut worktree_removed = false;
    if let Some(w) = &w {
        let target = real(&w.path);
        let others = sessions_in(d, &target).iter().any(|o| !o.pane.is_exited());
        if put_away && !others && nothing_to_lose(d, w) && worktree::put_away(&w.path).is_ok() {
            worktree_removed = true;
            let mut worktrees = d.worktrees.lock().unwrap();
            worktrees.retain(|o| o.path != w.path);
            save_worktrees(&worktrees);
            d.summaries.lock().unwrap().remove(&target);
            d.sizes.lock().unwrap().remove(&target);
        }
    }
    let mut archived = d.archived.lock().unwrap();
    archived.insert(0, Archived { saved, archived_at: now_secs(), worktree: w, worktree_removed });
    save_archived(&archived);
    Ok(())
}

/// Clean, and every commit either pushed or on the branch it came from.
fn nothing_to_lose(d: &Daemon, w: &SessionWorktree) -> bool {
    if pr::nothing_to_lose(&w.path) {
        return true;
    }
    let base = base_of(&w.repo);
    super::summary(d, &real(&w.path), Some(&w.branch), &base).is_some_and(|s| !s.dirty && (s.state == "merged" || s.state == "empty"))
}

/// What a repo's worktrees are compared with: what its main checkout has out.
fn base_of(repo: &Path) -> String {
    match worktree::list(repo).ok().and_then(|w| w.into_iter().next()).and_then(|w| w.branch) {
        Some(b) => b,
        None => worktree::head(repo),
    }
}

fn list(d: &Daemon) -> Vec<ipc::ArchivedInfo> {
    d.archived
        .lock()
        .unwrap()
        .iter()
        .map(|a| ipc::ArchivedInfo {
            id: a.saved.id.clone(),
            name: a.saved.name.clone(),
            label: a.saved.label.clone(),
            launcher: a.saved.launcher.clone(),
            cwd: a.saved.cwd.clone(),
            branch: a.worktree.as_ref().map(|w| w.branch.clone()),
            archived_at: a.archived_at,
            resumable: a.saved.agent_session.is_some(),
            worktree_removed: a.worktree_removed,
            agent: d.launcher(&a.saved.launcher).map_or_else(|| a.saved.launcher.clone(), |l| l.agent_id),
            agent_session: a.saved.agent_session.clone(),
            pinned: a.saved.pinned,
        })
        .collect()
}

/// Start it again: its worktree made again from the branch if it was put away, its agent's
/// conversation resumed where the agent can (`spawn` knows how), else a fresh start in the same place.
fn unarchive(d: &Daemon, id: &str) -> anyhow::Result<String> {
    let a = d.archived.lock().unwrap().iter().find(|a| a.saved.id == id).cloned().ok_or_else(|| anyhow::anyhow!("nothing archived as {id}"))?;
    d.allowed_launcher(&a.saved.launcher)?;
    if let (Some(w), true) = (&a.worktree, a.worktree_removed) {
        worktree::restore(&w.repo, &w.path, &w.branch)?;
        let mut worktrees = d.worktrees.lock().unwrap();
        if !worktrees.iter().any(|o| o.path == w.path) {
            worktrees.push(w.clone());
        }
        save_worktrees(&worktrees);
    }
    // Where it ran, or the nearest place still there.
    let cwd = if a.saved.host.is_some() {
        PathBuf::from(&a.saved.cwd)
    } else {
        [Some(PathBuf::from(&a.saved.cwd)), a.worktree.as_ref().map(|w| w.path.clone()), a.worktree.as_ref().map(|w| w.repo.clone())]
            .into_iter()
            .flatten()
            .find(|p| p.is_dir())
            .unwrap_or_else(super::home)
    };
    let saved = {
        let sessions = d.sessions.lock().unwrap();
        let taken = |n: &str| sessions.iter().any(|s| s.name == n);
        let stem = a.saved.name.clone();
        let name = std::iter::once(stem.clone()).chain((2..).map(|n| format!("{stem}-{n}"))).find(|n| !taken(n)).unwrap();
        // A new id: the old one may be a live session's by now.
        SavedSession { id: d.next_id.fetch_add(1, Ordering::Relaxed).to_string(), name, cwd: cwd.display().to_string(), ..a.saved.clone() }
    };
    let new = spawn(d, Launch { restore: Some(saved.clone()), ..Launch::new(&saved.launcher, saved.args.clone(), Some(saved.cwd.clone())) })?;
    let mut archived = d.archived.lock().unwrap();
    archived.retain(|o| o.saved.id != id);
    save_archived(&archived);
    drop(archived);
    save(d);
    Ok(new)
}

/// Forget an archived session. A worktree it left behind stays (see Storage); one put away is
/// only its branch now, which stays too.
fn delete(d: &Daemon, id: &str) -> anyhow::Result<()> {
    let mut archived = d.archived.lock().unwrap();
    let i = archived.iter().position(|a| a.saved.id == id).ok_or_else(|| anyhow::anyhow!("nothing archived as {id}"))?;
    let a = archived.remove(i);
    save_archived(&archived);
    dino_core::agent::qwen::forget(id);
    crate::forget_session_files(id);
    if let (Some(w), true) = (&a.worktree, a.worktree_removed) {
        let _ = trust::claude_forget(&w.path);
    }
    Ok(())
}

/// The worktree dino made that a session's deletion takes along: its own, or its fan-out seat.
struct Doomed {
    path: PathBuf,
    repo: PathBuf,
    branch: String,
}

/// Delete a session for good: its agent stopped, everything dinod keeps for it forgotten (its
/// restore state, screens, fan-out seat, PR watch), and the worktree dino made for it removed,
/// uncommitted work and all. Its branch goes too when nothing on it is unmerged; else it stays.
/// A shell, a session on another machine, or one in a folder of the user's takes nothing along,
/// and a worktree another session is in stays. The agent's conversation file is never touched:
/// Continue a Session still finds it. `dry_run` only says what would happen.
pub(crate) fn delete_session(d: &Daemon, id: &str, dry_run: bool) -> anyhow::Result<ipc::Deletion> {
    let s = d.sessions.lock().unwrap().iter().find(|s| s.id == id).cloned().ok_or_else(|| anyhow::anyhow!("no session {id}"))?;
    let member = d.groups.lock().unwrap().iter().find_map(|g| {
        g.members.iter().find(|m| m.session == id).map(|m| Doomed { path: m.worktree.clone(), repo: g.repo.clone(), branch: m.branch.clone() })
    });
    let own = || session_worktree(d, &s.cwd).map(|w| Doomed { path: w.path, repo: w.repo, branch: w.branch });
    let doomed = if s.host.is_some() || s.agent_id == "shell" { None } else { member.or_else(own) };

    let mut out = ipc::Deletion::default();
    let mut landed = false;
    if let Some(w) = &doomed {
        let target = real(&w.path);
        // Another session in it, ended or not: the worktree is still in use.
        if let Some(o) = sessions_in(d, &target).into_iter().find(|o| o.id != id) {
            out.kept_for = Some(o.label.lock().unwrap().clone().unwrap_or_else(|| o.name.clone()));
        } else if w.path.is_dir() {
            out.worktree = Some(target);
            out.branch = Some(w.branch.clone());
            if let Ok(r) = worktree::at_risk(&w.path, Some(&w.branch), &base_of(&w.repo)) {
                (out.uncommitted, out.unpushed, landed) = (r.uncommitted, r.unpushed, r.landed);
            }
            out.keeps_branch = !landed;
        }
    }
    if dry_run {
        return Ok(out);
    }

    let pid = s.pane.pid();
    kill(d, id);
    d.closing.lock().unwrap().remove(id);
    d.prs.lock().unwrap().remove(id);
    {
        let mut groups = d.groups.lock().unwrap();
        if groups.iter().any(|g| g.members.iter().any(|m| m.session == id)) {
            for g in groups.iter_mut() {
                g.members.retain(|m| m.session != id);
            }
            groups.retain(|g| !g.members.is_empty());
            save_groups(&groups);
        }
    }
    {
        let mut archived = d.archived.lock().unwrap();
        if archived.iter().any(|a| a.saved.id == id) {
            archived.retain(|a| a.saved.id != id);
            save_archived(&archived);
        }
    }
    save(d);

    let (Some(w), Some(_)) = (&doomed, &out.worktree) else { return Ok(out) };
    // The agent lets go of its folder before it goes.
    if let Some(pid) = pid {
        gone(pid, std::time::Duration::from_secs(3));
    }
    if landed {
        worktree::remove(&w.repo, &w.path, &w.branch);
    } else {
        worktree::discard(&w.repo, &w.path)?;
    }
    anyhow::ensure!(!w.path.exists(), "Couldn't remove its worktree {}", w.path.display());
    let target = real(&w.path);
    let _ = trust::claude_forget(&w.path);
    {
        let mut worktrees = d.worktrees.lock().unwrap();
        worktrees.retain(|o| o.path != w.path);
        save_worktrees(&worktrees);
    }
    d.summaries.lock().unwrap().remove(&target);
    d.sizes.lock().unwrap().remove(&target);
    d.pushed.lock().unwrap().remove(&target);
    // An archived session that ran here makes it again from its branch if it's started.
    let mut archived = d.archived.lock().unwrap();
    if archived.iter().any(|a| a.worktree.as_ref().is_some_and(|o| o.path == w.path)) {
        for a in archived.iter_mut().filter(|a| a.worktree.as_ref().is_some_and(|o| o.path == w.path)) {
            a.worktree_removed = true;
        }
        save_archived(&archived);
    }
    Ok(out)
}

/// Wait up to `max` for process `pid` to exit.
fn gone(pid: u32, max: std::time::Duration) {
    let since = Instant::now();
    // Signal 0 only checks; ESRCH means it's gone.
    while unsafe { libc::kill(pid as i32, 0) } == 0 && since.elapsed() < max {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

// ---- Storage: every worktree dino made, how big, and cleaning up. ----

/// Sizes older than this are measured again.
const SIZE_FRESH: std::time::Duration = std::time::Duration::from_secs(60);

fn storage(d: &Arc<Daemon>) -> Vec<ipc::StoredWorktree> {
    let archived: Vec<String> = d.archived.lock().unwrap().iter().filter_map(|a| a.worktree.as_ref()).map(|w| real(&w.path)).collect();
    let mut all: Vec<(PathBuf, PathBuf, String, bool)> =
        d.worktrees.lock().unwrap().iter().map(|w| (w.path.clone(), w.repo.clone(), w.branch.clone(), false)).collect();
    for g in d.groups.lock().unwrap().iter() {
        all.extend(g.members.iter().map(|m| (m.worktree.clone(), g.repo.clone(), m.branch.clone(), true)));
    }
    all.retain(|(p, ..)| p.is_dir());
    let mut bases: HashMap<PathBuf, String> = HashMap::new();
    let sizes = d.sizes.lock().unwrap().clone();
    let pushed = d.pushed.lock().unwrap().clone();
    let mut stale = vec![];
    let out = all
        .into_iter()
        .map(|(path, repo, branch, fanout)| {
            let key = real(&path);
            let base = bases.entry(repo.clone()).or_insert_with(|| base_of(&repo)).clone();
            let summary = super::summary(d, &key, Some(&branch), &base);
            let size = sizes.get(&key).map(|(at, n)| {
                if at.elapsed() > SIZE_FRESH {
                    stale.push(key.clone());
                }
                *n
            });
            if size.is_none() {
                stale.push(key.clone());
            }
            let here = sessions_in(d, &key);
            let live = here.iter().find(|s| !s.pane.is_exited());
            let session = live.map(|s| s.label.lock().unwrap().clone().unwrap_or_else(|| s.name.clone()));
            let session_state = match live {
                Some(s) if super::finished(d, s) => Some("idle"),
                Some(_) => Some("working"),
                None if !here.is_empty() => Some("ended"),
                None => None,
            };
            let state = summary.as_ref().map_or_else(|| "in_progress".into(), |s| s.state.clone());
            let dirty = summary.as_ref().is_none_or(|s| s.dirty);
            let landed = state == "merged" || state == "empty" || pushed.get(&key) == Some(&true);
            ipc::StoredWorktree {
                path: key.clone(),
                repo: real(&repo),
                branch,
                reclaimable: live.is_none() && !fanout && !dirty && landed,
                state,
                dirty,
                size,
                session,
                session_state: session_state.map(String::from),
                archived: archived.contains(&key),
                fanout,
            }
        })
        .collect();
    measure(d, stale);
    out
}

/// Measure these worktrees' sizes on a thread of its own, one run at a time.
fn measure(d: &Arc<Daemon>, paths: Vec<String>) {
    if paths.is_empty() || d.measuring.swap(true, Ordering::AcqRel) {
        return;
    }
    let d = d.clone();
    std::thread::spawn(move || {
        for p in paths {
            let n = worktree::disk_size(Path::new(&p));
            let pushed = pr::nothing_to_lose(Path::new(&p));
            d.pushed.lock().unwrap().insert(p.clone(), pushed);
            d.sizes.lock().unwrap().insert(p, (Instant::now(), n));
        }
        d.measuring.store(false, Ordering::Release);
    });
}

/// Remove a worktree dino made, never forcing. The branch goes too if git sees it merged. A
/// session that ended in it is archived, so its conversation can still be picked up.
fn remove_stored(d: &Daemon, path: &str) -> anyhow::Result<()> {
    let target = real(Path::new(path));
    let fanout = d.groups.lock().unwrap().iter().any(|g| g.members.iter().any(|m| real(&m.worktree) == target));
    anyhow::ensure!(!fanout, "It belongs to a fan-out: keep or discard the fan-out instead");
    let w = d.worktrees.lock().unwrap().iter().find(|w| real(&w.path) == target).cloned();
    let w = w.ok_or_else(|| anyhow::anyhow!("{path} isn't a worktree dino made"))?;
    let ended = sessions_in(d, &target);
    if let Some(s) = ended.iter().find(|s| !s.pane.is_exited()) {
        anyhow::bail!("{} is running in it", s.label.lock().unwrap().clone().unwrap_or_else(|| s.name.clone()));
    }
    worktree::clean(&w.path).map_err(|e| anyhow::anyhow!("Kept {}: {e}", w.branch))?;
    forget_stored(d, &w, &target, ended);
    Ok(())
}

/// dino's records of its worktree `w` (at `target`), now that it's gone: the sessions that ended
/// in it (`ended`, found while it was there: a path that's gone can't be resolved to compare) are
/// archived, and an archived one that ran there makes it again if it's started.
fn forget_stored(d: &Daemon, w: &SessionWorktree, target: &str, ended: Vec<Arc<super::Session>>) {
    for s in ended {
        if archive_as(d, &s.id, false).is_err() {
            kill(d, &s.id);
        }
    }
    {
        let mut worktrees = d.worktrees.lock().unwrap();
        worktrees.retain(|o| o.path != w.path);
        save_worktrees(&worktrees);
    }
    d.summaries.lock().unwrap().remove(target);
    d.sizes.lock().unwrap().remove(target);
    d.pushed.lock().unwrap().remove(target);
    // An archived session that ran here makes it again from its branch (or a new one) if it's started.
    let mut archived = d.archived.lock().unwrap();
    let mut kept = false;
    for a in archived.iter_mut().filter(|a| a.worktree.as_ref().is_some_and(|o| o.path == w.path)) {
        a.worktree_removed = true;
        kept = true;
    }
    save_archived(&archived);
    if !kept {
        let _ = trust::claude_forget(&w.path);
    }
}

/// The sidebar's Clean Up for any worktree of a repo, dino's or another tool's. Never the main
/// checkout, a fan-out's, or one something works in: a live session, a running subagent, a
/// program whose folder is inside it, or a change in the last few minutes (an agent between
/// commands). Without `force` one with uncommitted changes stays; with it they're lost. Its
/// branch goes only when git sees it merged, so commits are never lost.
pub(crate) fn clean_up(d: &Daemon, path: &str, force: bool) -> anyhow::Result<()> {
    let target = real(Path::new(path));
    let fanout = d.groups.lock().unwrap().iter().any(|g| g.members.iter().any(|m| real(&m.worktree) == target));
    anyhow::ensure!(!fanout, "it belongs to a fan-out: keep or discard the fan-out instead");
    let ended = sessions_in(d, &target);
    if let Some(s) = ended.iter().find(|s| !s.pane.is_exited()) {
        anyhow::bail!("{} is running in it", s.label.lock().unwrap().clone().unwrap_or_else(|| s.name.clone()));
    }
    anyhow::ensure!(
        !super::subagent_owners(d).iter().any(|(p, o)| *p == target && o.running),
        "a subagent is working in it"
    );
    let me = std::process::id();
    let inside = |p: &str| p == target || p.starts_with(&format!("{target}/"));
    let mut users: Vec<String> =
        dino_core::procinfo::working_dirs().into_iter().filter(|(pid, _, cwd)| *pid != me && inside(cwd)).map(|p| p.1).collect();
    users.sort();
    users.dedup();
    anyhow::ensure!(users.is_empty(), "{} is working in it", users.join(", "));
    anyhow::ensure!(
        !worktree::recently(worktree::last_changed(Path::new(&target)), super::now_secs()),
        "it changed in the last {} minutes",
        worktree::RECENTLY / 60
    );
    // Looked up while it's there: a path that's gone doesn't resolve to compare.
    let w = d.worktrees.lock().unwrap().iter().find(|w| real(&w.path) == target).cloned();
    worktree::clean_as(Path::new(path), force)?;
    match w {
        Some(w) => forget_stored(d, &w, &target, ended),
        None => {
            d.summaries.lock().unwrap().remove(&target);
        }
    }
    Ok(())
}

/// Remove every worktree that nothing would be lost from (see `Request::FreeUpSpace`), checked
/// again with git now rather than from the last Storage list. Returns what went and the bytes freed.
fn free_up_space(d: &Arc<Daemon>) -> (Vec<String>, u64) {
    let sizes = d.sizes.lock().unwrap().clone();
    let mut removed = vec![];
    let mut bytes = 0;
    for s in storage(d).into_iter().filter(|s| s.reclaimable) {
        let w = d.worktrees.lock().unwrap().iter().find(|w| real(&w.path) == s.path).cloned();
        let Some(w) = w else { continue };
        if !nothing_to_lose(d, &w) || remove_stored(d, &s.path).is_err() {
            continue;
        }
        bytes += sizes.get(&s.path).map_or(0, |(_, n)| *n);
        removed.push(s.path);
    }
    (removed, bytes)
}

/// dino's worktrees that no session runs in any more, whose work landed on the branch they came
/// from (a merged PR, say), go as a merged session's does (`Policies::close_merged`): only when
/// nothing would be lost and nothing works in them (see `clean_up`). Others' worktrees are never
/// removed on their own; the sidebar offers to.
pub(crate) fn sweep_merged(d: &Daemon) {
    if !dino_core::settings::Settings::load().policies.close_merged {
        return;
    }
    let mine = d.worktrees.lock().unwrap().clone();
    for w in mine {
        let target = real(&w.path);
        let sessions = sessions_in(d, &target);
        // Pinned sessions keep theirs until the user says otherwise, as after a merge.
        if sessions.iter().any(|s| !s.pane.is_exited() || s.pinned.load(Ordering::Relaxed)) {
            continue;
        }
        let base = base_of(&w.repo);
        let landed = super::summary(d, &target, Some(&w.branch), &base).is_some_and(|s| !s.dirty && s.state == "merged");
        if landed && clean_up(d, &target, false).is_ok() {
            super::reshaped(d);
            eprintln!("{} removed {target}: its work is on {base} and no session runs there", super::stamp());
        }
    }
}

/// Sessions whose PR merged or closed are archived rather than killed: a PR can have follow-ups,
/// and the conversation is worth keeping. After a merge their worktree is put away (when nothing
/// would be lost); after a close it stays, since the work never landed (see Storage).
pub(crate) fn archive_pr_done(d: &Daemon, target: &str, merged: bool) -> anyhow::Result<()> {
    for s in sessions_in(d, target) {
        archive_as(d, &s.id, merged)?;
    }
    Ok(())
}
