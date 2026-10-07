//! One adapter per agent dino works with: how it takes controls on its command line, how dino
//! wires it to the proxy and its status, where its conversations live, how to find one running,
//! and how to continue it. Everything that differs between agents goes through here, so an agent
//! is added in one file under `agent/`.

use std::path::{Path, PathBuf};

use crate::controls::Controls;
use crate::found::FoundSession;
use crate::history::Turn;
use crate::models::Catalog;
use crate::providers::{Format, ProviderModel};

pub(crate) mod amp;
mod claude;
pub mod codex;
mod codewhale;
mod copilot;
mod cursor;
mod hermes;
mod kimi;
mod opencode;
mod pi;
pub mod qwen;

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
    /// dino asks the agent's own store where its turn is (`turn_now`), a database rather than a
    /// file it appends to.
    Polled,
    /// The agent serves its own API on a port dino picks (`serve`), and dino follows the events
    /// it streams there (`server_event`); see dinod's `agentserver`.
    Server,
    /// It keeps no record dino can read as it goes: its output going quiet, and the questions its
    /// screen asks (`asking`), are all dino goes on.
    Screen,
}

/// What one event from an agent's own server (`StatusSource::Server`) says.
#[derive(Clone, Debug, PartialEq)]
pub enum ServerEvent {
    /// Conversation `0` (or a subagent's) is on a turn.
    Busy(String),
    Idle(String),
    /// It waits on the user for `what`, until `Answered(id)`.
    Asked { id: String, session: String, what: String },
    Answered(String),
    /// An answer in `session` read `used` tokens of `model`'s context.
    Context { session: String, model: String, used: u64 },
    /// Tool call `call` (tool `name`, as the agent names it) is out, or has ended (`done`). Said
    /// again as it goes: only its first word and its end count.
    Tool { call: String, name: String, done: bool },
    Other,
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

/// The header and environment variable that carry the proxy's secret for agents with
/// `keyed_urls` (the header is `dino_proxy::KEY_HEADER`).
pub const KEY_HEADER: &str = "x-dino-key";
pub const KEY_ENV: &str = "DINO_PROXY_KEY";

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
    /// What the agent itself calls `mode` (Claude says "Manual" where dino says "ask"), for the
    /// mode chip; `None` keeps dino's word.
    fn mode_label(&self, _mode: &str) -> Option<&'static str> {
        None
    }
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
    /// The permission mode its own screen shows it in now (Claude's footer), in dino's words;
    /// `None` when the screen doesn't say (a dialog over it, a mode dino has no word for).
    fn screen_mode(&self, _screen: &str) -> Option<String> {
        None
    }
    /// The key that steps it to its next permission mode in place, without a restart (Claude's
    /// Shift+Tab), and the modes it steps through in order as far as dino knows, for a session
    /// started with `args` (its mode flags included). `None` when it has no such key.
    fn mode_cycle(&self, _args: &[String]) -> Option<(&'static str, Vec<&'static str>)> {
        None
    }
    /// Arguments that put `mode` in its mode key's cycle (see `mode_cycle`) without starting in
    /// it (Claude's `--allow-dangerously-skip-permissions` for bypass), for a session in `cwd`
    /// whose config folder is `config` (its `CLAUDE_CONFIG_DIR`, if it has one). None when it has
    /// no such flag, or when the flag would bring up a question as it starts.
    fn reach_args(&self, _mode: &str, _cwd: &Path, _config: Option<&Path>) -> Vec<String> {
        vec![]
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
    /// Its catalog is read before dinod restarts sessions, so they get the efforts its models
    /// take. One that takes a while to ask, with no efforts to give, is read just after.
    fn catalog_first(&self) -> bool {
        true
    }

    // ---- Launching ----

    /// Env vars and arguments that route its API traffic through dino (`route`) and report its
    /// status. `base(provider)` is the proxy's URL for that provider; `status_line` a Claude
    /// `statusLine` setting to add.
    fn wiring(&self, route: bool, base: &dyn Fn(&str) -> String, status_line: Option<String>) -> Wiring;
    /// The API shapes it can talk to a provider in, best first: empty when it can't be pointed at one.
    fn provider_formats(&self) -> &'static [Format] {
        &[]
    }
    /// Env vars and arguments that run it on `model`, served at `url` (a dino proxy route) in
    /// `format`, one of its `provider_formats`. Its own settings and login stay as they are.
    fn provider_wiring(&self, _url: &str, _format: Format, _model: &str) -> Option<Wiring> {
        None
    }
    /// `provider_wiring`, with what the provider says of the model when dino has its list: for an
    /// agent that has to be told the model's context window, output limit or what it takes in.
    fn provider_wiring_for(&self, url: &str, format: Format, model: &str, _info: Option<&ProviderModel>) -> Option<Wiring> {
        self.provider_wiring(url, format, model)
    }
    /// It runs on dino's free tier, which picks the model for each turn and only runs on this Mac.
    fn free(&self) -> bool {
        false
    }
    /// dino can route its API traffic through the proxy, to meter it.
    fn metered(&self) -> bool {
        false
    }
    /// The proxy URLs it's given go on its command line, which every user of the Mac can read: it
    /// gets them without the proxy's secret, and sends that in the `KEY_HEADER` header, from the
    /// `KEY_ENV` environment variable. Agents given the URL in their environment or a private
    /// file keep the secret in it.
    fn keyed_urls(&self) -> bool {
        false
    }
    /// Arguments that give it `prompt` to start on, staying open for more. Nothing for one that
    /// takes no prompt on its command line: dinod types it in once it's ready for one.
    fn prompt_args(&self, prompt: String) -> Vec<String> {
        vec![prompt]
    }
    /// The prompt typed among its arguments (`dino new claude --model haiku "fix it"`), and the
    /// arguments without it: dinod gives it once, as the session starts, and never again as the
    /// session resumes its conversation. `None`: no prompt there, or no telling (an agent dino
    /// doesn't know the command line of, an option it doesn't know).
    fn launch_prompt(&self, _args: &[String]) -> Option<(Vec<String>, String)> {
        None
    }
    /// It can answer one request headless and leave: no tools, no questions, nothing kept, and
    /// its answer as plain text (see `one_shot`). The shell's ⌘I asks it for a command this way.
    /// Not yet: Qwen Code keeps tools however it's started (and runs a memory subagent with write
    /// tools), OpenCode and Kimi Code have no way to run without theirs, and Copilot CLI, which
    /// does, keeps every run as a session in its history (and so in dino's).
    fn answers_once(&self) -> bool {
        false
    }
    /// Arguments that run it that way on `ask`, for one that `answers_once`.
    fn one_shot(&self, _ask: &OneShot) -> Vec<String> {
        vec![]
    }
    /// Arguments that start conversation `session` (set when dino picks the id up front), or
    /// resume it when `restoring`: those before dino's other arguments, and those after.
    fn session_args(&self, session: &mut Option<String>, restoring: bool) -> (Vec<String>, Vec<String>);
    /// Just before dino starts it on conversation `session` (see `session_args`) in `cwd`: what
    /// has to be there for it to start on it quietly. Nothing for most.
    fn prepare_session(&self, _session: &str, _cwd: &Path) {}
    /// Conversation `session` is open in the agent's shared server right now (Codex 0.160.1's):
    /// resumed, it's taken over there, which only an agent started without command-line
    /// configuration does (dino's routing, its notices); one with it runs on its own, and finds the
    /// conversation taken until the server lets it go.
    fn in_shared_server(&self, _session: &str) -> bool {
        false
    }
    /// What a person types after its command to continue conversation `session` at a shell's
    /// prompt (`--resume <id>`, `resume <id>`), before and after their other flags: `session_args`
    /// without what dino adds for itself.
    fn resume_args(&self, session: &str) -> (Vec<String>, Vec<String>) {
        self.session_args(&mut Some(session.to_string()), true)
    }
    /// What a conversation it resumes keeps as it was saved, whatever its flags say: "model", or a
    /// mode id. Those can't be changed for a session that has one.
    fn resume_keeps(&self) -> &'static [&'static str] {
        &[]
    }
    /// `c`, as it will run when it resumes conversation `session`: what that keeps, and the mode
    /// it falls back to.
    fn resumed(&self, _session: &str, c: Controls) -> Controls {
        c
    }
    /// Arguments that start a new conversation as a copy of conversation `parent`, which stays as
    /// it is, working in `cwd`: the agent's own fork. Those before dino's other arguments, and those after;
    /// `session` is set to the new conversation's id when the agent takes one up front. `None`
    /// when it can't fork from its command line, or dino hasn't seen its way of doing it work.
    fn fork_args(&self, _parent: &str, _cwd: &Path, _session: &mut Option<String>) -> Option<(Vec<String>, Vec<String>)> {
        None
    }
    /// The conversation `session` was forked from, as the agent's own record of it says: a fork
    /// made in the agent (Claude's `/branch`, Codex's `/fork`).
    fn forked_from(&self, _session: &str) -> Option<String> {
        None
    }
    /// It reports its context window to a `statusLine` dino can wrap.
    fn statusline(&self) -> bool {
        false
    }
    /// It can be given dino's session tools (Settings → Experimental).
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
    /// A question its own screen puts to the user that its record doesn't (its folder trust, a
    /// permission dialog), in a few words; `None` when the screen asks nothing. Read in a dino
    /// shell, in a session's state, and, for `StatusSource::Polled`, while its store says it's on
    /// a turn (reported as waiting on the user).
    fn asking(&self, _screen: &str) -> Option<String> {
        None
    }
    /// Its screen can ask what `asking` reads, so it's worth reading.
    fn asks_on_screen(&self) -> bool {
        false
    }
    /// The tool calls a line of that file starts (`(name, true)`, the tool's name as the agent
    /// gives it) and ends (`(name, false)`; the name may be empty when the line doesn't repeat it).
    fn tool_calls(&self, _line: &serde_json::Value) -> Vec<(String, bool)> {
        vec![]
    }
    /// The model a line of its own record (that file, Codex's rollout) says its conversation is on
    /// from then on, as its command line names it: a switch made in the agent itself (`/model`)
    /// included, which the flags it was started with don't say. `None` for a line that doesn't say.
    fn log_model(&self, _line: &serde_json::Value) -> Option<String> {
        None
    }
    /// For agents that can't be given a conversation id up front: the conversation a process of it
    /// started in `cwd` at `since` (seconds) began, not one of `claimed`.
    fn new_conversation(&self, _cwd: &Path, _since: u64, _claimed: &[String]) -> Option<String> {
        None
    }
    /// With `StatusSource::Polled`: whether conversation `session` is on a turn, as its store says
    /// now, in a process of it started at `since` (seconds).
    fn turn_now(&self, _session: &str, _since: u64) -> Option<bool> {
        None
    }
    /// What its terminal takes to interrupt a turn and stay open, as the user would press it. Ctrl+C
    /// (`\x03`) quits some at their prompt, so it's only sent while a turn runs.
    fn interrupt_keys(&self) -> &'static [u8] {
        b"\x1b"
    }
    /// What its terminal takes to quit it at its prompt and leave the terminal as it found it, as
    /// the user would press it; empty when a SIGTERM does that (Claude Code says how to resume and
    /// goes). Sent to one typed into a dino shell that dino starts again there.
    fn quit_keys(&self) -> &'static [u8] {
        b""
    }
    /// With `StatusSource::Polled`: the model conversation `session` is on, as its store says,
    /// when that was written since a process of it started at `since` (seconds); see `log_model`.
    fn model_now(&self, _session: &str, _since: u64) -> Option<String> {
        None
    }
    /// With `StatusSource::Polled`: the tools conversation `session` is calling right now, by name.
    fn tools_now(&self, _session: &str) -> Vec<String> {
        vec![]
    }
    /// With `StatusSource::Server`: env vars and arguments that have it serve its API on `port`
    /// of this Mac, open only with `password`.
    fn serve(&self, _port: u16, _password: &str) -> Wiring {
        (vec![], vec![])
    }
    /// The user name its server takes with that password.
    fn server_user(&self) -> &'static str {
        ""
    }
    /// What an event from its server says.
    fn server_event(&self, _event: &serde_json::Value) -> ServerEvent {
        ServerEvent::Other
    }
    /// Where its server says how things stand now, read on connecting: what it was doing before
    /// dino followed its events.
    fn server_snapshot(&self) -> &'static [&'static str] {
        &[]
    }
    /// What the answer from snapshot `path` says, as events.
    fn server_snapshot_events(&self, _path: &str, _answer: &serde_json::Value) -> Vec<ServerEvent> {
        vec![]
    }
    /// Where its server says what its providers and models are.
    fn server_providers(&self) -> Option<&'static str> {
        None
    }
    /// `model`'s context window, from what its server says of its providers; `None` when it
    /// doesn't know.
    fn server_context_window(&self, _providers: &serde_json::Value, _model: &str) -> Option<u64> {
        None
    }
    /// Whether `session` is a conversation of its own rather than a subagent's.
    fn is_conversation(&self, _session: &str) -> bool {
        true
    }
    /// For agents whose conversations aren't files: a page of `session_id`'s turns ending before
    /// position `before` (the end when `None`), in the agent's own positions.
    fn page(&self, _session_id: &str, _before: Option<u64>) -> Option<crate::history::Page> {
        None
    }

    /// The title it puts on its terminal, as dino shows it: without what it adds to every one.
    fn shown_title(&self, title: &str) -> Option<String> {
        Some(title.to_string())
    }

    // ---- Its conversations ----

    /// Launch flags worth carrying over when continuing a session, minus the ones that pick or
    /// create one.
    fn portable_flags(&self, args: &[String]) -> Vec<String>;
    /// Its sessions running anywhere on this Mac, among `procs` (every process, listed once for
    /// all agents: see `found::scan`, which picks those a person could take over).
    fn running(&self, procs: &crate::procinfo::Procs) -> Vec<FoundSession>;
    /// It, if process `pid` (`comm`, arguments from `args`) is it, run by hand in a dino shell.
    fn inside(&self, pid: u32, comm: &str, args: &dyn Fn() -> Vec<String>) -> Option<FoundSession>;
    /// Whether `comm` may be it, so its arguments are worth reading.
    fn may_be(&self, _comm: &str) -> bool {
        false
    }
    /// Whether arguments `args` (after the program) run it headless: once and out (a print
    /// mode), as a server for another program, or a command that isn't a conversation. Nothing a
    /// person could take over, so never found running "on this Mac".
    fn headless(&self, _args: &[String]) -> bool {
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

    /// The model answers its own records hold that `seen` hasn't read yet: what it used, for
    /// usage statistics. Only what's new is read (see `usage::Seen`); empty for an agent whose
    /// records don't say.
    fn usage(&self, _seen: &mut crate::usage::Seen) -> Vec<crate::usage::Used> {
        vec![]
    }

    /// Variables besides keys, tokens and base URLs (see [`account_env`]) that pick the account,
    /// endpoint or config it runs with: `CLAUDE_CONFIG_DIR`, `CODEX_HOME`.
    fn account_vars(&self) -> &'static [&'static str] {
        &[]
    }
}

