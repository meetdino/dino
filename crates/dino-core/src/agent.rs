//! One adapter per agent dino works with: how it takes controls on its command line, how dino
//! wires it to the proxy and its status, where its conversations live, how to find one running,
//! and how to continue it. Everything that differs between agents goes through here, so an agent
//! is added in one file under `agent/`.

use std::path::{Path, PathBuf};

use crate::found::FoundSession;
use crate::history::Turn;
use crate::models::Catalog;

mod claude;
pub mod codex;
mod kimi;
mod pi;
mod qwen;

/// Which control a flag on an agent's command line sets.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ControlKind {
    Mode,
    Model,
    Effort,
}

/// Where dino learns what a session is doing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StatusSource {
    /// The agent reports its lifecycle to dino's hook sink (see `claude_hook_settings`).
    Hooks,
    /// dino reads the record the agent keeps of its turns (see dinod's `codex`).
    Rollout,
    /// dino follows the record the agent writes of its conversation as it goes (`log_path`),
    /// reading each new line (`log_event`); see dinod's `agentlog`.
    Log,
}

/// What one line of an agent's own record says about where its turn is.
#[derive(Clone, Debug, PartialEq)]
pub enum LogEvent {
    TurnStarted,
    TurnEnded,
    /// It waits on the user for this.
    Needs(String),
    /// Written whether or not anything happened for the user: says nothing about the turn.
    Bookkeeping,
    /// Anything else it does: it's no longer waiting.
    Other,
}

/// Proxy and hook wiring for a session: env vars, and arguments that go before dino's others.
pub type Wiring = (Vec<(String, String)>, Vec<String>);

pub trait Agent: Sync {
    /// The launcher's agent id: "claude", "codex", "qwen".
    fn id(&self) -> &'static str;

    // ---- Controls ----

    /// Mode ids from `controls::MODES` it offers, in that order.
    fn modes(&self) -> &'static [&'static str];
    /// Whether the model can be chosen.
    fn picks_model(&self) -> bool {
        true
    }
    /// The flags that put it in `mode`, one of its `modes`.
    fn mode_args(&self, mode: &str) -> Vec<String>;
    fn model_args(&self, model: &str) -> Vec<String>;
    fn effort_args(&self, effort: &str) -> Vec<String>;
    /// Its flags that take a value, as the next argument or after `=`.
    fn value_flags(&self) -> &'static [&'static str];
    /// Which control flag `name` (with `value`) sets, if any.
    fn control_of(&self, name: &str, value: Option<&str>) -> Option<ControlKind>;
    /// The mode its mode flags (in order, with their values) ask for, in dino's words; `None`
    /// when dino has no word for it.
    fn read_mode(&self, flags: &[(&str, Option<&str>)]) -> Option<String>;
    /// The effort an effort flag's value asks for.
    fn read_effort(&self, value: &str) -> Option<String> {
        Some(value.to_string())
    }
    /// A permission mode it reports through its hooks, in dino's words.
    fn reported_mode(&self, _mode: &str) -> Option<String> {
        None
    }

    // ---- Models ----

    /// The agent whose models it runs: the free tier shares Claude's.
    fn catalog_key(&self) -> &'static str {
        self.id()
    }
    /// The files its model list comes from, to notice when they change.
    fn catalog_sources(&self) -> Vec<PathBuf> {
        vec![]
    }
    /// The models its own files list, run as `program`; `None` when it keeps no list.
    fn catalog(&self, _program: &str) -> Option<Catalog> {
        None
    }

    // ---- Launching ----

    /// Env vars and arguments that route its API traffic through dino (`route`) and report its
    /// status. `base(provider)` is the proxy's URL for that provider; `status_line` a Claude
    /// `statusLine` setting to add.
    fn wiring(&self, route: bool, base: &dyn Fn(&str) -> String, status_line: Option<String>) -> Wiring;
    /// It runs on dino's free tier, which picks the model for each turn and only runs on this Mac.
    fn free(&self) -> bool {
        false
    }
    /// dino can route its API traffic through the proxy, to meter it.
    fn metered(&self) -> bool {
        false
    }
    /// Arguments that give it `prompt` to start on, staying open for more.
    fn prompt_args(&self, prompt: String) -> Vec<String> {
        vec![prompt]
    }
    /// Arguments that start conversation `session` (set when dino picks the id up front), or
    /// resume it when `restoring`: those before dino's other arguments, and those after.
    fn session_args(&self, session: &mut Option<String>, restoring: bool) -> (Vec<String>, Vec<String>);
    /// It reports its context window to a `statusLine` dino can wrap.
    fn statusline(&self) -> bool {
        false
    }
    /// It can be given dino's session tools (Settings → Policies).
    fn session_tools(&self) -> bool {
        false
    }
    /// It asks whether to trust a folder it hasn't seen before starting there.
    fn asks_trust(&self) -> bool {
        false
    }
    /// Where it trusts `dir` in repo `root`: `dir` or a folder above it, relative to `root`.
    fn trusted_in(&self, _dir: &Path, _root: &Path) -> Option<PathBuf> {
        None
    }
    /// Trust `dir`, so it starts there without asking.
    fn trust(&self, _dir: &Path) -> anyhow::Result<()> {
        Ok(())
    }

    // ---- Status ----

    fn status_source(&self) -> StatusSource;
    /// Whether process `pid` of it is on a turn, when it says; `None` when it doesn't.
    fn busy(&self, _pid: u32) -> Option<bool> {
        None
    }
    /// The conversation process `pid` of it is on, for agents that name it themselves.
    fn conversation_of(&self, _pid: u32) -> Option<String> {
        None
    }
    /// The file conversation `session` is written to as it goes (`StatusSource::Log`).
    fn log_path(&self, _session: &str) -> Option<PathBuf> {
        None
    }
    /// What a line of that file says.
    fn log_event(&self, _line: &serde_json::Value) -> LogEvent {
        LogEvent::Other
    }
    /// For agents that can't be given a conversation id up front: the conversation a process of it
    /// started in `cwd` at `since` (seconds) began, not one of `claimed`.
    fn new_conversation(&self, _cwd: &Path, _since: u64, _claimed: &[String]) -> Option<String> {
        None
    }

    // ---- Its conversations ----

    /// Launch flags worth carrying over when continuing a session, minus the ones that pick or
    /// create one.
    fn portable_flags(&self, args: &[String]) -> Vec<String>;
    /// Its sessions running in other terminals.
    fn running(&self) -> Vec<FoundSession>;
    /// It, if process `pid` (`comm`, arguments from `args`) is it, run by hand in a dino shell.
    fn inside(&self, pid: u32, comm: &str, args: &dyn Fn() -> Vec<String>) -> Option<FoundSession>;
    /// Whether `comm` may be it, so its arguments are worth reading.
    fn may_be(&self, _comm: &str) -> bool {
        false
    }
    /// Its conversations on disk, but those `running` says are running.
    fn recent(&self, running: &dyn Fn(&str) -> bool) -> Vec<FoundSession>;
    /// Its work in the provider's cloud, with `program` its CLI.
    fn cloud(&self, _program: &Path) -> Vec<FoundSession> {
        vec![]
    }
    /// Arguments that open cloud work `session_id` (empty: pick one).
    fn cloud_args(&self, session_id: &str) -> Vec<String>;
    /// The file conversation `session_id` is kept in.
    fn transcript(&self, session_id: &str) -> Option<PathBuf>;
    /// A page of that file as turns; `start` is where `text` begins in it.
    fn turns(&self, text: &str, path: &Path, start: u64) -> Vec<Turn>;
    /// Its latest turns as text, within `budget` characters, for another agent to read.
    fn tail(&self, _session_id: &str, _budget: usize) -> Option<String> {
        None
    }

    // ---- This Mac ----

    /// How it's signed in: "Claude Max", "ChatGPT login", "signed out".
    fn login(&self) -> Option<String> {
        None
    }
}

