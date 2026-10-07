//! Local pass-through proxy. Agents point their base URL at
//! `http://127.0.0.1:<port>/k/<secret>/s/<session>/<provider>`; we forward to the real API untouched
//! (auth included) and observe usage, in-flight state and quota headers on the way back.
//! The secret is new each start and only dino's agents are given it: loopback is open to every
//! user on the Mac, and to web pages through DNS rebinding.
//! The `free` provider is different: dino itself picks a free model and translates (see `free`).
//! `or` is OpenRouter with the key dino holds for it (see `openrouter`), `siwc` the ChatGPT plan
//! through Sign in with ChatGPT (see `siwc`), `local/<runtime>` a model server on this Mac (see `local`),
//! `plan/<id>` a coding plan with the key the user pasted (see `plan`).

mod accounts;
mod catalog;
pub mod codex;
pub mod computer;
pub mod fallback;
mod free;
pub mod local;
mod openrouter;
pub mod plan;
mod siwc;
pub mod tasks;
mod upstream;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Response, StatusCode, header};
use axum::routing::{any, post};
use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::Value;

pub const PROVIDERS: &[(&str, &str)] = &[
    ("anthropic", "https://api.anthropic.com"),
    ("openai", "https://api.openai.com"),
    // Codex signed in with ChatGPT.
    ("chatgpt", "https://chatgpt.com/backend-api"),
];

/// Where `provider` is: `PROVIDERS`, or for Anthropic and ChatGPT in a debug build what
/// `DINO_ANTHROPIC_UPSTREAM` and `DINO_CHATGPT_UPSTREAM` name (a stand-in to try a spent Claude
/// account or a rejected Codex model against, e.g. one that answers some calls itself and passes
/// the rest on). Never in a release: it would hand the account's token to any URL.
fn provider_upstream(provider: &str) -> Option<&'static str> {
    use std::sync::OnceLock;
    static ANTHROPIC: OnceLock<Option<String>> = OnceLock::new();
    static CHATGPT: OnceLock<Option<String>> = OnceLock::new();
    let (cell, var) = match provider {
        "anthropic" if cfg!(debug_assertions) => (&ANTHROPIC, "DINO_ANTHROPIC_UPSTREAM"),
        "chatgpt" if cfg!(debug_assertions) => (&CHATGPT, "DINO_CHATGPT_UPSTREAM"),
        _ => return PROVIDERS.iter().find(|(p, _)| *p == provider).map(|&(_, u)| u),
    };
    if let Some(u) = cell.get_or_init(|| std::env::var(var).ok().filter(|u| !u.is_empty())).as_deref() {
        return Some(u);
    }
    PROVIDERS.iter().find(|(p, _)| *p == provider).map(|&(_, u)| u)
}

#[derive(Clone, Debug, Default)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

/// What one route answered for a session: the agent's own account, or a route it fell back to.
#[derive(Clone, Debug, Default)]
pub struct RouteUsage {
    /// Where the proxy serves it: `anthropic`, `plan/zai`, `local/ollama`.
    pub route: String,
    /// As people know it: "Claude", "GLM Coding Plan".
    pub name: String,
    pub usage: Usage,
}

/// The route a call went to, for metering: its path and its name.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RouteTag {
    pub path: String,
    pub name: String,
}

impl Usage {
    pub(crate) fn add(&mut self, o: &Usage) {
        self.input += o.input;
        self.output += o.output;
        self.cache_read += o.cache_read;
        self.cache_write += o.cache_write;
    }

    /// Everything the model read, cached or not.
    pub fn total_input(&self) -> u64 {
        self.input + self.cache_read + self.cache_write
    }
}

/// What the agent itself reported via hooks (Claude Code HTTP hooks POST to `/s/<session>/hook`).
#[derive(Clone, Debug, PartialEq)]
pub enum Activity {
    Working,
    /// Blocked on the user, e.g. a permission prompt for this tool.
    NeedsPermission(String),
    /// Finished its turn.
    Done,
}

#[derive(Clone, Debug, Default)]
pub struct SessionStats {
    pub activity: Option<Activity>,
    /// Counts the questions its hooks asked (a permission prompt), so the same words asked again
    /// are told apart from the last time.
    pub questions: u64,
    pub requests: u64,
    pub in_flight: u32,
    pub errors: u64,
    pub usage: Usage,
    pub last_model: Option<String>,
    /// Why the session's last turn failed, as shown. Without hooks: the last model call's failure,
    /// cleared by the next one that succeeds. With hooks the agent says whether its turn failed,
    /// so a side call (a title, a summary) failing doesn't mark a turn that went fine.
    pub last_error: Option<String>,
    /// The last model call's failure, whether or not it's shown.
    pub call_error: Option<String>,
    /// A coding plan's limit (its window spent, its balance gone) turned the last model call
    /// down: shown even when the agent ends its turn as if it went fine (Claude Code's turn ends
    /// with Stop after it shows the API error), until a call goes through again.
    pub limit_error: Option<String>,
    /// The agent reports its turns through hooks (Claude).
    pub hooked: bool,
    /// The agent's own record says where its turns are (Codex's rollout): no guessing from quiet.
    pub tracked: bool,
    /// The permission mode the agent last said it's in, in its own words (Claude's hooks).
    pub agent_mode: Option<String>,
    /// The model the agent last said its session is on, as its command line names it: Claude's
    /// PostModelSwitch hook, or its own record (`report_model`). A switch made in the agent itself
    /// (`/model`) included, which the flags it was started with don't say.
    pub agent_model: Option<String>,
    /// Router tier for free-tier sessions ("fast", "code", "reason").
    pub tier: Option<String>,
    /// Which classifier made the last routing decision ("jev" or "llm").
    pub classifier: Option<String>,
    pub last_request: Option<Instant>,
    /// Per model, what its last call read (cached tokens included): how full its context is.
    /// Per model because agents make small side calls (titles, summaries) on other models.
    pub context: HashMap<String, u64>,
    /// The context window as the agent itself reports it (Claude's statusline), which beats `context`.
    pub reported_context: Option<ReportedContext>,
    /// Subagents the agent started, as its hooks reported them (Claude's Agent tool).
    pub subagents: Vec<Subagent>,
    /// Agent tool calls not answered yet: (tool_use_id, description, subagent_type).
    pending_agents: Vec<(String, Option<String>, Option<String>)>,
    /// The agent's task list (Claude's TaskCreate/TaskUpdate, or the older TodoWrite).
    pub todos: Vec<tasks::Todo>,
    /// Shell commands and monitors it runs in the background.
    pub background: Vec<tasks::Background>,
    /// What the agent said still ran when its last turn ended (ids in `subagents` and
    /// `background`): it isn't done while that work is.
    pub(crate) waiting_on: Vec<String>,
    /// Its use of the Mac or a browser, from the tools it calls (see `computer`).
    pub computer: Option<computer::ComputerUse>,
    /// Answered by a route it fell back to, while the one it uses is spent (see `fallback`).
    pub fallback: Option<fallback::OnFallback>,
    /// Its Codex model the ChatGPT backend rejects, and the one answering instead (see `codex`).
    pub substitute: Option<codex::Substitute>,
    /// What each route answered, the agent's own account and the routes it fell back to.
    pub by_route: Vec<RouteUsage>,
    /// The route its model calls go to, by key (see `fallback::route_key`), and its name.
    pub primary: Option<(String, String)>,
    /// Server errors from it in a row, and when the last came (Unix seconds).
    pub(crate) outages: (u32, u64),
    /// A fallback answered part of the conversation: what it thought stays behind when the first
    /// route takes over again (see `fallback::for_primary`).
    pub(crate) mixed: bool,
}

impl SessionStats {
    /// How many subagents and commands it ended its turn on still run. Monitors don't count:
    /// they watch for something rather than work towards an end.
    /// Its last turn ended while background task `id` still ran.
    pub fn waits_on(&self, id: &str) -> bool {
        self.waiting_on.iter().any(|w| w == id)
    }

    pub fn waiting(&self) -> (usize, usize) {
        let on = |id: &str| self.waiting_on.iter().any(|w| w == id);
        let agents = self.subagents.iter().filter(|a| a.running && on(&a.id)).count();
        let commands = self.background.iter().filter(|b| b.running && b.kind != "monitor" && on(&b.id)).count();
        (agents, commands)
    }

    /// A model call failed: shown right away unless the agent reports its turns itself.
    fn call_failed(&mut self, msg: String) {
        if !self.hooked {
            self.last_error = Some(msg.clone());
        }
        self.call_error = Some(msg);
    }

    /// The agent says how its turn went: only a failed turn shows an error.
    fn turn_hook(&mut self, event: &str, v: &Value) {
        self.hooked = true;
        if let Some(m) = v["permission_mode"].as_str() {
            self.agent_mode = Some(m.into());
        }
        // Its session's model changed (`/model`, its picker, its own fallback, a resume restoring
        // it): what it's on from now on, as it names it (`claude-opus-5-5[1m]`).
        if event == "PostModelSwitch"
            && let Some(m) = v["to_model"].as_str().filter(|m| !m.is_empty())
        {
            self.agent_model = Some(m.into());
        }
        match event {
            "UserPromptSubmit" => self.last_error = None,
            "Stop" => self.last_error = self.limit_error.clone(),
            // What the failed call said; the hook's own words when dino didn't see it.
            "StopFailure" => {
                let said = v["error_details"].as_str().or(v["error"].as_str()).map(String::from);
                self.last_error = self.call_error.clone().or(said).or_else(|| Some("The turn failed".into()));
            }
            _ => {}
        }
    }

    /// Count `u` toward the session, and toward the route that answered it.
    pub(crate) fn metered(&mut self, route: Option<&RouteTag>, u: &Usage) {
        self.usage.add(u);
        let Some(r) = route else { return };
        match self.by_route.iter_mut().find(|x| x.route == r.path) {
            Some(x) => {
                x.usage.add(u);
                // A live call knows the route's name best (a seeded one may only have its path).
                x.name.clone_from(&r.name);
            }
            None => self.by_route.push(RouteUsage { route: r.path.clone(), name: r.name.clone(), usage: u.clone() }),
        }
    }

    /// The conversation's context use: the biggest per-model one, since side calls are small.
    pub fn context(&self) -> Option<(&str, u64)> {
        self.context.iter().max_by_key(|(_, n)| **n).map(|(m, n)| (m.as_str(), *n))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReportedContext {
    /// Tokens in the window; `None` until the next answer after starting or `/compact`.
    pub used: Option<u64>,
    pub window: u64,
}

impl ReportedContext {
    /// From the JSON Claude Code gives its statusline command: `context_window.context_window_size`,
    /// and what the last call read (`current_usage`) or, failing that, `used_percentage`. Both are
    /// null before the first answer and right after `/compact` (Claude Code 2.1.284): the window is empty.
    pub fn from_statusline(v: &Value) -> Option<Self> {
        let cw = &v["context_window"];
        let window = cw["context_window_size"].as_u64().filter(|w| *w > 0)?;
        let usage = &cw["current_usage"];
        let used = if usage.is_object() {
            Some(["input_tokens", "cache_creation_input_tokens", "cache_read_input_tokens"].iter().filter_map(|k| usage[*k].as_u64()).sum())
        } else {
            cw["used_percentage"].as_f64().map(|p| (p.clamp(0.0, 100.0) * window as f64 / 100.0).round() as u64)
        };
        Some(Self { used, window })
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Subagent {
    pub id: String,
    pub agent_type: Option<String>,
    /// The task it was given.
    pub description: Option<String>,
    /// Where it runs: its own worktree when started with `isolation: "worktree"`.
    pub cwd: Option<String>,
    pub running: bool,
    /// Unix seconds; 0 until it starts.
    pub started: u64,
    pub finished: Option<u64>,
    /// Where a background agent's transcript goes, as the Agent tool said.
    pub output: Option<String>,
}

/// One rolling subscription window, e.g. Claude's 5h or 7d.
#[derive(Clone, Debug)]
pub struct Window {
    pub utilization: f32,
    pub resets_at: Option<u64>,
    pub status: Option<String>,
}

impl Window {
    pub fn resets_in_secs(&self) -> Option<u64> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
        self.resets_at.map(|t| t.saturating_sub(now))
    }
}

#[derive(Clone, Debug, Default)]
pub struct Quota {
    /// Window name ("5h", "7d") → state, as last reported by the provider.
    pub windows: Vec<(String, Window)>,
}

/// How a model call ended, for usage statistics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CallStatus {
    #[default]
    Ok,
    Error,
    /// The route's limit turned it down: a plan's window or balance, a 429.
    Limit,
}

/// One model call the proxy carried, kept until dinod takes it (`Stats::take_calls`) for usage
/// statistics. Recorded once per call, as it ends: nothing per chunk.
#[derive(Clone, Debug, Default)]
pub struct Call {
    /// When it came in, in ms since the epoch.
    pub at_ms: i64,
    pub session: String,
    /// The route it went out on, as its URL names it: "anthropic", "or", "local/ollama", "plan/<id>"…
    pub route: String,
    /// The model the answer named, or else the one the request asked for.
    pub model: Option<String>,
    pub usage: Usage,
    /// Until the answer's first byte, for streamed answers.
    pub ttft_ms: Option<u32>,
    /// Until its last byte.
    pub duration_ms: Option<u32>,
    pub status: CallStatus,
    /// Answered by a fallback: the route it stood in for (`route` is the one that answered);
    /// `None` when the session's own route answered.
    pub fallback: Option<String>,
    /// What the route said it cost (OpenRouter's `usage.cost`).
    pub cost: Option<f64>,
}

/// A refusal as the provider sent it: given to the agent again when its account is the one back
/// first (see `Stats::refusals`).
#[derive(Clone, Debug)]
struct Refusal {
    status: StatusCode,
    headers: HeaderMap,
    text: Bytes,
}

/// Calls kept for dinod at most; past that (dinod not taking them) the newest are dropped.
const MAX_CALLS: usize = 100_000;

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

fn ms(d: std::time::Duration) -> u32 {
    d.as_millis().min(u32::MAX as u128) as u32
}

#[derive(Default)]
pub struct Stats {
    pub sessions: Mutex<HashMap<String, SessionStats>>,
    pub quotas: Mutex<HashMap<String, Quota>>,
    /// A coding plan's last refusal (its key, its balance, its limit), by plan id, until a call
    /// to it goes through again: shown with the plan in Settings → Models & Providers.
    pub plan_errors: Mutex<HashMap<String, String>>,
    /// Routes found spent (a window, a balance) or down, by key (see `fallback::route_key`),
    /// until they're tried again.
    pub limited: Mutex<fallback::LimitedRoutes>,
    /// Each Claude account's last subscription-limit refusal, by key (see `accounts::key`): with
    /// every account spent, the agent is given the one of the account back first.
    refusals: Mutex<HashMap<String, Refusal>>,
    /// Model calls not yet taken by dinod for usage statistics.
    calls: Mutex<Vec<Call>>,
}

impl Stats {
    /// The model calls that ended since the last take.
    pub fn take_calls(&self) -> Vec<Call> {
        std::mem::take(&mut *self.calls.lock().unwrap())
    }

    pub(crate) fn record_call(&self, call: Call) {
        let mut calls = self.calls.lock().unwrap();
        if calls.len() < MAX_CALLS {
            calls.push(call);
        }
    }

    /// What session `id` used before this dinod started (from usage statistics), so its tokens and
    /// its budget carry on across a restart. Only into a session that hasn't been metered yet.
    pub fn seed(&self, id: &str, routes: &[(String, Usage)]) {
        self.update(id, |s| {
            if s.usage.total_input() + s.usage.output > 0 {
                return;
            }
            for (route, u) in routes {
                let tag = (!route.is_empty()).then(|| RouteTag { path: route.clone(), name: fallback::seed_name(route) });
                s.metered(tag.as_ref(), u);
            }
        });
    }

    pub fn session(&self, id: &str) -> SessionStats {
        self.sessions.lock().unwrap().get(id).cloned().unwrap_or_default()
    }

    /// The agent's turn is over although no hook said so (Esc while a tool runs fires none).
    pub fn end_turn(&self, id: &str) {
        self.update(id, |s| {
            if s.activity == Some(Activity::Working) && s.in_flight == 0 {
                s.activity = Some(Activity::Done);
                s.calls_over();
            }
        });
    }

    /// The question `asked` was answered or dismissed although no hook said so (Esc on a permission
    /// prompt fires none): the turn is over. A newer question since is left alone.
    pub fn end_question(&self, id: &str, asked: &str) {
        self.update(id, |s| {
            if matches!(&s.activity, Some(Activity::NeedsPermission(m)) if m == asked) {
                s.activity = Some(Activity::Done);
            }
        });
    }

    /// The question `asked` was answered and the agent went on (an approved tool runs), although
    /// no hook said so: it's working. A newer question since is left alone.
    pub fn question_answered(&self, id: &str, asked: &str) {
        self.update(id, |s| {
            if matches!(&s.activity, Some(Activity::NeedsPermission(m)) if m == asked) {
                s.activity = Some(Activity::Working);
            }
        });
    }

    /// Where the agent's turn is, as its own record says (Codex's rollout and notices): what the
    /// hooks say for Claude.
    pub fn report(&self, id: &str, activity: Activity) {
        self.update(id, |s| {
            s.tracked = true;
            // A turn that ended has no tool call out, whatever its record said last.
            if activity == Activity::Done {
                s.calls_over();
            }
            s.activity = Some(activity);
        });
    }

    /// The model the agent's own record says it's on now (Codex's rollout): see `agent_model`.
    pub fn report_model(&self, id: &str, model: String) {
        self.update(id, |s| s.agent_model = Some(model));
    }

    /// dino wired the agent's hooks: it reports its own turns, so only a failed turn is an error,
    /// not any call that failed (Claude's quota probe as it resumes, which can be rate limited
    /// when dinod restarts and resumes everything at once). Known before its first hook, which
    /// for a session resumed idle may never come.
    pub fn reports_turns(&self, id: &str) {
        self.update(id, |s| s.hooked = true);
    }

    /// Forget context use, e.g. when the agent restarts on another model.
    /// The agent is starting over: what it said about itself no longer holds.
    pub fn restarted(&self, id: &str) {
        self.update(id, |s| {
            s.agent_mode = None;
            s.agent_model = None;
        });
    }

    /// The agent said how full its context window is, other than to its statusline (OpenCode's server).
    pub fn report_context(&self, id: &str, context: ReportedContext) {
        self.update(id, |s| s.reported_context = Some(context));
    }

    pub fn reset_context(&self, id: &str) {
        self.update(id, |s| {
            s.context.clear();
            s.reported_context = None;
        });
    }

    /// A fallback answered part of session `id`'s conversation.
    pub(crate) fn mixed(&self, id: &str) -> bool {
        self.sessions.lock().unwrap().get(id).is_some_and(|s| s.mixed)
    }

    /// Route `key` is spent or down, and isn't tried again yet.
    pub fn limited(&self, key: &str) -> Option<fallback::Limited> {
        let now = fallback::now();
        self.limited.lock().unwrap().get(key).filter(|l| l.retry_at > now).cloned()
    }

    /// Every route found spent or down, whether or not it's time to try it again.
    pub fn limited_routes(&self) -> Vec<(String, fallback::Limited)> {
        self.limited.lock().unwrap().iter().map(|(k, l)| (k.clone(), l.clone())).collect()
    }

    /// Of the accounts `keys`, when every one is spent: the one back first, and the refusal it
    /// last gave, its `retry-after` counted down to now. `None` while any isn't spent.
    fn first_back(&self, keys: &[String]) -> Option<(String, Refusal)> {
        let mut spent = vec![];
        for k in keys {
            let l = self.limited(k)?;
            spent.push((l.resets_at.unwrap_or(l.retry_at), k));
        }
        let (at, key) = spent.into_iter().min()?;
        let mut r = self.refusals.lock().unwrap().get(key)?.clone();
        if r.headers.contains_key("retry-after") {
            let left = at.saturating_sub(fallback::now()).max(1);
            r.headers.insert("retry-after", HeaderValue::from(left));
        }
        Some((key.clone(), r))
    }

    /// Route `key` (`name`) said it's spent.
    pub(crate) fn mark_limited(&self, key: &str, name: &str, t: &fallback::Trigger) -> fallback::Limited {
        let l = fallback::Limited { name: name.into(), kind: t.kind, said: t.said.clone(), resets_at: t.resets_at, retry_at: fallback::retry_at(t) };
        log(format_args!("{key} ({name}) is spent ({}) until {}: {}", t.kind.word(), l.retry_at, t.said));
        self.limited.lock().unwrap().insert(key.to_string(), l.clone());
        l
    }

    /// Route `key` answered: it's neither spent nor down.
    pub(crate) fn not_limited(&self, key: &str) {
        let mut limited = self.limited.lock().unwrap();
        if limited.remove(key).is_some() {
            log(format_args!("{key} answers again"));
        }
    }

    /// Another server error from session `id`'s route `key`: down, once they come in a row.
    pub(crate) fn outage(&self, id: &str, key: &str, name: &str, said: String) -> Option<fallback::Limited> {
        let now = fallback::now();
        let mut n = 0;
        self.update(id, |s| {
            let (count, last) = s.outages;
            s.outages = (if now.saturating_sub(last) <= fallback::OUTAGE_WINDOW { count + 1 } else { 1 }, now);
            n = s.outages.0;
        });
        (n >= fallback::OUTAGE_AFTER).then(|| self.mark_limited(key, name, &fallback::Trigger { kind: fallback::Kind::Outage, resets_at: None, said }))
    }

    /// Why plan `id` last turned a call down, if it still does.
    pub fn plan_error(&self, id: &str) -> Option<String> {
        self.plan_errors.lock().unwrap().get(id).cloned()
    }

    pub fn quota(&self, provider: &str) -> Option<Quota> {
        self.quotas.lock().unwrap().get(provider).cloned()
    }

    /// The agent a shell ran has exited: what it said of itself (its turn, tasks, mode, error,
    /// context), through its hooks or its own record, goes with it, so the shell is a plain shell
    /// again. What it cost stays counted. True when it had said anything.
    pub fn agent_left(&self, id: &str) -> bool {
        let mut sessions = self.sessions.lock().unwrap();
        let Some(s) = sessions.get_mut(id) else { return false };
        if !s.hooked && !s.tracked && s.activity.is_none() {
            return false;
        }
        s.activity = None;
        s.hooked = false;
        s.tracked = false;
        s.context.clear();
        s.agent_mode = None;
        s.agent_model = None;
        s.last_error = None;
        s.call_error = None;
        s.limit_error = None;
        s.reported_context = None;
        s.subagents.clear();
        s.pending_agents.clear();
        s.todos.clear();
        s.background.clear();
        s.waiting_on.clear();
        s.computer = None;
        s.fallback = None;
        s.substitute = None;
        s.outages = (0, 0);
        true
    }

    pub(crate) fn update(&self, id: &str, f: impl FnOnce(&mut SessionStats)) {
        f(self.sessions.lock().unwrap().entry(id.to_string()).or_default());
    }
}

pub struct Proxy {
    pub port: u16,
    /// Where agents reach the proxy, `http://127.0.0.1:<port>/k/<secret>`: without the secret
    /// nothing is served.
    pub root: String,
    pub stats: Arc<Stats>,
    keys: Arc<RwLock<HashMap<String, String>>>,
    budget: Arc<AtomicU64>,
    /// The same secret, for agents that send it in the `KEY_HEADER` header (see `header_base_url`).
    secret: String,
    /// Hooks only, for sessions on other machines (see `remote_hook_url`).
    pub remote_port: u16,
    /// A remote session's token → its session id.
    remote: Arc<RwLock<HashMap<String, String>>>,
    state: AppState,
    runtime: tokio::runtime::Handle,
}

/// The hook-only listener's state: which tokens stand for which sessions.
#[derive(Clone)]
struct RemoteState {
    app: AppState,
    tokens: Arc<RwLock<HashMap<String, String>>>,
}

impl Proxy {
    /// Start on a random localhost port, on its own runtime thread.
    /// `keys` are provider credentials dino itself uses (e.g. `NVIDIA_API_KEY` for the free tier).
    pub fn start(keys: HashMap<String, String>) -> anyhow::Result<Self> {
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        std_listener.set_nonblocking(true)?;
        let port = std_listener.local_addr()?.port();
        let secret = random_secret()?;
        let root = format!("http://127.0.0.1:{port}/k/{secret}");
        let remote_listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        remote_listener.set_nonblocking(true)?;
        let remote_port = remote_listener.local_addr()?.port();
        let remote: Arc<RwLock<HashMap<String, String>>> = Arc::default();
        let stats = Arc::new(Stats::default());
        let keys = Arc::new(RwLock::new(keys));
        let budget = Arc::new(AtomicU64::new(0));
        let state = AppState {
            free_models: Arc::default(),
            free_wake: Arc::default(),
            stats: stats.clone(),
            upstream: Arc::default(),
            router: Arc::default(),
            keys: keys.clone(),
            plans: Arc::default(),
            chains: Arc::default(),
            budget: budget.clone(),
            substitutes: Arc::default(),
            port,
            secret: secret.clone().into(),
        };

        let remote_tokens = remote.clone();
        let kept = state.clone();
        let (handle_tx, handle_rx) = std::sync::mpsc::channel();
        std::thread::Builder::new().name("dino-proxy".into()).spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
            let _ = handle_tx.send(rt.handle().clone());
            rt.block_on(async move {
                // What an ssh tunnel reaches: hooks, by a token only that session knows. The
                // remote port is open to everyone on that machine, so no API routes and no ids.
                let remote_app = axum::Router::new()
                    .route("/r/{token}/hook", post(remote_hook))
                    .with_state(RemoteState { app: state.clone(), tokens: remote_tokens });
                let remote_listener = tokio::net::TcpListener::from_std(remote_listener).unwrap();
                tokio::spawn(async move { axum::serve(remote_listener, remote_app).await });
                let listener = tokio::net::TcpListener::from_std(std_listener).unwrap();
                let app = axum::Router::new().route("/k/{key}/s/{session}/{provider}/{*rest}", any(forward))
                    .route("/h/s/{session}/{provider}/{*rest}", any(forward_keyed))
                    .route("/k/{key}/s/{session}/hook", post(hook))
                    .fallback(|req: Request| async move {
                        // Not the secret, should the path carry it.
                        let path: Vec<&str> = req.uri().path().split('/').enumerate().map(|(i, s)| if i == 2 { "…" } else { s }).collect();
                        log(format_args!("unrouted {} {}", req.method(), path.join("/")));
                        StatusCode::NOT_FOUND
                    })
                    .with_state(state);
                let _ = axum::serve(listener, app).await;
            });
        })?;
        let runtime = handle_rx.recv()?;
        Ok(Self { port, root, secret, stats, keys, budget, remote_port, remote, state: kept, runtime })
    }

