//! Forking a session: a new session on a copy of its conversation, made by the agent's own fork
//! (`claude --resume <id> --fork-session`, `codex fork <id>`), the original left as it was. And
//! forks made in the agent itself (Claude's `/branch`, Codex's `/fork`), which move the session's
//! own process to the copy: the original conversation stays in dino as the session it came from.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use dino_core::agent::{StatusSource, agent};
use dino_core::ipc::ForkedFrom;

use super::{Daemon, Launch, SavedSession, Session, mode, now_secs, save, session_name, spawn, spawn_in_worktree};

/// How long a move to a conversation the agent hasn't written down yet waits for it, to tell a
/// fork (written at once) from a new conversation (`/clear`, written at its first prompt).
const RECORD_WAIT: Duration = Duration::from_secs(3);

/// What a session is called where people see it: the name given it, else what its agent's
/// records call its conversation (Codex's), else its agent's title without the spinner or status
/// mark in front (Claude's "✳"), else its name.
fn shown(s: &Session) -> String {
    let title = || {
        let t = s.pane.title()?;
        let t = agent(&s.agent_id).map_or(Some(t.clone()), |a| a.shown_title(&t))?;
        let t = t.trim_start_matches(|c: char| !c.is_alphanumeric()).trim().to_string();
        (!t.is_empty()).then_some(t)
    };
    let named = s.rollout.lock().unwrap().title.clone();
    s.label.lock().unwrap().clone().or(named).or_else(title).unwrap_or_else(|| s.name.clone())
}

/// The first of `stem`, `stem-2`, … no session is called.
fn free_name(d: &Daemon, stem: &str) -> String {
    let sessions = d.sessions.lock().unwrap();
    let taken = |n: &str| sessions.iter().any(|s| s.name == n);
    std::iter::once(stem.to_string()).chain((2..).map(|n| format!("{stem}-{n}"))).find(|n| !taken(n)).unwrap()
}

/// Fork session `id` (see `Request::Fork`): its launcher, flags, mode, model, effort, provider
/// route and account, on a new conversation copied from its own.
pub(crate) fn fork(d: &Daemon, id: &str, name: Option<String>, worktree: bool, prompt: Option<String>) -> anyhow::Result<String> {
    let s = d.sessions.lock().unwrap().iter().find(|s| s.id == id).cloned().ok_or_else(|| anyhow::anyhow!("no session {id}"))?;
    anyhow::ensure!(s.host.is_none(), "forking runs on this Mac; {} is on {}", s.name, s.host.as_deref().unwrap_or_default());
    let l = d.allowed_launcher(&s.launcher)?;
    let a = agent(&s.agent_id).filter(|a| a.fork_args("", std::path::Path::new(""), &mut None).is_some());
    let a = a.ok_or_else(|| anyhow::anyhow!("{} can't fork a conversation from dino", l.label))?;
    let conversation = s.agent_session.lock().unwrap().clone().or_else(|| super::conversation_of(&s));
    let conversation = conversation.filter(|c| a.transcript(c).is_some());
    let conversation = conversation.ok_or_else(|| anyhow::anyhow!("{} has no conversation to fork yet: send it a prompt first", s.name))?;
    let prompt = prompt.map(|p| p.trim().to_string()).filter(|p| !p.is_empty());
    if let Some(p) = &prompt {
        dino_core::agent::check_prompt(p)?;
    }
    let label = name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    let new = d.next_id.fetch_add(1, Ordering::Relaxed).to_string();
    let restore = SavedSession {
        id: new,
        name: free_name(d, &s.launcher),
        launcher: s.launcher.clone(),
        args: s.args.clone(),
        cwd: s.cwd.display().to_string(),
        started_at: now_secs(),
        // The mode it's in now, which it may have switched to since it started.
        controls: mode::current(d, &s),
        label,
        route: s.route.clone(),
        account: s.account.clone(),
        forked_from: Some(ForkedFrom { session: s.id.clone(), name: shown(&s), conversation }),
        fork_pending: true,
        ..Default::default()
    };
    let (cols, rows) = s.pane.size();
    let launch = Launch { cols, rows, prompt, restore: Some(restore.clone()), ..Launch::new(&restore.launcher, restore.args.clone(), Some(restore.cwd.clone())) };
    let id = if worktree { spawn_in_worktree(d, launch)? } else { spawn(d, launch)? };
    save(d);
    Ok(id)
}

