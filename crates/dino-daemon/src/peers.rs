//! Sessions seeing and driving each other, through `dino mcp`: start one, read one, message one.
//! Also the side chat, which reads a session through the same tools.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use dino_core::ipc::{self, LauncherInfo};
use dino_proxy::Activity;

use crate::schedule::check_trust;
use crate::{Daemon, Launch, Session, idle, save, send_input, spawn, spawn_in_worktree, stats, work_dir};

/// Screen lines `read_session` gives by default, and at most.
const SCREEN_LINES: u32 = 60;
const MAX_SCREEN_LINES: u32 = 400;
/// Characters of conversation `read_session` gives.
const CONVERSATION_BUDGET: usize = 12_000;

/// For Claude: `--mcp-config` that gives it dino's tools, and the settings (already in `args`, from
/// the proxy wiring) extended so the tools that only look don't ask.
pub(crate) fn wire_claude(id: &str, args: &mut Vec<String>) {
    let Ok(dino) = std::env::current_exe() else { return };
    if let Some(i) = args.iter().position(|a| a == "--settings")
        && let Some(mut v) = args.get(i + 1).and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
    {
        v["permissions"]["allow"] = serde_json::json!(dino_core::mcp::READ_TOOLS);
        args[i + 1] = v.to_string();
    }
    // `--mcp-config` takes several values: it goes before the flags that follow it, never last.
    args.splice(0..0, ["--mcp-config".to_string(), dino_core::mcp::config(&dino, Some(id), false)]);
}

pub(crate) fn find(d: &Daemon, id: &str) -> anyhow::Result<Arc<Session>> {
    d.sessions.lock().unwrap().iter().find(|s| s.id == id).cloned().ok_or_else(|| anyhow::anyhow!("no session {id}"))
}

/// A new session for session `by`'s agent. Claude isn't started where it would ask whether to
/// trust the folder: the agent asking can't answer that for the user. Nor is a shell: its
/// commands would run with none of the checks an agent asks the user through.
pub(crate) fn start(d: &Daemon, launcher: &str, cwd: Option<String>, prompt: Option<String>, worktree: bool, by: Option<String>) -> anyhow::Result<String> {
    let l: LauncherInfo = d.allowed_launcher(launcher)?;
    anyhow::ensure!(l.agent_id != "shell", "an agent can't start a shell session: its commands would run without asking the user. Start an agent instead");
    // Where the asking session is, when no folder is given.
    let cwd = cwd.or_else(|| by.as_deref().and_then(|b| find(d, b).ok()).filter(|s| s.host.is_none()).map(|s| s.cwd.display().to_string()));
    let dir = work_dir(cwd.as_deref());
    anyhow::ensure!(dir.is_dir(), "{} isn't a folder", dir.display());
    check_trust(&l, &dir, worktree)?;
    let prompt = prompt.map(|p| p.trim().to_string()).filter(|p| !p.is_empty());
    let launch = Launch { prompt, started_by: by, ..Launch::new(&l.short, vec![], Some(dir.display().to_string())) };
    let id = if worktree { spawn_in_worktree(d, launch)? } else { spawn(d, launch)? };
    save(d);
    Ok(id)
}

/// What session `id` has been doing: its conversation when dino can read it, then its screen.
pub(crate) fn read(d: &Daemon, id: &str, lines: Option<u32>) -> anyhow::Result<String> {
    let s = find(d, id)?;
    let mut out = format!("{} ({}), {} in {}\n", s.name, s.agent_id, status(d, &s), s.cwd.display());
    let name_of = |id: &str| find(d, id).map_or_else(|_| format!("session {id} (gone)"), |o| o.name.clone());
    if let Some(by) = &s.started_by {
        out.push_str(&format!("Started by {}'s agent\n", name_of(by)));
    }
    if let Some(by) = s.messaged_by.lock().unwrap().clone() {
        out.push_str(&format!("Last messaged by {}'s agent\n", name_of(&by)));
    }
    let session = s.agent_session.lock().unwrap().clone();
    let conversation = session.zip(dino_core::agent::agent(&s.agent_id)).and_then(|(u, a)| a.tail(&u, CONVERSATION_BUDGET));
    if let Some(c) = conversation {
        out.push_str(&format!("\n## Conversation (latest last)\n{c}\n"));
    }
    let lines = lines.unwrap_or(SCREEN_LINES).clamp(1, MAX_SCREEN_LINES) as usize;
    let screen = s.pane.text(lines);
    let screen: Vec<&str> = screen.lines().collect();
    out.push_str(&format!("\n## Screen now\n{}", screen[screen.len().saturating_sub(lines)..].join("\n")));
    Ok(out)
}

