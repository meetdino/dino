//! Local pass-through proxy. Agents point their base URL at
//! `http://127.0.0.1:<port>/s/<session>/<provider>`; we forward to the real API untouched
//! (auth included) and observe usage, in-flight state and quota headers on the way back.
//! The `free` provider is different: dino itself picks a free model and translates (see `free`).

mod codex;
mod free;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, HeaderName, Response, StatusCode};
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

#[derive(Clone, Debug, Default)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
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
    pub requests: u64,
    pub in_flight: u32,
    pub errors: u64,
    pub usage: Usage,
    pub last_model: Option<String>,
    /// Why the last model call failed; cleared by the next one that succeeds.
    pub last_error: Option<String>,
    /// Router tier for free-tier sessions ("fast", "code", "reason").
    pub tier: Option<String>,
    /// Which classifier made the last routing decision ("jev" or "llm").
    pub classifier: Option<String>,
    pub last_request: Option<Instant>,
    /// Per model, what its last call read (cached tokens included): how full its context is.
    /// Per model because agents make small side calls (titles, summaries) on other models.
    pub context: HashMap<String, u64>,
    /// Subagents the agent started, as its hooks reported them (Claude's Agent tool).
    pub subagents: Vec<Subagent>,
    /// Agent tool calls not answered yet: (tool_use_id, description, subagent_type).
    pending_agents: Vec<(String, Option<String>, Option<String>)>,
}

impl SessionStats {
    /// The conversation's context use: the biggest per-model one, since side calls are small.
    pub fn context(&self) -> Option<(&str, u64)> {
        self.context.iter().max_by_key(|(_, n)| **n).map(|(m, n)| (m.as_str(), *n))
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

#[derive(Default)]
pub struct Stats {
    pub sessions: Mutex<HashMap<String, SessionStats>>,
    pub quotas: Mutex<HashMap<String, Quota>>,
}

impl Stats {
    pub fn session(&self, id: &str) -> SessionStats {
        self.sessions.lock().unwrap().get(id).cloned().unwrap_or_default()
    }

    /// The agent's turn is over although no hook said so (Esc while a tool runs fires none).
    pub fn end_turn(&self, id: &str) {
        self.update(id, |s| {
            if s.activity == Some(Activity::Working) && s.in_flight == 0 {
                s.activity = Some(Activity::Done);
            }
        });
    }

    /// Forget context use, e.g. when the agent restarts on another model.
    pub fn reset_context(&self, id: &str) {
        self.update(id, |s| s.context.clear());
    }

    pub fn quota(&self, provider: &str) -> Option<Quota> {
        self.quotas.lock().unwrap().get(provider).cloned()
    }

    pub(crate) fn update(&self, id: &str, f: impl FnOnce(&mut SessionStats)) {
        f(self.sessions.lock().unwrap().entry(id.to_string()).or_default());
    }
}

pub struct Proxy {
    pub port: u16,
    pub stats: Arc<Stats>,
    keys: Arc<RwLock<HashMap<String, String>>>,
    budget: Arc<AtomicU64>,
}

impl Proxy {
    /// Start on a random localhost port, on its own runtime thread.
    /// `keys` are provider credentials dino itself uses (e.g. `NVIDIA_API_KEY` for the free tier).
    pub fn start(keys: HashMap<String, String>) -> anyhow::Result<Self> {
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        std_listener.set_nonblocking(true)?;
        let port = std_listener.local_addr()?.port();
        let stats = Arc::new(Stats::default());
        let keys = Arc::new(RwLock::new(keys));
        let budget = Arc::new(AtomicU64::new(0));
        let state = AppState {
            stats: stats.clone(),
            client: reqwest::Client::builder().build()?,
            router: Arc::default(),
            keys: keys.clone(),
            budget: budget.clone(),
            substitutes: Arc::default(),
        };

        std::thread::Builder::new().name("dino-proxy".into()).spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
            rt.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(std_listener).unwrap();
                let app = axum::Router::new().route("/s/{session}/{provider}/{*rest}", any(forward))
                    .route("/s/{session}/hook", post(hook))
                    .fallback(|req: Request| async move {
                        log(format_args!("unrouted {} {}", req.method(), req.uri()));
                        StatusCode::NOT_FOUND
                    })
                    .with_state(state);
                let _ = axum::serve(listener, app).await;
            });
        })?;
        Ok(Self { port, stats, keys, budget })
    }

    /// Use these keys from the next request on.
    pub fn set_keys(&self, keys: HashMap<String, String>) {
        *self.keys.write().unwrap() = keys;
    }

    /// Most tokens one session may use before its model calls are refused; 0 means no limit.
    pub fn set_budget(&self, tokens: u64) {
        self.budget.store(tokens, Ordering::Relaxed);
    }

    /// Base URL an agent should use for `provider`, attributed to `session`.
    pub fn base_url(&self, session: &str, provider: &str) -> String {
        format!("http://127.0.0.1:{}/s/{session}/{provider}", self.port)
    }
}

