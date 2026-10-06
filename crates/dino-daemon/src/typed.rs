//! An agent typed by hand into a dino shell (`claude`, `codex` at its prompt) is that session's
//! agent while it runs, as one dino started is: dinod follows its status, mode, model,
//! conversation, tasks and forks by the same code, the shell's session standing for it (see
//! `Session::agent_now`). Nothing is added to the agent for it: Claude reports through the hooks a
//! dino shell's `claude` gets anyway (`dino-agents.*`), Codex through its own record. A change that
//! takes a restart (another model or effort), dinod restarting and an unarchive start it again in
//! its shell, on its conversation, typed at the shell's prompt as a person would type it (`claude
//! --resume <id>`), so the shell and its scrollback stay. When it exits, the shell is a plain shell
//! again.
//!
//! Settings → Agents' "Show agents started in a shell in the sidebar", and a shell's Keep as
//! Terminal, leave it a plain program: dino doesn't follow it.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use dino_core::agent::{Agent, agent};
use dino_core::controls::{self, Controls};
use dino_core::found::{FoundSession, Source};
use dino_core::settings::Settings;

use super::{Daemon, Session, conversation_of, keeps_terminal, mode, save, send_input, stop};

/// An agent typed into a shell, as kept: what it takes to start it there again.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub(crate) struct Typed {
    /// The agent, by id ("claude").
    pub agent: String,
    /// Its conversation, once it has one.
    #[serde(default)]
    pub conversation: Option<String>,
    /// The flags it was typed with that carry over (see `Agent::portable_flags`).
    #[serde(default)]
    pub args: Vec<String>,
    /// What it runs with: its mode and model as it says they are, or as chosen in dino since.
    #[serde(default)]
    pub controls: Controls,
    /// Where it runs.
    #[serde(default)]
    pub cwd: Option<String>,
    /// What it calls its conversation, shown until it's back.
    #[serde(default)]
    pub title: String,
}

/// How long a shell's agent that dino is starting again stays its agent before it's back: its
/// shell drawing its prompt, the line typed there, the agent starting.
const BACK_WITHIN: Duration = Duration::from_secs(20);

/// How long a shell that marks no prompts has to start before a line is typed into it.
const UNMARKED: Duration = Duration::from_secs(5);

/// Whether agents typed into shell `id` are followed, as the settings and its Keep as Terminal say.
pub(crate) fn followed(settings: &Settings, id: &str) -> bool {
    settings.machine.shell_integration && settings.machine.shell_agents && !keeps_terminal(id)
}

impl Session {
    /// The agent typed into this shell, while it runs there and dino follows it (see
    /// `watch_shells`); while dino starts it again, what it will be (no process yet).
    pub(crate) fn typed(&self) -> Option<FoundSession> {
        if self.agent_id != "shell" {
            return None;
        }
        self.inside.lock().unwrap().found.clone()
    }

    /// The agent this session runs: its own, or the one typed into it.
    pub(crate) fn agent_now(&self) -> String {
        self.typed().map_or_else(|| self.agent_id.clone(), |f| f.agent)
    }

    /// Its agent's adapter; none for a plain shell.
    pub(crate) fn adapter(&self) -> Option<&'static dyn Agent> {
        agent(&self.agent_now())
    }

    /// Its agent's process: the session's own program, or the agent typed into its shell.
    pub(crate) fn agent_pid(&self) -> Option<u32> {
        if self.agent_id == "shell" { self.typed()?.pid } else { self.pane.pid() }
    }

    /// The arguments its agent was started with: dino's, or the flags typed with it.
    pub(crate) fn agent_args(&self) -> Vec<String> {
        self.typed().map_or_else(|| self.args.clone(), |f| f.args)
    }

    /// The controls its agent was started with: dino's, or what the flags typed with it say.
    pub(crate) fn launched_controls(&self) -> Controls {
        self.typed().map_or_else(|| self.controls.clone(), |f| controls::from_args(&f.agent, &f.args))
    }

    /// The launcher its agent starts with: the session's, or the typed agent's own.
    pub(crate) fn agent_launcher(&self) -> String {
        self.typed().map_or_else(|| self.launcher.clone(), |f| f.agent)
    }

    /// Where its agent runs: where dino started it, or the folder the typed one runs in.
    pub(crate) fn agent_cwd(&self) -> PathBuf {
        let Some(f) = self.typed() else { return self.cwd.clone() };
        let here = || self.pane.shared.cwd.lock().unwrap().clone();
        f.cwd.or_else(here).map_or_else(|| self.cwd.clone(), PathBuf::from)
    }

    /// What chose its agent's account: what dino started it with, or the typed one's own
    /// environment (see `agent::account_env`).
    pub(crate) fn agent_account(&self) -> Vec<(String, String)> {
        match self.typed() {
            Some(f) => f.pid.map(|pid| super::account_of(&f.agent, pid)).unwrap_or_default(),
            None => self.account.clone(),
        }
    }
}