/// What in `env` (a process's `NAME=value` environment, as `procinfo::args_and_env` reads it)
/// chose the account an agent of `agent_id` talks to: its keys and tokens, base URLs, and its own
/// `account_vars`. A conversation dino continues for someone (one started by hand, `claude
/// --resume` with another account's token) keeps them, or it falls back to the user's own login.
/// Never dino's own variables, nor a base URL of a dino proxy (dino wires its own), nor what a
/// parent agent sets.
pub fn account_env(agent_id: &str, env: &[String]) -> Vec<(String, String)> {
    const SUFFIXES: &[&str] = &["_API_KEY", "_AUTH_TOKEN", "_OAUTH_TOKEN", "_BASE_URL"];
    let own = agent(agent_id).map(|a| a.account_vars()).unwrap_or_default();
    let mut out: Vec<(String, String)> = env
        .iter()
        .filter_map(|kv| kv.split_once('='))
        .filter(|(k, v)| !v.is_empty() && !k.starts_with("DINO_") && !crate::PARENT_AGENT_ENV.contains(k))
        .filter(|(k, _)| SUFFIXES.iter().any(|s| k.ends_with(s)) || own.contains(k))
        .filter(|(k, v)| !(k.ends_with("_BASE_URL") && crate::is_proxy_url(v)))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    out.sort();
    out.dedup_by(|a, b| a.0 == b.0);
    out
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
static HERMES: hermes::Hermes = hermes::Hermes { free: false };
static HERMES_FREE: hermes::Hermes = hermes::Hermes { free: true };
static CODEWHALE: codewhale::CodeWhale = codewhale::CodeWhale;
static OPENCODE: opencode::OpenCode = opencode::OpenCode;
static COPILOT: copilot::Copilot = copilot::Copilot;
static CURSOR: cursor::Cursor = cursor::Cursor;
static AMP: amp::Amp = amp::Amp;

/// The agents dino works with, in the order they're listed and looked for.
pub fn all() -> [&'static dyn Agent; 11] {
    [&CLAUDE, &CODEX, &QWEN, &KIMI, &PI, &HERMES, &CODEWHALE, &OPENCODE, &COPILOT, &CURSOR, &AMP]
}

/// The adapter for launcher agent id `id` (the free-tier ones, "<agent>-free", too); `None` for
/// shells and agents dino only launches.
pub fn agent(id: &str) -> Option<&'static dyn Agent> {
    match id {
        "claude-free" => Some(&CLAUDE_FREE),
        "qwen-free" => Some(&QWEN_FREE),
        "kimi-free" => Some(&KIMI_FREE),
        "pi-free" => Some(&PI_FREE),
        "hermes-free" => Some(&HERMES_FREE),
        _ => all().into_iter().find(|a| a.id() == id),
    }
}