    /// Find the free tier's models and keep them current, remembering what was learned in `cache`.
    pub fn keep_free_models(&self, cache: std::path::PathBuf) {
        self.runtime.spawn(catalog::keep_fresh(self.state.clone(), cache));
    }

    /// Use these keys from the next request on.
    pub fn set_keys(&self, keys: HashMap<String, String>) {
        *self.keys.write().unwrap() = keys;
        self.state.free_wake.notify_one();
    }

    /// Route `key` is spent (or down) and nothing of the user's own stands in: for Claude Code's
    /// subscription, none of their other Claude accounts answers instead (see `accounts`).
    pub fn spent(&self, key: &str) -> Option<fallback::Limited> {
        let l = self.stats.limited(key)?;
        let subscription = key.starts_with("anthropic#") && l.name == fallback::CLAUDE;
        let spare = subscription && accounts::others(&self.keys.read().unwrap()).iter().any(|(_, t)| self.stats.limited(&accounts::key(t)).is_none());
        (!spare).then_some(l)
    }

    /// The user's Claude accounts as they stand: Claude Code's own (signed in with a
    /// subscription), spent until when, if it was found so; and each other account's, by token.
    pub fn claude_accounts(&self, tokens: &[&str]) -> (Option<fallback::Limited>, Vec<Option<fallback::Limited>>) {
        let own = self
            .stats
            .limited_routes()
            .into_iter()
            .filter(|(k, l)| k.starts_with("anthropic#") && !accounts::is_key(k) && l.name == fallback::CLAUDE)
            .filter_map(|(k, _)| self.stats.limited(&k))
            .max_by_key(|l| l.retry_at);
        (own, tokens.iter().map(|t| self.stats.limited(&accounts::key(t))).collect())
    }

    /// The user's Claude accounts as calls find them, for the state: each one's number (1 for
    /// Claude Code's own sign-in), whether it's spent until when, and its windows as Anthropic last
    /// reported them on a call it signed. `None` with no other account. From what the proxy holds
    /// already: no key store read, as the state is looked at many times a second.
    pub fn claude_accounts_now(&self) -> Option<Vec<(u32, Option<fallback::Limited>, Option<Quota>)>> {
        let others = accounts::others(&self.keys.read().unwrap());
        if others.is_empty() {
            return None;
        }
        let tokens: Vec<&str> = others.iter().map(|(_, t)| t.as_str()).collect();
        let (own, spent) = self.claude_accounts(&tokens);
        let mut out = vec![(1, own, self.stats.quota("anthropic"))];
        out.extend(others.iter().zip(spent).map(|((n, t), l)| (*n, l, self.stats.quota(&accounts::key(t)))));
        Some(out)
    }

    /// The coding plans to serve at `plan/<id>`, from the next request on.
    pub fn set_plans(&self, plans: HashMap<String, plan::Plan>) {
        let mut errors = self.stats.plan_errors.lock().unwrap();
        // A key changed: what the old one was told no longer holds.
        let old = self.state.plans.read().unwrap().clone();
        errors.retain(|id, _| old.get(id).zip(plans.get(id)).is_some_and(|(a, b)| a == b));
        *self.state.plans.write().unwrap() = plans;
    }

    /// What session `session` falls back to when its route is spent, from its next call on; `None`
    /// (or no steps) for nothing.
    pub fn set_fallback(&self, session: &str, chain: Option<fallback::Chain>) {
        let mut chains = self.state.chains.write().unwrap();
        match chain.filter(|c| !c.steps.is_empty()) {
            Some(c) => {
                chains.insert(session.to_string(), Arc::new(c));
            }
            None => {
                chains.remove(session);
            }
        }
    }

    /// Serve the free tier (Settings → Experimental). Off, its requests are refused before anything
    /// leaves this Mac, and its model list isn't refreshed.
    pub fn set_free_models(&self, on: bool) {
        self.state.free_models.store(on, Ordering::Relaxed);
        if on {
            self.state.free_wake.notify_one();
        }
    }

    pub fn free_models(&self) -> bool {
        self.state.free_models.load(Ordering::Relaxed)
    }

    /// Most tokens one session may use before its model calls are refused; 0 means no limit.
    pub fn set_budget(&self, tokens: u64) {
        self.budget.store(tokens, Ordering::Relaxed);
    }

    /// The hook URL for a session on another machine, as that machine sees it: `remote_port` there
    /// is forwarded to `self.remote_port` here. `token` stands for the session; pick an
    /// unguessable one. Replaces the session's earlier token.
    pub fn remote_hook_url(&self, session: &str, token: &str, remote_port: u16) -> String {
        let mut tokens = self.remote.write().unwrap();
        tokens.retain(|_, s| s != session);
        tokens.insert(token.to_string(), session.to_string());
        format!("http://127.0.0.1:{remote_port}/r/{token}/hook")
    }

    /// Stop taking hooks for a remote session.
    pub fn forget_remote(&self, session: &str) {
        self.remote.write().unwrap().retain(|_, s| s != session);
    }

    /// Base URL an agent should use for `provider`, attributed to `session`. It carries the
    /// secret: give it to agents through their environment or a private file, never their command
    /// line, which other users of the Mac can read.
    pub fn base_url(&self, session: &str, provider: &str) -> String {
        format!("{}/s/{session}/{provider}", self.root)
    }

    /// The same without the secret, for agents that can only be pointed at a URL on their command
    /// line: they send the secret in the `KEY_HEADER` header, from an environment variable.
    pub fn header_base_url(&self, session: &str, provider: &str) -> String {
        format!("http://127.0.0.1:{}/h/s/{session}/{provider}", self.port)
    }

    /// What goes in `KEY_HEADER`.
    pub fn secret(&self) -> &str {
        &self.secret
    }
}

/// The header an agent can carry the secret in instead of its path (see `header_base_url`).
/// dino never passes it on.
pub const KEY_HEADER: &str = "x-dino-key";

/// 128 random bits as hex, from `/dev/urandom` like the remote hook tokens (`new_uuid`).
fn random_secret() -> std::io::Result<String> {
    let mut b = [0u8; 16];
    std::io::Read::read_exact(&mut std::fs::File::open("/dev/urandom")?, &mut b)?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// Only dino's own agents get in: the path carries the secret they were given, the Host is this
/// listener (not some name rebound to it), and no browser sent it (agents send no Origin).
fn admitted(secret: &str, port: u16, key: &str, headers: &HeaderMap) -> bool {
    let host = headers.get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or_default();
    let local = [format!("127.0.0.1:{port}"), format!("localhost:{port}")].iter().any(|l| l.eq_ignore_ascii_case(host));
    let ok = same(key.as_bytes(), secret.as_bytes()) && local && !headers.contains_key(header::ORIGIN);
    if !ok {
        log(format_args!("refused: host {host:?}, origin {:?}", headers.get(header::ORIGIN)));
    }
    ok
}

/// Equal, compared without stopping at the first difference: timing says nothing of a guess.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

/// `rest` as it goes upstream, without a leading or trailing `/`; `None` if it could reach past
/// the API it names once a URL parser has it: `.`, `..` and empty segments, and what would be
/// read as a separator or decoded again (`Path` has decoded it once already).
fn clean_path(rest: &str) -> Option<&str> {
    let rest = rest.trim_matches('/');
    let segments = rest.split('/').all(|s| !matches!(s, "" | "." | ".."));
    let chars = rest.bytes().all(|b| b.is_ascii_graphic() && !matches!(b, b'%' | b'\\' | b'?' | b'#'));
    (segments && chars).then_some(rest)
}

#[derive(Clone)]
pub(crate) struct AppState {
    /// The free tier is turned on (see `Proxy::set_free_models`).
    free_models: Arc<AtomicBool>,
    /// Wakes the free tier's model look (`catalog::keep_fresh`) when it's turned on or a key comes.
    free_wake: Arc<tokio::sync::Notify>,
    stats: Arc<Stats>,
    upstream: Arc<upstream::Upstream>,
    router: Arc<dino_router::Router>,
    keys: Arc<RwLock<HashMap<String, String>>>,
    /// Coding plans, by id (see `plan`).
    plans: Arc<RwLock<plan::Plans>>,
    /// What each session falls back to, by session id (see `fallback`).
    chains: Arc<RwLock<HashMap<String, Arc<fallback::Chain>>>>,
    /// The session token budget; 0 means none.
    budget: Arc<AtomicU64>,
    /// Codex models the backend rejected, and the one that answered instead.
    substitutes: Arc<Mutex<HashMap<String, codex::Substitute>>>,
    /// The listener's port and the secret its paths must carry (see `admitted`).
    port: u16,
    secret: Arc<str>,
}

impl AppState {
    /// The client for hosted APIs (the free tier's provider, its catalog).
    fn client(&self) -> reqwest::Client {
        self.upstream.client(false)
    }
}

/// Most of a request (or an upstream answer) held at once. Generous: requests carry images.
const MAX_BODY: usize = 64 << 20;

/// Headers we must not copy between the two connections.
fn hop_by_hop(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "host" | "connection" | "keep-alive" | "transfer-encoding" | "upgrade" | "proxy-connection" | "te" | "trailer"
            | "content-length" | "accept-encoding" | KEY_HEADER
    )
}

async fn forward_keyed(State(st): State<AppState>, Path((session, provider, rest)): Path<(String, String, String)>, req: Request) -> Response<Body> {
    let key = req.headers().get(KEY_HEADER).and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
    forward(State(st), Path((key, session, provider, rest)), req).await
}