/// Follow which conversation each Claude Code session is on, as its live session file says:
/// `/clear` and `/resume` move it to another, `/branch` to a fork of its own. (Codex's are
/// followed with its rollout, in `codex`.)
pub(crate) fn follow(d: &Daemon) {
    let sessions: Vec<Arc<Session>> = d
        .sessions
        .lock()
        .unwrap()
        .iter()
        .filter(|s| s.host.is_none() && !s.pane.is_exited() && !s.replaced.load(Ordering::Relaxed))
        .filter(|s| agent(&s.agent_id).is_some_and(|a| a.status_source() == StatusSource::Hooks))
        .cloned()
        .collect();
    for s in sessions {
        let Some(a) = agent(&s.agent_id) else { continue };
        let Some(now) = super::conversation_of(&s) else { continue };
        let known = s.agent_session.lock().unwrap().clone();
        if known.as_deref() == Some(now.as_str()) {
            s.moving_to.lock().unwrap().take();
            // The fork's own conversation is saved: from now on it resumes that.
            if s.fork_pending.load(Ordering::Relaxed) && a.transcript(&now).is_some() {
                s.fork_pending.store(false, Ordering::Relaxed);
                save(d);
            }
            continue;
        }
        if a.transcript(&now).is_none() {
            let mut moving = s.moving_to.lock().unwrap();
            match &*moving {
                Some((to, at)) if *to == now && at.elapsed() >= RECORD_WAIT => {}
                Some((to, _)) if *to == now => continue,
                _ => {
                    *moving = Some((now, Instant::now()));
                    continue;
                }
            }
        }
        s.moving_to.lock().unwrap().take();
        moved(d, &s, known, now);
    }
}

/// Session `s`'s agent is on conversation `now` (it was on `known`): a fork of `known`, made in
/// the agent, keeps `known` in dino as an ended session it came from, to resume when wanted.
pub(crate) fn moved(d: &Daemon, s: &Arc<Session>, known: Option<String>, now: String) {
    let Some(a) = agent(&s.agent_id) else { return };
    // A fork dino started goes through the original as it starts: it's the copy it's on.
    let forked = s.forked_from.lock().unwrap().clone();
    if forked.as_ref().is_some_and(|f| f.conversation == now) && known.as_deref().is_none_or(|k| a.transcript(k).is_none()) {
        return;
    }
    *s.agent_session.lock().unwrap() = Some(now.clone());
    // Its own conversation, saved (Codex's is known once its rollout is written): it resumes that.
    s.fork_pending.store(false, Ordering::Relaxed);
    let Some(old) = known.filter(|k| *k != now) else {
        save(d);
        return;
    };
    if a.forked_from(&now).as_deref() != Some(old.as_str()) {
        eprintln!("{} dinod: session {} moved to conversation {now}", super::stamp(), s.id);
        save(d);
        return;
    }
    let title = conversation_title(&s.agent_id, &old).unwrap_or_else(|| shown(s));
    let parent = d.next_id.fetch_add(1, Ordering::Relaxed).to_string();
    let restore = SavedSession {
        id: parent.clone(),
        name: free_name(d, &session_name(&title)),
        launcher: s.launcher.clone(),
        args: s.args.clone(),
        cwd: s.cwd.display().to_string(),
        started_at: s.started_at,
        agent_session: Some(old.clone()),
        controls: mode::current(d, s),
        label: Some(title.clone()),
        route: s.route.clone(),
        account: s.account.clone(),
        // What `s` came from, if anything, the original came from.
        forked_from: forked,
        ended: true,
        exit_code: Some(0),
        ..Default::default()
    };
    let (cols, rows) = s.pane.size();
    match spawn(d, Launch { cols, rows, restore: Some(restore.clone()), ..Launch::new(&restore.launcher, restore.args.clone(), Some(restore.cwd.clone())) }) {
        Ok(_) => {
            eprintln!("{} dinod: session {} forked its conversation {old} into {now}; the original is session {parent}", super::stamp(), s.id);
            *s.forked_from.lock().unwrap() = Some(ForkedFrom { session: parent, name: title, conversation: old });
        }
        Err(e) => eprintln!("{} dinod: session {} forked {old}, keeping the original failed: {e}", super::stamp(), s.id),
    }
    save(d);
}

/// What the agent's own record calls conversation `id`.
fn conversation_title(agent_id: &str, id: &str) -> Option<String> {
    match agent_id {
        "claude" | "claude-free" => dino_core::history::claude_title(id),
        "codex" => dino_core::history::codex_titles().remove(id),
        _ => None,
    }
}