/// One request for an agent that `answers_once`.
pub struct OneShot<'a> {
    /// What it's told it is for, in place of its own instructions where it takes them.
    pub instructions: &'a str,
    pub request: &'a str,
    /// Its model and effort flags (`controls::args`).
    pub controls: Vec<String>,
    /// Where it runs.
    pub cwd: &'a Path,
    /// A new empty file only the user can read, for an agent that writes its answer to a file
    /// rather than printing it.
    pub answer: &'a Path,
}

/// An agent's command line as far as finding its prompt goes (see `positional_prompt`).
pub(crate) struct Cli {
    /// Options that take the next word as their value (or `--option=value`).
    pub value: &'static [&'static str],
    /// Options that take the next word as their value when it isn't an option itself.
    pub optional: &'static [&'static str],
    /// Options that take every word up to the next option.
    pub variadic: &'static [&'static str],
    /// Options that take no value.
    pub flags: &'static [&'static str],
    /// Its subcommands: a command line naming one has no prompt.
    pub commands: &'static [&'static str],
}

/// The prompt in `args` for an agent that takes it as its one positional argument, read as its
/// own parser would; `None` when there's none, more than one, a subcommand, or a word after an
/// option `cli` doesn't know (it may be that option's value).
pub(crate) fn positional_prompt(args: &[String], cli: &Cli) -> Option<(Vec<String>, String)> {
    let mut positional = vec![];
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--" {
            positional.extend(i + 1..args.len());
            break;
        }
        if a.len() > 1 && a.starts_with('-') {
            // `--option=value` holds its value; otherwise the words after it may be its.
            let next_is_word = args.get(i + 1).is_some_and(|n| !n.starts_with('-'));
            if a.contains('=') {
                // Its value is in it.
            } else if cli.value.contains(&a) {
                i += 1;
            } else if cli.optional.contains(&a) {
                i += usize::from(next_is_word);
            } else if cli.variadic.contains(&a) {
                while args.get(i + 1).is_some_and(|n| !n.starts_with('-')) {
                    i += 1;
                }
            } else if !cli.flags.contains(&a) && next_is_word {
                return None;
            }
        } else {
            positional.push(i);
        }
        i += 1;
    }
    let [at] = positional[..] else { return None };
    if cli.commands.contains(&args[at].as_str()) || args[at].trim().is_empty() {
        return None;
    }
    let mut rest = args.to_vec();
    let prompt = rest.remove(at);
    // A `--` left with nothing after it.
    if rest.last().is_some_and(|l| l == "--") {
        rest.pop();
    }
    Some((rest, prompt))
}