async fn forward(
    State(st): State<AppState>,
    Path((key, session, provider, rest)): Path<(String, String, String, String)>,
    req: Request,
) -> Response<Body> {
    let (started, at_ms) = (Instant::now(), now_ms());
    if !admitted(&st.secret, st.port, &key, req.headers()) {
        return error(StatusCode::FORBIDDEN, "dino proxy: not for you".into());
    }
    // Checked for every provider: `siwc::allowed` and `is_model_call` see the path that goes out.
    let mut rest = match clean_path(&rest) {
        Some(r) => r.to_string(),
        None => return error(StatusCode::BAD_REQUEST, format!("dino proxy: bad path /{rest}")),
    };
    // A model server on this Mac: `local/<runtime>/…`.
    let mut runtime = None;
    if provider == local::PROVIDER {
        let Some((id, r)) = rest.split_once('/').map(|(i, r)| (i.to_string(), r.to_string())) else {
            return error(StatusCode::NOT_FOUND, "which model server? local/<runtime>/…".into());
        };
        let Some((name, base)) = local::runtime(&id) else { return error(StatusCode::NOT_FOUND, format!("dino doesn't know a model server called {id}")) };
        rest = r;
        runtime = Some((id, name, base));
    }
    // A coding plan: `plan/<id>/…`.
    let mut coding_plan = None;
    if provider == plan::PROVIDER {
        let Some((id, r)) = rest.split_once('/').map(|(i, r)| (i.to_string(), r.to_string())) else {
            return error(StatusCode::NOT_FOUND, "which coding plan? plan/<id>/…".into());
        };
        let Some(p) = st.plans.read().unwrap().get(&id).cloned() else {
            return error(StatusCode::UNAUTHORIZED, format!("the coding plan {id} isn't connected: add its key in dino's Settings → Models & Providers"));
        };
        rest = r;
        coding_plan = Some((id, p));
    }
    // Only model calls count toward activity; ignore e.g. token counting and telemetry.
    let is_model_call = rest.ends_with("messages") || rest.ends_with("chat/completions") || rest.ends_with("responses");
    // The route, as statistics name it.
    let route = match (&runtime, &coding_plan) {
        (Some((id, ..)), _) => format!("{provider}/{id}"),
        (_, Some((id, _))) => format!("{provider}/{id}"),
        _ => provider.clone(),
    };
    if is_model_call && let Some(resp) = over_budget(&st, &session, &provider) {
        return resp;
    }
    if provider == "free" {
        let Ok(body) = axum::body::to_bytes(req.into_body(), MAX_BODY).await else {
            return error(StatusCode::BAD_REQUEST, "unreadable body, or over 64 MiB".into());
        };
        return free::handle(st, session, &rest, body).await;
    }
    // Providers dino signs in to (OpenRouter, the ChatGPT plan) go out with dino's credentials,
    // not the agent's: (what to add, which of the agent's to drop, where).
    type Hosted = (Vec<(&'static str, String)>, fn(&str) -> bool, String);
    let hosted: Option<Hosted> = {
        let keys = st.keys.read().unwrap();
        match provider.as_str() {
            openrouter::PROVIDER => match openrouter::headers(&keys) {
                Some(h) => Some((h.to_vec(), openrouter::is_credential, openrouter::upstream().into())),
                None => return error(StatusCode::UNAUTHORIZED, "OpenRouter isn't connected: connect it in dino's Settings → Providers".into()),
            },
            siwc::PROVIDER if !siwc::allowed(&rest) => return error(StatusCode::NOT_FOUND, "the ChatGPT plan only takes the Responses API (v1/responses)".into()),
            siwc::PROVIDER => match siwc::headers(&keys) {
                Ok(h) => Some((h.to_vec(), siwc::is_credential, siwc::upstream().into())),
                Err(why) => return error(StatusCode::UNAUTHORIZED, why.into()),
            },
            local::PROVIDER => runtime.as_ref().map(|r| (vec![], local::is_credential as fn(&str) -> bool, r.2.to_string())),
            // The plan's base for this API: the path that goes out is the whole URL.
            plan::PROVIDER => {
                let (_, p) = coding_plan.as_ref().expect("a plan");
                let (url, headers) = match plan::url(p, &rest).and_then(|u| plan::headers(p, &rest).map(|h| (u, h))) {
                    Ok(x) => x,
                    Err(why) => return error(StatusCode::NOT_FOUND, why),
                };
                Some((headers, plan::is_credential, url))
            }
            _ => None,
        }
    };
    // Tests put a stand-in where a provider is, for one session.
    #[cfg(test)]
    let in_tests = tests::upstream_for(&session, &provider);
    #[cfg(not(test))]
    let in_tests: Option<String> = None;
    let upstream = match in_tests.as_deref().or_else(|| provider_upstream(&provider)) {
        Some(upstream) => upstream,
        None if hosted.is_some() => hosted.as_ref().map(|h| h.2.as_str()).unwrap_or_default(),
        None => return error(StatusCode::NOT_FOUND, format!("unknown provider {provider}")),
    };
    let query = req.uri().query().map(|q| format!("?{q}")).unwrap_or_default();
    let url = if coding_plan.is_some() { format!("{upstream}{query}") } else { format!("{upstream}/{rest}{query}") };
    let (parts, body) = req.into_parts();
    let Ok(mut body) = axum::body::to_bytes(body, MAX_BODY).await else {
        return error(StatusCode::BAD_REQUEST, "unreadable body, or over 64 MiB".into());
    };
    let requested = model_of(&body);
    // The model the request names as it goes out.
    let mut model = requested.clone();
    // The ChatGPT plan streams; an agent that asked for one JSON answer gets it put together.
    let collect = provider == siwc::PROVIDER && is_model_call && !siwc::wants_stream(&body);
    if provider == siwc::PROVIDER && is_model_call && let Some(b) = siwc::shape(&body) {
        body = Bytes::from(b);
    }
    // A Codex model the backend rejected lately: straight to the one that answered instead, and
    // the session says so. Choosing another model the account lists clears that.
    if provider == "chatgpt" && is_model_call {
        let sub = requested.as_ref().and_then(|m| codex::remembered(&mut st.substitutes.lock().unwrap(), m));
        if let Some(sub) = sub
            && let Some(b) = codex::with_model(&body, &sub.using)
        {
            body = b;
            model = Some(sub.using.clone());
            st.stats.update(&session, |s| s.substitute = Some(sub));
        } else if let Some(m) = &requested
            && st.stats.sessions.lock().unwrap().get(&session).is_some_and(|s| s.substitute.is_some())
            && codex::listed(m)
        {
            st.stats.update(&session, |s| s.substitute = None);
        }
    }
    // The route this model call is for, as fallbacks know it (see `fallback`).
    let primary = is_model_call.then(|| {
        let (path, name) = match (&coding_plan, &runtime) {
            (Some((id, p)), _) => (format!("plan/{id}"), p.name.clone()),
            (_, Some((id, name, _))) => (format!("local/{id}"), name.to_string()),
            _ => (provider.clone(), fallback::primary_name(&provider, &parts.headers)),
        };
        Primary { key: fallback::route_key(&path, &parts.headers), tag: RouteTag { path, name } }
    });
    if is_model_call {
        st.stats.update(&session, |s| {
            s.requests += 1;
            s.in_flight += 1;
            s.last_request = Some(Instant::now());
            if let Some(m) = &model {
                s.last_model = Some(m.clone());
            }
            if let Some(p) = &primary
                && s.primary.as_ref().is_none_or(|(k, _)| *k != p.key)
            {
                s.primary = Some((p.key.clone(), p.tag.name.clone()));
            }
        });
    }
    let mut guard = is_model_call.then(|| InFlight { stats: st.stats.clone(), session: session.clone() });

    // The user's other Claude accounts, for Claude Code's own calls to Anthropic signed in with a
    // subscription: while the one it signed in with is spent, the first that isn't answers.
    let accounts = if provider == "anthropic" && is_model_call && parts.headers.get("authorization").and_then(|v| v.to_str().ok()).is_some_and(fallback::is_claude_subscription) {
        accounts::others(&st.keys.read().unwrap())
    } else {
        vec![]
    };

    // What the session falls back to, if anything: only for the APIs that answer a turn, and not
    // for an agent that wanted the ChatGPT plan's stream put together.
    let api = if is_model_call && !collect { fallback::Api::of(&rest) } else { None };

    // Which account signs the call: its own while that isn't spent; one turn stays on one account.
    let key_of = |n: u32| accounts.iter().find(|a| a.0 == n).map(|a| accounts::key(&a.1)).unwrap_or_default();
    let spare = |n: u32| st.stats.limited(&key_of(n)).is_none();
    let mut account: Option<(u32, String)> = None;
    if let Some(p) = &primary
        && !accounts.is_empty()
    {
        let mut on = None;
        st.stats.update(&session, |s| on = s.fallback.as_ref().filter(|f| f.from == p.key).and_then(|f| f.account));
        let mid_turn = on.filter(|_| api.is_some_and(|a| !fallback::turn_start(a, &body)));
        let pick = if let Some(n) = mid_turn.filter(|n| spare(*n)) {
            Some(n)
        } else if st.stats.limited(&p.key).is_some() {
            accounts.iter().map(|a| a.0).find(|n| spare(*n))
        } else {
            None
        };
        account = pick.and_then(|n| accounts.iter().find(|a| a.0 == n).cloned());
    }
    let chain = api.and_then(|_| st.chains.read().unwrap().get(&session).cloned());
    let query_ref = query.as_str();
    // The chain was asked already, and nothing in it answered: not again for the same call.
    let mut tried = false;
    if let (Some(chain), Some(api), Some(p)) = (&chain, api, &primary) {
        // Spent (or down, when that counts), or back from a fallback but in the middle of a turn:
        // the chain answers, without asking the route that can't.
        if account.is_none()
            && let Some(why) = skip_primary(&st.stats, &session, &p.key, chain.on_outage, api, &body)
        {
            if let Some(r) = steps(&st, &session, chain, api, &body, &parts.headers, &parts.method, query_ref, p, &why, &mut guard).await {
                return r;
            }
            tried = true;
        }
        // What a fallback thought can't be checked here: it stays behind.
        if st.stats.mixed(&session)
            && let Some(b) = fallback::for_primary(&body, api)
        {
            body = b;
        }
    }

    let method = parts.method.clone();
    let on_mac = runtime.is_some();
    let (headers, hosted_ref, url_ref, method_ref) = (&parts.headers, &hosted, &url, &method);
    // `account`: another of the user's Claude accounts signs the call instead (see `accounts`).
    let send = |body: Bytes, account: Option<&str>| {
        let bearer = account.map(|t| format!("Bearer {t}"));
        st.upstream.send(on_mac, move |client| {
            let mut up = client.request(method_ref.clone(), url_ref).body(body.clone());
            for (name, value) in headers.iter().filter(|(n, _)| !hop_by_hop(n)) {
                if hosted_ref.as_ref().is_some_and(|h| h.1(name.as_str())) || (bearer.is_some() && name == "authorization") {
                    continue;
                }
                up = up.header(name, value);
            }
            if let Some(b) = &bearer {
                up = up.header("authorization", b);
            }
            for (name, value) in hosted_ref.iter().flat_map(|h| &h.0) {
                up = up.header(*name, value);
            }
            up
        })
    };
    // A model call that got no answer to count, for statistics.
    let failed = |status: CallStatus, model: Option<String>| {
        if is_model_call {
            st.stats.record_call(Call { at_ms, session: session.clone(), route: route.clone(), model, duration_ms: Some(ms(started.elapsed())), status, ..Default::default() });
        }
    };
    // Answered as the provider would when it's briefly unreachable, so the agent's own retries
    // take over, as they would without dino in between.
    let upstream_error = |e: reqwest::Error| {
        failed(CallStatus::Error, model.clone());
        let msg = match &runtime {
            Some((_, name, base)) if e.is_connect() => local::unreachable(name, base),
            _ => format!("dino couldn't reach {}: {}", upstream.trim_start_matches("https://"), reason(&e)),
        };
        st.stats.update(&session, |s| {
            s.errors += 1;
            s.call_failed(msg.clone());
        });
        log(format_args!("{session} {provider} {method} /{rest} -> upstream error: {e:?}"));
        upstream::unreachable(&rest, &msg)
    };
    let mut resp = match send(body.clone(), account.as_ref().map(|a| a.1.as_str())).await {
        Ok(r) => r,
        Err(e) => {
            // Unreachable: down, if it goes on and the chain counts outages.
            if let (Some(chain), Some(api), Some(p)) = (chain.as_ref().filter(|c| c.on_outage && !tried), api, &primary)
                && let Some(why) = st.stats.outage(&session, &p.key, &p.tag.name, format!("couldn't reach it ({})", reason(&e)))
                && let Some(r) = steps(&st, &session, chain, api, &body, &parts.headers, &parts.method, query_ref, p, &why, &mut guard).await
            {
                failed(CallStatus::Error, model.clone());
                return r;
            }
            return upstream_error(e);
        }
    };

    // A failed model call is a small JSON body: read it to say why, and for Codex maybe retry another model.
    if is_model_call && !resp.status().is_success() {
        let status = resp.status();
        let headers = resp.headers().clone();
        // Cut off while it answered: say so the way a dropped connection would read to the agent,
        // retryable, rather than pass on half an error.
        let text = match read_capped(resp, MAX_BODY).await {
            Ok(t) => t,
            Err(e) => return upstream_error(e),
        };
        // With every Claude account spent, the refusal of the one back first (see below).
        let mut first_back: Option<Refusal> = None;
        resp = 'retry: {
            if provider == "chatgpt" && status == StatusCode::NOT_FOUND && codex::model_not_found(&text) {
                let rejected = requested.clone().unwrap_or_default();
                let sent = model.clone().unwrap_or_default();
                for using in codex::fallbacks(&[&rejected, &sent]) {
                    let Some(retry) = codex::with_model(&body, &using) else { continue };
                    match send(retry, None).await {
                        Ok(r) if r.status().is_success() => {
                            log(format_args!("{session} chatgpt: {rejected} rejected, using {using}"));
                            let sub = codex::Substitute { rejected: rejected.clone(), using: using.clone(), said: error_message(&text), since: fallback::now() };
                            st.substitutes.lock().unwrap().insert(rejected, sub.clone());
                            st.stats.update(&session, |s| {
                                s.last_model = Some(using);
                                s.substitute = Some(sub);
                            });
                            break 'retry r;
                        }
                        Ok(r) => log(format_args!("{session} chatgpt: fallback {using} -> {}", r.status())),
                        Err(e) => return upstream_error(e),
                    }
                }
            }
            // The account that signed it is spent: its own, when another answered and its own
            // isn't known spent, then the user's other Claude accounts in order, until one answers.
            if !accounts.is_empty()
                && let Some(p) = &primary
                && let Some(t) = fallback::classify(status.as_u16(), &headers, &text).filter(|t| t.kind == fallback::Kind::Quota)
            {
                let current = account.as_ref().map(|a| a.0);
                let signer = |a: Option<u32>| match a {
                    Some(n) => (key_of(n), accounts::name(n)),
                    None => (p.key.clone(), p.tag.name.clone()),
                };
                let (key, name) = signer(current);
                st.stats.mark_limited(&key, &name, &t);
                st.stats.refusals.lock().unwrap().insert(key.clone(), Refusal { status, headers: headers.clone(), text: text.clone() });
                record_quota(&st.stats, &quota_key(&provider, account.as_ref()), &headers);
                let own = (current.is_some() && st.stats.limited(&p.key).is_none()).then_some(None);
                let others = accounts.iter().filter(|(n, _)| Some(*n) != current && spare(*n)).map(Some);
                for next in own.into_iter().chain(others) {
                    let (next_key, next_name) = signer(next.map(|a| a.0));
                    match send(body.clone(), next.map(|a| a.1.as_str())).await {
                        Ok(r) if r.status().is_success() => {
                            log(format_args!("{session} {name} is spent; {next_name} answers"));
                            // The refusal is a call of its own, for statistics.
                            failed(CallStatus::Limit, model.clone());
                            account = next.cloned();
                            break 'retry r;
                        }
                        Ok(r) => {
                            let (s2, h2) = (r.status(), r.headers().clone());
                            let t2 = read_capped(r, MAX_BODY).await.unwrap_or_default();
                            log(format_args!("{session} {next_name} -> {s2}"));
                            record_quota(&st.stats, &quota_key(&provider, next), &h2);
                            if let Some(tr) = fallback::classify(s2.as_u16(), &h2, &t2) {
                                st.stats.mark_limited(&next_key, &next_name, &tr);
                                if tr.kind == fallback::Kind::Quota {
                                    st.stats.refusals.lock().unwrap().insert(next_key, Refusal { status: s2, headers: h2, text: t2 });
                                }
                            }
                        }
                        Err(e) => log(format_args!("{session} {next_name}: {}", reason(&e))),
                    }
                }
                // Every account is spent: the agent waits for the one back first, not for the
                // last one asked (Claude Code says when it goes on from the refusal it gets).
                let keys: Vec<String> = std::iter::once(p.key.clone()).chain(accounts.iter().map(|(n, _)| key_of(*n))).collect();
                first_back = st.stats.first_back(&keys).filter(|(k, _)| *k != key).map(|(_, r)| r);
            }
            log(format_args!("{session} {provider} {method} /{rest} -> {status}"));
            // Each Claude account's windows are its own: kept by the account that signed the call.
            record_quota(&st.stats, &quota_key(&provider, account.as_ref()), &headers);
            // Spent: known as such (for new sessions, and the other sessions on it), and with a
            // chain, answered by it. Down, for a chain that counts outages, once it goes on.
            if let Some(p) = &primary {
                let why = match fallback::classify(status.as_u16(), &headers, &text) {
                    Some(t) => {
                        // A plan's spent window is the plan's state, whoever answers instead.
                        if let Some((id, plan)) = &coding_plan {
                            st.stats.plan_errors.lock().unwrap().insert(id.clone(), plan::refused(&plan.name, status.as_u16(), &error_message(&text)));
                        }
                        // Signed by another Claude account, spent too: its own stays as it was
                        // found, or else as that account is.
                        let other = account.as_ref().and_then(|(n, _)| st.stats.limited(&p.key).or_else(|| st.stats.limited(&key_of(*n))));
                        Some(other.unwrap_or_else(|| st.stats.mark_limited(&p.key, &p.tag.name, &t)))
                    }
                    None if fallback::is_outage(status.as_u16()) && chain.as_ref().is_some_and(|c| c.on_outage) => {
                        st.stats.outage(&session, &p.key, &p.tag.name, format!("{} {}", status.as_u16(), error_message(&text)))
                    }
                    None => None,
                };
                if let (Some(why), Some(chain), Some(api)) = (why, chain.as_ref().filter(|_| !tried), api)
                    && let Some(r) = steps(&st, &session, chain, api, &body, &parts.headers, &parts.method, query_ref, p, &why, &mut guard).await
                {
                    // Its own route's refusal is a call of its own, for statistics.
                    failed(if why.kind == fallback::Kind::Outage { CallStatus::Error } else { CallStatus::Limit }, model.clone());
                    return r;
                }
            }
            let msg = match provider.as_str() {
                siwc::PROVIDER => siwc::refused(status.as_u16(), &error_message(&text)),
                local::PROVIDER => runtime.as_ref().map_or_else(String::new, |(id, name, _)| local::refused(id, name, status.as_u16(), &error_message(&text), requested.as_deref())),
                plan::PROVIDER => coding_plan.as_ref().map_or_else(String::new, |(_, p)| plan::refused(&p.name, status.as_u16(), &error_message(&text))),
                _ => format!("{} {}", status.as_u16(), error_message(&text)),
            };
            // The plan's key, balance or limit: the plan's state, not just this call's.
            let limit = coding_plan.is_some() && plan::limited(status.as_u16(), &error_message(&text)).is_some();
            // A limit is a spent quota, window or balance. Another 429 (a short rate limit, or the
            // one-token check Claude Code makes as it starts, which Anthropic answers with a bare 429
            // when the account has no extra usage) is an error of the call, not the account's limit.
            let quota = fallback::classify(status.as_u16(), &headers, &text).is_some_and(|t| t.kind == fallback::Kind::Quota);
            failed(if limit || quota { CallStatus::Limit } else { CallStatus::Error }, model.clone());
            if let Some((id, _)) = coding_plan.as_ref().filter(|_| limit || matches!(status.as_u16(), 401 | 403)) {
                st.stats.plan_errors.lock().unwrap().insert(id.clone(), msg.clone());
            }
            st.stats.update(&session, |s| {
                s.errors += 1;
                if limit {
                    s.limit_error = Some(msg.clone());
                }
                // Its own route answered, if with a refusal: whatever fallback it was on, it isn't now.
                if account.is_none() && primary.as_ref().is_some_and(|p| s.fallback.as_ref().is_some_and(|f| f.from == p.key)) {
                    s.fallback = None;
                }
                s.call_failed(msg);
            });
            drop(guard);
            let (status, headers, text) = match first_back {
                Some(r) => {
                    log(format_args!("{session} every Claude account is spent: passing on the refusal of the one back first"));
                    (r.status, r.headers, r.text)
                }
                None => (status, headers, text),
            };
            let mut builder = Response::builder().status(status.as_u16());
            for (name, value) in headers.iter().filter(|(n, _)| !hop_by_hop(n)) {
                builder = builder.header(name, value);
            }
            return builder.body(Body::from(text)).unwrap_or_else(|_| error(StatusCode::BAD_GATEWAY, "bad response".into()));
        };
    }

    let status = resp.status();
    log(format_args!("{session} {provider} {method} /{rest} -> {status}"));
    if std::env::var_os("DINO_PROXY_LOG_HEADERS").is_some() {
        let names: Vec<String> = resp.headers().iter().filter(|(n, _)| (n.as_str().contains("limit") || n.as_str().starts_with("x-codex")) && !n.as_str().ends_with("turn-state")).map(|(n, v)| format!("{n}={}", v.to_str().unwrap_or("?"))).collect();
        log(format_args!("  headers: {}", names.join(" ")));
    }
    record_quota(&st.stats, &quota_key(&provider, account.as_ref()), resp.headers());
    if !status.is_success() && is_model_call {
        st.stats.update(&session, |s| s.errors += 1);
    } else if is_model_call {
        if let Some((id, _)) = &coding_plan {
            st.stats.plan_errors.lock().unwrap().remove(id);
        }
        // Answered by another Claude account: its own is still spent.
        if let Some(p) = primary.as_ref().filter(|_| account.is_none()) {
            st.stats.not_limited(&p.key);
        }
        st.stats.update(&session, |s| {
            s.call_error = None;
            s.limit_error = None;
            if !s.hooked {
                s.last_error = None;
            }
            s.outages = (0, 0);
            // Answered by another of the user's Claude accounts: shown, and noticed once, as a
            // fallback; back on its own account, as any.
            let other = account.as_ref().zip(primary.as_ref()).map(|((n, _), p)| (accounts::name(*n), p));
            match other {
                Some((name, p)) if s.fallback.as_ref().is_none_or(|f| f.name != name) => {
                    let spent = st.stats.limited(&p.key);
                    s.fallback = Some(fallback::OnFallback {
                        route: "anthropic".into(),
                        name,
                        model: model.clone().unwrap_or_default(),
                        from: p.key.clone(),
                        from_name: p.tag.name.clone(),
                        kind: fallback::Kind::Quota,
                        said: spent.as_ref().map(|l| l.said.clone()).unwrap_or_default(),
                        resets_at: spent.as_ref().and_then(|l| l.resets_at),
                        retry_at: spent.as_ref().map_or(0, |l| l.retry_at),
                        since: fallback::now(),
                        step: 0,
                        account: account.as_ref().map(|a| a.0),
                    });
                }
                // Still on it: when its own account is back is as that's found now (it may have
                // been found spent again, until a later reset).
                Some((_, p)) => {
                    if let (Some(f), Some(l)) = (s.fallback.as_mut(), st.stats.limited(&p.key)) {
                        f.resets_at = l.resets_at;
                        f.retry_at = l.retry_at;
                    }
                }
                None => {
                    if let Some(f) = s.fallback.take() {
                        log(format_args!("{session} back on {} from {}", f.from_name, f.name));
                    }
                }
            }
        });
    }

    let mut builder = Response::builder().status(status.as_u16());
    // Put together into one answer: its content type is JSON, not the stream's.
    for (name, value) in resp.headers().iter().filter(|(n, _)| !hop_by_hop(n) && !(collect && n.as_str() == "content-type")) {
        builder = builder.header(name, value);
    }
    if collect && status.is_success() {
        // A stream cut off halfway is no answer: never hand the agent a partial one as complete.
        let whole = match read_capped(resp, MAX_BODY).await {
            Ok(w) => w,
            Err(e) => return upstream_error(e),
        };
        let mut tap = Tap { meter: Meter::default(), stats: st.stats.clone(), session, route: primary.map(|p| p.tag), _in_flight: guard, call: is_model_call.then(|| Call { at_ms, route, model, ..Default::default() }), started, first: None };
        tap.meter.feed(&whole);
        let answer = siwc::collect(&whole).unwrap_or_else(|| whole.to_vec());
        return builder.header("content-type", "application/json").body(Body::from(answer)).unwrap_or_else(|_| error(StatusCode::BAD_GATEWAY, "bad response".into()));
    }

    // Tee the body: pass every chunk through immediately, scan a copy for usage.
    let tap = Tap { meter: Meter::default(), stats: st.stats.clone(), session, route: primary.map(|p| p.tag), _in_flight: guard, call: is_model_call.then(|| Call { at_ms, route, model, ..Default::default() }), started, first: None };
    builder.body(tapped(resp, tap)).unwrap_or_else(|_| error(StatusCode::BAD_GATEWAY, "bad response".into()))
}

/// An answer passed on as it comes, every chunk through at once, a copy read for usage.
fn tapped(resp: reqwest::Response, mut tap: Tap) -> Body {
    Body::from_stream(resp.bytes_stream().map(move |chunk| {
        if let Ok(bytes) = &chunk {
            if tap.first.is_none() {
                tap.first = Some(Instant::now());
            }
            tap.meter.feed(bytes);
        }
        chunk
    }))
}

/// The route a model call is for: its key (see `fallback::route_key`), its path and name.
pub(crate) struct Primary {
    key: String,
    tag: RouteTag,
}

/// Whether a call skips its route for the chain: the route is spent (or down, for a chain that
/// counts outages), or the session is back from a fallback but mid-turn. Why, if so.
fn skip_primary(stats: &Stats, session: &str, key: &str, on_outage: bool, api: fallback::Api, body: &[u8]) -> Option<fallback::Limited> {
    if let Some(l) = stats.limited(key).filter(|l| l.kind != fallback::Kind::Outage || on_outage) {
        return Some(l);
    }
    let mut on = None;
    stats.update(session, |s| {
        // Another Claude account answers with the same model: its own can take over mid-turn.
        on = s.fallback.as_ref().filter(|f| f.from == key && f.account.is_none()).map(|f| fallback::Limited {
            name: f.from_name.clone(),
            kind: f.kind,
            said: f.said.clone(),
            resets_at: f.resets_at,
            retry_at: f.retry_at,
        })
    });
    // Its time is up, but one turn isn't answered by two models: back at the next.
    on.filter(|_| !fallback::turn_start(api, body))
}

/// Send the call down `chain`, each route in turn, until one answers; that answer goes to the
/// agent as if its own route had given it. `None` when none did: the agent gets its own route's
/// answer. The agent's credentials never go along, and a Claude subscription never leaves for
/// another route.
#[allow(clippy::too_many_arguments)]
async fn steps(
    st: &AppState,
    session: &str,
    chain: &fallback::Chain,
    api: fallback::Api,
    body: &Bytes,
    headers: &HeaderMap,
    method: &axum::http::Method,
    query: &str,
    primary: &Primary,
    why: &fallback::Limited,
    guard: &mut Option<InFlight>,
) -> Option<Response<Body>> {
    // Mid-turn, a session stays where it is in the chain; it moves up only as a turn starts.
    let mut current = None;
    st.stats.update(session, |s| current = s.fallback.as_ref().filter(|f| f.from == primary.key && f.account.is_none()).map(|f| f.step));
    let start = current.filter(|_| !fallback::turn_start(api, body)).unwrap_or(0);
    for (i, step) in chain.steps.iter().enumerate().skip(start) {
        if step.route == primary.tag.path {
            continue;
        }
        if let Some(l) = st.stats.limited(&step.route).filter(|l| l.kind != fallback::Kind::Outage || chain.on_outage) {
            log(format_args!("{session} fallback {}: spent too ({})", step.route, l.said));
            continue;
        }
        let out = fallback::for_step(body, api, &step.model)?;
        let answered = |guard: &mut Option<InFlight>| {
            log(format_args!("{session} {} spent ({}): {} answers with {}", primary.tag.name, why.kind.word(), step.name, step.model));
            st.stats.update(session, |s| {
                let since = s.fallback.as_ref().filter(|f| f.from == primary.key).map_or_else(fallback::now, |f| f.since);
                s.fallback = Some(fallback::OnFallback {
                    route: step.route.clone(),
                    name: step.name.clone(),
                    model: step.model.clone(),
                    from: primary.key.clone(),
                    from_name: primary.tag.name.clone(),
                    kind: why.kind,
                    said: why.said.clone(),
                    resets_at: why.resets_at,
                    retry_at: why.retry_at,
                    since,
                    step: i,
                    account: None,
                });
                s.mixed = true;
                s.call_error = None;
                s.limit_error = None;
                if !s.hooked {
                    s.last_error = None;
                }
                s.last_model = Some(step.model.clone());
            });
            guard.take()
        };
        // dino's free models: dino picks the model, and answers in the agent's API itself.
        if step.route == "free" {
            let r = free::handle(st.clone(), session.to_string(), api.path(), out).await;
            if r.status().is_success() {
                drop(answered(guard));
                return Some(r);
            }
            log(format_args!("{session} fallback free -> {}", r.status()));
            continue;
        }
        let target = match step_target(st, &step.route, api, &out) {
            Ok(t) => t,
            Err(why) => {
                log(format_args!("{session} fallback {}: {why}", step.route));
                continue;
            }
        };
        // The subscription rule, whatever the key store holds: it never goes to another route.
        if target.headers.iter().any(|(_, v)| fallback::is_claude_subscription(v)) {
            log(format_args!("{session} fallback {}: refused, that's a Claude subscription token", step.route));
            continue;
        }
        let url = format!("{}{query}", target.url);
        // For statistics: a call to this route, in place of the session's own.
        let (step_started, step_at) = (Instant::now(), now_ms());
        let call = |status: CallStatus| Call {
            at_ms: step_at,
            session: session.to_string(),
            route: step.route.clone(),
            model: Some(step.model.clone()),
            status,
            fallback: Some(primary.tag.path.clone()),
            ..Default::default()
        };
        let sent = st
            .upstream
            .send(target.local, |client| {
                let mut up = client.request(method.clone(), &url).body(target.body.clone());
                for (name, value) in headers.iter().filter(|(n, _)| !hop_by_hop(n) && !fallback::is_credential(n.as_str())) {
                    up = up.header(name, value);
                }
                for (name, value) in &target.headers {
                    up = up.header(*name, value);
                }
                up
            })
            .await;
        let resp = match sent {
            Ok(r) => r,
            Err(e) => {
                log(format_args!("{session} fallback {}: {}", step.route, reason(&e)));
                st.stats.record_call(Call { duration_ms: Some(ms(step_started.elapsed())), ..call(CallStatus::Error) });
                st.stats.mark_limited(&step.route, &step.name, &fallback::Trigger { kind: fallback::Kind::Outage, resets_at: None, said: reason(&e) });
                continue;
            }
        };
        let status = resp.status();
        if !status.is_success() {
            let h = resp.headers().clone();
            let text = read_capped(resp, 1 << 20).await.unwrap_or_default();
            let trigger = fallback::classify(status.as_u16(), &h, &text);
            if let Some(t) = &trigger {
                st.stats.mark_limited(&step.route, &step.name, t);
            }
            let limit = trigger.is_some_and(|t| t.kind != fallback::Kind::Outage) || status == StatusCode::TOO_MANY_REQUESTS;
            st.stats.record_call(Call { duration_ms: Some(ms(step_started.elapsed())), ..call(if limit { CallStatus::Limit } else { CallStatus::Error }) });
            log(format_args!("{session} fallback {} -> {status}: {}", step.route, error_message(&text)));
            continue;
        }
        record_quota(&st.stats, &step.route, resp.headers());
        st.stats.not_limited(&step.route);
        let in_flight = answered(guard);
        let mut builder = Response::builder().status(status.as_u16());
        for (name, value) in resp.headers().iter().filter(|(n, _)| !hop_by_hop(n)) {
            builder = builder.header(name, value);
        }
        let route = Some(RouteTag { path: step.route.clone(), name: step.name.clone() });
        let tap = Tap { meter: Meter::default(), stats: st.stats.clone(), session: session.to_string(), route, _in_flight: in_flight, call: Some(call(CallStatus::Ok)), started: step_started, first: None };
        return Some(builder.body(tapped(resp, tap)).unwrap_or_else(|_| error(StatusCode::BAD_GATEWAY, "bad response".into())));
    }
    log(format_args!("{session} {} spent, and no fallback answered", primary.tag.name));
    None
}

/// Where a fallback route takes a call, with what credentials, and the body as it takes it.
struct Target {
    url: String,
    headers: Vec<(&'static str, String)>,
    body: Bytes,
    /// A model server on this Mac.
    local: bool,
}

fn step_target(st: &AppState, route: &str, api: fallback::Api, body: &Bytes) -> Result<Target, String> {
    let path = api.path();
    let keys = st.keys.read().unwrap();
    if route == openrouter::PROVIDER {
        let h = openrouter::headers(&keys).ok_or("OpenRouter isn't connected")?;
        return Ok(Target { url: format!("{}/{path}", openrouter::upstream()), headers: h.to_vec(), body: body.clone(), local: false });
    }
    if route == siwc::PROVIDER {
        if api != fallback::Api::Responses || !siwc::wants_stream(body) {
            return Err("the ChatGPT plan only takes streamed Responses calls".into());
        }
        let h = siwc::headers(&keys)?;
        let shaped = siwc::shape(body).map(Bytes::from).ok_or("unreadable request")?;
        return Ok(Target { url: format!("{}/{path}", siwc::upstream()), headers: h.to_vec(), body: shaped, local: false });
    }
    if let Some(id) = route.strip_prefix("plan/") {
        let p = st.plans.read().unwrap().get(id).cloned().ok_or_else(|| format!("the coding plan {id} isn't connected"))?;
        return Ok(Target { url: plan::url(&p, path)?, headers: plan::headers(&p, path)?, body: body.clone(), local: false });
    }
    if let Some(id) = route.strip_prefix("local/") {
        let (_, base) = local::runtime(id).ok_or_else(|| format!("dino doesn't know a model server called {id}"))?;
        return Ok(Target { url: format!("{base}/{path}"), headers: vec![], body: body.clone(), local: true });
    }
    Err(format!("{route} isn't a route dino falls back to"))
}

/// The model a request names. Read without building the rest of it: an agent's request carries its
/// whole conversation, often hundreds of kilobytes, and this is on the way to the first byte.
fn model_of(body: &[u8]) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Named {
        model: Option<String>,
    }
    serde_json::from_slice::<Named>(body).ok()?.model
}

/// An upstream answer's body, up to `limit` bytes; the rest isn't read.
/// The upstream's answer up to `limit` bytes. A connection that breaks partway is an error, never a
/// shorter answer: the agent must not take half a reply for a whole one.
async fn read_capped(mut resp: reqwest::Response, limit: usize) -> Result<Bytes, reqwest::Error> {
    let mut out: Vec<u8> = Vec::new();
    while out.len() < limit {
        let Some(chunk) = resp.chunk().await? else { break };
        out.extend_from_slice(&chunk[..chunk.len().min(limit - out.len())]);
    }
    Ok(Bytes::from(out))
}

/// Lives as long as the response body; records usage when the stream ends or is dropped.
struct Tap {
    meter: Meter,
    stats: Arc<Stats>,
    session: String,
    /// The route answering, to count what it answered toward.
    route: Option<RouteTag>,
    _in_flight: Option<InFlight>,
    /// A model call's record so far, finished as the answer ends.
    call: Option<Call>,
    started: Instant,
    /// When the answer's first byte came, streamed.
    first: Option<Instant>,
}

impl Drop for Tap {
    fn drop(&mut self) {
        self.meter.finish();
        let aborted = self._in_flight.is_some() && !self.meter.complete;
        if aborted {
            log(format_args!("{} model call dropped by the agent", self.session));
        }
        if let Some(e) = &self.meter.error {
            log(format_args!("{} model call answered 200 with an error: {e}", self.session));
        }
        if let Some(mut call) = self.call.take() {
            call.session = self.session.clone();
            if let Some(m) = &self.meter.model {
                call.model = Some(m.clone());
            }
            call.usage = self.meter.seen.clone().unwrap_or_default();
            call.cost = self.meter.cost;
            call.ttft_ms = self.first.filter(|_| self.meter.sse == Some(true)).map(|f| ms(f - self.started));
            call.duration_ms = Some(ms(self.started.elapsed()));
            if self.meter.error.is_some() {
                call.status = CallStatus::Error;
            }
            self.stats.record_call(call);
        }
        self.stats.update(&self.session, |s| {
            if let Some(e) = self.meter.error.take() {
                s.errors += 1;
                s.call_failed(e);
            }
            if let Some(u) = self.meter.seen.take() {
                s.metered(self.route.as_ref(), &u);
                // Probes (Claude checks its quota with a one-word call) say nothing about the conversation.
                let probe = u.total_input() < 100;
                if let Some(model) = self.meter.model.take().or_else(|| s.last_model.clone()).filter(|_| !probe) {
                    s.context.insert(model, u.total_input());
                }
            }
            // The agent hung up mid-answer with nothing else in flight: the user interrupted the
            // turn (Esc). Claude Code fires no hook for that, so the turn would look busy forever.
            if aborted && s.in_flight <= 1 && s.activity == Some(Activity::Working) {
                s.activity = Some(Activity::Done);
            }
        });
    }
}

async fn remote_hook(State(rs): State<RemoteState>, Path(token): Path<String>, body: Bytes) -> StatusCode {
    let Some(session) = rs.tokens.read().unwrap().get(&token).cloned() else { return StatusCode::NOT_FOUND };
    on_hook(&rs.app, &session, &body)
}

async fn hook(State(st): State<AppState>, Path((key, session)): Path<(String, String)>, headers: HeaderMap, body: Bytes) -> StatusCode {
    if !admitted(&st.secret, st.port, &key, &headers) {
        return StatusCode::FORBIDDEN;
    }
    on_hook(&st, &session, &body)
}

fn on_hook(st: &AppState, session: &str, body: &[u8]) -> StatusCode {
    let Ok(v) = serde_json::from_slice::<Value>(body) else { return StatusCode::BAD_REQUEST };
    // Not a hook: `dino statusline` passing on what Claude Code gave the statusline, every few
    // seconds (too often to log).
    if v["hook_event_name"].is_null() && v["context_window"].is_object() {
        if let Some(c) = ReportedContext::from_statusline(&v) {
            st.stats.update(session, |s| s.reported_context = Some(c));
        }
        return StatusCode::OK;
    }
    log(format_args!("{session} hook {v}"));
    let event = v["hook_event_name"].as_str().unwrap_or_default();
    let tool = || v["tool_name"].as_str().unwrap_or("tool").to_string();
    record_subagent(&st.stats, session, event, &v);
    tasks::record(&st.stats, session, event, &v);
    // Its subagents' calls too: a subagent clicking in an app is the session using the Mac. A tool
    // reached through a bridge (Qwen's `tool_call` for tools it defers) by the one it names.
    let called = || match (v["tool_name"].as_str(), v["tool_input"]["name"].as_str()) {
        (Some("tool_call"), Some(name)) => name.to_string(),
        _ => tool(),
    };
    match event {
        "PreToolUse" => st.stats.tool_call(session, &called(), computer::Phase::Started),
        "PostToolUse" | "PostToolUseFailure" => st.stats.tool_call(session, &called(), computer::Phase::Ended),
        "Stop" | "StopFailure" => st.stats.tools_done(session),
        _ => {}
    }
    // A subagent's own tool calls: the parent's turn may be over (background agents), and the
    // subagent's model calls show as the session thinking anyway.
    let from_subagent = v["agent_id"].is_string();
    let activity = match event {
        "UserPromptSubmit" | "PreToolUse" | "PostToolUse" | "PostToolUseFailure" if from_subagent => None,
        // An interrupted tool, when the agent reports one (Claude often sends nothing; see `end_turn`).
        "PostToolUseFailure" if v["is_interrupt"].as_bool() == Some(true) => Some(Activity::Done),
        "UserPromptSubmit" | "PreToolUse" | "PostToolUse" | "PostToolUseFailure" => Some(Activity::Working),
        "PermissionRequest" => Some(Activity::NeedsPermission(asking(&tool(), &v["tool_input"]))),
        "Notification" => match v["notification_type"].as_str() {
            Some("permission_prompt" | "elicitation_dialog" | "agent_needs_input") => {
                let msg = v["message"].as_str().unwrap_or("needs input").to_string();
                Some(Activity::NeedsPermission(msg))
            }
            // Sent after a minute at the prompt: a backstop for any turn end we missed.
            Some("idle_prompt") => Some(Activity::Done),
            _ => None,
        },
        "Stop" | "StopFailure" | "SessionStart" => Some(Activity::Done),
        _ => None,
    };
    if !from_subagent {
        st.stats.update(session, |s| s.turn_hook(event, &v));
    }
    if let Some(a) = activity {
        // A notification about the same prompt shouldn't clobber the more specific tool name.
        st.stats.update(session, |s| {
            if !(event == "Notification" && matches!(s.activity, Some(Activity::NeedsPermission(_)))) {
                if matches!(a, Activity::NeedsPermission(_)) {
                    s.questions += 1;
                }
                s.activity = Some(a);
            }
        });
    }
    StatusCode::OK
}

/// Keep track of the subagents a session starts: PostToolUse of the Agent tool names the task and
/// the agent's id, SubagentStart says where it runs, SubagentStop when it's finished. A foreground
/// agent's PostToolUse only comes when it's done, so until then its task is guessed from the
/// oldest unanswered Agent call of its type.
/// What a permission prompt asks, in a few words: "Run: npm test?", "Edit src/main.rs?". The
/// tool's name alone ("Bash") doesn't say what you'd be allowing.
fn asking(tool: &str, input: &Value) -> String {
    let field = |k: &str| input[k].as_str().map(|s| s.lines().next().unwrap_or("").trim().to_string()).filter(|s| !s.is_empty());
    let file = || field("file_path").or_else(|| field("notebook_path")).map(|p| p.rsplit('/').next().unwrap_or(&p).to_string());
    let what = match tool {
        "Bash" => field("command").map(|c| format!("Run: {c}")),
        "Edit" | "MultiEdit" | "NotebookEdit" => file().map(|f| format!("Edit {f}")),
        "Write" => file().map(|f| format!("Write {f}")),
        "WebFetch" => field("url").map(|u| format!("Fetch {u}")),
        "WebSearch" => field("query").map(|q| format!("Search the web for {q}")),
        _ => None,
    };
    let mut s = what.unwrap_or_else(|| format!("Use {tool}"));
    if s.chars().count() > 120 {
        s = s.chars().take(119).collect::<String>() + "…";
    }
    s + "?"
}

fn record_subagent(stats: &Stats, session: &str, event: &str, v: &Value) {
    let text = |x: &Value| x.as_str().filter(|s| !s.is_empty()).map(String::from);
    let agent_tool = matches!(v["tool_name"].as_str(), Some("Agent" | "Task")) && v["agent_id"].is_null();
    if event == "PreToolUse" && agent_tool {
        let call = (text(&v["tool_use_id"]).unwrap_or_default(), text(&v["tool_input"]["description"]), text(&v["tool_input"]["subagent_type"]));
        stats.update(session, |s| s.pending_agents.push(call));
        return;
    }
    let (id, known) = match event {
        "PostToolUse" | "PostToolUseFailure" if agent_tool => {
            let call = text(&v["tool_use_id"]).unwrap_or_default();
            stats.update(session, |s| s.pending_agents.retain(|p| p.0 != call));
            (text(&v["tool_response"]["agentId"]), true)
        }
        // Claude also stops internal helpers it never started through the Agent tool: no type.
        "SubagentStart" | "SubagentStop" => (text(&v["agent_id"]), text(&v["agent_type"]).is_some()),
        _ => return,
    };
    let Some(id) = id else { return };
    stats.update(session, |s| {
        let i = match s.subagents.iter().position(|a| a.id == id) {
            Some(i) => i,
            None if known => {
                s.subagents.push(Subagent { id: id.clone(), ..Default::default() });
                s.subagents.len() - 1
            }
            None => return,
        };
        let a = &mut s.subagents[i];
        match event {
            "PostToolUse" => {
                a.description = text(&v["tool_input"]["description"]).or(a.description.take());
                a.output = text(&v["tool_response"]["outputFile"]).or(a.output.take());
            }
            "SubagentStart" => {
                a.running = true;
                a.started = tasks::now();
                a.finished = None;
                a.cwd = text(&v["cwd"]);
                a.agent_type = text(&v["agent_type"]);
                if a.description.is_none() {
                    let ty = a.agent_type.clone();
                    if let Some(p) = s.pending_agents.iter().position(|p| p.2.is_none() || p.2 == ty) {
                        s.subagents[i].description = s.pending_agents.remove(p).1;
                    }
                }
            }
            _ => {
                if a.running {
                    a.finished = Some(tasks::now());
                }
                a.running = false;
            }
        }
    });
}

/// Append a line to `$DINO_PROXY_LOG`, if set. Debugging aid.
pub(crate) fn log(line: std::fmt::Arguments<'_>) {
    use std::io::Write;
    let Some(path) = std::env::var_os("DINO_PROXY_LOG") else { return };
    // One write per line so concurrent requests don't interleave.
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(format!("{line}\n").as_bytes());
    }
}