static CLAUDE: claude::Claude = claude::Claude { free: false };
static CLAUDE_FREE: claude::Claude = claude::Claude { free: true };
static CODEX: codex::Codex = codex::Codex;
static QWEN: qwen::Qwen = qwen::Qwen { free: false };
static QWEN_FREE: qwen::Qwen = qwen::Qwen { free: true };
static KIMI: kimi::Kimi = kimi::Kimi { free: false };
static KIMI_FREE: kimi::Kimi = kimi::Kimi { free: true };
static PI: pi::Pi = pi::Pi { free: false };
static PI_FREE: pi::Pi = pi::Pi { free: true };

/// The agents dino works with, in the order they're listed and looked for.
pub fn all() -> [&'static dyn Agent; 5] {
    [&CLAUDE, &CODEX, &QWEN, &KIMI, &PI]
}

/// The adapter for launcher agent id `id` (the free-tier ones, "claude-free", "qwen-free",
/// "kimi-free" and "pi-free", too); `None` for shells and agents dino only launches.
pub fn agent(id: &str) -> Option<&'static dyn Agent> {
    match id {
        "claude-free" => Some(&CLAUDE_FREE),
        "qwen-free" => Some(&QWEN_FREE),
        "kimi-free" => Some(&KIMI_FREE),
        "pi-free" => Some(&PI_FREE),
        _ => all().into_iter().find(|a| a.id() == id),
    }
}

pub(crate) fn strings(s: &[&str]) -> Vec<String> {
    s.iter().map(|s| s.to_string()).collect()
}

/// `c`'s flags for `a`, as `controls::args` checked them.
pub(crate) fn control_args(a: &dyn Agent, mode: Option<&str>, model: Option<&str>, effort: Option<&str>) -> Vec<String> {
    let mut out = vec![];
    out.extend(mode.map(|m| a.mode_args(m)).unwrap_or_default());
    out.extend(model.map(|m| a.model_args(m)).unwrap_or_default());
    out.extend(effort.map(|e| a.effort_args(e)).unwrap_or_default());
    out
}