/// What it takes to start `s`'s typed agent again: its conversation, the flags it was typed with
/// and the controls it runs with now. Reads its screen (see `mode::current`): never call it
/// holding a lock of `s`'s.
pub(crate) fn saved(d: &Daemon, s: &Session) -> Option<Typed> {
    let f = s.typed()?;
    if s.pane.is_exited() || s.host.is_some() {
        return None;
    }
    let known = s.agent_session.lock().unwrap().clone();
    let conversation = known.or_else(|| Some(f.session_id.clone()).filter(|c| !c.is_empty())).or_else(|| conversation_of(s));
    Some(Typed { conversation, controls: mode::current(d, s), cwd: f.cwd.clone(), title: f.title.clone(), args: f.args, agent: f.agent })
}

/// What starts `t` at a shell's prompt, as a person would type it: its command, the flags it was
/// typed with (a prompt among them was its first message, given once) and its controls, on its
/// conversation.
pub(crate) fn command_line(d: &Daemon, t: &Typed) -> Option<String> {
    let a = agent(&t.agent)?;
    let bin = dino_core::KNOWN_AGENTS.iter().find(|k| k.id == t.agent)?.bin;
    let args = a.launch_prompt(&t.args).map_or_else(|| t.args.clone(), |(rest, _)| rest);
    let args = controls::without(&t.agent, &args, &t.controls);
    let (before, after) = t.conversation.as_deref().map(|c| a.resume_args(c)).unwrap_or_default();
    let set = controls::args(&t.agent, &t.controls, &d.knobs(&t.agent, true));
    let words: Vec<String> = std::iter::once(bin.to_string()).chain(before).chain(args).chain(set).chain(after).collect();
    Some(words.iter().map(|w| dino_core::ssh::quote(w)).collect::<Vec<_>>().join(" "))
}

/// `s`'s agent until `t` is back in its shell, `quitting` (the process it was) gone: shown as
/// starting, with what it will run with.
fn hold(d: &Daemon, s: &Session, t: &Typed, quitting: Option<u32>) {
    let mut args = controls::without(&t.agent, &t.args, &t.controls);
    args.extend(controls::args(&t.agent, &t.controls, &d.knobs(&t.agent, true)));
    let mut i = s.inside.lock().unwrap();
    i.found = Some(FoundSession {
        source: Source::Running,
        agent: t.agent.clone(),
        session_id: t.conversation.clone().unwrap_or_default(),
        title: t.title.clone(),
        cwd: t.cwd.clone(),
        updated_at: 0,
        pid: None,
        status: Some("starting".into()),
        terminal: Some("dino".into()),
        args,
        url: None,
        tmux: None,
    });
    i.resuming = Some((Instant::now() + BACK_WITHIN, quitting));
}

/// Start `s`'s typed agent again with `controls` (another model, or a mode its key can't reach):
/// quit it the way its own keys do and type it again at its shell's prompt, on its conversation.
/// The shell, its scrollback and the session stay.
pub(crate) fn restart(d: &Daemon, s: &Arc<Session>, controls: Controls) -> anyhow::Result<()> {
    let f = s.typed().ok_or_else(|| anyhow::anyhow!("no agent is running in {}", s.name))?;
    let pid = f.pid.ok_or_else(|| anyhow::anyhow!("{} is starting again already", s.name))?;
    let mut t = saved(d, s).ok_or_else(|| anyhow::anyhow!("no agent is running in {}", s.name))?;
    if t.controls.model != controls.model {
        d.proxy.stats.reset_context(&s.id);
    }
    t.controls = controls;
    let line = command_line(d, &t).ok_or_else(|| anyhow::anyhow!("dino can't start {} again", t.agent))?;
    let keys = agent(&t.agent).map(|a| a.quit_keys()).unwrap_or_default();
    let prompts = s.pane.shared.prompts.load(Ordering::Relaxed);
    hold(d, s, &t, Some(pid));
    // What the agent said of itself goes with it; the one starting again says it anew.
    d.proxy.stats.restarted(&s.id);
    *s.mode_seen.lock().unwrap() = mode::Seen::default();
    eprintln!("{} dinod: session {}: {} starts again in its shell: {line}", super::stamp(), s.id, t.agent);
    let s = s.clone();
    std::thread::spawn(move || {
        quit(&s, pid, keys);
        type_back(&s, &line, prompts);
    });
    Ok(())
}