#[derive(Clone)]
pub(crate) struct AppState {
    stats: Arc<Stats>,
    client: reqwest::Client,
    router: Arc<dino_router::Router>,
    keys: Arc<RwLock<HashMap<String, String>>>,
    /// The session token budget; 0 means none.
    budget: Arc<AtomicU64>,
    /// Codex models the backend rejected, and the model that answered instead.
    substitutes: Arc<Mutex<HashMap<String, String>>>,
}

/// Headers we must not copy between the two connections.
fn hop_by_hop(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "host" | "connection" | "keep-alive" | "transfer-encoding" | "upgrade" | "proxy-connection" | "te" | "trailer"
            | "content-length" | "accept-encoding"
    )
}

async fn forward(
    State(st): State<AppState>,
    Path((session, provider, rest)): Path<(String, String, String)>,
    req: Request,
) -> Response<Body> {
    // Only model calls count toward activity; ignore e.g. token counting and telemetry.
    let is_model_call = rest.ends_with("messages") || rest.ends_with("chat/completions") || rest.ends_with("responses");
    if is_model_call && let Some(resp) = over_budget(&st, &session, &provider) {
        return resp;
    }
    if provider == "free" {
        let Ok(body) = axum::body::to_bytes(req.into_body(), usize::MAX).await else {
            return error(StatusCode::BAD_REQUEST, "unreadable body".into());
        };
        return free::handle(st, session, &rest, body).await;
    }
    let Some(&(_, upstream)) = PROVIDERS.iter().find(|(p, _)| *p == provider) else {
        return error(StatusCode::NOT_FOUND, format!("unknown provider {provider}"));
    };
    let query = req.uri().query().map(|q| format!("?{q}")).unwrap_or_default();
    let url = format!("{upstream}/{rest}{query}");
    let (parts, body) = req.into_parts();
    let Ok(mut body) = axum::body::to_bytes(body, usize::MAX).await else {
        return error(StatusCode::BAD_REQUEST, "unreadable body".into());
    };
    let requested = serde_json::from_slice::<Value>(&body).ok().and_then(|v| v["model"].as_str().map(String::from));
    // A Codex model the backend already rejected: go straight to the one that answered instead.
    if provider == "chatgpt" {
        let sub = requested.as_ref().and_then(|m| st.substitutes.lock().unwrap().get(m).cloned());
        if let Some(b) = sub.and_then(|m| codex::with_model(&body, &m)) {
            body = b;
        }
    }

    if is_model_call {
        st.stats.update(&session, |s| {
            s.requests += 1;
            s.in_flight += 1;
            s.last_request = Some(Instant::now());
            if let Some(m) = serde_json::from_slice::<Value>(&body).ok().and_then(|v| v["model"].as_str().map(String::from)) {
                s.last_model = Some(m);
            }
        });
    }
    let guard = is_model_call.then(|| InFlight { stats: st.stats.clone(), session: session.clone() });

    let method = parts.method.clone();
    let send = |body: Bytes| {
        let mut up = st.client.request(method.clone(), &url).body(body);
        for (name, value) in parts.headers.iter().filter(|(n, _)| !hop_by_hop(n)) {
            up = up.header(name, value);
        }
        up.send()
    };
    let upstream_error = |e: reqwest::Error| {
        let msg = format!("dino proxy: {e}");
        st.stats.update(&session, |s| {
            s.errors += 1;
            s.last_error = Some(msg.clone());
        });
        log(format_args!("{session} {provider} {method} /{rest} -> upstream error: {e}"));
        error(StatusCode::BAD_GATEWAY, msg)
    };
    let mut resp = match send(body.clone()).await {
        Ok(r) => r,
        Err(e) => return upstream_error(e),
    };

    // A failed model call is a small JSON body: read it to say why, and for Codex maybe retry another model.
    if is_model_call && !resp.status().is_success() {
        let status = resp.status();
        let headers = resp.headers().clone();
        let text = resp.bytes().await.unwrap_or_default();
        resp = 'retry: {
            if provider == "chatgpt" && status == StatusCode::NOT_FOUND && codex::model_not_found(&text) {
                let rejected = requested.unwrap_or_default();
                for model in codex::fallbacks(&rejected) {
                    let Some(retry) = codex::with_model(&body, &model) else { continue };
                    match send(retry).await {
                        Ok(r) if r.status().is_success() => {
                            log(format_args!("{session} chatgpt: {rejected} rejected, using {model}"));
                            st.substitutes.lock().unwrap().insert(rejected, model.clone());
                            st.stats.update(&session, |s| s.last_model = Some(model));
                            break 'retry r;
                        }
                        Ok(r) => log(format_args!("{session} chatgpt: fallback {model} -> {}", r.status())),
                        Err(e) => return upstream_error(e),
                    }
                }
            }
            log(format_args!("{session} {provider} {method} /{rest} -> {status}"));
            record_quota(&st.stats, &provider, &headers);
            let msg = format!("{} {}", status.as_u16(), codex::error_message(&text));
            st.stats.update(&session, |s| {
                s.errors += 1;
                s.last_error = Some(msg);
            });
            drop(guard);
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
    record_quota(&st.stats, &provider, resp.headers());
    if !status.is_success() && is_model_call {
        st.stats.update(&session, |s| s.errors += 1);
    } else if is_model_call {
        st.stats.update(&session, |s| s.last_error = None);
    }

    let mut builder = Response::builder().status(status.as_u16());
    for (name, value) in resp.headers().iter().filter(|(n, _)| !hop_by_hop(n)) {
        builder = builder.header(name, value);
    }

    // Tee the body: pass every chunk through immediately, scan a copy for usage.
    let mut tap = Tap { meter: Meter::default(), stats: st.stats.clone(), session, _in_flight: guard };
    let stream = resp.bytes_stream().map(move |chunk| {
        if let Ok(bytes) = &chunk {
            tap.meter.feed(bytes);
        }
        chunk
    });
    builder.body(Body::from_stream(stream)).unwrap_or_else(|_| error(StatusCode::BAD_GATEWAY, "bad response".into()))
}

/// Lives as long as the response body; records usage when the stream ends or is dropped.
struct Tap {
    meter: Meter,
    stats: Arc<Stats>,
    session: String,
    _in_flight: Option<InFlight>,
}

impl Drop for Tap {
    fn drop(&mut self) {
        let aborted = self._in_flight.is_some() && !self.meter.complete;
        if aborted {
            log(format_args!("{} model call dropped by the agent", self.session));
        }
        self.stats.update(&self.session, |s| {
            if let Some(u) = self.meter.seen.take() {
                s.usage.add(&u);
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

async fn hook(State(st): State<AppState>, Path(session): Path<String>, body: Bytes) -> StatusCode {
    let Ok(v) = serde_json::from_slice::<Value>(&body) else { return StatusCode::BAD_REQUEST };
    log(format_args!("{session} hook {v}"));
    let event = v["hook_event_name"].as_str().unwrap_or_default();
    let tool = || v["tool_name"].as_str().unwrap_or("tool").to_string();
    record_subagent(&st.stats, &session, event, &v);
    // A subagent's own tool calls: the parent's turn may be over (background agents), and the
    // subagent's model calls show as the session thinking anyway.
    let from_subagent = v["agent_id"].is_string();
    let activity = match event {
        "UserPromptSubmit" | "PreToolUse" | "PostToolUse" | "PostToolUseFailure" if from_subagent => None,
        // An interrupted tool, when the agent reports one (Claude often sends nothing; see `end_turn`).
        "PostToolUseFailure" if v["is_interrupt"].as_bool() == Some(true) => Some(Activity::Done),
        "UserPromptSubmit" | "PreToolUse" | "PostToolUse" | "PostToolUseFailure" => Some(Activity::Working),
        "PermissionRequest" => Some(Activity::NeedsPermission(tool())),
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
    if let Some(a) = activity {
        // A notification about the same prompt shouldn't clobber the more specific tool name.
        st.stats.update(&session, |s| {
            if !(event == "Notification" && matches!(s.activity, Some(Activity::NeedsPermission(_)))) {
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
            "PostToolUse" => a.description = text(&v["tool_input"]["description"]).or(a.description.take()),
            "SubagentStart" => {
                a.running = true;
                a.cwd = text(&v["cwd"]);
                a.agent_type = text(&v["agent_type"]);
                if a.description.is_none() {
                    let ty = a.agent_type.clone();
                    if let Some(p) = s.pending_agents.iter().position(|p| p.2.is_none() || p.2 == ty) {
                        s.subagents[i].description = s.pending_agents.remove(p).1;
                    }
                }
            }
            _ => a.running = false,
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
    let msg = format!("dino: this session used {used} tokens, over its budget of {budget}. Start a new session, or raise the budget in Settings → Policies.");
    st.stats.update(session, |s| {
        s.errors += 1;
        s.last_error = Some(msg.clone());
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

fn error(status: StatusCode, msg: String) -> Response<Body> {
    Response::builder().status(status).body(Body::from(msg)).unwrap()
}

/// Subscription windows from response headers: Claude's `anthropic-ratelimit-unified-5h-utilization`
/// and friends, or Codex's `x-codex-primary-used-percent` / `-window-minutes` / `-reset-at`.
fn record_quota(stats: &Stats, provider: &str, headers: &HeaderMap) {
    if let Some(windows) = codex_windows(headers) {
        stats.quotas.lock().unwrap().insert(provider.to_string(), Quota { windows });
        return;
    }
    const PREFIX: &str = "anthropic-ratelimit-unified-";
    let mut windows: HashMap<String, Window> = HashMap::new();
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
}

impl Meter {
    fn feed(&mut self, bytes: &Bytes) {
        let sse = *self.sse.get_or_insert_with(|| bytes.starts_with(b"event:") || bytes.starts_with(b"data:"));
        if !sse {
            self.body.extend_from_slice(bytes);
            if let Ok(v) = serde_json::from_slice::<Value>(&self.body) {
                self.observe(&v);
                self.complete = true;
            }
            return;
        }
        for &b in bytes.iter() {
            if b == b'\n' {
                let line = std::mem::take(&mut self.line);
                if let Some(data) = line.strip_prefix(b"data:") {
                    let data = data.trim_ascii();
                    if let Ok(v) = serde_json::from_slice::<Value>(data) {
                        self.observe(&v);
                        // Anthropic's last event, and the Responses API's ways to end.
                        let last = ["message_stop", "error", "response.completed", "response.incomplete", "response.failed"];
                        self.complete |= v["type"].as_str().is_some_and(|t| last.contains(&t));
                    }
                    // Chat Completions.
                    self.complete |= data == b"[DONE]";
                }
            } else {
                self.line.push(b);
            }
        }
    }

    fn observe(&mut self, v: &Value) {
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
            let seen = self.seen.get_or_insert_with(Usage::default);
            seen.input = seen.input.max(next.input);
            seen.output = seen.output.max(next.output);
            seen.cache_read = seen.cache_read.max(next.cache_read);
            seen.cache_write = seen.cache_write.max(next.cache_write);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn context_from_the_last_call_per_model() {
        let stats = Arc::new(Stats::default());
        let call = |events: String| {
            let mut tap = Tap { meter: Meter::default(), stats: stats.clone(), session: "1".into(), _in_flight: None };
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
