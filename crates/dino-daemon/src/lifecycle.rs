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
    Daemon, Launch, SavedSession, SessionWorktree, find_codex_session, kill, now_secs, real, save, save_worktrees,
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
    let tmp = archived_path().with_extension("json.tmp");
    if std::fs::write(&tmp, serde_json::to_vec_pretty(all).unwrap_or_default()).is_ok() {
        let _ = std::fs::rename(tmp, archived_path());
    }
}

/// Answer a lifecycle request.
pub(crate) fn serve(d: &Arc<Daemon>, req: Request) -> Response {
    let done = |r: anyhow::Result<()>| match r {
        Ok(()) => Response::Ok,
        Err(e) => Response::Error { message: e.to_string() },
    };
    match req {
        Request::Rename { id, name } => done(rename(d, &id, &name)),
        Request::Archive { id } => done(archive(d, &id)),
        Request::Archived => Response::Archived { sessions: list(d) },
        Request::Unarchive { id } => match unarchive(d, &id) {
            Ok(id) => Response::Created { id },
            Err(e) => Response::Error { message: e.to_string() },
        },
        Request::DeleteArchived { id } => done(delete(d, &id)),
        Request::Storage => Response::Storage { worktrees: storage(d) },
        Request::RemoveStored { path } => done(remove_stored(d, &path)),
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

/// Stop the session and keep what it takes to start it again. Its worktree is put away when no
/// other session runs there and nothing in it would be lost: clean, and pushed or merged.
pub(crate) fn archive(d: &Daemon, id: &str) -> anyhow::Result<()> {
    let s = d.sessions.lock().unwrap().iter().find(|s| s.id == id).cloned().ok_or_else(|| anyhow::anyhow!("no session {id}"))?;
    anyhow::ensure!(super::group_of(d, id).is_none(), "{} is part of a fan-out: keep or discard it instead", s.name);
    let agent_session = {
        let known = s.agent_session.lock().unwrap().clone();
        if known.is_none() && s.agent_id == "codex" && s.host.is_none() {
            let claimed: Vec<String> =
                d.sessions.lock().unwrap().iter().filter(|o| o.id != s.id).filter_map(|o| o.agent_session.lock().unwrap().clone()).collect();
            find_codex_session(&s.cwd, s.started_at, &claimed)
        } else {
            known
        }
    };
    let saved = SavedSession {
        id: s.id.clone(),
        name: s.name.clone(),
        label: s.label.lock().unwrap().clone(),
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
        if !others && nothing_to_lose(d, w) && worktree::put_away(&w.path).is_ok() {
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
    if let (Some(w), true) = (&a.worktree, a.worktree_removed) {
        let _ = trust::claude_forget(&w.path);
    }
    Ok(())
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
            let session = sessions_in(d, &key).into_iter().find(|s| !s.pane.is_exited()).map(|s| s.label.lock().unwrap().clone().unwrap_or_else(|| s.name.clone()));
            ipc::StoredWorktree {
                path: key.clone(),
                repo: real(&repo),
                branch,
                state: summary.as_ref().map_or_else(|| "in_progress".into(), |s| s.state.clone()),
                dirty: summary.as_ref().is_none_or(|s| s.dirty),
                size,
                session,
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
            d.sizes.lock().unwrap().insert(p, (Instant::now(), n));
        }
        d.measuring.store(false, Ordering::Release);
    });
}

/// Remove a worktree dino made, never forcing. The branch goes too if git sees it merged.
fn remove_stored(d: &Daemon, path: &str) -> anyhow::Result<()> {
    let target = real(Path::new(path));
    let fanout = d.groups.lock().unwrap().iter().any(|g| g.members.iter().any(|m| real(&m.worktree) == target));
    anyhow::ensure!(!fanout, "It belongs to a fan-out: keep or discard the fan-out instead");
    let w = d.worktrees.lock().unwrap().iter().find(|w| real(&w.path) == target).cloned();
    let w = w.ok_or_else(|| anyhow::anyhow!("{path} isn't a worktree dino made"))?;
    if let Some(s) = sessions_in(d, &target).into_iter().find(|s| !s.pane.is_exited()) {
        anyhow::bail!("{} is running in it", s.label.lock().unwrap().clone().unwrap_or_else(|| s.name.clone()));
    }
    worktree::clean(&w.path).map_err(|e| anyhow::anyhow!("Kept {}: {e}", w.branch))?;
    for s in sessions_in(d, &target) {
        kill(d, &s.id);
    }
    {
        let mut worktrees = d.worktrees.lock().unwrap();
        worktrees.retain(|o| o.path != w.path);
        save_worktrees(&worktrees);
    }
    d.summaries.lock().unwrap().remove(&target);
    d.sizes.lock().unwrap().remove(&target);
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
    Ok(())
}

/// Sessions whose PR merged are archived rather than killed, with their worktree put away: a
/// merged PR can have follow-ups, and the conversation is worth keeping.
pub(crate) fn archive_merged(d: &Daemon, target: &str) -> anyhow::Result<()> {
    for s in sessions_in(d, target) {
        archive(d, &s.id)?;
    }
    Ok(())
}