/// Start `t` again in shell `s`, just started on dinod's restart or an unarchive: its line typed at
/// the shell's first prompt (the screen from before shows ones of the shell before it).
pub(crate) fn resume(d: &Daemon, s: &Arc<Session>, t: Typed) {
    let Some(line) = command_line(d, &t) else { return };
    *s.agent_session.lock().unwrap() = t.conversation.clone();
    if followed(&Settings::load(), &s.id) {
        hold(d, s, &t, None);
    }
    eprintln!("{} dinod: session {}: {} resumes in its shell: {line}", super::stamp(), s.id, t.agent);
    let prompts = s.pane.shared.prompts.load(Ordering::Relaxed);
    let s = s.clone();
    std::thread::spawn(move || type_back(&s, &line, prompts));
}

/// Quit agent `pid` in `s`'s terminal with its own `keys` (again, for a draft the first ones
/// cleared), else as `stop` does.
fn quit(s: &Session, pid: u32, keys: &[u8]) {
    let gone = |within: Duration| {
        let until = Instant::now() + within;
        loop {
            // SAFETY: signal 0 only asks whether it's there.
            if unsafe { libc::kill(pid as libc::pid_t, 0) } != 0 {
                return true;
            }
            if Instant::now() > until {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    if !keys.is_empty() {
        for _ in 0..2 {
            s.pane.write(keys.to_vec());
            if gone(Duration::from_secs(3)) {
                return;
            }
        }
        eprintln!("{} dinod: session {}: its agent didn't quit with its own keys; stopping it", super::stamp(), s.id);
    }
    if let Err(e) = stop(pid) {
        eprintln!("{} dinod: session {}: {e}", super::stamp(), s.id);
    }
}

/// Type `line` at `s`'s prompt and press Return, once its shell has drawn a prompt since it had
/// drawn `prompts` (a shell without the shell integration's marks: once it's in front and quiet
/// for a while), as a person would. Something else in front (a tmux its startup went into), or
/// nothing by `BACK_WITHIN`: left alone, and the shell is a plain shell again.
fn type_back(s: &Session, line: &str, prompts: u64) {
    let since = Instant::now();
    let shell = s.pane.pid();
    loop {
        if s.pane.is_exited() || since.elapsed() > BACK_WITHIN {
            s.inside.lock().unwrap().resuming = None;
            return;
        }
        let at_prompt = s.pane.foreground().is_none_or(|fg| Some(fg) == shell);
        let drawn = s.pane.shared.prompts.load(Ordering::Relaxed);
        let quiet = s.last_write.lock().unwrap().is_none_or(|t| t.elapsed() > Duration::from_millis(700));
        let unmarked = drawn == prompts && since.elapsed() > UNMARKED;
        if at_prompt && quiet && (drawn > prompts || unmarked) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    send_input(s, line, true);
}

/// `s`'s typed agent has gone: what dino followed of it goes too, and the shell is a plain shell.
pub(crate) fn left(d: &Daemon, s: &Session) {
    d.proxy.stats.agent_left(&s.id);
    *s.agent_session.lock().unwrap() = None;
    *s.mode_seen.lock().unwrap() = mode::Seen::default();
    *s.rollout.lock().unwrap() = Default::default();
    *s.log.lock().unwrap() = Default::default();
    *s.pending.lock().unwrap() = None;
    *s.asked.lock().unwrap() = None;
    *s.moving_to.lock().unwrap() = None;
    *s.forked_from.lock().unwrap() = None;
    s.fork_pending.store(false, Ordering::Relaxed);
    s.servers.lock().unwrap().clear();
    save(d);
}

/// `s`'s typed agent `f`, just seen: its conversation, when it names one dino doesn't know yet
/// (agents dino follows the record of move it on themselves, see `fork::follow`, `codex`); and,
/// typed through the shell's `claude` (`dino-agents.*`), that it reports its turns to this session,
/// as one dino started does: known before its first hook, so a call that fails as it starts (its
/// quota probe, rate limited) isn't taken for a failed turn.
pub(crate) fn seen(d: &Daemon, s: &Session, f: &FoundSession) {
    let Some(pid) = f.pid else { return };
    if !d.proxy.stats.session(&s.id).hooked && hooked_here(s, pid) {
        d.proxy.stats.reports_turns(&s.id);
    }
    if f.session_id.is_empty() {
        return;
    }
    let mut known = s.agent_session.lock().unwrap();
    if known.is_none() {
        *known = Some(f.session_id.clone());
    }
}

/// Agent process `pid` was started with this shell's hooks (`--settings <its agent settings>`).
fn hooked_here(s: &Session, pid: u32) -> bool {
    let ours = super::shell_agent_settings(&s.id).display().to_string();
    dino_core::procinfo::args_and_env(pid).is_some_and(|(args, _)| args.windows(2).any(|w| w[0] == "--settings" && w[1] == ours))
}