/// One word or phrase for where a session is at, as agents are told.
pub(crate) fn status(d: &Daemon, s: &Session) -> String {
    if s.pane.is_exited() {
        return "exited".into();
    }
    match stats(d, s).activity {
        Some(Activity::NeedsPermission(what)) => format!("waiting on the user ({what})"),
        _ if idle(d, s) => "idle".into(),
        Some(Activity::Working) => "working".into(),
        _ => "busy".into(),
    }
}

/// Send `text` to session `id` as a message from session `by`, if it's between turns.
pub(crate) fn message(d: &Daemon, id: &str, text: &str, by: Option<String>) -> anyhow::Result<()> {
    let s = find(d, id)?;
    anyhow::ensure!(by.as_deref() != Some(id), "that's this session; message another one");
    anyhow::ensure!(!text.trim().is_empty(), "the message is empty");
    anyhow::ensure!(!s.pane.is_exited(), "{} has exited", s.name);
    // Its mode as dino started it, as the agent says it is now (Claude's Shift+Tab), and as
    // it's about to become.
    let now = super::mode::now(&s, d.proxy.stats.session(&s.id).agent_mode.as_deref());
    let pending = s.pending.lock().unwrap().as_ref().and_then(|c| c.mode.clone());
    check_messageable(&s.name, &s.agent_id, &[s.controls.mode.as_deref(), now.as_deref(), pending.as_deref()])?;
    // Typed into a permission prompt or mid-turn, the text would answer the wrong question.
    anyhow::ensure!(idle(d, &s), "{} is {}; message it once it's idle", s.name, status(d, &s));
    // Hand over only once nobody is typing into it, so the text doesn't join a half-written prompt.
    anyhow::ensure!(s.attached.load(Ordering::Relaxed) == 0 || s.poked.lock().unwrap().is_none_or(|t| t.elapsed() > USER_TYPING), "the user is typing in {}; try again shortly", s.name);
    *s.messaged_by.lock().unwrap() = by;
    send_input(&s, text.trim(), true);
    Ok(())
}

/// Another session's agent may only message an agent that still asks the user before acting: not
/// a shell, which would run the message as a command, nor an agent in bypass mode (`modes`).
fn check_messageable(name: &str, agent_id: &str, modes: &[Option<&str>]) -> anyhow::Result<()> {
    anyhow::ensure!(agent_id != "shell", "{name} is a shell: another session's agent can't type commands into it");
    anyhow::ensure!(!modes.contains(&Some("bypass")), "{name} is in bypass mode, running whatever it's told without asking: another session's agent can't message it");
    Ok(())
}

/// Recent enough a keystroke that the user may be mid-sentence.
const USER_TYPING: std::time::Duration = std::time::Duration::from_secs(5);

/// Side chat: answer `question` about session `id`.
pub(crate) fn ask(d: &Daemon, id: &str, question: &str) -> anyhow::Result<String> {
    anyhow::ensure!(!question.trim().is_empty(), "ask a question");
    let s = find(d, id)?;
    let dino = std::env::current_exe()?;
    let about = dino_core::ask::About { id: &s.id, name: &s.name, agent: &s.agent_id, cwd: &s.cwd };
    dino_core::ask::run(&ask_key(id), &about, question.trim(), &dino)
}

/// Asks and reviews share the runner; their keys mustn't collide.
pub(crate) fn ask_key(id: &str) -> String {
    format!("ask:{id}")
}

pub(crate) fn ipc_result(r: anyhow::Result<()>) -> ipc::Response {
    match r {
        Ok(()) => ipc::Response::Ok,
        Err(e) => ipc::Response::Error { message: e.to_string() },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_agents_that_still_ask_can_be_messaged() {
        assert!(check_messageable("claude", "claude", &[Some("ask"), None, None]).is_ok());
        assert!(check_messageable("codex", "codex", &[None, None, None]).is_ok());
        assert!(check_messageable("zsh", "shell", &[None, None, None]).is_err());
        // Set by dino, switched to in the agent, or about to be.
        for i in 0..3 {
            let mut modes = [Some("ask"), None, None];
            modes[i] = Some("bypass");
            let e = check_messageable("claude", "claude", &modes).unwrap_err().to_string();
            assert!(e.contains("bypass"), "{e}");
        }
    }
}