pub(crate) struct InFlight {
    stats: Arc<Stats>,
    session: String,
}

impl Drop for InFlight {
    fn drop(&mut self) {
        self.stats.update(&self.session, |s| s.in_flight = s.in_flight.saturating_sub(1));
    }
}

/// The session used up its token budget: refuse the call in the shape its agent shows as an error.
fn over_budget(st: &AppState, session: &str, provider: &str) -> Option<Response<Body>> {
    let budget = st.budget.load(Ordering::Relaxed);
    let used = st.stats.session(session).usage;
    let used = used.total_input() + used.output;
    if budget == 0 || used < budget {
        return None;
    }
    let msg = format!("dino: this session used {used} tokens, over its budget of {budget}. Start a new session, or raise the budget in Settings → Agents → Limits.");
    st.stats.update(session, |s| {
        s.errors += 1;
        // dino's own refusal: shown whether or not the agent reports its turns.
        s.last_error = Some(msg.clone());
        s.call_error = Some(msg.clone());
    });
    log(format_args!("{session} {provider} refused: over budget ({used} of {budget})"));
    // Claude Code retries 429s and 5xx; a 400 is shown to the user as is.
    let body = if matches!(provider, "anthropic" | "free") {
        serde_json::json!({"type": "error", "error": {"type": "invalid_request_error", "message": msg}})
    } else {
        serde_json::json!({"error": {"type": "invalid_request_error", "code": "dino_budget", "message": msg}})
    };
    Some(Response::builder().status(StatusCode::BAD_REQUEST).header("content-type", "application/json").body(Body::from(body.to_string())).unwrap())
}