/// The prompt given with option `flags` (`-i "fix it"`, `--prompt=fix it`), and `args` without it.
pub(crate) fn option_prompt(args: &[String], flags: &[&str]) -> Option<(Vec<String>, String)> {
    for (i, a) in args.iter().enumerate() {
        if let Some((name, value)) = a.split_once('=')
            && flags.contains(&name)
        {
            let mut rest = args.to_vec();
            rest.remove(i);
            return Some((rest, value.to_string()));
        }
        if flags.contains(&a.as_str()) {
            let value = args.get(i + 1).filter(|v| !v.starts_with('-'))?.clone();
            let mut rest = args.to_vec();
            rest.drain(i..=i + 1);
            return Some((rest, value));
        }
    }
    None
}

/// A prompt an agent is started on goes on its command line (see `Agent::prompt_args`), where one
/// starting with `-` would be read as a flag (`--dangerously-skip-permissions`, `--settings=...`):
/// refuse it, since not every agent's command line honours `--`.
pub fn check_prompt(prompt: &str) -> anyhow::Result<()> {
    anyhow::ensure!(!prompt.trim_start().starts_with('-'), "a prompt to start an agent on can't begin with \"-\": it would be read as a command-line flag");
    Ok(())
}

/// `args` have one of `flags` (alone, or as `--flag=value`), or start with one of `commands`
/// (after the script, for an agent Node runs: `node /…/bin/qwen serve`).
pub(crate) fn runs_with(args: &[String], flags: &[&str], commands: &[&str]) -> bool {
    let args = if args.first().is_some_and(|a| a.contains('/') && !a.starts_with('-')) { &args[1..] } else { args };
    args.first().is_some_and(|a| commands.contains(&a.as_str())) || args.iter().any(|a| flags.contains(&a.split('=').next().unwrap_or(a)))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hand_run_agents_account_comes_from_its_keys_tokens_and_endpoints() {
        let env: Vec<String> = [
            "PATH=/usr/bin",
            "HOME=/Users/x",
            "CLAUDE_CODE_OAUTH_TOKEN=sk-ant-oat01-second",
            "ANTHROPIC_API_KEY=",
            "ANTHROPIC_AUTH_TOKEN=gateway-token",
            "ANTHROPIC_BASE_URL=http://127.0.0.1:4100/k/secret/s/7/anthropic",
            "OPENAI_BASE_URL=https://gateway.example/v1",
            "OPENAI_API_KEY=sk-openai",
            "CLAUDE_CONFIG_DIR=/Users/x/.claude-work",
            "DINO_CLAUDE_BASE_URL=http://127.0.0.1:4100/k/secret/s/7/anthropic",
            "DINO_PROXY_KEY=secret",
            "CODEX_HOME=/Users/x/.codex-work",
            "not a variable",
        ]
        .map(String::from)
        .to_vec();
        let names = |agent: &str| account_env(agent, &env).into_iter().map(|(k, _)| k).collect::<Vec<_>>();
        // Keys, tokens and its own config folder; never dino's, an empty one, or a dino proxy's URL.
        assert_eq!(names("claude"), ["ANTHROPIC_AUTH_TOKEN", "CLAUDE_CODE_OAUTH_TOKEN", "CLAUDE_CONFIG_DIR", "OPENAI_API_KEY", "OPENAI_BASE_URL"]);
        assert_eq!(names("codex"), ["ANTHROPIC_AUTH_TOKEN", "CLAUDE_CODE_OAUTH_TOKEN", "CODEX_HOME", "OPENAI_API_KEY", "OPENAI_BASE_URL"]);
        assert_eq!(account_env("claude", &env).iter().find(|(k, _)| k == "CLAUDE_CODE_OAUTH_TOKEN").map(|(_, v)| v.as_str()), Some("sk-ant-oat01-second"));
        // A base URL of the user's own is theirs to keep.
        let own = vec!["ANTHROPIC_BASE_URL=https://gateway.example".to_string()];
        assert_eq!(account_env("claude", &own), [("ANTHROPIC_BASE_URL".to_string(), "https://gateway.example".to_string())]);
        assert!(account_env("shell", &["PATH=/bin".to_string()]).is_empty());
    }

    #[test]
    fn the_prompt_among_an_agents_arguments_is_found_as_its_parser_would() {
        let w = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<String>>();
        let claude = agent("claude").unwrap();
        let split = |a: &dyn Agent, args: &[&str]| a.launch_prompt(&w(args));
        assert_eq!(split(claude, &["--model", "haiku", "fix the bug"]), Some((w(&["--model", "haiku"]), "fix the bug".into())));
        assert_eq!(split(claude, &["fix it", "--model=haiku", "--verbose"]), Some((w(&["--model=haiku", "--verbose"]), "fix it".into())));
        assert_eq!(split(claude, &["--tools", "", "-p", "hi"]), Some((w(&["--tools", "", "-p"]), "hi".into())));
        assert_eq!(split(claude, &["--", "fix it"]), Some((vec![], "fix it".into())));
        // Values, not prompts: a model, tools a variadic option takes, a debug filter.
        assert_eq!(split(claude, &["--model", "haiku"]), None);
        assert_eq!(split(claude, &["--allowedTools", "Bash", "fix it"]), None, "Claude reads it as a tool too");
        assert_eq!(split(claude, &["--allowedTools=Bash", "fix it"]), Some((w(&["--allowedTools=Bash"]), "fix it".into())));
        assert_eq!(split(claude, &["--debug", "api"]), None);
        assert_eq!(split(claude, &["mcp", "list"]), None, "a subcommand");
        assert_eq!(split(claude, &["one", "two"]), None, "two words: not one prompt");
        assert_eq!(split(claude, &["--some-new-option", "value"]), None, "maybe that option's value");
        assert_eq!(split(claude, &[]), None);
        let codex = agent("codex").unwrap();
        assert_eq!(split(codex, &["-m", "gpt-5.5", "-c", "x=1", "add tests"]), Some((w(&["-m", "gpt-5.5", "-c", "x=1"]), "add tests".into())));
        assert_eq!(split(codex, &["resume", "--last"]), None);
        assert_eq!(split(codex, &["-i", "a.png", "b.png"]), None, "images");
        // Given with an option.
        let qwen = agent("qwen").unwrap();
        assert_eq!(split(qwen, &["-m", "q", "-i", "fix it"]), Some((w(&["-m", "q"]), "fix it".into())));
        assert_eq!(split(qwen, &["--prompt-interactive=fix it"]), Some((vec![], "fix it".into())));
        assert_eq!(split(agent("opencode").unwrap(), &["--prompt", "fix it", "--model", "x"]), Some((w(&["--model", "x"]), "fix it".into())));
        assert_eq!(split(agent("copilot").unwrap(), &["--model", "gpt-5.5"]), None);
        assert_eq!(split(agent("pi").unwrap(), &["fix it"]), None, "a command line dino doesn't know");
    }

    #[test]
    fn headless_runs_by_flag_or_command() {
        let a = |v: &[&str]| strings(v);
        let claude = agent("claude").unwrap();
        assert!(claude.headless(&a(&["-p", "hi"])) && claude.headless(&a(&["--print"])) && claude.headless(&a(&["--output-format=stream-json"])));
        assert!(claude.headless(&a(&["mcp", "list"])));
        assert!(!claude.headless(&a(&["--model", "opus"])) && !claude.headless(&a(&["--resume", "x"])) && !claude.headless(&a(&[])));
        let codex = agent("codex").unwrap();
        assert!(codex.headless(&a(&["exec", "hi"])) && codex.headless(&a(&["app-server"])) && codex.headless(&a(&["e", "x"])));
        assert!(!codex.headless(&a(&["resume", "--last"])) && !codex.headless(&a(&["fix the build"])) && !codex.headless(&a(&["-m", "o3"])));
        let qwen = agent("qwen").unwrap();
        assert!(qwen.headless(&a(&["/opt/homebrew/bin/qwen", "serve"])) && qwen.headless(&a(&["/x/qwen", "-p", "hi"])) && qwen.headless(&a(&["--acp"])));
        assert!(!qwen.headless(&a(&["/x/qwen", "-i", "hi"])));
        let pi = agent("pi").unwrap();
        assert!(pi.headless(&a(&["--mode", "rpc"])) && pi.headless(&a(&["--mode=json"])) && pi.headless(&a(&["-p", "x"])));
        assert!(!pi.headless(&a(&["--mode", "text"])) && !pi.headless(&a(&["hello"])));
        let copilot = agent("copilot").unwrap();
        assert!(copilot.headless(&a(&["-p", "x"])) && copilot.headless(&a(&["--acp"])) && !copilot.headless(&a(&["-i", "x"])));
        let opencode = agent("opencode").unwrap();
        assert!(opencode.headless(&a(&["run", "x"])) && opencode.headless(&a(&["serve"])) && !opencode.headless(&a(&["--prompt", "x"])));
        assert!(agent("cursor").unwrap().headless(&a(&["-p", "x"])) && !agent("cursor").unwrap().headless(&a(&["fix it"])));
        assert!(agent("kimi").unwrap().headless(&a(&["--print"])) && !agent("kimi").unwrap().headless(&a(&["-c"])));
        assert!(agent("amp").unwrap().headless(&a(&["-x", "hi"])) && !agent("amp").unwrap().headless(&a(&["threads", "continue"])));
        assert!(agent("codewhale").unwrap().headless(&a(&["exec", "x"])) && !agent("codewhale").unwrap().headless(&a(&["resume"])));
        assert!(agent("hermes").unwrap().headless(&a(&["-z", "x"])) && !agent("hermes").unwrap().headless(&a(&["chat", "-q", "x"])));
    }

    const URL: &str = "http://127.0.0.1:5000/s/7/local/ollama";

    fn env<'a>(w: &'a Wiring, k: &str) -> Option<&'a str> {
        w.0.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
    }

    #[test]
    fn agents_that_answer_once_get_the_request_and_their_instructions() {
        let dir = std::env::temp_dir();
        let answer = dir.join("answer.txt");
        let ask = OneShot { instructions: "Reply with one command.", request: "Request: list files", controls: strings(&["--model", "m"]), cwd: &dir, answer: &answer };
        let answering: Vec<&str> = all().into_iter().filter(|a| a.answers_once()).map(|a| a.id()).collect();
        assert_eq!(answering, ["claude", "codex", "pi"]);
        for id in answering {
            let args = agent(id).unwrap().one_shot(&ask);
            assert!(args.last().is_some_and(|l| l.ends_with("Request: list files")), "{id}: {args:?}");
            assert!(args.iter().any(|a| a.contains("Reply with one command.")), "{id}: {args:?}");
            assert!(args.windows(2).any(|w| w == ["--model", "m"]), "{id}: {args:?}");
        }
        for free in ["claude-free", "qwen-free", "pi-free"] {
            assert!(!agent(free).unwrap().answers_once(), "{free} picks its model turn by turn");
        }
    }

    #[test]
    fn each_agent_is_pointed_at_the_provider_route_in_its_own_way() {
        let claude = agent("claude").unwrap().provider_wiring(URL, Format::Anthropic, "qwen3:4b").unwrap();
        assert_eq!(env(&claude, "ANTHROPIC_BASE_URL"), Some(URL));
        assert_eq!(env(&claude, "ANTHROPIC_API_KEY"), Some(""));
        for var in ["ANTHROPIC_MODEL", "ANTHROPIC_DEFAULT_HAIKU_MODEL", "ANTHROPIC_SMALL_FAST_MODEL"] {
            assert_eq!(env(&claude, var), Some("qwen3:4b"), "{var}");
        }
        // Claude only speaks Anthropic Messages; the free tier is a launcher of its own.
        assert!(agent("claude").unwrap().provider_wiring(URL, Format::Chat, "m").is_none());
        assert!(agent("claude-free").unwrap().provider_formats().is_empty());

        let codex = agent("codex").unwrap().provider_wiring(URL, Format::Responses, "qwen3:4b").unwrap();
        let args = codex.1.join(" ");
        assert!(args.contains("model_provider=\"dino\"") && args.contains(&format!("base_url=\"{URL}/v1\"")) && args.ends_with("-m qwen3:4b"), "{args}");
        assert_eq!(env(&codex, "DINO_PROVIDER_KEY"), Some("dino"));
        // Its URL goes on its command line (`-c`): dinod gives it one without the proxy's secret,
        // which Codex sends in a header from its environment.
        assert!(agent("codex").unwrap().keyed_urls());
        assert!(args.contains(r#"model_providers.dino.env_http_headers={"x-dino-key"="DINO_PROXY_KEY"}"#), "{args}");

        let qwen = agent("qwen").unwrap();
        assert_eq!(qwen.provider_formats()[0], Format::Chat);
        let chat = qwen.provider_wiring(URL, Format::Chat, "m").unwrap();
        assert!(chat.1.join(" ").contains("--auth-type openai "), "{:?}", chat.1);
        assert_eq!(env(&chat, "OPENAI_BASE_URL"), Some(format!("{URL}/v1").as_str()));
        assert!(!chat.1.iter().any(|a| a.contains(URL)), "the URL (and the proxy's secret in it) stays off its command line: {:?}", chat.1);
        let anthropic = qwen.provider_wiring(URL, Format::Anthropic, "m").unwrap();
        assert_eq!(env(&anthropic, "ANTHROPIC_BASE_URL"), Some(URL));

        let kimi = agent("kimi").unwrap().provider_wiring(URL, Format::Responses, "m").unwrap();
        assert_eq!(env(&kimi, "KIMI_MODEL_PROVIDER_TYPE"), Some("openai_responses"));
        let hermes = agent("hermes").unwrap();
        assert_eq!(hermes.provider_formats(), [Format::Chat]);
        assert!(hermes.provider_wiring(URL, Format::Anthropic, "m").is_none());

        let codewhale = agent("codewhale").unwrap();
        assert_eq!(codewhale.provider_formats(), [Format::Chat, Format::Anthropic]);
        let chat = codewhale.provider_wiring(URL, Format::Chat, "qwen3:4b").unwrap();
        assert_eq!(env(&chat, "OPENAI_BASE_URL"), Some(format!("{URL}/v1").as_str()));
        assert!(!chat.1.iter().any(|a| a.contains(URL)), "the URL stays off its command line: {:?}", chat.1);

        let opencode = agent("opencode").unwrap();
        assert_eq!(opencode.provider_formats()[0], Format::Chat);
        let chat = opencode.provider_wiring(URL, Format::Chat, "qwen3:4b").unwrap();
        assert_eq!(chat.1, ["-m", "dino/qwen3:4b"]);
        assert!(env(&chat, "OPENCODE_CONFIG_CONTENT").is_some_and(|c| c.contains(&format!("{URL}/v1"))));
        assert!(!opencode.keyed_urls(), "its URL is in its environment");
    }

    /// A dino shell isn't routed itself (every program there using the Anthropic SDK would be);
    /// it carries the route for the agents typed into it (see `dino-agents.*`). Unless routing is off.
    #[test]
    fn a_shell_carries_the_route_only_for_its_agents() {
        let base = |p: &str| format!("http://127.0.0.1:5000/k/s3cret/s/7/{p}");
        let shell = crate::proxy_wiring("shell", true, &base, None);
        assert!(env(&shell, "ANTHROPIC_BASE_URL").is_none(), "{shell:?}");
        // As long as the test's own environment has no base URL of a user's own.
        if std::env::var("ANTHROPIC_BASE_URL").map_or(true, |v| crate::is_proxy_url(&v)) {
            assert_eq!(env(&shell, crate::SHELL_CLAUDE_BASE_URL), Some(base("anthropic").as_str()));
        }
        assert_eq!(crate::proxy_wiring("shell", false, &base, None), (vec![], vec![]));
    }

    /// Claude Code and Codex fork by their own commands (checked against Claude Code 2.1.289 and
    /// Codex 0.160); the others aren't offered it until dino has seen theirs work.
    #[test]
    fn only_agents_whose_fork_dino_has_seen_work_fork() {
        let forks: Vec<&str> = all().into_iter().filter(|a| a.fork_args("p", Path::new("/r"), &mut None).is_some()).map(|a| a.id()).collect();
        assert_eq!(forks, ["claude", "codex"]);
        let mut session = None;
        let (before, after) = agent("claude").unwrap().fork_args("parent-id", Path::new("/r/wt"), &mut session).unwrap();
        let new = session.clone().unwrap();
        assert!(before.is_empty());
        assert_eq!(after, strings(&["--resume", "parent-id", "--fork-session", "--session-id", &new]));
        // Started again before the copy is saved: the same new id.
        assert_eq!(agent("claude").unwrap().fork_args("parent-id", Path::new("/r/wt"), &mut session).unwrap().1[4], new);
        let mut none = None;
        let (before, after) = agent("codex").unwrap().fork_args("parent-id", Path::new("/r/wt"), &mut none).unwrap();
        assert_eq!(before, ["fork"]);
        // In the folder it's started in: asked, Codex would offer the original's.
        assert_eq!(after[..3], ["-C", "/r/wt", "parent-id"]);
        assert!(none.is_none(), "Codex names the fork's id in its rollout");
    }

    #[test]
    fn a_prompt_that_would_be_read_as_a_flag_is_refused() {
        for p in ["--dangerously-skip-permissions", "  --settings={\"hooks\":{}}", "\n\t-p", "-", "--dangerously-bypass-approvals-and-sandbox"] {
            assert!(check_prompt(p).is_err(), "{p:?}");
        }
        for p in ["fix the tests", "  fix -- the tests", "why does `-x` fail?", "", "   "] {
            assert!(check_prompt(p).is_ok(), "{p:?}");
        }
    }

    /// Claude's footer, as Claude Code 2.1.289 draws it (from a real session's screen).
    #[test]
    fn claudes_mode_is_read_off_its_footer() {
        let claude = agent("claude").unwrap();
        let screen = |footer: &str| format!("❯ Say hi\n⏺ Hey there!\n\n{}\n❯ \n{}\n  {footer}\n", "─".repeat(40), "─".repeat(40));
        let mode = |footer: &str| claude.screen_mode(&screen(footer));
        assert_eq!(mode("⏸ manual mode on · ? for shortcuts · ← for agents").as_deref(), Some("ask"));
        assert_eq!(mode("⏵⏵ accept edits on (shift+tab to cycle) · ← for agents").as_deref(), Some("edits"));
        assert_eq!(mode("⏸ plan mode on (shift+tab to cycle) · esc to interrupt").as_deref(), Some("plan"));
        assert_eq!(mode("⏵⏵ auto mode on (shift+tab to cycle) · ← for agents").as_deref(), Some("auto"));
        assert_eq!(mode("⏵⏵ bypass permissions on (shift+tab to cycle) · ← for agents").as_deref(), Some("bypass"));
        assert_eq!(mode("? for shortcuts").as_deref(), Some("ask"), "an older Claude names no mode in its default one");
        assert_eq!(mode("⏵⏵ some new mode on"), None, "one dino has no word for");
        // A dialog over the prompt: no footer, nothing said; nor in what it wrote higher up.
        assert_eq!(claude.screen_mode("⏵⏵ accept edits on\n\n Do you want to proceed?\n ❯ 1. Yes\n   2. No\n\n Esc to cancel"), None);
        assert_eq!(agent("codex").unwrap().screen_mode(&screen("⏵⏵ accept edits on")), None);
    }

    /// The model Codex says it's on, as Codex 0.160 wrote its rollout (from a real one, trimmed):
    /// each turn's, and a change between turns as it's made.
    #[test]
    fn codexs_model_is_read_off_its_rollout() {
        let codex = agent("codex").unwrap();
        let line = |s: &str| codex.log_model(&serde_json::from_str(s).unwrap());
        let turn = r#"{"timestamp":"2026-09-28T18:17:10.526Z","type":"turn_context","payload":{"turn_id":"01a0e93c-1aa1-7bc2-8f00-4781b282c05d","cwd":"/Users/me/x","approval_policy":"on-request","model":"gpt-5.6-luna","effort":"xhigh","collaboration_mode":{"mode":"default","settings":{"model":"gpt-5.6-luna","reasoning_effort":"xhigh"}},"summary":"none"}}"#;
        assert_eq!(line(turn).as_deref(), Some("gpt-5.6-luna"));
        let switched = r#"{"timestamp":"2026-09-28T18:17:37.738Z","type":"event_msg","payload":{"type":"thread_settings_applied","thread_id":"01a0e93b-2fcf-7a20-8efb-916be31ad524","thread_settings":{"model":"gpt-5.5","model_provider_id":"dino","service_tier":"default","approval_policy":"on-request","reasoning_effort":"xhigh","personality":"pragmatic"}}}"#;
        assert_eq!(line(switched).as_deref(), Some("gpt-5.5"));
        let started = r#"{"timestamp":"2026-09-28T18:17:05.481Z","type":"event_msg","payload":{"type":"task_started","turn_id":"01a0e93c-1aa1-7bc2-8f00-4781b282c05d","model_context_window":258400}}"#;
        assert_eq!(line(started), None);
        assert_eq!(line(r#"{"type":"turn_context","payload":{"model":""}}"#), None);
        assert_eq!(agent("claude").unwrap().log_model(&serde_json::from_str(turn).unwrap()), None, "Claude says it through its hooks");
    }

    /// The model other agents' records say they're on, as their command lines take it: Pi 0.99's
    /// and Copilot CLI 1.0.91's as they wrote them; Kimi Code's as its source writes them.
    #[test]
    fn the_model_is_read_off_each_agents_record() {
        let said = |id: &str, line: &str| agent(id).unwrap().log_model(&serde_json::from_str(line).unwrap());
        let pi = r#"{"type":"model_change","id":"a1","parentId":null,"timestamp":"2026-09-30T04:05:59.200Z","provider":"anthropic","modelId":"claude-opus-4-8"}"#;
        assert_eq!(said("pi", pi).as_deref(), Some("anthropic/claude-opus-4-8"), "as `--model` and its model list name it");
        assert_eq!(said("pi", r#"{"type":"message","message":{"role":"assistant","provider":"anthropic","model":"claude-sonnet-4-5"}}"#), None, "what answered, not its setting");
        let start = r#"{"type":"session.start","data":{"sessionId":"b2fcbbeb-4980-4063-8e3d-4f1a5585ed36","copilotVersion":"1.0.91","selectedModel":"stub-model","context":{"cwd":"/private/tmp/proj"}},"id":"fb6cb0b2","timestamp":"2026-10-04T04:21:17.924Z"}"#;
        assert_eq!(said("copilot", start).as_deref(), Some("stub-model"));
        let change = r#"{"type":"session.model_change","data":{"newModel":"gpt-5.5","previousModel":"stub-model","source":"user"},"id":"3ffa6930","timestamp":"2026-10-04T04:21:18.137Z"}"#;
        assert_eq!(said("copilot", change).as_deref(), Some("gpt-5.5"));
        assert_eq!(said("copilot", r#"{"type":"assistant.usage","data":{"model":"stub-model"}}"#), None);
        assert_eq!(said("kimi", r#"{"type":"profile.bind","agentId":"main","modelAlias":"k2","time":1790739703600}"#).as_deref(), Some("k2"));
        assert_eq!(said("kimi", r#"{"type":"config.update","agentId":"main","modelAlias":"k3","time":1790739709600}"#).as_deref(), Some("k3"));
        assert_eq!(said("kimi", r#"{"type":"config.update","agentId":"main","modelAlias":"__kimi_env_model__"}"#), None, "its environment's, which `-m` can't name");
        assert_eq!(said("kimi", r#"{"type":"llm.request","agentId":"main","model":"kimi-k2","time":1790739703600}"#), None);
        assert_eq!(said("cursor", r#"{"role":"user","message":{"content":[]}}"#), None, "its record names no model");
    }

    #[test]
    fn claude_steps_through_bypass_only_when_started_with_it_allowed() {
        let claude = agent("claude").unwrap();
        let cycle = |args: &[&str]| claude.mode_cycle(&strings(args)).map(|(key, order)| (key, order.join(" ")));
        assert_eq!(cycle(&["--model", "haiku"]), Some(("\x1b[Z", "ask edits plan auto".into())));
        assert_eq!(cycle(&["--permission-mode", "bypassPermissions"]).unwrap().1, "ask edits plan bypass auto");
        assert_eq!(cycle(&["--allow-dangerously-skip-permissions"]).unwrap().1, "ask edits plan bypass auto");
        assert_eq!(cycle(&["--permission-mode", "plan"]).unwrap().1, "ask edits plan auto");
        assert_eq!(agent("codex").unwrap().mode_cycle(&[]), None);
    }
}
