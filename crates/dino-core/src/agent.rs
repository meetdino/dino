//! One adapter per agent dino works with: how it takes controls on its command line, how dino
//! wires it to the proxy and its status, where its conversations live, how to find one running,
//! and how to continue it. Everything that differs between agents goes through here, so an agent
//! is added in one file under `agent/`.

use std::path::{Path, PathBuf};

use crate::controls::Controls;
use crate::found::FoundSession;
use crate::history::Turn;
use crate::models::Catalog;
use crate::providers::Format;

mod claude;
pub mod codex;
mod codewhale;
mod hermes;
mod kimi;
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
    /// The API shapes it can talk to a provider in, best first: empty when it can't be pointed at one.
    fn provider_formats(&self) -> &'static [Format] {
        &[]
    }
    /// Env vars and arguments that run it on `model`, served at `url` (a dino proxy route) in
    /// `format`, one of its `provider_formats`. Its own settings and login stay as they are.
    fn provider_wiring(&self, _url: &str, _format: Format, _model: &str) -> Option<Wiring> {
        None
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
    /// Arguments that give it `prompt` to start on, staying open for more.
    fn prompt_args(&self, prompt: String) -> Vec<String> {
        vec![prompt]
    }
    /// Arguments that start conversation `session` (set when dino picks the id up front), or
    /// resume it when `restoring`: those before dino's other arguments, and those after.
    fn session_args(&self, session: &mut Option<String>, restoring: bool) -> (Vec<String>, Vec<String>);
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
    /// With `StatusSource::Polled`: whether conversation `session` is on a turn, as its store says
    /// now, in a process of it started at `since` (seconds).
    fn turn_now(&self, _session: &str, _since: u64) -> Option<bool> {
        None
    }
    /// With `StatusSource::Polled`, for agents whose store can't say they wait on the user: what
    /// its own dialog on `screen` asks of them, while it's on a turn.
    fn asking(&self, _screen: &str) -> Option<String> {
        None
    }
    /// For agents whose conversations aren't files: a page of `session_id`'s turns ending before
    /// position `before` (the end when `None`), in the agent's own positions.
    fn page(&self, _session_id: &str, _before: Option<u64>) -> Option<crate::history::Page> {
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
static HERMES: hermes::Hermes = hermes::Hermes { free: false };
static HERMES_FREE: hermes::Hermes = hermes::Hermes { free: true };
static CODEWHALE: codewhale::CodeWhale = codewhale::CodeWhale;

/// The agents dino works with, in the order they're listed and looked for.
pub fn all() -> [&'static dyn Agent; 7] {
    [&CLAUDE, &CODEX, &QWEN, &KIMI, &PI, &HERMES, &CODEWHALE]
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

/// A prompt an agent is started on goes on its command line (see `Agent::prompt_args`), where one
/// starting with `-` would be read as a flag (`--dangerously-skip-permissions`, `--settings=...`):
/// refuse it, since not every agent's command line honours `--`.
pub fn check_prompt(prompt: &str) -> anyhow::Result<()> {
    anyhow::ensure!(!prompt.trim_start().starts_with('-'), "a prompt to start an agent on can't begin with \"-\": it would be read as a command-line flag");
    Ok(())
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

    const URL: &str = "http://127.0.0.1:5000/s/7/local/ollama";

    fn env<'a>(w: &'a Wiring, k: &str) -> Option<&'a str> {
        w.0.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
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

    #[test]
    fn a_prompt_that_would_be_read_as_a_flag_is_refused() {
        for p in ["--dangerously-skip-permissions", "  --settings={\"hooks\":{}}", "\n\t-p", "-", "--dangerously-bypass-approvals-and-sandbox"] {
            assert!(check_prompt(p).is_err(), "{p:?}");
        }
        for p in ["fix the tests", "  fix -- the tests", "why does `-x` fail?", "", "   "] {
            assert!(check_prompt(p).is_ok(), "{p:?}");
        }
    }
}