/// What went wrong with a request, in words: reqwest's own text ("error sending request for url")
/// hides the cause in its source chain.
fn reason(e: &reqwest::Error) -> String {
    let mut words = vec![];
    let mut cause: Option<&dyn std::error::Error> = std::error::Error::source(e);
    while let Some(c) = cause {
        words.push(c.to_string());
        cause = c.source();
    }
    let kind = if e.is_connect() { "couldn't connect" } else if e.is_timeout() { "timed out" } else { "the connection failed" };
    match words.last() {
        Some(w) => format!("{kind} ({w})"),
        None => kind.into(),
    }
}

fn error(status: StatusCode, msg: String) -> Response<Body> {
    Response::builder().status(status).body(Body::from(msg)).unwrap()
}

/// Where a call's usage windows are kept: by its provider, or for another of the user's Claude
/// accounts, by that account (see `accounts::key`), so one account's use never reads as another's.
fn quota_key(provider: &str, account: Option<&(u32, String)>) -> String {
    account.map_or_else(|| provider.to_string(), |(_, token)| accounts::key(token))
}

/// Subscription windows from response headers: Claude's `anthropic-ratelimit-unified-5h-utilization`
/// and friends, or Codex's `x-codex-primary-used-percent` / `-window-minutes` / `-reset-at`.
fn record_quota(stats: &Stats, provider: &str, headers: &HeaderMap) {
    if let Some(windows) = codex_windows(headers) {
        stats.quotas.lock().unwrap().insert(provider.to_string(), Quota { windows });
        return;
    }
    const PREFIX: &str = "anthropic-ratelimit-unified-";
    if !headers.keys().any(|n| n.as_str().starts_with(PREFIX)) {
        return;
    }
    // On top of what was known: a refusal may say only which window refused, not the others' use.
    let mut windows: HashMap<String, Window> = stats.quotas.lock().unwrap().get(provider).map(|q| q.windows.iter().cloned().collect()).unwrap_or_default();
    for (name, value) in headers {
        let Some(rest) = name.as_str().strip_prefix(PREFIX) else { continue };
        let Some((window, field)) = rest.split_once('-') else { continue };
        let Ok(value) = value.to_str() else { continue };
        let w = windows.entry(window.to_string()).or_insert(Window { utilization: 0.0, resets_at: None, status: None });
        match field {
            "utilization" => w.utilization = value.parse().unwrap_or(0.0),
            "reset" => w.resets_at = value.parse().ok(),
            "status" => w.status = Some(value.to_string()),
            _ => {}
        }
    }
    // Only windows: `-overage-status`, `-representative-claim` and `-fallback-percentage` share the
    // prefix but have no reset to count down to.
    windows.retain(|_, w| w.resets_at.is_some());
    if windows.is_empty() {
        return;
    }
    let mut windows: Vec<_> = windows.into_iter().collect();
    windows.sort_by_key(|(name, _)| window_hours(name));
    stats.quotas.lock().unwrap().insert(provider.to_string(), Quota { windows });
}

fn codex_windows(headers: &HeaderMap) -> Option<Vec<(String, Window)>> {
    let get = |k: String| headers.get(k).and_then(|v| v.to_str().ok()).filter(|v| !v.is_empty()).map(String::from);
    let mut out = vec![];
    for which in ["primary", "secondary"] {
        let minutes: u64 = get(format!("x-codex-{which}-window-minutes")).and_then(|m| m.parse().ok()).unwrap_or(0);
        if minutes == 0 {
            continue;
        }
        let used: f32 = get(format!("x-codex-{which}-used-percent"))?.parse().ok()?;
        let name = match minutes {
            m if m % 1440 == 0 => format!("{}d", m / 1440),
            m if m % 60 == 0 => format!("{}h", m / 60),
            m => format!("{m}m"),
        };
        let resets_at = get(format!("x-codex-{which}-reset-at")).and_then(|t| t.parse().ok());
        out.push((name, Window { utilization: used / 100.0, resets_at, status: None }));
    }
    (!out.is_empty()).then_some(out)
}

fn window_hours(name: &str) -> u64 {
    let (n, unit) = name.split_at(name.len().saturating_sub(1));
    let n: u64 = n.parse().unwrap_or(0);
    if unit == "d" { n * 24 } else { n }
}

/// Most of a plain JSON answer, or of one SSE line, kept to read usage from.
const METER_LIMIT: usize = 16 << 20;

/// Finds token usage in a response body, whether SSE or plain JSON, across API dialects.
/// Usage counters within one response are cumulative, so we keep the max of each field.
#[derive(Default)]
struct Meter {
    line: Vec<u8>,
    body: Vec<u8>,
    sse: Option<bool>,
    seen: Option<Usage>,
    /// The model that answered, as the response says.
    model: Option<String>,
    /// The whole answer came through. Agents hang up once they have it, so the body running
    /// out can't tell a finished answer from an interrupted one; its last event can.
    complete: bool,
    /// What the route said the call cost (OpenRouter's `usage.cost`).
    cost: Option<f64>,
    /// The answer was an error after all, though it came with 200: OpenRouter passes an upstream
    /// failure ("provider_overloaded") on in the body, and a stream can end in an error event.
    error: Option<String>,
}

impl Meter {
    fn feed(&mut self, bytes: &Bytes) {
        let sse = *self.sse.get_or_insert_with(|| bytes.starts_with(b"event:") || bytes.starts_with(b"data:"));
        if !sse {
            // Read once it's all here (`finish`), not again with every chunk. One too big to hold
            // isn't read for usage, nor called dropped: whether it all came through can't be told.
            if self.body.len() + bytes.len() > METER_LIMIT {
                self.body = Vec::new();
                self.complete = true;
            } else if !self.complete {
                self.body.extend_from_slice(bytes);
            }
            return;
        }
        let mut rest: &[u8] = bytes;
        while let Some(i) = memchr::memchr(b'\n', rest) {
            let (part, after) = (&rest[..i], &rest[i + 1..]);
            rest = after;
            if self.line.is_empty() {
                self.line_ended(part);
            } else {
                self.keep(part);
                let line = std::mem::take(&mut self.line);
                self.line_ended(&line);
                self.line = line;
                self.line.clear();
            }
        }
        self.keep(rest);
    }

    /// Part of an SSE line, kept until its end comes.
    fn keep(&mut self, part: &[u8]) {
        let room = METER_LIMIT.saturating_sub(self.line.len());
        self.line.extend_from_slice(&part[..part.len().min(room)]);
    }

    fn line_ended(&mut self, line: &[u8]) {
        let Some(data) = line.strip_prefix(b"data:") else { return };
        let data = data.trim_ascii();
        // Chat Completions.
        self.complete |= data == b"[DONE]";
        if !self.worth_reading(data) {
            return;
        }
        if let Ok(v) = serde_json::from_slice::<Value>(data) {
            self.observe(&v);
            // Anthropic's last event, and the Responses API's ways to end.
            let last = ["message_stop", "error", "response.completed", "response.incomplete", "response.failed"];
            self.complete |= v["type"].as_str().is_some_and(|t| last.contains(&t));
        }
    }

    /// Whether an event can say anything `observe` or the end of a stream reads: usage, the model
    /// (until it's known), an error, a last event. Text and tool-input deltas, nearly all of a
    /// stream, can't, and aren't parsed: they'd cost more CPU than passing the answer on.
    /// Quoted, so the words inside an answer's text (where quotes are escaped) don't count.
    fn worth_reading(&self, data: &[u8]) -> bool {
        use memchr::memmem::Finder;
        static MARKS: std::sync::LazyLock<Vec<Finder<'static>>> = std::sync::LazyLock::new(|| {
            [&b"\"usage\""[..], b"\"error\"", b"\"message_stop\"", b"\"response.completed\"", b"\"response.incomplete\"", b"\"response.failed\""]
                .into_iter()
                .map(Finder::new)
                .collect()
        });
        static MODEL: std::sync::LazyLock<Finder<'static>> = std::sync::LazyLock::new(|| Finder::new(b"\"model\""));
        MARKS.iter().any(|f| f.find(data).is_some()) || (self.model.is_none() && MODEL.find(data).is_some())
    }

    /// The body ended (or the agent hung up): a plain JSON answer is read now.
    fn finish(&mut self) {
        if self.sse == Some(false) && !self.body.is_empty() {
            if let Ok(v) = serde_json::from_slice::<Value>(&self.body) {
                self.observe(&v);
                self.complete = true;
            }
            self.body = Vec::new();
        }
    }

    fn observe(&mut self, v: &Value) {
        // `{"error": {...}}` (OpenRouter, a Chat stream's chunk, Anthropic's error event) or a
        // failed Responses answer. A finished Responses answer carries `"error": null`.
        if self.error.is_none()
            && let Some(e) = [&v["error"], &v["response"]["error"]].into_iter().find(|e| e.is_object())
        {
            let said = e["message"].as_str().or(e["type"].as_str()).unwrap_or("the provider failed");
            self.error = Some(match e["code"].as_u64() {
                Some(code) => format!("{code} {said}"),
                None => said.to_string(),
            });
            self.complete = true;
        }
        if self.model.is_none() {
            self.model = [&v["message"]["model"], &v["response"]["model"], &v["model"]].iter().find_map(|m| m.as_str()).map(String::from);
        }
        for u in [&v["usage"], &v["message"]["usage"], &v["response"]["usage"]] {
            if !u.is_object() {
                continue;
            }
            let get = |keys: &[&str]| keys.iter().filter_map(|k| u[*k].as_u64()).max().unwrap_or(0);
            let cached_openai = u["prompt_tokens_details"]["cached_tokens"].as_u64().or(u["input_tokens_details"]["cached_tokens"].as_u64()).unwrap_or(0);
            let next = Usage {
                input: get(&["input_tokens", "prompt_tokens"]).saturating_sub(cached_openai),
                output: get(&["output_tokens", "completion_tokens"]),
                cache_read: get(&["cache_read_input_tokens"]) + cached_openai,
                cache_write: get(&["cache_creation_input_tokens"]),
            };
            if let Some(c) = u["cost"].as_f64() {
                self.cost = Some(c);
            }
            let seen = self.seen.get_or_insert_with(Usage::default);
            seen.input = seen.input.max(next.input);
            seen.output = seen.output.max(next.output);
            seen.cache_read = seen.cache_read.max(next.cache_read);
            seen.cache_write = seen.cache_write.max(next.cache_write);
        }
    }
}


/// The human part of an upstream error body: `{"error":{"message":…}}`, `{"detail":…}` or the text.
pub(crate) fn error_message(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    let msg = serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().or(v["detail"].as_str()).or(v["message"].as_str()).map(String::from))
        .unwrap_or_else(|| text.trim().to_string());
    msg.chars().take(200).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_is_seeded_once_from_its_history() {
        let stats = Stats::default();
        let u = |input, output| Usage { input, output, ..Default::default() };
        stats.seed("1", &[("anthropic".into(), u(100, 10)), ("plan/zai".into(), u(5, 1))]);
        let s = stats.session("1");
        assert_eq!((s.usage.total_input(), s.usage.output), (105, 11));
        assert_eq!(s.by_route.iter().find(|r| r.route == "anthropic").map(|r| r.name.as_str()), Some("Anthropic"));
        // A second seed (the same session started again) doesn't count it twice.
        stats.seed("1", &[("anthropic".into(), u(100, 10))]);
        assert_eq!(stats.session("1").usage.total_input(), 105);
        // A live call adds to it, and names the route as it knows it.
        stats.update("1", |s| s.metered(Some(&RouteTag { path: "anthropic".into(), name: "Claude".into() }), &u(1, 1)));
        let s = stats.session("1");
        assert_eq!((s.usage.total_input(), s.usage.output), (106, 12));
        assert_eq!(s.by_route.iter().find(|r| r.route == "anthropic").map(|r| r.name.as_str()), Some("Claude"));
    }
    use serde_json::json;

    #[test]
    fn a_permission_prompt_says_what_it_asks() {
        assert_eq!(asking("Bash", &json!({"command": "touch notes.txt\nls"})), "Run: touch notes.txt?");
        assert_eq!(asking("Edit", &json!({"file_path": "/repo/src/main.rs"})), "Edit main.rs?");
        assert_eq!(asking("mcp__x__y", &json!({})), "Use mcp__x__y?");
        assert_eq!(asking("Bash", &json!({})), "Use Bash?");
    }

    #[test]
    fn hooks_decide_whether_a_turn_failed() {
        // No hooks: a failed call shows until one succeeds.
        let mut s = SessionStats::default();
        s.call_failed("503 busy".into());
        assert_eq!(s.last_error.as_deref(), Some("503 busy"));

        // Hooks: a side call failing after a good turn shows nothing.
        let mut s = SessionStats::default();
        s.turn_hook("UserPromptSubmit", &json!({"permission_mode": "plan"}));
        assert_eq!(s.agent_mode.as_deref(), Some("plan"));
        s.turn_hook("Stop", &json!({}));
        s.call_failed("503 Grammar compilation is temporarily unavailable.".into());
        assert_eq!(s.last_error, None);

        // A failed turn shows what the call said.
        s.turn_hook("UserPromptSubmit", &json!({}));
        s.call_failed("529 overloaded".into());
        s.turn_hook("StopFailure", &json!({"error": "server_error"}));
        assert_eq!(s.last_error.as_deref(), Some("529 overloaded"));
        s.turn_hook("UserPromptSubmit", &json!({}));
        assert_eq!(s.last_error, None);

        // Resumed idle with its hooks wired: its quota probe failing isn't the session's error,
        // but a turn that then fails says why.
        let stats = Stats::default();
        stats.reports_turns("1");
        stats.update("1", |s| s.call_failed("429 Error".into()));
        assert_eq!(stats.session("1").last_error, None);
        stats.update("1", |s| {
            s.turn_hook("UserPromptSubmit", &json!({}));
            s.call_failed("429 Error".into());
            s.turn_hook("StopFailure", &json!({}));
        });
        assert_eq!(stats.session("1").last_error.as_deref(), Some("429 Error"));

        // A coding plan's spent window shows though the agent ends its turn with Stop, until a
        // call goes through again.
        let mut s = SessionStats::default();
        s.turn_hook("UserPromptSubmit", &json!({}));
        s.limit_error = Some("GLM Coding Plan's usage limit was reached".into());
        s.call_failed("GLM Coding Plan's usage limit was reached".into());
        s.turn_hook("Stop", &json!({}));
        assert_eq!(s.last_error.as_deref(), Some("GLM Coding Plan's usage limit was reached"));
        s.limit_error = None;
        s.turn_hook("Stop", &json!({}));
        assert_eq!(s.last_error, None);
    }

    /// Over real connections: a coding plan gets its own key and never the agent's credentials (a
    /// claude.ai login among them), at its base for the API asked; its spent window is said as such.
    #[test]
    fn a_coding_plan_gets_its_key_and_its_limit_is_said() {
        use std::io::{Read, Write};
        let plan_side = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}/api/anthropic", plan_side.local_addr().unwrap().port());
        let (tx, seen) = std::sync::mpsc::channel::<String>();
        std::thread::spawn(move || {
            let (mut s, _) = plan_side.accept().unwrap();
            let mut buf = vec![0; 65536];
            let n = s.read(&mut buf).unwrap();
            tx.send(String::from_utf8_lossy(&buf[..n]).to_lowercase()).unwrap();
            let body = r#"{"error":{"code":"1308","message":"Usage limit reached for 5 hour. Your limit will reset at 2026-10-04 02:00:00"}}"#;
            write!(s, "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        });
        let proxy = Proxy::start(HashMap::new()).unwrap();
        let p = plan::Plan { name: "GLM Coding Plan".into(), anthropic: Some(base), openai: None, key: "plan-key-1".into() };
        proxy.set_plans(HashMap::from([("zai".to_string(), p)]));
        let here = format!("127.0.0.1:{}", proxy.port);
        let send = |provider: &str, rest: &str| {
            let path = proxy.base_url("7", provider).strip_prefix(&format!("http://{here}")).unwrap().to_string() + rest;
            let body = r#"{"model":"glm-x","max_tokens":10,"messages":[{"role":"user","content":"hi"}]}"#;
            let mut c = std::net::TcpStream::connect(&here).unwrap();
            write!(c, "POST {path} HTTP/1.1\r\nHost: {here}\r\nAuthorization: Bearer sk-ant-oat01-the-users-claude-login\r\nx-api-key: agents-own\r\nanthropic-version: 2023-06-01\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            let mut out = String::new();
            let _ = c.read_to_string(&mut out);
            out
        };
        assert!(send("plan/kimi", "/v1/messages").starts_with("HTTP/1.1 401"), "a plan with no key isn't served");
        assert!(send("plan/zai", "/v1/files").starts_with("HTTP/1.1 404"), "only model calls");
        let out = send("plan/zai", "/v1/messages?beta=true");
        assert!(out.starts_with("HTTP/1.1 429") && out.contains("1308"), "the plan's own answer reaches the agent: {out}");
        let got = seen.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert!(got.starts_with("post /api/anthropic/v1/messages?beta=true "), "{got}");
        assert!(got.contains("authorization: bearer plan-key-1\r\n") && got.contains("x-api-key: plan-key-1\r\n"), "{got}");
        assert!(!got.contains("sk-ant-oat") && !got.contains("agents-own"), "the agent's credentials stay here: {got}");
        let said = proxy.stats.plan_error("zai").unwrap();
        assert!(said.contains("usage limit was reached") && said.contains("reset at 2026-10-04 02:00:00"), "{said}");
        assert_eq!(proxy.stats.session("7").limit_error.as_deref(), Some(said.as_str()));
        // Statistics: the refused call is a limit hit on that plan; what never went out (no key, not
        // a model call) isn't a call.
        let calls = proxy.stats.take_calls();
        assert_eq!(calls.len(), 1, "{calls:?}");
        let hit = &calls[0];
        assert_eq!(hit.route, "plan/zai");
        assert_eq!((hit.status, hit.session.as_str(), hit.model.as_deref()), (CallStatus::Limit, "7", Some("glm-x")));
        assert!(proxy.stats.take_calls().is_empty(), "taken once");
        // Another key: what the old one was told no longer holds.
        proxy.set_plans(HashMap::from([("zai".to_string(), plan::Plan { name: "GLM Coding Plan".into(), anthropic: None, openai: None, key: "plan-key-2".into() })]));
        assert_eq!(proxy.stats.plan_error("zai"), None);
    }

    /// A provider stand-in on a real port: `answer` gets each request (lowercased head, then the
    /// body) and says what to send back. What came is passed on.
    fn stand_in(answer: impl Fn(&str) -> String + Send + Sync + 'static) -> (String, std::sync::mpsc::Receiver<String>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}/api/anthropic", listener.local_addr().unwrap().port());
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        let answer = Arc::new(answer);
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut s) = conn else { continue };
                let (tx, answer) = (tx.clone(), answer.clone());
                std::thread::spawn(move || {
                    let mut buf = vec![];
                    let mut chunk = [0u8; 65536];
                    let head_end = loop {
                        let n = s.read(&mut chunk).unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        if let Some(i) = memchr::memmem::find(&buf, b"\r\n\r\n") {
                            break i + 4;
                        }
                    };
                    let head = String::from_utf8_lossy(&buf[..head_end]).to_lowercase();
                    let len: usize = head.lines().find_map(|l| l.strip_prefix("content-length:")).and_then(|v| v.trim().parse().ok()).unwrap_or(0);
                    while buf.len() < head_end + len {
                        let n = s.read(&mut chunk).unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                    }
                    let request = format!("{head}{}", String::from_utf8_lossy(&buf[head_end..]));
                    let reply = answer(&request);
                    let _ = tx.send(request);
                    let _ = s.write_all(reply.as_bytes());
                });
            }
        });
        (base, rx)
    }

    fn reply(status: &str, headers: &str, body: &str) -> String {
        format!("HTTP/1.1 {status}\r\n{headers}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
    }

    /// Stand-ins for a provider, for one test session: (session, provider) → base URL.
    static STAND_INS: std::sync::Mutex<Vec<(String, String, String)>> = std::sync::Mutex::new(Vec::new());

    pub(super) fn upstream_for(session: &str, provider: &str) -> Option<String> {
        STAND_INS.lock().unwrap().iter().find(|(s, p, _)| s == session && p == provider).map(|(_, _, u)| u.clone())
    }

    /// The Claude subscription limit as Anthropic answers it (status, headers and body as seen),
    /// its 5-hour window reset at `reset`.
    fn claude_spent(reset: u64) -> String {
        let headers = format!(
            "anthropic-ratelimit-unified-status: rejected\r\nanthropic-ratelimit-unified-5h-status: rejected\r\n\
             anthropic-ratelimit-unified-5h-reset: {reset}\r\nanthropic-ratelimit-unified-7d-status: allowed\r\n\
             anthropic-ratelimit-unified-representative-claim: five_hour\r\nanthropic-ratelimit-unified-reset: {reset}\r\n"
        );
        reply("429 Too Many Requests", &headers, r#"{"type":"error","error":{"type":"rate_limit_error","message":"This request would exceed your account's rate limit. Please try again later."}}"#)
    }

    /// Anthropic's stream, as a route that answers sends it.
    fn streamed(model: &str, text: &str) -> String {
        let body = format!(
            "event: message_start\ndata: {{\"type\":\"message_start\",\"message\":{{\"model\":\"{model}\",\"usage\":{{\"input_tokens\":120,\"output_tokens\":1}}}}}}\n\n\
             event: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":\"{text}\"}}}}\n\n\
             event: message_delta\ndata: {{\"type\":\"message_delta\",\"usage\":{{\"output_tokens\":9}}}}\n\n\
             event: message_stop\ndata: {{\"type\":\"message_stop\"}}\n\n"
        );
        format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
    }

    /// A session that was on a fallback goes back to its own route at a turn's start. When that
    /// route refuses again and nothing else answers (its chain since removed), the session isn't
    /// on the fallback any more: it says nothing stale, and the agent gets its own route's answer.
    #[test]
    fn back_on_its_own_route_is_off_the_fallback_even_when_refused() {
        use std::io::{Read, Write};
        let (a, _from_a) = stand_in(|_| reply("429 Too Many Requests", "", r#"{"error":{"code":"1308","message":"Usage limit reached for 5 hour. Your limit will reset at 2026-10-04 02:00:00"}}"#));
        let (b, _from_b) = stand_in(|_| streamed("glm-b", "PELICAN"));
        let proxy = Proxy::start(HashMap::new()).unwrap();
        let plan = |name: &str, base: &str, key: &str| plan::Plan { name: name.into(), anthropic: Some(base.into()), openai: None, key: key.into() };
        proxy.set_plans(HashMap::from([("a".to_string(), plan("Plan A", &a, "key-a")), ("b".to_string(), plan("Plan B", &b, "key-b"))]));
        proxy.set_fallback("8", Some(fallback::Chain { steps: vec![fallback::Step { route: "plan/b".into(), model: "glm-b".into(), name: "Plan B".into() }], on_outage: false }));
        let here = format!("127.0.0.1:{}", proxy.port);
        let send = |text: &str| {
            let path = proxy.base_url("8", "plan/a").strip_prefix(&format!("http://{here}")).unwrap().to_string() + "/v1/messages?beta=true";
            let body = json!({"model": "claude-opus-5-5", "max_tokens": 10, "stream": true, "messages": [{"role": "user", "content": text}]}).to_string();
            let mut c = std::net::TcpStream::connect(&here).unwrap();
            write!(c, "POST {path} HTTP/1.1\r\nHost: {here}\r\nanthropic-version: 2023-06-01\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            let mut out = String::new();
            let _ = c.read_to_string(&mut out);
            out
        };
        assert!(send("hi").contains("PELICAN"));
        assert!(proxy.stats.session("8").fallback.is_some(), "on Plan B");
        // The chain is removed; Plan A's time is up, and it refuses again as the next turn starts.
        proxy.set_fallback("8", None);
        proxy.stats.limited.lock().unwrap().get_mut("plan/a").unwrap().retry_at = 0;
        proxy.stats.update("8", |s| s.fallback.as_mut().unwrap().retry_at = 0);
        let out = send("again");
        assert!(out.starts_with("HTTP/1.1 429") && out.contains("1308"), "its own route's answer: {out}");
        assert!(proxy.stats.session("8").fallback.is_none(), "not on Plan B any more");
    }

    /// Over real connections, the whole way: a plan's spent window is answered by the next route
    /// in the chain, with that route's key and model and never the agent's credentials; the
    /// session says so and stays there mid-turn; after the reset it goes back as a turn starts,
    /// without the thinking the fallback did. Each route's usage is its own.
    #[test]
    fn a_spent_route_falls_back_and_comes_back() {
        use std::io::{Read, Write};
        use std::sync::atomic::AtomicBool;
        let reset = Arc::new(AtomicBool::new(false));
        let r = reset.clone();
        let (a, from_a) = stand_in(move |_| {
            if r.load(Ordering::Relaxed) {
                streamed("glm-a", "BACK")
            } else {
                reply("429 Too Many Requests", "", r#"{"error":{"code":"1308","message":"Usage limit reached for 5 hour. Your limit will reset at 2026-10-04 02:00:00"}}"#)
            }
        });
        let (b, from_b) = stand_in(|_| streamed("glm-b", "PELICAN"));
        let proxy = Proxy::start(HashMap::new()).unwrap();
        let plan = |name: &str, base: &str, key: &str| plan::Plan { name: name.into(), anthropic: Some(base.into()), openai: None, key: key.into() };
        proxy.set_plans(HashMap::from([
            ("a".to_string(), plan("Plan A", &a, "key-a")),
            ("b".to_string(), plan("Plan B", &b, "key-b")),
            ("stolen".to_string(), plan("Not a plan", &b, "sk-ant-oat01-a-claude-login")),
        ]));
        let step = |route: &str, model: &str, name: &str| fallback::Step { route: route.into(), model: model.into(), name: name.into() };
        proxy.set_fallback("7", Some(fallback::Chain { steps: vec![step("plan/stolen", "x", "Not a plan"), step("plan/b", "glm-b", "Plan B")], on_outage: false }));
        let here = format!("127.0.0.1:{}", proxy.port);
        let send = |messages: serde_json::Value| {
            let path = proxy.base_url("7", "plan/a").strip_prefix(&format!("http://{here}")).unwrap().to_string() + "/v1/messages?beta=true";
            let body = json!({"model": "claude-opus-5-5", "max_tokens": 10, "stream": true, "messages": messages}).to_string();
            let mut c = std::net::TcpStream::connect(&here).unwrap();
            write!(c, "POST {path} HTTP/1.1\r\nHost: {here}\r\nAuthorization: Bearer sk-ant-oat01-the-users-claude-login\r\nanthropic-version: 2023-06-01\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            let mut out = String::new();
            let _ = c.read_to_string(&mut out);
            out
        };
        let wait = || std::time::Duration::from_secs(5);
        let user = |text: &str| json!({"role": "user", "content": text});
        let tool = json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1", "content": "ok"}]});

        // The window is spent: Plan B answers, the agent sees only its answer.
        let out = send(json!([user("hi")]));
        assert!(out.starts_with("HTTP/1.1 200") && out.contains("PELICAN"), "{out}");
        assert!(from_a.recv_timeout(wait()).is_ok());
        let got = from_b.recv_timeout(wait()).unwrap();
        assert!(got.starts_with("post /api/anthropic/v1/messages?beta=true "), "{got}");
        assert!(got.contains("x-api-key: key-b\r\n") && got.contains("\"model\":\"glm-b\""), "its key, its model: {got}");
        assert!(!got.contains("sk-ant-oat") && !got.contains("key-a"), "nobody else's credentials: {got}");
        let s = proxy.stats.session("7");
        let f = s.fallback.clone().expect("on fallback");
        assert_eq!((f.route.as_str(), f.name.as_str(), f.model.as_str(), f.from_name.as_str(), f.kind), ("plan/b", "Plan B", "glm-b", "Plan A", fallback::Kind::Quota));
        assert!(f.said.contains("reset at 2026-10-04 02:00:00"));
        assert_eq!((s.last_error, s.limit_error), (None, None), "the agent's turn didn't fail");
        assert!(proxy.stats.limited("plan/a").is_some(), "known spent, for new sessions");
        assert!(proxy.stats.plan_error("a").is_some_and(|e| e.contains("usage limit was reached")), "and Settings says so");

        // Mid-turn: the spent route isn't asked again.
        assert!(send(json!([user("hi"), {"role": "assistant", "content": [{"type": "tool_use", "id": "t1", "name": "Bash", "input": {}}]}, tool])).contains("PELICAN"));
        assert!(from_a.recv_timeout(std::time::Duration::from_millis(300)).is_err(), "Plan A not asked while it's spent");
        from_b.recv_timeout(wait()).unwrap();

        // Its window resets. Mid-turn it stays put; as the next turn starts it goes back, and the
        // fallback's thinking stays behind.
        reset.store(true, Ordering::Relaxed);
        proxy.stats.limited.lock().unwrap().get_mut("plan/a").unwrap().retry_at = 0;
        proxy.stats.update("7", |s| s.fallback.as_mut().unwrap().retry_at = 0);
        assert!(send(json!([user("hi"), tool])).contains("PELICAN"));
        assert!(from_a.recv_timeout(std::time::Duration::from_millis(300)).is_err(), "not back mid-turn");
        from_b.recv_timeout(wait()).unwrap();
        let thought = json!({"role": "assistant", "content": [{"type": "thinking", "thinking": "hmm", "signature": "from-b"}, {"type": "text", "text": "PELICAN"}]});
        let out = send(json!([user("hi"), thought, user("again")]));
        assert!(out.contains("BACK"), "{out}");
        let got = from_a.recv_timeout(wait()).unwrap();
        assert!(!got.contains("from-b") && got.contains("\"model\":\"claude-opus-5-5\""), "{got}");
        let s = proxy.stats.session("7");
        assert!(s.fallback.is_none() && proxy.stats.limited("plan/a").is_none());
        let used: Vec<(&str, u64, u64)> = s.by_route.iter().map(|r| (r.route.as_str(), r.usage.input, r.usage.output)).collect();
        assert_eq!(used, [("plan/b", 360, 27), ("plan/a", 120, 9)]);
        assert_eq!(s.usage.output, 36);
        // Statistics: Plan A's limit hit, then three calls Plan B answered in its place, then
        // Plan A again, its own.
        let calls: Vec<(String, Option<String>, CallStatus)> = proxy.stats.take_calls().into_iter().map(|c| (c.route, c.fallback, c.status)).collect();
        let call = |route: &str, fallback: Option<&str>, status| (route.to_string(), fallback.map(String::from), status);
        let b = call("plan/b", Some("plan/a"), CallStatus::Ok);
        assert_eq!(calls, [call("plan/a", None, CallStatus::Limit), b.clone(), b.clone(), b, call("plan/a", None, CallStatus::Ok)]);
    }

    /// The user's other Claude accounts: Claude Code's own account at its limit, the same call
    /// goes on signed by the next account that isn't (that token, no other credentials), the turn
    /// doesn't fail, the session says which account answers; back on its own once its window
    /// resets. A token that isn't a Claude subscription is no account, and an agent that didn't
    /// sign in with a subscription gets none of them.
    #[test]
    fn a_spent_claude_account_goes_on_with_the_next() {
        use std::io::{Read, Write};
        use std::sync::atomic::AtomicBool;
        let reset = Arc::new(AtomicBool::new(false));
        let r = reset.clone();
        let spent_until = fallback::now() + 3600;
        let (anthropic, from) = stand_in(move |req| {
            // Its own until the reset, and account 2 throughout, are spent.
            let own = req.contains("authorization: bearer sk-ant-oat01-own");
            if (own && !r.load(Ordering::Relaxed)) || req.contains("authorization: bearer sk-ant-oat01-third") {
                claude_spent(spent_until)
            } else if req.contains("sk-ant-oat01-own") {
                streamed("claude-opus-5-5", "OWN")
            } else {
                // Account 3 reports its own windows.
                let h = format!("anthropic-ratelimit-unified-status: allowed\r\nanthropic-ratelimit-unified-5h-utilization: 0.07\r\nanthropic-ratelimit-unified-5h-reset: {}\r\n", spent_until + 600);
                streamed("claude-opus-5-5", "OTHER").replacen("Content-Type", &format!("{h}Content-Type"), 1)
            }
        });
        let base = anthropic.trim_end_matches("/api/anthropic").to_string();
        STAND_INS.lock().unwrap().extend([("acct".into(), "anthropic".into(), base.clone()), ("acct-api".into(), "anthropic".into(), base)]);
        let keys = HashMap::from([
            ("CLAUDE_ACCOUNT_3".to_string(), "sk-ant-oat01-fourth".to_string()),
            ("CLAUDE_ACCOUNT_2".to_string(), "sk-ant-oat01-third".to_string()),
            ("CLAUDE_ACCOUNT_5".to_string(), "sk-ant-api03-not-an-account".to_string()),
        ]);
        let proxy = Proxy::start(keys).unwrap();
        let here = format!("127.0.0.1:{}", proxy.port);
        let turn = json!([{"role": "user", "content": "hi"}]);
        let tool = json!([{"role": "user", "content": "hi"}, {"role": "assistant", "content": [{"type": "tool_use", "id": "t1", "name": "Bash", "input": {}}]}, {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1", "content": "ok"}]}]);
        let ask = |session: &str, auth: &str, messages: &Value| {
            let path = proxy.base_url(session, "anthropic").strip_prefix(&format!("http://{here}")).unwrap().to_string() + "/v1/messages?beta=true";
            let body = json!({"model": "claude-opus-5-5", "max_tokens": 10, "stream": true, "messages": messages}).to_string();
            let mut c = std::net::TcpStream::connect(&here).unwrap();
            write!(c, "POST {path} HTTP/1.1\r\nHost: {here}\r\nAuthorization: Bearer {auth}\r\nanthropic-version: 2023-06-01\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            let mut out = String::new();
            let _ = c.read_to_string(&mut out);
            out
        };
        let send = |session: &str, auth: &str| ask(session, auth, &turn);
        let wait = || std::time::Duration::from_secs(5);
        let auth_of = |req: &str| req.lines().find_map(|l| l.strip_prefix("authorization: ")).unwrap_or_default().to_string();

        // Its own account is spent; account 2 is too; account 3 answers. The agent sees its answer.
        let out = send("acct", "sk-ant-oat01-own");
        assert!(out.starts_with("HTTP/1.1 200") && out.contains("OTHER"), "{out}");
        let asked: Vec<String> = (0..3).map(|_| auth_of(&from.recv_timeout(wait()).unwrap())).collect();
        assert_eq!(asked, ["bearer sk-ant-oat01-own", "bearer sk-ant-oat01-third", "bearer sk-ant-oat01-fourth"]);
        assert!(from.recv_timeout(std::time::Duration::from_millis(300)).is_err(), "not the key that isn't an account");
        let s = proxy.stats.session("acct");
        let f = s.fallback.clone().expect("says which account answers");
        assert_eq!((f.name.as_str(), f.from_name.as_str(), f.resets_at), ("Claude account 3", "Claude", Some(spent_until)));
        assert_eq!((s.last_error, s.limit_error), (None, None), "the turn didn't fail");
        let own = proxy.stats.limited_routes().into_iter().find(|(k, _)| k.starts_with("anthropic#") && !k.contains("account")).unwrap().0;
        assert!(proxy.stats.limited(&own).is_some() && proxy.spent(&own).is_none(), "Claude Code isn't at its limit while another account answers");
        // As Settings lists them: its own and account 2 spent until the reset, account 3 not; by
        // token, so reordered they stay as they were found.
        let (own_spent, others) = proxy.claude_accounts(&["sk-ant-oat01-fourth", "sk-ant-oat01-third"]);
        assert_eq!(own_spent.map(|l| l.resets_at), Some(Some(spent_until)));
        assert_eq!(others.iter().map(|l| l.as_ref().map(|l| l.resets_at)).collect::<Vec<_>>(), [None, Some(Some(spent_until))]);

        // Each account's windows are its own: account 3's use isn't Claude Code's own account's,
        // which keeps its spent window; account 2 reported none.
        let now = proxy.claude_accounts_now().expect("more than one account");
        let window = |n: u32| now.iter().find(|a| a.0 == n).and_then(|a| a.2.clone()).map(|q| q.windows.into_iter().map(|(name, w)| (name, w.utilization)).collect::<Vec<_>>());
        assert_eq!(window(3), Some(vec![("5h".to_string(), 0.07)]));
        assert_eq!(window(1).map(|w| w.into_iter().map(|(n, _)| n).collect::<Vec<_>>()), Some(vec!["5h".to_string()]));
        assert!(proxy.stats.quota("anthropic").unwrap().windows.iter().all(|(_, w)| w.utilization != 0.07));
        assert_eq!(now.iter().map(|a| (a.0, a.1.is_some())).collect::<Vec<_>>(), [(1, true), (2, true), (3, false)]);

        // The next call goes straight to account 3.
        assert!(send("acct", "sk-ant-oat01-own").contains("OTHER"));
        assert_eq!(auth_of(&from.recv_timeout(wait()).unwrap()), "bearer sk-ant-oat01-fourth");
        assert!(from.recv_timeout(std::time::Duration::from_millis(300)).is_err());

        // Its own found spent again meanwhile, until later: the session says the later reset.
        proxy.stats.limited.lock().unwrap().get_mut(&own).unwrap().resets_at = Some(spent_until + 60);
        assert!(send("acct", "sk-ant-oat01-own").contains("OTHER"));
        assert_eq!(auth_of(&from.recv_timeout(wait()).unwrap()), "bearer sk-ant-oat01-fourth");
        assert_eq!(proxy.stats.session("acct").fallback.and_then(|f| f.resets_at), Some(spent_until + 60));

        // Its window resets: mid-turn, the turn stays on account 3; back on its own as the next starts.
        reset.store(true, Ordering::Relaxed);
        proxy.stats.limited.lock().unwrap().get_mut(&own).unwrap().retry_at = 0;
        assert!(ask("acct", "sk-ant-oat01-own", &tool).contains("OTHER"));
        assert_eq!(auth_of(&from.recv_timeout(wait()).unwrap()), "bearer sk-ant-oat01-fourth");
        assert!(send("acct", "sk-ant-oat01-own").contains("OWN"));
        assert_eq!(auth_of(&from.recv_timeout(wait()).unwrap()), "bearer sk-ant-oat01-own");
        assert!(proxy.stats.session("acct").fallback.is_none(), "back on its own");

        // An API key isn't a subscription: no account signs its calls.
        send("acct-api", "sk-ant-api03-a-key");
        assert_eq!(auth_of(&from.recv_timeout(wait()).unwrap()), "bearer sk-ant-api03-a-key");
        assert!(from.recv_timeout(std::time::Duration::from_millis(300)).is_err());
    }

    /// Every Claude account spent: the agent is given the refusal of the account back first, not
    /// that of the last one asked, so it waits only as long as it has to.
    #[test]
    fn with_every_account_spent_the_agent_waits_for_the_first_back() {
        use std::io::{Read, Write};
        use std::sync::atomic::AtomicBool;
        let two_spent = Arc::new(AtomicBool::new(false));
        let t = two_spent.clone();
        let (own_back, two_back) = (fallback::now() + 1800, fallback::now() + 3 * 3600);
        let (anthropic, from) = stand_in(move |req| {
            if req.contains("authorization: bearer sk-ant-oat01-own") {
                // Its own account says so with a wait too.
                claude_spent(own_back).replacen("Content-Type", "retry-after: 1800\r\nContent-Type", 1)
            } else if t.load(Ordering::Relaxed) {
                claude_spent(two_back)
            } else {
                streamed("claude-opus-5-5", "TWO")
            }
        });
        let base = anthropic.trim_end_matches("/api/anthropic").to_string();
        STAND_INS.lock().unwrap().push(("allspent".into(), "anthropic".into(), base));
        let keys = HashMap::from([("CLAUDE_ACCOUNT_2".to_string(), "sk-ant-oat01-two".to_string())]);
        let proxy = Proxy::start(keys).unwrap();
        let here = format!("127.0.0.1:{}", proxy.port);
        let send = || {
            let path = proxy.base_url("allspent", "anthropic").strip_prefix(&format!("http://{here}")).unwrap().to_string() + "/v1/messages?beta=true";
            let body = json!({"model": "claude-opus-5-5", "max_tokens": 10, "stream": true, "messages": [{"role": "user", "content": "hi"}]}).to_string();
            let mut c = std::net::TcpStream::connect(&here).unwrap();
            write!(c, "POST {path} HTTP/1.1\r\nHost: {here}\r\nAuthorization: Bearer sk-ant-oat01-own\r\nanthropic-version: 2023-06-01\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            let mut out = String::new();
            let _ = c.read_to_string(&mut out);
            out.to_lowercase()
        };
        let asked = || {
            let mut v = vec![];
            while let Ok(r) = from.recv_timeout(std::time::Duration::from_millis(500)) {
                v.push(r.lines().find_map(|l| l.strip_prefix("authorization: bearer ")).unwrap_or_default().to_string());
            }
            v
        };
        let reset = |out: &str| out.lines().find_map(|l| l.strip_prefix("anthropic-ratelimit-unified-reset: ")).and_then(|v| v.trim().parse::<u64>().ok());

        // Its own account is spent: account 2 answers, and the session stays on it.
        assert!(send().contains("two"));
        assert_eq!(asked(), ["sk-ant-oat01-own", "sk-ant-oat01-two"]);

        // Account 2 runs out too, later than its own comes back: asked alone (its own is known
        // spent), it refuses, and the agent is told its own account's reset, not account 2's.
        two_spent.store(true, Ordering::Relaxed);
        let out = send();
        assert!(out.starts_with("http/1.1 429"), "{out}");
        assert_eq!(asked(), ["sk-ant-oat01-two"]);
        assert_eq!(reset(&out), Some(own_back), "{out}");
        let wait: u64 = out.lines().find_map(|l| l.strip_prefix("retry-after: ")).and_then(|v| v.trim().parse().ok()).unwrap();
        assert!((1..=1800).contains(&wait), "counted down to now: {wait}");

        // Asked again with every account spent: its own is, and says the same.
        let out = send();
        assert_eq!(reset(&out), Some(own_back), "{out}");
        assert_eq!(asked(), ["sk-ant-oat01-own"]);
    }

    /// Claude Code's one-token check as it starts, which Anthropic answers with a bare 429 when
    /// the account has no extra usage, and a short rate limit: errors of the call, not the
    /// account's limit. A spent subscription is a limit. Only real windows (with a reset) are
    /// kept from Anthropic's headers, as it sends them.
    #[test]
    fn a_bare_429_is_not_a_limit_and_windows_are_windows() {
        use std::io::{Read, Write};
        let reset = fallback::now() + 3600;
        let (anthropic, _from) = stand_in(move |req| {
            if req.contains("\"max_tokens\":1,") {
                reply("429 Too Many Requests", "", r#"{"type":"error","error":{"type":"rate_limit_error","message":"Error"}}"#)
            } else if req.contains("retry-me") {
                reply("429 Too Many Requests", "retry-after: 3\r\n", r#"{"type":"error","error":{"type":"rate_limit_error","message":"Number of request tokens has exceeded your per-minute rate limit"}}"#)
            } else if req.contains("spent-now") {
                claude_spent(reset)
            } else {
                let h = format!(
                    "anthropic-ratelimit-unified-status: allowed\r\nanthropic-ratelimit-unified-5h-status: allowed\r\n\
                     anthropic-ratelimit-unified-5h-utilization: 0.02\r\nanthropic-ratelimit-unified-5h-reset: {reset}\r\n\
                     anthropic-ratelimit-unified-7d-status: allowed\r\nanthropic-ratelimit-unified-7d-utilization: 0.18\r\n\
                     anthropic-ratelimit-unified-7d-reset: {}\r\nanthropic-ratelimit-unified-overage-status: rejected\r\n\
                     anthropic-ratelimit-unified-overage-disabled-reason: org_level_disabled\r\n\
                     anthropic-ratelimit-unified-representative-claim: five_hour\r\nanthropic-ratelimit-unified-fallback-percentage: 0.5\r\n\
                     anthropic-ratelimit-unified-reset: {reset}\r\n",
                    reset + 86400
                );
                reply("200 OK", &h, r#"{"id":"m","type":"message","role":"assistant","content":[{"type":"text","text":"OK"}],"model":"claude-opus-5-5","stop_reason":"end_turn","usage":{"input_tokens":5,"output_tokens":1}}"#)
            }
        });
        let base = anthropic.trim_end_matches("/api/anthropic").to_string();
        STAND_INS.lock().unwrap().push(("probe".into(), "anthropic".into(), base));
        let proxy = Proxy::start(HashMap::new()).unwrap();
        let here = format!("127.0.0.1:{}", proxy.port);
        let ask = |max_tokens: u32, text: &str| {
            let path = proxy.base_url("probe", "anthropic").strip_prefix(&format!("http://{here}")).unwrap().to_string() + "/v1/messages?beta=true";
            let body = json!({"model": "claude-opus-5-5", "max_tokens": max_tokens, "messages": [{"role": "user", "content": text}]}).to_string();
            let mut c = std::net::TcpStream::connect(&here).unwrap();
            write!(c, "POST {path} HTTP/1.1\r\nHost: {here}\r\nAuthorization: Bearer sk-ant-oat01-own\r\nanthropic-version: 2023-06-01\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            let mut out = String::new();
            let _ = c.read_to_string(&mut out);
            out
        };
        assert!(ask(1, "quota").starts_with("HTTP/1.1 429"));
        assert!(ask(10, "retry-me").starts_with("HTTP/1.1 429"));
        assert!(ask(10, "hi").starts_with("HTTP/1.1 200"));
        assert!(ask(10, "spent-now").starts_with("HTTP/1.1 429"));
        let status: Vec<CallStatus> = proxy.stats.take_calls().into_iter().map(|c| c.status).collect();
        assert_eq!(status, [CallStatus::Error, CallStatus::Error, CallStatus::Ok, CallStatus::Limit]);
        let quota = proxy.stats.quotas.lock().unwrap().get("anthropic").cloned().expect("windows");
        let names: Vec<&str> = quota.windows.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["5h", "7d"], "only windows with a reset");
    }

    /// ChatGPT rejects a Codex model the account lists: the next one it lists answers, the session
    /// says so, and later calls go straight to it. Choosing another listed model clears the note.
    #[test]
    fn a_rejected_codex_model_is_answered_by_the_next_and_shown() {
        use std::io::{Read, Write};
        let dir = std::env::temp_dir().join(format!("dino-codex-cache-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cache = dir.join("models_cache.json");
        std::fs::write(&cache, codex::tests::CACHE_JSON).unwrap();
        *codex::tests::CACHE.lock().unwrap() = Some(cache);
        let (base, seen) = stand_in(|req| {
            if req.contains(r#""model":"gpt-5.5""#) {
                reply("404 Not Found", "", r#"{"error":{"message":"The model `gpt-5.5` does not exist or you do not have access to it.","type":"invalid_request_error","param":null,"code":"model_not_found"}}"#)
            } else {
                reply("200 OK", "", r#"{"id":"r","object":"response","status":"completed","output":[],"usage":{"input_tokens":5,"output_tokens":1,"total_tokens":6}}"#)
            }
        });
        let base = base.trim_end_matches("/api/anthropic").to_string();
        STAND_INS.lock().unwrap().push(("codex-sub".into(), "chatgpt".into(), base));
        let proxy = Proxy::start(HashMap::new()).unwrap();
        let here = format!("127.0.0.1:{}", proxy.port);
        let ask = |model: &str| {
            let path = proxy.base_url("codex-sub", "chatgpt").strip_prefix(&format!("http://{here}")).unwrap().to_string() + "/codex/responses";
            let body = json!({"model": model, "input": [{"role": "user", "content": "hi"}], "stream": false}).to_string();
            let mut c = std::net::TcpStream::connect(&here).unwrap();
            write!(c, "POST {path} HTTP/1.1\r\nHost: {here}\r\nAuthorization: Bearer chatgpt-token\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            let mut out = String::new();
            let _ = c.read_to_string(&mut out);
            out
        };
        let model_of = |req: String| serde_json::from_str::<Value>(&req[req.find("\r\n\r\n").unwrap() + 4..]).unwrap()["model"].as_str().unwrap().to_string();

        assert!(ask("gpt-5.5").starts_with("HTTP/1.1 200"));
        assert_eq!([model_of(seen.recv().unwrap()), model_of(seen.recv().unwrap())], ["gpt-5.5", "gpt-5.4"]);
        let st = proxy.stats.session("codex-sub");
        let sub = st.substitute.expect("shown on the session");
        assert_eq!((sub.rejected.as_str(), sub.using.as_str()), ("gpt-5.5", "gpt-5.4"));
        assert!(sub.said.contains("does not exist"), "{}", sub.said);
        assert_eq!(st.last_model.as_deref(), Some("gpt-5.4"));

        // Remembered: no second 404 first.
        assert!(ask("gpt-5.5").starts_with("HTTP/1.1 200"));
        assert_eq!(model_of(seen.recv().unwrap()), "gpt-5.4");
        assert!(seen.try_recv().is_err());
        assert!(proxy.stats.session("codex-sub").substitute.is_some());

        // A hidden model Codex uses on its own leaves the note; another listed one clears it.
        assert!(ask("codex-auto-review").starts_with("HTTP/1.1 200"));
        assert!(proxy.stats.session("codex-sub").substitute.is_some());
        assert!(ask("gpt-5.4-mini").starts_with("HTTP/1.1 200"));
        assert_eq!(proxy.stats.session("codex-sub").substitute, None);
        *codex::tests::CACHE.lock().unwrap() = None;
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The real thing, by hand: `DINO_CLAUDE_ACCOUNT_FILE=<file with a setup-token> cargo test -p
    /// dino-proxy --lib -- --ignored real_claude_account`. With Claude Code's own account spent,
    /// Anthropic answers the call signed by the other account.
    #[test]
    #[ignore]
    fn real_claude_account_answers() {
        use std::io::{Read, Write};
        let token = std::fs::read_to_string(std::env::var("DINO_CLAUDE_ACCOUNT_FILE").unwrap()).unwrap().trim().to_string();
        let proxy = Proxy::start(HashMap::from([("CLAUDE_ACCOUNT_2".to_string(), token)])).unwrap();
        let mut h = HeaderMap::new();
        h.insert("authorization", "Bearer sk-ant-oat01-own-spent".parse().unwrap());
        let own = fallback::route_key("anthropic", &h);
        proxy.stats.mark_limited(&own, "Claude", &fallback::Trigger { kind: fallback::Kind::Quota, resets_at: Some(fallback::now() + 600), said: "spent".into() });
        let here = format!("127.0.0.1:{}", proxy.port);
        let path = proxy.base_url("real", "anthropic").strip_prefix(&format!("http://{here}")).unwrap().to_string() + "/v1/messages?beta=true";
        let body = json!({"model": "claude-haiku-4-5", "max_tokens": 20, "system": "You are Claude Code, Anthropic's official CLI for Claude.", "messages": [{"role": "user", "content": "Reply with the word OK"}]}).to_string();
        let mut c = std::net::TcpStream::connect(&here).unwrap();
        write!(c, "POST {path} HTTP/1.1\r\nHost: {here}\r\nAuthorization: Bearer sk-ant-oat01-own-spent\r\nanthropic-version: 2023-06-01\r\nanthropic-beta: oauth-2025-04-20\r\ncontent-type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        let mut out = String::new();
        let _ = c.read_to_string(&mut out);
        assert!(out.starts_with("HTTP/1.1 200") && out.contains("OK"), "{}", out.lines().next().unwrap_or_default());
        assert_eq!(proxy.stats.session("real").fallback.map(|f| f.name), Some("Claude account 2".into()));
    }

    /// What isn't a spent route doesn't move a session: a short rate limit the agent waits out,
    /// an overload unless the chain counts outages. With it, three in a row do. With every route
    /// spent, the agent gets its own route's answer.
    #[test]
    fn only_a_spent_or_down_route_moves_a_session() {
        use std::io::{Read, Write};
        use std::sync::atomic::AtomicUsize;
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let (a, _from_a) = stand_in(move |_| match c.fetch_add(1, Ordering::Relaxed) {
            0 => reply("429 Too Many Requests", "Retry-After: 20\r\n", r#"{"type":"error","error":{"type":"rate_limit_error","message":"Rate limit reached for requests"}}"#),
            _ => reply("529 Overloaded", "", r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#),
        });
        let (b, from_b) = stand_in(|_| streamed("b", "FROM-B"));
        let (c2, _) = stand_in(|_| reply("402 Payment Required", "", r#"{"error":{"message":"Insufficient credits","code":402}}"#));
        let proxy = Proxy::start(HashMap::new()).unwrap();
        let plan = |base: &str| plan::Plan { name: base.into(), anthropic: Some(base.into()), openai: None, key: "k".into() };
        proxy.set_plans(HashMap::from([("a".to_string(), plan(&a)), ("b".to_string(), plan(&b)), ("c".to_string(), plan(&c2))]));
        let step = |route: &str| fallback::Step { route: route.into(), model: "m".into(), name: route.into() };
        let here = format!("127.0.0.1:{}", proxy.port);
        let send = |session: &str| {
            let path = proxy.base_url(session, "plan/a").strip_prefix(&format!("http://{here}")).unwrap().to_string() + "/v1/messages";
            let body = r#"{"model":"m","max_tokens":10,"messages":[{"role":"user","content":"hi"}]}"#;
            let mut c = std::net::TcpStream::connect(&here).unwrap();
            write!(c, "POST {path} HTTP/1.1\r\nHost: {here}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            let mut out = String::new();
            let _ = c.read_to_string(&mut out);
            out
        };
        proxy.set_fallback("1", Some(fallback::Chain { steps: vec![step("plan/b")], on_outage: false }));
        assert!(send("1").starts_with("HTTP/1.1 429"), "a short rate limit is the agent's to wait out");
        assert!(send("1").starts_with("HTTP/1.1 529"), "an overload, when outages don't count");
        assert!(from_b.try_recv().is_err() && proxy.stats.limited("plan/a").is_none());

        proxy.set_fallback("2", Some(fallback::Chain { steps: vec![step("plan/b")], on_outage: true }));
        assert!(send("2").starts_with("HTTP/1.1 529") && send("2").starts_with("HTTP/1.1 529"), "the agent's own retries first");
        assert!(send("2").contains("FROM-B"), "then the next route");
        assert_eq!(proxy.stats.session("2").fallback.map(|f| f.kind), Some(fallback::Kind::Outage));
        assert!(send("1").starts_with("HTTP/1.1 529"), "down only counts for a chain that counts it");

        proxy.set_fallback("3", Some(fallback::Chain { steps: vec![step("plan/c")], on_outage: true }));
        let out = send("3");
        assert!(out.starts_with("HTTP/1.1 529") && out.contains("Overloaded"), "every route spent: its own answer: {out}");
        assert_eq!(proxy.stats.limited("plan/c").map(|l| l.kind), Some(fallback::Kind::Balance));
        assert!(proxy.stats.session("3").fallback.is_none());
    }

    #[test]
    fn only_dinos_agents_get_in() {
        let h = |pairs: &[(&'static str, &str)]| {
            let mut m = HeaderMap::new();
            for (k, v) in pairs {
                m.insert(*k, v.parse().unwrap());
            }
            m
        };
        let here = h(&[("host", "127.0.0.1:5000")]);
        assert!(admitted("s3cret", 5000, "s3cret", &here));
        assert!(admitted("s3cret", 5000, "s3cret", &h(&[("host", "localhost:5000")])));
        assert!(!admitted("s3cret", 5000, "s3cres", &here) && !admitted("s3cret", 5000, "s3cret!", &here) && !admitted("s3cret", 5000, "", &here));
        // DNS rebinding: the right address under someone else's name.
        assert!(!admitted("s3cret", 5000, "s3cret", &h(&[("host", "evil.example:5000")])));
        assert!(!admitted("s3cret", 5000, "s3cret", &h(&[("host", "127.0.0.1:5001")])));
        assert!(!admitted("s3cret", 5000, "s3cret", &HeaderMap::new()));
        // A browser.
        assert!(!admitted("s3cret", 5000, "s3cret", &h(&[("host", "127.0.0.1:5000"), ("origin", "http://evil.example")])));
    }

    #[test]
    fn paths_that_stay_inside_the_api() {
        assert_eq!(clean_path("v1/messages"), Some("v1/messages"));
        assert_eq!(clean_path("/v1/messages/"), Some("v1/messages"), "a stray slash still counts as a model call");
        assert_eq!(clean_path("v1/models/gpt-5.5"), Some("v1/models/gpt-5.5"));
        assert_eq!(clean_path("codex/responses"), Some("codex/responses"));
        let bad = [
            "v1/models/../../v1/files", "v1/./messages", "v1//messages", "v1/%2e%2e/files", "v1/%2F/files", "v1/..\\files",
            "v1/messages?x", "v1/messages#", "v1/mes sages", "v1/.\t./files", "v1/é", "", "/",
        ];
        for rest in bad {
            assert_eq!(clean_path(rest), None, "{rest:?}");
        }
    }

    /// Over a real connection: nothing is served without the secret, or to anyone but agents here.
    #[test]
    fn the_proxy_wants_its_secret() {
        use std::io::{Read, Write};
        let proxy = Proxy::start(HashMap::new()).unwrap();
        let port = proxy.port;
        let send = |host: &str, path: &str, extra: &str, body: &str| {
            let mut c = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
            write!(c, "POST {path} HTTP/1.1\r\nHost: {host}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            let mut out = String::new();
            let _ = c.read_to_string(&mut out);
            out.split(' ').nth(1).unwrap_or_default().to_string()
        };
        let here = format!("127.0.0.1:{port}");
        let origin = format!("http://{here}");
        assert!(proxy.root.starts_with(&format!("{origin}/k/")) && proxy.root.len() == origin.len() + 3 + 32);
        let hook = proxy.base_url("7", "hook");
        let hook = hook.strip_prefix(&origin).unwrap();
        assert_eq!(send(&here, "/s/7/hook", "", ""), "404");
        assert_eq!(send(&here, &format!("/k/{}/s/7/hook", "0".repeat(32)), "", ""), "403");
        assert_eq!(send("evil.example", hook, "", ""), "403");
        assert_eq!(send(&here, hook, "Origin: http://evil.example\r\n", ""), "403");
        assert_eq!(proxy.stats.session("7").reported_context, None);
        assert_eq!(send(&here, hook, "", r#"{"context_window":{"context_window_size":1000}}"#), "200");
        assert_eq!(proxy.stats.session("7").reported_context.map(|c| c.window), Some(1000));
        // The ChatGPT plan's allowlist sees the path that goes out, however it was spelled.
        let siwc = proxy.base_url("7", "siwc");
        let siwc = siwc.strip_prefix(&origin).unwrap();
        assert_eq!(send(&here, &format!("{siwc}/v1/models/%2e%2e/%2e%2e/v1/files"), "", ""), "400");
        assert_eq!(send(&here, &format!("{siwc}/v1/models/../../v1/files"), "", ""), "400");
        assert_eq!(send(&here, &format!("{siwc}/v1/files"), "", ""), "404");
        // Without the secret in the path, the header must carry it; it never goes upstream.
        let keyed = proxy.header_base_url("7", "siwc");
        assert!(!keyed.contains(proxy.secret()));
        let keyed = keyed.strip_prefix(&origin).unwrap();
        assert_eq!(send(&here, &format!("{keyed}/v1/files"), "", ""), "403");
        assert_eq!(send(&here, &format!("{keyed}/v1/files"), &format!("{KEY_HEADER}: {}\r\n", "0".repeat(32)), ""), "403");
        assert_eq!(send(&here, &format!("{keyed}/v1/files"), &format!("{KEY_HEADER}: {}\r\n", proxy.secret()), ""), "404");
        assert!(hop_by_hop(&HeaderName::from_static(KEY_HEADER)));
    }

    /// Claude Code's hooks (and Qwen's, in the same shape) say when it uses the Mac or a browser.
    #[test]
    fn hooks_say_when_it_uses_the_mac() {
        use std::io::{Read, Write};
        let proxy = Proxy::start(HashMap::new()).unwrap();
        let origin = format!("http://127.0.0.1:{}", proxy.port);
        let hook = proxy.base_url("7", "hook");
        let hook = hook.strip_prefix(&origin).unwrap().to_string();
        let post = |body: serde_json::Value| {
            let body = body.to_string();
            let mut c = std::net::TcpStream::connect(("127.0.0.1", proxy.port)).unwrap();
            write!(c, "POST {hook} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", proxy.port, body.len()).unwrap();
            let _ = c.read_to_string(&mut String::new());
        };
        let call = |event: &str, tool: &str, input: serde_json::Value| json!({"hook_event_name": event, "tool_name": tool, "tool_input": input});
        post(call("PreToolUse", "Bash", json!({"command": "ls"})));
        assert_eq!(proxy.stats.using("7"), None);
        post(call("PreToolUse", "mcp__computer-use__screenshot", json!({})));
        assert_eq!(proxy.stats.using("7"), Some(computer::Reach::Computer));
        post(json!({"hook_event_name": "Stop"}));
        proxy.stats.update("7", |s| s.computer.as_mut().unwrap().last -= computer::LINGER);
        assert_eq!(proxy.stats.using("7"), None, "the turn is over");
        // Through Qwen's bridge for the tools it defers.
        post(call("PreToolUse", "tool_call", json!({"name": "mcp__claude-in-chrome__navigate", "params": {}})));
        assert_eq!(proxy.stats.using("7"), Some(computer::Reach::Browser));
    }

    #[test]
    fn the_model_is_the_one_claude_says_it_switched_to() {
        use std::io::{Read, Write};
        let proxy = Proxy::start(HashMap::new()).unwrap();
        let origin = format!("http://127.0.0.1:{}", proxy.port);
        let hook = proxy.base_url("7", "hook");
        let hook = hook.strip_prefix(&origin).unwrap().to_string();
        let post = |body: serde_json::Value| {
            let body = body.to_string();
            let mut c = std::net::TcpStream::connect(("127.0.0.1", proxy.port)).unwrap();
            write!(c, "POST {hook} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", proxy.port, body.len()).unwrap();
            let mut answer = String::new();
            let _ = c.read_to_string(&mut answer);
            answer
        };
        let switch = |from: &str, to: &str, source: &str| {
            json!({"hook_event_name": "PostModelSwitch", "session_id": "c1", "from_model": from, "to_model": to, "requested_model": null, "source": source, "context_tokens": 0})
        };
        assert_eq!(proxy.stats.session("7").agent_model, None, "nothing said: what it was started with");
        post(json!({"hook_event_name": "Stop"}));
        proxy.stats.report("7", Activity::Done);
        // `/model sonnet` in Claude itself: Claude Code 2.1.291's input, as the hook gets it.
        let answer = post(switch("claude-opus-5-5", "claude-sonnet-5-5", "command"));
        assert!(answer.starts_with("HTTP/1.1 200") && answer.ends_with("\r\n\r\n"), "nothing for Claude to add to its context: {answer}");
        let s = proxy.stats.session("7");
        assert_eq!(s.agent_model.as_deref(), Some("claude-sonnet-5-5"));
        assert_eq!(s.activity, Some(Activity::Done), "a switch isn't a turn");
        // Its own fallback, and the 1M variant as it names it.
        post(switch("claude-sonnet-5-5", "claude-opus-5-5[1m]", "auto"));
        assert_eq!(proxy.stats.session("7").agent_model.as_deref(), Some("claude-opus-5-5[1m]"));
        post(json!({"hook_event_name": "PostModelSwitch", "to_model": ""}));
        assert_eq!(proxy.stats.session("7").agent_model.as_deref(), Some("claude-opus-5-5[1m]"), "an empty one says nothing");
        // Started again: it's on what it's started with until it says otherwise.
        proxy.stats.restarted("7");
        assert_eq!(proxy.stats.session("7").agent_model, None);
        proxy.stats.report_model("7", "gpt-5.5".into());
        assert_eq!(proxy.stats.session("7").agent_model.as_deref(), Some("gpt-5.5"));
    }

    /// Free models off (the default): a free-tier request is refused before anything leaves this
    /// Mac, keys or not, so no turn's text reaches a free model or the classifier.
    #[test]
    fn free_models_off_sends_nothing() {
        use std::io::{Read, Write};
        let keys = HashMap::from([("NVIDIA_API_KEY".to_string(), "nv".to_string()), ("TYPESAFE_API_KEY".to_string(), "ts".to_string())]);
        let proxy = Proxy::start(keys).unwrap();
        assert!(!proxy.free_models());
        let here = format!("127.0.0.1:{}", proxy.port);
        let send = |rest: &str| {
            let path = proxy.base_url("7", "free").strip_prefix(&format!("http://{here}")).unwrap().to_string() + rest;
            let body = r#"{"model":"auto","max_tokens":10,"messages":[{"role":"user","content":"refactor the parser"}]}"#;
            let mut c = std::net::TcpStream::connect(&here).unwrap();
            write!(c, "POST {path} HTTP/1.1\r\nHost: {here}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            let mut out = String::new();
            let _ = c.read_to_string(&mut out);
            out
        };
        let out = send("/v1/messages");
        assert!(out.starts_with("HTTP/1.1 403") && out.contains("Settings → Experimental"), "{out}");
        assert!(send("/v1/chat/completions").starts_with("HTTP/1.1 403"));
        let s = proxy.stats.session("7");
        assert_eq!((s.classifier, s.tier), (None, None), "never classified");
    }

    #[test]
    fn a_json_answer_in_pieces_is_read_once_it_is_all_there() {
        let stats = Arc::new(Stats::default());
        {
            let mut tap = Tap { meter: Meter::default(), stats: stats.clone(), session: "1".into(), route: None, _in_flight: None, call: None, started: Instant::now(), first: None };
            tap.meter.feed(&Bytes::from_static(br#"{"model":"m","usage":{"prompt"#));
            assert!(tap.meter.seen.is_none());
            tap.meter.feed(&Bytes::from_static(br#"_tokens":500,"completion_tokens":7}}"#));
        }
        let s = stats.session("1");
        assert_eq!((s.usage.input, s.usage.output), (500, 7));
    }

    /// An Anthropic stream cut anywhere (past its first bytes, which say it's a stream): usage, the
    /// model and its end are read wherever the chunks break, and text about errors and usage is
    /// only text.
    #[test]
    fn a_stream_in_any_pieces_reads_the_same() {
        let delta = |t: &str| format!("event: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":{}}}}}\n\n", json!(t));
        let mut stream = String::from("event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-haiku-4-5\",\"usage\":{\"input_tokens\":120,\"cache_read_input_tokens\":3000,\"output_tokens\":1}}}\n\n");
        for t in ["The \"error\" field ", "and \"usage\": {\"output_tokens\": 99999} ", "are \"message_stop\" words."] {
            stream += &delta(t);
        }
        stream += "event: message_delta\r\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":42}}\r\n\r\n";
        let ends = stream.len();
        stream += "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
        for size in [7, 13, 64, 300, stream.len()] {
            let mut m = Meter::default();
            for piece in stream.as_bytes().chunks(size) {
                m.feed(&Bytes::copy_from_slice(piece));
            }
            let u = m.seen.clone().unwrap();
            assert_eq!((u.input, u.cache_read, u.output), (120, 3000, 42), "pieces of {size}");
            assert_eq!(m.model.as_deref(), Some("claude-haiku-4-5"));
            assert!(m.complete && m.error.is_none(), "pieces of {size}");
        }
        // Cut off before its last event: not complete.
        let mut m = Meter::default();
        m.feed(&Bytes::copy_from_slice(&stream.as_bytes()[..ends]));
        assert!(!m.complete);
    }

    /// Each model call is written down once, as it ends: its tokens, the model that answered, how
    /// long its first byte took, and what the route said it cost.
    #[test]
    fn a_finished_call_is_recorded_once_with_its_timing_and_cost() {
        let stats = Arc::new(Stats::default());
        let started = Instant::now() - std::time::Duration::from_millis(900);
        let mut tap = Tap {
            meter: Meter::default(),
            stats: stats.clone(),
            session: "4".into(),
            route: None,
            _in_flight: None,
            call: Some(Call { at_ms: 1, route: "or".into(), model: Some("asked/model".into()), ..Default::default() }),
            started,
            first: Some(started + std::time::Duration::from_millis(300)),
        };
        tap.meter.feed(&Bytes::from_static(b"data: {\"model\":\"answered/model\",\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n"));
        tap.meter.feed(&Bytes::from_static(b"data: {\"choices\":[],\"usage\":{\"prompt_tokens\":120,\"completion_tokens\":40,\"prompt_tokens_details\":{\"cached_tokens\":100},\"cost\":0.00042}}\n\ndata: [DONE]\n\n"));
        drop(tap);
        let calls = stats.take_calls();
        assert_eq!(calls.len(), 1);
        let c = &calls[0];
        assert_eq!((c.session.as_str(), c.route.as_str(), c.model.as_deref()), ("4", "or", Some("answered/model")));
        assert_eq!((c.usage.input, c.usage.cache_read, c.usage.output), (20, 100, 40));
        assert_eq!(c.cost, Some(0.00042));
        assert_eq!(c.ttft_ms, Some(300));
        assert!(c.duration_ms.unwrap() >= 900);
        assert_eq!(c.status, CallStatus::Ok);
    }

    #[test]
    fn a_200_that_is_an_error_counts_as_one() {
        let stats = Arc::new(Stats::default());
        let call = |body: &str| {
            let mut tap = Tap { meter: Meter::default(), stats: stats.clone(), session: "1".into(), route: None, _in_flight: None, call: None, started: Instant::now(), first: None };
            tap.meter.feed(&Bytes::from(body.to_string()));
        };
        // OpenRouter, as it answered for real: HTTP 200, and an upstream 503 in the body.
        call(r#"{"id":"gen-1790742907-SecNFKzI8AeVJ53NBH9p","error":{"message":"Upstream error from Nvidia: Service temporarily overloaded","code":503,"metadata":{"error_type":"provider_overloaded"}}}"#);
        let s = stats.session("1");
        assert_eq!(s.errors, 1);
        assert_eq!(s.last_error.as_deref(), Some("503 Upstream error from Nvidia: Service temporarily overloaded"));

        // The same inside a Chat Completions stream, and a Responses stream that failed.
        call("data: {\"id\":\"x\",\"choices\":[{\"delta\":{\"content\":\"PEL\"}}]}\n\ndata: {\"error\":{\"message\":\"Provider returned error\",\"code\":429}}\n\n");
        assert_eq!(stats.session("1").last_error.as_deref(), Some("429 Provider returned error"));
        call("event: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\",\"error\":{\"code\":\"server_error\",\"message\":\"The model failed\"}}}\n\n");
        assert_eq!(stats.session("1").errors, 3);
        assert_eq!(stats.session("1").last_error.as_deref(), Some("The model failed"));

        // A finished Responses answer says `"error": null`: not an error.
        let before = stats.session("1").errors;
        call("event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"error\":null,\"usage\":{\"input_tokens\":5,\"output_tokens\":1}}}\n\n");
        call(r#"{"id":"c","choices":[{"message":{"content":"PELICAN"}}],"usage":{"prompt_tokens":5,"completion_tokens":1}}"#);
        assert_eq!(stats.session("1").errors, before);
    }

    #[test]
    fn context_from_the_last_call_per_model() {
        let stats = Arc::new(Stats::default());
        let call = |events: String| {
            let mut tap = Tap { meter: Meter::default(), stats: stats.clone(), session: "1".into(), route: None, _in_flight: None, call: None, started: Instant::now(), first: None };
            tap.meter.feed(&Bytes::from(events));
        };
        let start = |model: &str, input: u64, cached: u64| {
            format!("event: message_start\ndata: {{\"type\":\"message_start\",\"message\":{{\"model\":\"{model}\",\"usage\":{{\"input_tokens\":{input},\"cache_read_input_tokens\":{cached},\"output_tokens\":1}}}}}}\n\n")
        };
        call(start("claude-opus-5-5", 10, 50_000));
        call(start("claude-haiku-4-5", 300, 0));
        call(start("claude-opus-5-5", 20, 60_000));
        call(start("claude-opus-5-5", 8, 0));
        let s = stats.session("1");
        assert_eq!(s.context(), Some(("claude-opus-5-5", 60_020)), "the conversation, not the side call or the probe");
        assert_eq!(s.usage.total_input(), 110_338);
        stats.reset_context("1");
        assert_eq!(stats.session("1").context(), None);
    }

    /// As Claude Code 2.1 sends it: before the first answer (and after `/compact`), after one.
    #[test]
    fn context_from_the_statusline() {
        let before = json!({"model":{"id":"claude-opus-5-5[1m]"},"context_window":{"total_input_tokens":0,"context_window_size":1_000_000,"current_usage":null,"used_percentage":null,"remaining_percentage":null}});
        assert_eq!(ReportedContext::from_statusline(&before), Some(ReportedContext { used: None, window: 1_000_000 }));
        let after = json!({"context_window":{"context_window_size":200_000,"current_usage":{"input_tokens":12,"output_tokens":40,"cache_creation_input_tokens":3_000,"cache_read_input_tokens":20_000},"used_percentage":12}});
        assert_eq!(ReportedContext::from_statusline(&after), Some(ReportedContext { used: Some(23_012), window: 200_000 }));
        let compacted = json!({"context_window":{"context_window_size":200_000,"current_usage":null,"used_percentage":0}});
        assert_eq!(ReportedContext::from_statusline(&compacted), Some(ReportedContext { used: Some(0), window: 200_000 }));
        let pct = json!({"context_window":{"context_window_size":200_000,"used_percentage":25.5}});
        assert_eq!(ReportedContext::from_statusline(&pct).unwrap().used, Some(51_000));
        assert_eq!(ReportedContext::from_statusline(&json!({"context_window":{"context_window_size":0}})), None);
        assert_eq!(ReportedContext::from_statusline(&json!({"hook_event_name":"Stop"})), None);
    }

    /// The hooks as Claude sends them: one agent in the background, one in the foreground.
    #[test]
    fn subagents_from_hooks() {
        let stats = Stats::default();
        let wt = "/r/.claude/worktrees/agent-";
        let feed = |event: &str, v: Value| record_subagent(&stats, "s1", event, &v);
        let call = |id: &str, desc: &str| {
            json!({"tool_name": "Agent", "tool_use_id": id,
                "tool_input": {"description": desc, "subagent_type": "general-purpose", "isolation": "worktree"}})
        };
        feed("PreToolUse", call("t1", "Count python files"));
        feed("PreToolUse", call("t2", "Read the README"));
        // Background: launched, then started.
        let mut launched = call("t2", "Read the README");
        launched["tool_response"] = json!({"isAsync": true, "status": "async_launched", "agentId": "bbb"});
        feed("PostToolUse", launched);
        feed("SubagentStart", json!({"agent_id": "bbb", "agent_type": "general-purpose", "cwd": format!("{wt}bbb")}));
        // Foreground: started first, its PostToolUse only once it's done.
        feed("SubagentStart", json!({"agent_id": "aaa", "agent_type": "general-purpose", "cwd": format!("{wt}aaa")}));
        // Its own tool calls say whose they are and change nothing here.
        feed("PreToolUse", json!({"tool_name": "Bash", "agent_id": "aaa", "agent_type": "general-purpose"}));
        // A helper Claude runs by itself.
        feed("SubagentStop", json!({"agent_id": "zzz", "agent_type": ""}));

        let s = stats.session("s1");
        let got: Vec<_> =
            s.subagents.iter().map(|a| (a.id.as_str(), a.description.as_deref(), a.cwd.clone(), a.running)).collect();
        assert_eq!(got, vec![
            ("bbb", Some("Read the README"), Some(format!("{wt}bbb")), true),
            ("aaa", Some("Count python files"), Some(format!("{wt}aaa")), true),
        ]);
        assert!(s.pending_agents.is_empty());

        feed("SubagentStop", json!({"agent_id": "aaa", "agent_type": "general-purpose"}));
        let mut done = call("t1", "Count python files");
        done["tool_response"] = json!({"status": "completed", "agentId": "aaa"});
        feed("PostToolUse", done);
        let s = stats.session("s1");
        assert_eq!(s.subagents.iter().map(|a| a.running).collect::<Vec<_>>(), vec![true, false]);
        assert_eq!(s.subagents[1].description.as_deref(), Some("Count python files"));
    }
}
