//! dinod: owns agent sessions (PTY + terminal state), the proxy and the router. Clients (the TUI,
//! `dino attach` inside a Ghostty surface, the future app) talk to it over a Unix socket; agents
//! keep running when every client goes away.

use std::collections::{HashMap, HashSet};
use std::io;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use dino_core::found::{self, FoundSession, Source};
use dino_core::ipc::{self, LauncherInfo, QuotaInfo, Request, Response, SessionInfo, WindowInfo};
use dino_core::controls::{self, Controls, Knobs};
use dino_core::models::Catalog;
use dino_core::settings::{self, Settings};
use dino_core::agent::{StatusSource, agent};
use dino_core::{detect_agents, load_keys, new_uuid, pr, proxy_wiring, ssh, trust, user_shell, worktree};
use dino_proxy::{Activity, Proxy, SessionStats};
use dino_term::{Pane, SpawnSpec};

mod chatgpt;
mod codex;
mod lifecycle;
mod peers;
mod preview;
mod providers;
mod schedule;
mod servers;
mod shell;

/// Scrollback lines replayed to a newly attached client.
const REPLAY_HISTORY: usize = 2000;

struct Session {
    id: String,
    name: String,
    agent_id: String,
    launcher: String,
    /// Extra args the user passed, re-used on resume.
    args: Vec<String>,
    cwd: PathBuf,
    started_at: u64,
    /// The agent's own conversation id (Claude's session UUID, Codex's rollout id), for resume.
    agent_session: Mutex<Option<String>>,
    pane: Arc<Pane>,
    subscribers: Arc<Mutex<Vec<(u64, Sender<Vec<u8>>)>>>,
    /// When the agent last wrote something the user didn't just cause (see `USER_ECHO`).
    last_output: Arc<Mutex<Option<Instant>>>,
    /// When the agent last wrote anything at all.
    last_write: Arc<Mutex<Option<Instant>>>,
    /// The user's last keystroke, resize or attach.
    poked: Arc<Mutex<Option<Instant>>>,
    /// The last local web address the agent printed.
    local_url: Arc<Mutex<Option<String>>>,
    attached: AtomicUsize,
    auto: Mutex<AutoState>,
    /// Mode, model and effort it was started with.
    controls: Controls,
    /// Asked for mid-turn: `restart` with these once the turn is over.
    pending: Mutex<Option<Controls>>,
    /// The scheduled task that started it, by name.
    scheduled: Option<String>,
    /// The session whose agent started it, by id.
    started_by: Option<String>,
    /// The session whose agent last messaged it, by id.
    messaged_by: Mutex<Option<String>>,
    /// The name the user gave it, shown over the agent's title.
    label: Mutex<Option<String>>,
    /// Kept at the top of its group, and never archived by dino on its own.
    pinned: AtomicBool,
    /// The SSH host it runs on; `cwd` is then a path there, and nothing local applies to it.
    host: Option<String>,
    /// Codex's rollout: which conversation it's on, where its turn is, its context window.
    rollout: Mutex<codex::Rollout>,
    /// A shell's: the agent someone started in it by hand.
    inside: Mutex<Inside>,
    /// Once it has ended: its last screen is on disk (see `save`).
    screen_saved: AtomicBool,
    /// Background commands its agent left serving, as last looked at (see `servers`).
    servers: Mutex<Vec<servers::Server>>,
}

/// What a shell is running in the foreground, as last looked at.
#[derive(Default)]
struct Inside {
    fg: Option<u32>,
    checked: Option<Instant>,
    found: Option<FoundSession>,
    /// The shell's own title from before the command started, put back when an agent leaves.
    before: Option<Option<String>>,
}

/// How often to look again at a foreground command that hasn't changed: an agent's title and
/// status move while it runs, and a wrapper script may start one late.
const INSIDE_RECHECK: std::time::Duration = std::time::Duration::from_secs(2);

impl Daemon {
    fn launcher(&self, short: &str) -> Option<LauncherInfo> {
        self.launchers.read().unwrap().iter().find(|l| l.short == short).cloned()
    }

    /// A launcher to start something new with: known, and allowed by the policies.
    fn allowed_launcher(&self, short: &str) -> anyhow::Result<LauncherInfo> {
        let l = self.launcher(short).ok_or_else(|| anyhow::anyhow!("unknown agent {short}"))?;
        anyhow::ensure!(Settings::load().policies.allows(short), "{} isn't allowed by your policies (Settings → Policies)", l.label);
        Ok(l)
    }

    /// Every launcher, with the controls the policies let it offer.
    fn all_launchers(&self) -> Vec<LauncherInfo> {
        let allow_bypass = Settings::load().policies.allow_bypass;
        let mut out = self.launchers.read().unwrap().clone();
        for l in &mut out {
            l.knobs = self.knobs(&l.agent_id, allow_bypass);
        }
        out
    }

    /// What `agent_id` offers, its models as its own files last said.
    fn knobs(&self, agent_id: &str, allow_bypass: bool) -> Knobs {
        let catalogs = self.catalogs.read().unwrap();
        let catalog = agent(agent_id).and_then(|a| catalogs.get(a.catalog_key()));
        controls::knobs(agent_id, allow_bypass, catalog)
    }

    /// The launchers to offer: the allowed ones, the default (Claude Code unless set) first.
    fn offered(&self) -> Vec<LauncherInfo> {
        let p = Settings::load().policies;
        let mut out: Vec<LauncherInfo> = self.all_launchers().into_iter().filter(|l| p.allows(&l.short)).collect();
        let default = p.default_agent.unwrap_or_else(|| "claude".into());
        if let Some(i) = out.iter().position(|l| l.short == default) {
            let l = out.remove(i);
            out.insert(0, l);
        }
        out
    }
}

struct Daemon {
    proxy: Proxy,
    /// Rebuilt when keys change: the free tier needs one.
    launchers: RwLock<Vec<LauncherInfo>>,
    /// Agent id → the models its own files list (see `watch_catalogs`).
    catalogs: RwLock<HashMap<String, Catalog>>,
    sessions: Mutex<Vec<Arc<Session>>>,
    groups: Mutex<Vec<Group>>,
    worktrees: Mutex<Vec<SessionWorktree>>,
    /// Session id → the PR from its branch, as of the last poll.
    prs: Mutex<HashMap<String, ipc::PrInfo>>,
    /// One PR poll at a time, so an automatic step is never taken twice.
    pr_poll: Mutex<()>,
    /// Sessions whose PR merged or closed, to archive once their turn is over; since when.
    closing: Mutex<HashMap<String, Instant>>,
    /// Dev servers started for previews, each tied to a session.
    previews: Mutex<Vec<Arc<preview::Server>>>,
    /// Subagents that run in a worktree of their own, and whose session started them.
    subagents: Mutex<Vec<SubagentWorktree>>,
    /// Worktree path → its git summary and when it was read; git is too slow for every tree poll.
    summaries: Mutex<HashMap<String, (Instant, Option<worktree::Summary>)>>,
    schedule: schedule::Scheduler,
    /// Stopped sessions kept to start again, newest first.
    archived: Mutex<Vec<lifecycle::Archived>>,
    /// Worktree path → its size on disk and when it was measured.
    sizes: Mutex<HashMap<String, (Instant, u64)>>,
    /// Worktree path → every commit in it is pushed, measured with its size (git is too slow per poll).
    pushed: Mutex<HashMap<String, bool>>,
    /// A thread is measuring sizes.
    measuring: AtomicBool,
    next_id: AtomicU64,
    next_sub: AtomicU64,
}

pub fn run() -> anyhow::Result<()> {
    // Nothing dinod starts is a child of the agent session that may have started dinod.
    for var in dino_core::PARENT_AGENT_ENV {
        // SAFETY: first thing, before dinod starts any thread.
        unsafe { std::env::remove_var(var) };
    }
    let path = ipc::socket_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if UnixStream::connect(&path).is_ok() {
        anyhow::bail!("dinod is already running ({})", path.display());
    }
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;
    let saved = load_saved();

    let keys = load_keys();
    let free_tier = keys.contains_key("NVIDIA_API_KEY");
    let proxy = Proxy::start(keys)?;
    proxy.set_budget(Settings::load().policies.session_token_budget);
    let daemon = new_daemon(proxy, launchers(free_tier));
    // Before sessions restart, so they get the efforts their models take.
    let mut stamps = CatalogStamps::new();
    read_catalogs(&daemon, &mut stamps);
    restore(&daemon, saved);
    {
        // Pick up late-discovered agent ids (Codex) and sessions that exited on their own.
        let d = daemon.clone();
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(std::time::Duration::from_secs(5));
                save(&d);
            }
        });
    }
    {
        let d = daemon.clone();
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(std::time::Duration::from_secs(1));
                apply_pending(&d);
            }
        });
    }
    {
        let d = daemon.clone();
        std::thread::spawn(move || watch_catalogs(&d, stamps));
    }
    {
        let d = daemon.clone();
        std::thread::spawn(move || {
            loop {
                refresh_prs(&d);
                std::thread::sleep(std::time::Duration::from_secs(30));
            }
        });
    }
    {
        let d = daemon.clone();
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(std::time::Duration::from_secs(1));
                watch_shells(&d);
            }
        });
    }
    {
        let d = daemon.clone();
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(std::time::Duration::from_secs(2));
                servers::watch(&d);
            }
        });
    }
    {
        let d = daemon.clone();
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(std::time::Duration::from_millis(500));
                codex::watch(&d);
            }
        });
    }
    schedule::start(&daemon);
    providers::start();
    {
        // The ChatGPT sign-in's access token lasts an hour: a new one before it runs out.
        let d = daemon.clone();
        std::thread::spawn(move || {
            loop {
                match chatgpt::refresh_if_due(&chatgpt::Endpoints::default().token, &load_keys()) {
                    Ok(Some(t)) => {
                        if let Err(e) = save_chatgpt(&d, &t) {
                            eprintln!("dinod: couldn't keep the new ChatGPT sign-in: {e}");
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        eprintln!("dinod: ChatGPT sign-in refresh: {e}");
                        // Turned down: signed out, and it says why, rather than asking again every time.
                        if e.downcast_ref::<chatgpt::Refused>().is_some() && chatgpt::SIGNED_IN.iter().try_for_each(|k| settings::set_key(k, None)).is_ok() {
                            keys_changed(&d);
                        }
                        providers::set_error("chatgpt", Some(format!("Sign in with ChatGPT again: {e}")));
                    }
                }
                std::thread::sleep(std::time::Duration::from_secs(30));
            }
        });
    }
    eprintln!("dinod listening on {}", path.display());
    for stream in listener.incoming().flatten() {
        let daemon = daemon.clone();
        std::thread::spawn(move || {
            let _ = serve(&daemon, stream);
        });
    }
    Ok(())
}

/// Each catalog's files (and the agent's program) as last read, with when they changed.
type CatalogStamps = HashMap<&'static str, Vec<(PathBuf, Option<SystemTime>)>>;

/// Set `d.catalogs` to what each agent's own files list, for those whose files (or the agent
/// itself) changed since `seen`.
fn read_catalogs(d: &Daemon, seen: &mut CatalogStamps) {
    for a in dino_core::agent::all() {
        let agent = a.id();
        let program = d.launchers.read().unwrap().iter().find(|l| l.agent_id == agent).map(|l| l.program.clone());
        let Some(program) = program else {
            d.catalogs.write().unwrap().remove(agent);
            continue;
        };
        let mut files = a.catalog_sources();
        files.push(PathBuf::from(&program));
        let stamp: Vec<_> = files
            .into_iter()
            .map(|p| {
                let m = p.metadata().and_then(|m| m.modified()).ok();
                (p, m)
            })
            .collect();
        if seen.get(agent) == Some(&stamp) {
            continue;
        }
        let catalog = a.catalog(&program);
        let mut catalogs = d.catalogs.write().unwrap();
        match catalog {
            Some(c) => catalogs.insert(agent.to_string(), c),
            None => catalogs.remove(agent),
        };
        seen.insert(agent, stamp);
    }
}

/// Keep reading the catalogs as the agents rewrite them; off the request path.
fn watch_catalogs(d: &Daemon, mut seen: CatalogStamps) {
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3));
        read_catalogs(d, &mut seen);
    }
}

/// The daemon's state, with what was saved of it (all but the sessions, see `restore`).
fn new_daemon(proxy: Proxy, launchers: Vec<LauncherInfo>) -> Arc<Daemon> {
    let daemon = Arc::new(Daemon {
        proxy,
        launchers: RwLock::new(launchers),
        catalogs: RwLock::default(),
        sessions: Mutex::default(),
        groups: Mutex::new(load_groups()),
        worktrees: Mutex::new(load_worktrees()),
        prs: Mutex::default(),
        pr_poll: Mutex::default(),
        closing: Mutex::default(),
        previews: Mutex::default(),
        subagents: Mutex::new(load_subagents()),
        summaries: Mutex::default(),
        schedule: schedule::Scheduler::load(),
        archived: Mutex::new(lifecycle::load_archived()),
        sizes: Mutex::default(),
        pushed: Mutex::default(),
        measuring: AtomicBool::new(false),
        next_id: AtomicU64::new(1),
        next_sub: AtomicU64::new(1),
    });
    // Archived ids stay theirs, so a new session never takes one.
    let max_id = daemon.archived.lock().unwrap().iter().filter_map(|a| a.saved.id.parse::<u64>().ok()).max().unwrap_or(0);
    daemon.next_id.fetch_max(max_id + 1, Ordering::Relaxed);
    daemon
}

fn launchers(free_tier: bool) -> Vec<LauncherInfo> {
    launchers_from(free_tier, detect_agents())
}

/// dino's key store changed: the proxy, what can be started and the providers follow.
/// A ChatGPT sign-in's tokens go to dino's key store, and the proxy uses them from the next request.
fn save_chatgpt(d: &Daemon, t: &chatgpt::Tokens) -> anyhow::Result<()> {
    for (k, v) in t.keys() {
        settings::set_key(k, Some(&v))?;
    }
    providers::set_error("chatgpt", None);
    keys_changed(d);
    Ok(())
}

fn keys_changed(d: &Daemon) {
    let keys = load_keys();
    *d.launchers.write().unwrap() = launchers(keys.contains_key("NVIDIA_API_KEY"));
    d.proxy.set_keys(keys);
    std::thread::spawn(|| providers::refresh(true));
}

fn launchers_from(free_tier: bool, agents: Vec<dino_core::Detected>) -> Vec<LauncherInfo> {
    let mut out = vec![];
    for d in agents {
        let program: String = d.path.to_string_lossy().into();
        if d.kind.id == "claude" && free_tier {
            out.push(LauncherInfo { short: "free".into(), agent_id: "claude-free".into(), label: "Claude Code · free models".into(), program: program.clone(), knobs: Default::default() });
        }
        if d.kind.id == "qwen" && free_tier {
            out.push(LauncherInfo { short: "qwen-free".into(), agent_id: "qwen-free".into(), label: "Qwen Code · free models".into(), program: program.clone(), knobs: Default::default() });
        }
        out.push(LauncherInfo { short: d.kind.id.into(), agent_id: d.kind.id.into(), label: d.kind.name.into(), program, knobs: Default::default() });
    }
    let shell = user_shell();
    let shell_name = shell.rsplit('/').next().unwrap_or("shell").to_string();
    out.push(LauncherInfo { short: "shell".into(), agent_id: "shell".into(), label: format!("Shell ({shell_name})"), program: shell, knobs: Default::default() });
    out
}

/// Every agent dino knows, as it is on this Mac now. Looked up on the login shell's current `PATH`,
/// so one installed since dinod started is found, and becomes one dino can start.
fn agent_setup(d: &Daemon) -> Vec<ipc::AgentSetupInfo> {
    let mut path = dino_core::discover::login_path().unwrap_or_default();
    if let Some(own) = std::env::var_os("PATH") {
        path.push(":");
        path.push(own);
    }
    let found = dino_core::detect_agents_in(&path);
    let startable: Vec<String> = d.launchers.read().unwrap().iter().map(|l| l.agent_id.clone()).collect();
    if found.iter().any(|a| !startable.iter().any(|s| s == a.kind.id)) {
        *d.launchers.write().unwrap() = launchers_from(load_keys().contains_key("NVIDIA_API_KEY"), found.clone());
    }
    std::thread::scope(|s| {
        let probes: Vec<_> = dino_core::KNOWN_AGENTS
            .iter()
            .filter(|k| k.id != "gemini")
            .map(|kind| {
                let bin = found.iter().find(|a| a.kind.id == kind.id).map(|a| a.path.clone());
                s.spawn(move || {
                    let setup = dino_core::discover::setup(kind.id);
                    let status = bin.as_deref().and_then(|b| dino_core::discover::sign_in_status(kind.id, b));
                    ipc::AgentSetupInfo {
                        id: kind.id.into(),
                        name: kind.name.into(),
                        version: bin.as_deref().and_then(dino_core::discover::version_of),
                        path: bin.map(|b| b.display().to_string()),
                        signed_in: status.as_ref().map(|s| s.0),
                        account: status.and_then(|s| s.1),
                        install: dino_core::discover::install_hint(kind.id).into(),
                        sign_in: setup.sign_in.map(String::from),
                        sign_in_hint: setup.sign_in_hint.map(String::from),
                        homepage: setup.homepage.into(),
                    }
                })
            })
            .collect();
        probes.into_iter().map(|p| p.join().unwrap()).collect()
    })
}

/// Run agent `id`'s own install or sign-in command in a new shell, typed at its prompt as if by
/// hand: the user sees it run, answers its questions, and the shell stays when it's done.
fn agent_action(d: &Daemon, id: &str, action: &str) -> anyhow::Result<String> {
    let kind = dino_core::KNOWN_AGENTS.iter().find(|k| k.id == id).ok_or_else(|| anyhow::anyhow!("unknown agent {id}"))?;
    let (command, label) = match action {
        "install" => (dino_core::discover::install_hint(id), format!("Install {}", kind.name)),
        "sign_in" => (dino_core::discover::setup(id).sign_in.unwrap_or_default(), format!("Sign in to {}", kind.name)),
        _ => anyhow::bail!("unknown action {action}"),
    };
    anyhow::ensure!(!command.is_empty(), "dino doesn't know how to {} {}", action.replace('_', " "), kind.name);
    let session = spawn(d, Launch::new("shell", vec![], Some(home().display().to_string())))?;
    let s = d.sessions.lock().unwrap().iter().find(|s| s.id == session).cloned().ok_or_else(|| anyhow::anyhow!("the shell went away"))?;
    *s.label.lock().unwrap() = Some(label);
    type_at_prompt(s, command.to_string());
    Ok(session)
}

/// Type `line` at a new shell's prompt and press Return, as if by hand: once the shell has drawn
/// its prompt, so the line lands there and not in its startup.
fn type_at_prompt(s: Arc<Session>, line: String) {
    std::thread::spawn(move || {
        let started = Instant::now();
        while s.pane.text(0).trim().is_empty() && started.elapsed() < std::time::Duration::from_secs(5) {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
        send_input(&s, &line, true);
    });
}

fn serve(d: &Arc<Daemon>, mut stream: UnixStream) -> io::Result<()> {
    loop {
        let (kind, payload) = ipc::read_frame(&mut stream)?;
        if kind != ipc::JSON {
            continue;
        }
        let req: Request = match serde_json::from_slice(&payload) {
            Ok(r) => r,
            Err(e) => {
                ipc::write_json(&mut stream, &Response::Error { message: format!("bad request: {e}") })?;
                continue;
            }
        };
        let resp = match req {
            Request::State => state(d),
            Request::Launchers => Response::Launchers { launchers: d.offered() },
            Request::AllLaunchers => Response::Launchers { launchers: d.all_launchers() },
            Request::AgentSetup => Response::AgentSetup { agents: agent_setup(d) },
            Request::AgentAction { id, action } => match agent_action(d, &id, &action) {
                Ok(id) => {
                    save(d);
                    Response::Created { id }
                }
                Err(e) => Response::Error { message: e.to_string() },
            },
            Request::Settings => {
                Response::Settings {
                    settings: Settings::load(),
                    locked: dino_core::settings::Managed::load().locked_paths(),
                    ssh_config_hosts: ssh::config_hosts(),
                }
            }
            Request::SetSettings { settings } => match settings.save() {
                Ok(()) => {
                    d.proxy.set_budget(Settings::load().policies.session_token_budget);
                    schedule::keep_awake(d);
                    Response::Ok
                }
                Err(e) => Response::Error { message: e.to_string() },
            },
            Request::ScheduleList => Response::Schedule { tasks: schedule::list(d) },
            Request::SchedulePut { task } => match schedule::put(d, task) {
                Ok(_) => Response::Schedule { tasks: schedule::list(d) },
                Err(e) => Response::Error { message: e.to_string() },
            },
            Request::ScheduleDelete { id } => match schedule::delete(d, &id) {
                Ok(()) => Response::Schedule { tasks: schedule::list(d) },
                Err(e) => Response::Error { message: e.to_string() },
            },
            Request::ScheduleRun { id } => match schedule::run_now(d, &id) {
                Ok(id) => Response::Created { id },
                Err(e) => Response::Error { message: e.to_string() },
            },
            Request::ScheduleTick { now } => {
                schedule::tick(d, now.unwrap_or_else(now_secs));
                Response::Schedule { tasks: schedule::list(d) }
            }
            Request::Keys => Response::Keys { keys: settings::key_status() },
            Request::SetKey { name, value } => match settings::set_key(&name, value.as_deref()) {
                Ok(()) => {
                    keys_changed(d);
                    Response::Ok
                }
                Err(e) => Response::Error { message: e.to_string() },
            },
            Request::ConnectProvider { provider } if provider == "openrouter" => {
                let d = d.clone();
                match providers::connect_openrouter(move |key| {
                    settings::set_key(providers::OPENROUTER_KEY, Some(&key))?;
                    keys_changed(&d);
                    Ok(())
                }) {
                    Ok(url) => Response::Connect { url },
                    Err(e) => Response::Error { message: e.to_string() },
                }
            }
            Request::DisconnectProvider { provider } if provider == "openrouter" => match settings::set_key(providers::OPENROUTER_KEY, None) {
                Ok(()) => {
                    providers::forget("openrouter");
                    keys_changed(d);
                    Response::Ok
                }
                Err(e) => Response::Error { message: e.to_string() },
            },
            Request::ConnectProvider { provider } if provider == "chatgpt" => {
                let d = d.clone();
                match chatgpt::connect(chatgpt::Endpoints::default(), move |t| {
                    save_chatgpt(&d, &t)
                }) {
                    Ok(url) => Response::Connect { url },
                    Err(e) => Response::Error { message: e.to_string() },
                }
            }
            Request::DisconnectProvider { provider } if provider == "chatgpt" => {
                match chatgpt::SIGNED_IN.iter().try_for_each(|k| settings::set_key(k, None)) {
                    Ok(()) => {
                        providers::forget("chatgpt");
                        keys_changed(d);
                        Response::Ok
                    }
                    Err(e) => Response::Error { message: e.to_string() },
                }
            }
            Request::ConnectProvider { provider } | Request::DisconnectProvider { provider } => {
                Response::Error { message: format!("{provider} doesn't sign in: dino finds it on this Mac") }
            }
            Request::Providers => Response::Providers { providers: providers::list() },
            Request::Models { provider } => {
                let (models, loading, error) = providers::rows(&provider);
                Response::Models { provider, models, loading, error }
            }
            Request::New { launcher, args, cwd, cols, rows, worktree, controls, host, prompt, by } => {
                // A shell's "prompt" is a line typed at its prompt (a script opened with dino, a
                // man page), not an argument.
                let (prompt, line) = if launcher == "shell" { (None, prompt) } else { (prompt, None) };
                let launch = Launch { cols, rows, controls, host, prompt, started_by: by, ..Launch::new(&launcher, args, cwd) };
                match if worktree { spawn_in_worktree(d, launch) } else { spawn(d, launch) } {
                    Ok(id) => {
                        if let Some(line) = line {
                            if let Some(s) = d.sessions.lock().unwrap().iter().find(|s| s.id == id).cloned() {
                                type_at_prompt(s, line);
                            }
                        }
                        save(d);
                        Response::Created { id }
                    }
                    Err(e) => Response::Error { message: e.to_string() },
                }
            }
            Request::SetControls { id, controls } => match set_controls(d, &id, controls) {
                Ok(()) => Response::Ok,
                Err(e) => Response::Error { message: e.to_string() },
            },
            Request::Kill { id } => {
                if kill(d, &id) {
                    save(d);
                    Response::Ok
                } else {
                    Response::Error { message: format!("no session {id}") }
                }
            }
            Request::Attach { id, cols, rows, wait } => {
                let session = loop {
                    match d.sessions.lock().unwrap().iter().find(|s| s.id == id).cloned() {
                        // Ended: wait for it to be resumed, from this client or any other.
                        Some(s) if wait && s.pane.is_exited() => {}
                        other => break other,
                    }
                    if client_gone(&stream) {
                        return Ok(());
                    }
                    std::thread::sleep(std::time::Duration::from_millis(200));
                };
                return match session {
                    Some(s) => attach(d, &s, stream, cols, rows),
                    None => ipc::write_json(&mut stream, &Response::Error { message: format!("no session {id}") }),
                };
            }
            Request::StopServer { id, task } => {
                let s = d.sessions.lock().unwrap().iter().find(|s| s.id == id).cloned();
                match s.map(|s| servers::stop(d, &s, &task)) {
                    Some(Ok(())) => Response::Ok,
                    Some(Err(e)) => Response::Error { message: e.to_string() },
                    None => Response::Error { message: format!("no session {id}") },
                }
            }
            Request::Resume { id } => match resume(d, &id) {
                Ok(()) => Response::Ok,
                Err(e) => Response::Error { message: e.to_string() },
            },
            Request::Found { cloud, running_only } => Response::Found { sessions: discover(d, cloud, running_only) },
            Request::TakeOver { id } => match take_over(d, &id) {
                Ok(()) => Response::Ok,
                Err(e) => Response::Error { message: e.to_string() },
            },
            Request::Conversation { agent, session_id, before } => match dino_core::history::conversation(&agent, &session_id, before) {
                Some(page) => Response::Conversation { page },
                None => Response::Error { message: "no transcript for that session on this Mac".into() },
            },
            Request::Adopt { session, cwd } => match adopt(d, session, cwd) {
                Ok(id) => {
                    save(d);
                    Response::Created { id }
                }
                Err(e) => Response::Error { message: e.to_string() },
            },
            Request::Fanout { prompt, launchers, cwd } => match fanout(d, &prompt, &launchers, cwd) {
                Ok(group) => {
                    save(d);
                    Response::Created { id: group }
                }
                Err(e) => Response::Error { message: e.to_string() },
            },
            Request::Groups => Response::Groups { groups: groups(d) },
            Request::Tree { folders } => Response::Tree { repos: tree(d, folders) },
            Request::Diff { session } => match member_diff(d, &session) {
                Ok((stat, text)) => Response::Diff { stat, text },
                Err(e) => Response::Error { message: e.to_string() },
            },
            Request::Changes { id } => changes(d, &id).unwrap_or_else(|e| Response::Error { message: e.to_string() }),
            // Each connection has its own thread, so a review blocks only the one asking.
            Request::Review { id } => match review(d, &id) {
                Ok(findings) => Response::Review { findings },
                Err(e) => Response::Error { message: e.to_string() },
            },
            Request::Start { launcher, cwd, prompt, worktree, by } => match peers::start(d, &launcher, cwd, prompt, worktree, by) {
                Ok(id) => Response::Created { id },
                Err(e) => Response::Error { message: e.to_string() },
            },
            Request::ReadSession { id, lines } => match peers::read(d, &id, lines) {
                Ok(text) => Response::Text { text },
                Err(e) => Response::Error { message: e.to_string() },
            },
            Request::ReadSubagent { session, agent, worktree } => {
                let found = match (&session, &agent, &worktree) {
                    (Some(session), Some(agent), _) => read_subagent(d, session, agent),
                    (_, _, Some(worktree)) => subagent_of_worktree(d, worktree),
                    _ => None,
                };
                match found {
                    Some(subagent) => Response::Subagent { subagent },
                    None => Response::Error { message: "dino doesn't know that subagent".into() },
                }
            }
            Request::Message { id, text, by } => peers::ipc_result(peers::message(d, &id, &text, by)),
            Request::Ask { id, question } => match peers::ask(d, &id, &question) {
                Ok(text) => Response::Text { text },
                Err(e) => Response::Error { message: e.to_string() },
            },
            Request::AskCancel { id } => {
                dino_core::review::cancel(&peers::ask_key(&id));
                Response::Ok
            }
            Request::ReviewCancel { id } => {
                dino_core::review::cancel(&id);
                Response::Ok
            }
            Request::SendInput { id, text, submit } => {
                let session = d.sessions.lock().unwrap().iter().find(|s| s.id == id).cloned();
                match session {
                    Some(s) if !s.pane.is_exited() => {
                        send_input(&s, &text, submit);
                        Response::Ok
                    }
                    Some(_) => Response::Error { message: format!("{id} has exited") },
                    None => Response::Error { message: format!("no session {id}") },
                }
            }
            Request::SendKeys { id, text } => match d.sessions.lock().unwrap().iter().find(|s| s.id == id).cloned() {
                Some(s) if !s.pane.is_exited() => {
                    s.pane.write(text.into_bytes());
                    Response::Ok
                }
                Some(_) => Response::Error { message: format!("{id} has exited") },
                None => Response::Error { message: format!("no session {id}") },
            },
            Request::ShellOutput { id } => match d.sessions.lock().unwrap().iter().find(|s| s.id == id).cloned() {
                Some(s) => Response::ShellOutput { output: s.pane.shared.last_output.lock().unwrap().clone(), exit: *s.pane.shared.last_exit.lock().unwrap() },
                None => Response::Error { message: format!("no session {id}") },
            },
            Request::PrDraft { id } => match pr_session(d, &id) {
                Ok(s) => {
                    let came_from = session_worktree(d, &s.cwd).and_then(|w| pr::branch(&w.checkout));
                    Response::PrDraft { draft: pr::draft(&s.cwd, came_from.as_deref(), s.pane.title().as_deref()) }
                }
                Err(e) => Response::Error { message: e.to_string() },
            },
            Request::PrCreate { id, title, body, base, draft } => {
                match pr_session(d, &id).and_then(|s| pr::create(&s.cwd, &title, &body, &base, draft)) {
                    Ok(pr) => {
                        d.prs.lock().unwrap().insert(id, pr.clone());
                        refresh_prs_soon(d);
                        Response::Pr { pr }
                    }
                    Err(e) => Response::Error { message: e.to_string() },
                }
            }
            Request::PrFix { id } => match pr_fix(d, &id) {
                Ok(()) => {
                    refresh_prs_soon(d);
                    Response::Ok
                }
                Err(e) => Response::Error { message: e.to_string() },
            },
            Request::PrMerge { id } => {
                let result = pr_session(d, &id).and_then(|s| {
                    let branch = pr::branch(&s.cwd).ok_or_else(|| anyhow::anyhow!("Not on a branch"))?;
                    pr::merge(&s.cwd, &branch)
                });
                match result {
                    Ok(pr) => {
                        pr_done(d, &id, &pr);
                        refresh_prs_soon(d);
                        Response::Pr { pr }
                    }
                    Err(e) => Response::Error { message: e.to_string() },
                }
            }
            Request::PrAuto { id, fix, merge } => match pr_session(d, &id) {
                Ok(s) => {
                    {
                        let mut a = s.auto.lock().unwrap();
                        if let Some(on) = fix {
                            // Turned on again: three more tries.
                            if on && !a.pr.fix {
                                a.pr.fixes = 0;
                            }
                            a.pr.fix = on;
                        }
                        if let Some(on) = merge {
                            if on && !a.pr.merge {
                                a.merge_failed = None;
                            }
                            a.pr.merge = on;
                        }
                        a.pr.note = None;
                    }
                    save(d);
                    // Act on the PR as it is now, not in half a minute.
                    refresh_prs_soon(d);
                    Response::Ok
                }
                Err(e) => Response::Error { message: e.to_string() },
            },
            Request::PreviewConfigs { id } => match session_cwd(d, &id) {
                Ok(cwd) => match dino_core::preview::configs(&cwd) {
                    Ok(configs) => Response::PreviewConfigs { configs, error: None },
                    Err(e) => Response::PreviewConfigs { configs: vec![], error: Some(e.to_string()) },
                },
                Err(e) => Response::Error { message: e.to_string() },
            },
            Request::PreviewStart { id, name } => match preview_start(d, &id, &name) {
                Ok(()) => Response::Ok,
                Err(e) => Response::Error { message: e.to_string() },
            },
            Request::PreviewStop { id, name } => {
                if let Some(s) = d.previews.lock().unwrap().iter().find(|s| s.session == id && s.config.name == name) {
                    s.stop();
                }
                Response::Ok
            }
            Request::PreviewLog { id, name } => match d.previews.lock().unwrap().iter().find(|s| s.session == id && s.config.name == name) {
                Some(s) => Response::PreviewLog { text: s.log() },
                None => Response::Error { message: format!("{name} hasn't been started") },
            },
            Request::Keep { session } => match keep(d, &session) {
                Ok(()) => Response::Ok,
                Err(e) => Response::Error { message: e.to_string() },
            },
            Request::Discard { group } => match close_group(d, &group) {
                Ok(()) => Response::Ok,
                Err(e) => Response::Error { message: e.to_string() },
            },
            Request::RemoveWorktree { path, apply } => match remove_worktree(d, &path, apply) {
                Ok(()) => Response::Ok,
                Err(e) => Response::Error { message: e.to_string() },
            },
            Request::CleanWorktree { path } => match (real(Path::new(&path)), worktree::clean(Path::new(&path))) {
                (key, Ok(_)) => {
                    d.summaries.lock().unwrap().remove(&key);
                    Response::Ok
                }
                (_, Err(e)) => Response::Error { message: format!("Couldn't clean up {path}: {e}") },
            },
            req @ (Request::Rename { .. }
            | Request::Pin { .. }
            | Request::Archive { .. }
            | Request::Archived
            | Request::Unarchive { .. }
            | Request::DeleteArchived { .. }
            | Request::Storage
            | Request::RemoveStored { .. }
            | Request::FreeUpSpace) => lifecycle::serve(d, req),
            Request::Shutdown => {
                // Saved first: `dino stop` pauses sessions, the next dinod resumes them.
                save(d);
                ipc::write_json(&mut stream, &Response::Ok)?;
                for s in d.sessions.lock().unwrap().drain(..) {
                    s.pane.kill();
                }
                for s in d.previews.lock().unwrap().drain(..) {
                    s.stop();
                }
                let _ = std::fs::remove_file(ipc::socket_path());
                std::process::exit(0);
            }
        };
        ipc::write_json(&mut stream, &resp)?;
    }
}

/// What to start. `restore` resumes a saved session; `prompt` is sent once, at the start.
#[derive(Default)]
struct Launch {
    launcher: String,
    args: Vec<String>,
    cwd: Option<String>,
    cols: u16,
    rows: u16,
    restore: Option<SavedSession>,
    name: Option<String>,
    prompt: Option<String>,
    /// For a new session; what's left open comes from Settings → Agents. A restored one keeps its own.
    controls: Controls,
    /// The scheduled task starting it, by name.
    scheduled: Option<String>,
    /// The session whose agent is starting it, by id.
    started_by: Option<String>,
    /// Over SSH, on this host; a restored session keeps its own.
    host: Option<String>,
}

impl Launch {
    fn new(launcher: &str, args: Vec<String>, cwd: Option<String>) -> Self {
        Self { launcher: launcher.into(), args, cwd, cols: 120, rows: 40, ..Default::default() }
    }
}

fn spawn(d: &Daemon, launch: Launch) -> anyhow::Result<String> {
    let Launch { launcher, args, cwd, cols, rows, restore, name, prompt, controls, scheduled, started_by, host } = launch;
    let host = restore.as_ref().map_or(host, |r| r.host.clone());
    let launcher = launcher.as_str();
    // Sessions already running come back even if the policies changed since; new ones must be allowed.
    let l = match restore {
        Some(_) => d.launcher(launcher).ok_or_else(|| anyhow::anyhow!("unknown agent {launcher}"))?,
        None => d.allowed_launcher(launcher)?,
    };
    let settings = Settings::load();
    // Control flags typed into the args count as the session's controls, and give way to one
    // chosen later: a session started with --dangerously-skip-permissions is in bypass.
    let in_args = controls::from_args(&l.agent_id, &args);
    let controls = match &restore {
        Some(r) => r.controls.or(&in_args),
        None => {
            let controls = controls.or(&in_args);
            check_bypass(&controls, &settings)?;
            // A default saved while bypass was allowed falls back to the agent's own mode.
            let mut defaults = settings.agent_defaults(&l.agent_id);
            if check_bypass(&defaults, &settings).is_err() {
                defaults.mode = None;
            }
            controls.or(&defaults)
        }
    };
    let args = controls::without(&l.agent_id, &args, &controls);
    let id = match &restore {
        Some(r) => r.id.clone(),
        None => d.next_id.fetch_add(1, Ordering::Relaxed).to_string(),
    };
    let mut agent_session = restore.as_ref().and_then(|r| r.agent_session.clone());
    // Ended before dinod stopped: it comes back as it was, not running, until resumed.
    let ended = restore.as_ref().filter(|r| r.ended);
    let (spec, cwd) = match &host {
        _ if ended.is_some() => (None, PathBuf::from(ended.map(|r| r.cwd.clone()).unwrap_or_default())),
        Some(host) => {
            let folder = cwd.filter(|c| !c.is_empty()).or_else(|| settings.ssh.get(host).map(|h| h.folder.clone())).filter(|f| !f.is_empty());
            let folder = folder.unwrap_or_else(|| "~".into());
            let restoring = restore.is_some();
            (Some(remote_spec(d, &settings, &l, host, &folder, &id, &mut agent_session, restoring, &controls, &args, prompt)?), PathBuf::from(folder))
        }
        None => {
            let (spec, cwd) = local_spec(d, &settings, &l, cwd, &id, &mut agent_session, restore.is_some(), &controls, &args, prompt);
            (Some(spec), cwd)
        }
    };

    let subscribers: Arc<Mutex<Vec<(u64, Sender<Vec<u8>>)>>> = Arc::default();
    let last_output: Arc<Mutex<Option<Instant>>> = Arc::default();
    let poked: Arc<Mutex<Option<Instant>>> = Arc::default();
    let last_write: Arc<Mutex<Option<Instant>>> = Arc::default();
    let local_url: Arc<Mutex<Option<String>>> = Arc::default();
    let (subs, last, poke, write, url) = (subscribers.clone(), last_output.clone(), poked.clone(), last_write.clone(), local_url.clone());
    let mut tail = String::new();
    let tap = move |bytes: &[u8]| {
        if !bytes.is_empty() {
            *write.lock().unwrap() = Some(Instant::now());
        }
        // Echoes and redraws answer the user; they don't mean the agent is working.
        let echo = poke.lock().unwrap().is_some_and(|t| t.elapsed() < USER_ECHO);
        if !bytes.is_empty() && !echo {
            *last.lock().unwrap() = Some(Instant::now());
            // A dev server the agent started, or told the user about: the app offers a preview.
            // Only up to the last space, so an address split across two chunks is read whole, from
            // the next one with the unread tail in front.
            let text = format!("{tail}{}", preview::strip_ansi(&String::from_utf8_lossy(bytes)));
            let done = text.char_indices().rfind(|(_, c)| c.is_whitespace()).map_or(0, |(i, c)| i + c.len_utf8());
            if let Some(found) = dino_core::preview::find_local_url(&text[..done]) {
                *url.lock().unwrap() = Some(found);
            }
            let rest = &text[done..];
            tail = if rest.len() > 256 { String::new() } else { rest.to_string() };
        }
        // An empty chunk means EOF; it's forwarded so clients learn the session ended.
        subs.lock().unwrap().retain(|(_, tx)| tx.send(bytes.to_vec()).is_ok());
    };
    let pane = match spec {
        Some(spec) => Pane::spawn(spec, cols.max(20), rows.max(5), tap)?,
        None => Pane::ended(&load_screen(&id), cols.max(20), rows.max(5), ended.and_then(|r| r.exit_code)),
    };

    let mut sessions = d.sessions.lock().unwrap();
    let name = match (&restore, name) {
        (Some(r), _) => r.name.clone(),
        (None, Some(name)) => name,
        (None, None) => {
            // The first free name: counting would repeat one once an earlier session is gone.
            let taken = |n: &str| sessions.iter().any(|s| s.name == n);
            std::iter::once(l.short.clone()).chain((2..).map(|n| format!("{}-{n}", l.short))).find(|n| !taken(n)).unwrap()
        }
    };
    sessions.push(Arc::new(Session {
        id: id.clone(),
        name,
        agent_id: l.agent_id.clone(),
        launcher: l.short.clone(),
        args,
        cwd,
        started_at: restore.as_ref().map_or_else(now_secs, |r| r.started_at),
        agent_session: Mutex::new(agent_session),
        pane,
        subscribers,
        last_output,
        last_write,
        poked,
        local_url,
        attached: AtomicUsize::new(0),
        auto: Mutex::new(restore.as_ref().map(|r| r.auto.clone()).unwrap_or_default()),
        controls,
        pending: Mutex::default(),
        scheduled: restore.as_ref().map_or(scheduled, |r| r.scheduled.clone()),
        started_by: restore.as_ref().map_or(started_by, |r| r.started_by.clone()),
        messaged_by: Mutex::new(restore.as_ref().and_then(|r| r.messaged_by.clone())),
        label: Mutex::new(restore.as_ref().and_then(|r| r.label.clone())),
        pinned: AtomicBool::new(restore.as_ref().is_some_and(|r| r.pinned)),
        host,
        rollout: Mutex::default(),
        inside: Mutex::default(),
        screen_saved: AtomicBool::new(false),
        servers: Mutex::new(vec![]),
    }));
    Ok(id)
}

/// Session `id` on this Mac: the agent itself, wired to dino's proxy and hooks, in `cwd`.
#[allow(clippy::too_many_arguments)]
fn local_spec(
    d: &Daemon,
    settings: &Settings,
    l: &LauncherInfo,
    cwd: Option<String>,
    id: &str,
    agent_session: &mut Option<String>,
    restoring: bool,
    controls: &Controls,
    args: &[String],
    prompt: Option<String>,
) -> (SpawnSpec, PathBuf) {
    let cwd = cwd.map(PathBuf::from).or_else(|| std::env::current_dir().ok()).unwrap_or_default();
    // Claude reports its context window to its statusline; wrap the user's, if they have one.
    let adapter = agent(&l.agent_id);
    let status_line = std::env::current_exe()
        .ok()
        .filter(|_| adapter.is_some_and(|a| a.statusline()))
        .and_then(|dino| dino_core::statusline::wrapper(&cwd, &dino, &d.proxy.base_url(id, "hook")));
    let (wiring_env, mut wired_args) = proxy_wiring(&l.agent_id, settings.routing.proxy, &|provider| d.proxy.base_url(id, provider), status_line);
    // The repo's environment first: dino's own wiring must win, or metering and hooks break.
    let mut env: HashMap<String, String> = repo_env(settings, &cwd).into_iter().collect();
    env.extend(wiring_env);
    // Which session this is, for `dino mcp` run inside it (added to an agent's config by hand).
    env.insert("DINO_SESSION".into(), id.to_string());
    if adapter.is_some_and(|a| a.session_tools()) && settings.policies.session_tools {
        peers::wire_claude(id, &mut wired_args);
    }
    if l.agent_id == "shell" && settings.machine.shell_integration {
        shell::wire(&l.program, &mut env, &mut wired_args);
    }

    // Resume the agent's own conversation when we know it; otherwise start one we can resume later.
    if let Some(a) = adapter {
        let (before, after) = a.session_args(agent_session, restoring);
        wired_args.splice(0..0, before);
        wired_args.extend(after);
    }
    wired_args.extend(controls::args(&l.agent_id, controls, &d.knobs(&l.agent_id, true)));
    wired_args.extend(args.iter().cloned());
    wired_args.extend(prompt.map(|p| prompt_args(&l.agent_id, p)).unwrap_or_default());
    (SpawnSpec { program: l.program.clone(), args: wired_args, cwd: Some(cwd.clone()), env }, cwd)
}

/// Session `id` on `host`: `ssh` in the terminal, the agent in `folder` there. Only Claude's
/// status hooks come back, through a tunnel to a hooks-only port (see `dino_core::ssh`); API
/// traffic goes direct from the host, since routing it here would put dino's proxy, and the keys
/// behind it, within reach of everyone on that machine.
#[allow(clippy::too_many_arguments)]
fn remote_spec(
    d: &Daemon,
    settings: &Settings,
    l: &LauncherInfo,
    host: &str,
    folder: &str,
    id: &str,
    agent_session: &mut Option<String>,
    restoring: bool,
    controls: &Controls,
    args: &[String],
    prompt: Option<String>,
) -> anyhow::Result<SpawnSpec> {
    anyhow::ensure!(settings.ssh.contains_key(host), "{host} isn't one of your environments (Settings → Environments)");
    let mut wired: Vec<String> = vec![];
    let mut tunnel = None;
    let bin = Path::new(&l.program).file_name().map_or_else(|| l.program.clone(), |n| n.to_string_lossy().into_owned());
    let program = match l.agent_id.as_str() {
        "claude" => {
            let port = ssh::pick_port();
            tunnel = Some((port, d.proxy.remote_port));
            wired.extend(["--settings".into(), dino_core::claude_hook_settings(&d.proxy.remote_hook_url(id, &new_uuid(), port), None)]);
            ssh::Program::Claude { session: agent_session.get_or_insert_with(new_uuid), resume: restoring }
        }
        _ if agent(&l.agent_id).is_some_and(|a| a.free()) => anyhow::bail!("{} runs through dino on this Mac; start it here instead", l.label),
        "shell" => ssh::Program::Shell,
        _ => ssh::Program::Agent { bin: &bin, name: &l.label },
    };
    wired.extend(controls::args(&l.agent_id, controls, &d.knobs(&l.agent_id, true)));
    wired.extend(args.iter().cloned());
    wired.extend(prompt.map(|p| prompt_args(&l.agent_id, p)).unwrap_or_default());
    let command = ssh::remote_command(host, folder, &program, &wired);
    let dir = ssh::control_dir();
    std::fs::create_dir_all(&dir)?;
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    let env = HashMap::from([("DINO_SESSION".to_string(), id.to_string())]);
    Ok(SpawnSpec { program: "ssh".into(), args: ssh::ssh_args(host, tunnel, &command), cwd: Some(home()), env })
}

/// The mode that never asks is refused unless the policies allow it.
fn check_bypass(c: &Controls, settings: &Settings) -> anyhow::Result<()> {
    anyhow::ensure!(c.mode.as_deref() != Some("bypass") || settings.policies.allow_bypass, "bypass mode isn't allowed by your policies (Settings → Policies)");
    Ok(())
}

/// Settings → Repositories' environment for sessions in `dir`: its repo's, found by the main
/// checkout so that worktrees share it.
fn repo_env(settings: &Settings, dir: &Path) -> Vec<(String, String)> {
    if settings.repos.values().all(|r| r.env.is_empty()) {
        return vec![];
    }
    // Keys are main checkouts, but one chosen by hand may be a worktree or a folder inside the repo.
    let main_of = |p: &Path| worktree::list(p).ok().and_then(|w| w.into_iter().next()).map(|w| real(Path::new(&w.path)));
    let Some(main) = main_of(dir) else { return vec![] };
    let repo = |k: &String| real(Path::new(k)) == main || main_of(Path::new(k)).as_ref() == Some(&main);
    settings.repos.iter().filter(|(k, r)| !r.env.is_empty() && repo(k)).flat_map(|(_, r)| r.env.clone()).collect()
}

/// Change session `id`'s controls: now if nothing of its own is running, else once it's done.
fn set_controls(d: &Daemon, id: &str, controls: Controls) -> anyhow::Result<()> {
    let settings = Settings::load();
    let s = d.sessions.lock().unwrap().iter().find(|s| s.id == id).cloned().ok_or_else(|| anyhow::anyhow!("no session {id}"))?;
    if s.controls.mode != controls.mode {
        check_bypass(&controls, &settings)?;
    }
    // Switched in the agent itself (Claude's Shift+Tab): choosing the mode dino has on file still restarts it into that mode.
    let agent_mode = d.proxy.stats.session(id).agent_mode.as_deref().and_then(|m| controls::reported_mode(&s.agent_id, m));
    let switched = controls.mode.is_some() && agent_mode.is_some() && agent_mode != controls.mode;
    if controls == s.controls && !switched {
        *s.pending.lock().unwrap() = None;
        return Ok(());
    }
    // An ended session keeps them for when it's resumed: it doesn't start again on its own.
    if restartable(d, &s) && !s.pane.is_exited() {
        *s.pending.lock().unwrap() = None;
        restart(d, id, controls)
    } else {
        *s.pending.lock().unwrap() = Some(controls);
        Ok(())
    }
}

/// See `state`: how long a working agent can be silent before its turn counts as over.
const TURN_OVER_QUIET: std::time::Duration = std::time::Duration::from_millis(2500);

/// How long after a keystroke, resize or attach the agent's output is taken as its answer to
/// that (an echo, a redraw) rather than as work of its own.
const USER_ECHO: std::time::Duration = std::time::Duration::from_millis(700);

impl Session {
    fn poke(&self) {
        *self.poked.lock().unwrap() = Some(Instant::now());
    }
}

/// Whether the client on `stream` hung up. Only for a client that sends nothing meanwhile.
fn client_gone(stream: &UnixStream) -> bool {
    use std::os::fd::AsRawFd;
    let mut b = 0u8;
    // SAFETY: a one-byte peek into a live local.
    let n = unsafe { libc::recv(stream.as_raw_fd(), (&raw mut b).cast(), 1, libc::MSG_PEEK | libc::MSG_DONTWAIT) };
    n == 0 || (n < 0 && io::Error::last_os_error().kind() != io::ErrorKind::WouldBlock)
}

/// What an attached client is told when `s`'s program ends: the line to show if the session is
/// kept to be resumed, nothing if it's gone (killed, archived).
fn ended_note(d: &Daemon, s: &Arc<Session>) -> Vec<u8> {
    if !d.sessions.lock().unwrap().iter().any(|o| Arc::ptr_eq(o, s)) {
        return vec![];
    }
    // The output ends a moment before the process is reaped.
    let since = Instant::now();
    while s.pane.exit_code().is_none() && since.elapsed() < std::time::Duration::from_secs(1) {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let label = d.launcher(&s.launcher).map_or_else(|| s.launcher.clone(), |l| l.label);
    let what = match s.pane.exit_code() {
        Some(c) if c != 0 => format!("{label} exited with code {c}"),
        _ => format!("{label} ended"),
    };
    let action = if s.agent_id == "shell" { "Enter starts a new one" } else { "Enter resumes" };
    format!("{what} · {action}").into_bytes()
}

/// Stream a session to a client until it detaches or the session ends.
fn attach(d: &Arc<Daemon>, s: &Arc<Session>, mut stream: UnixStream, cols: u16, rows: u16) -> io::Result<()> {
    ipc::write_json(&mut stream, &Response::Ok)?;
    s.poke();
    // The most recent client decides the size, like tmux's "latest".
    s.pane.resize(cols, rows);
    // The client's terminal answers queries now; the daemon's emulator must stay quiet.
    s.attached.fetch_add(1, Ordering::Relaxed);
    s.pane.shared.answer_queries.store(false, Ordering::Relaxed);

    let sub_id = d.next_sub.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = channel::<Vec<u8>>();
    let replay = s.pane.replay_then(REPLAY_HISTORY, |bytes| {
        s.subscribers.lock().unwrap().push((sub_id, tx));
        bytes
    });
    let mut out = stream.try_clone()?;
    ipc::write_frame(&mut out, ipc::DATA, &replay)?;
    let exited_already = s.pane.is_exited();
    let (d2, s2) = (d.clone(), s.clone());
    let writer = std::thread::spawn(move || {
        if exited_already {
            let _ = ipc::write_frame(&mut out, ipc::EXIT, &ended_note(&d2, &s2));
            return;
        }
        while let Ok(bytes) = rx.recv() {
            if bytes.is_empty() {
                // A fullscreen agent ends on its last screen, not the one it switched back to.
                if s2.pane.shared.kept_alt.load(Ordering::Relaxed) {
                    let screen = [b"\x1b[H\x1b[2J\x1b[3J".as_slice(), &s2.pane.replay(REPLAY_HISTORY)].concat();
                    let _ = ipc::write_frame(&mut out, ipc::DATA, &screen);
                }
                let _ = ipc::write_frame(&mut out, ipc::EXIT, &ended_note(&d2, &s2));
                break;
            }
            if ipc::write_frame(&mut out, ipc::DATA, &bytes).is_err() {
                break;
            }
        }
        let _ = out.shutdown(std::net::Shutdown::Both);
    });

    while let Ok((kind, payload)) = ipc::read_frame(&mut stream) {
        match kind {
            ipc::DATA => {
                // Includes the focus in/out reports agents ask for: Claude repaints on those.
                s.poke();
                s.pane.write(payload)
            }
            ipc::RESIZE => {
                s.poke();
                if let Some((c, r)) = ipc::parse_resize(&payload) {
                    s.pane.resize(c, r);
                }
            }
            _ => {}
        }
    }
    s.subscribers.lock().unwrap().retain(|(id, _)| *id != sub_id);
    if s.attached.fetch_sub(1, Ordering::Relaxed) == 1 {
        s.pane.shared.answer_queries.store(true, Ordering::Relaxed);
    }
    let _ = writer.join();
    Ok(())
}

/// The session's usage and activity.
fn stats(d: &Daemon, s: &Session) -> SessionStats {
    let st = d.proxy.stats.session(&s.id);
    // Mid-turn, Claude's spinner redraws many times a second, focused or not, even
    // while a tool runs. Gone quiet with no model call out: the turn was interrupted.
    let quiet = s.last_write.lock().unwrap().is_none_or(|t| t.elapsed() > TURN_OVER_QUIET);
    if st.activity == Some(Activity::Working) && st.in_flight == 0 && quiet && !st.tracked {
        d.proxy.stats.end_turn(&s.id);
        return d.proxy.stats.session(&s.id);
    }
    st
}

/// Tokens in the context window, and its size as the agent reports it: Claude to its statusline,
/// Codex in its rollout. Without a report, the tokens the proxy saw and no size: the window
/// depends on the model, its variant and the agent's own settings, so dino doesn't guess. A report
/// without usage (a new or just compacted conversation) means an empty window.
fn context_use(s: &Session, st: &SessionStats) -> (u64, Option<u64>) {
    if let Some(r) = st.reported_context {
        return (r.used.unwrap_or(0), Some(r.window));
    }
    if watched(s) && let Some((used, window)) = codex::context(s) {
        return (used, Some(window));
    }
    (st.context().map_or(0, |(_, used)| used), None)
}

/// Whether a restart would cut nothing short: between turns, with no subagent or background
/// command of its own still running, since those end with the agent.
fn restartable(d: &Daemon, s: &Session) -> bool {
    if s.pane.is_exited() {
        return true;
    }
    let st = d.proxy.stats.session(&s.id);
    idle(d, s) && !st.subagents.iter().any(|a| a.running) && !st.background.iter().any(|b| b.running)
}

/// "1 agent", "2 commands", "1 agent, 1 command": what a turn ended on and still runs.
fn ports(servers: &[servers::Server]) -> String {
    servers.iter().flat_map(|x| x.ports.iter().map(u16::to_string)).collect::<Vec<_>>().join(", ")
}

fn waiting_words(agents: usize, commands: usize) -> String {
    let n = |k: usize, one: &str| (k > 0).then(|| format!("{k} {one}{}", if k == 1 { "" } else { "s" }));
    [n(agents, "agent"), n(commands, "command")].into_iter().flatten().collect::<Vec<_>>().join(", ")
}

/// Between turns: not working, not waiting on the user, and quiet.
fn idle(d: &Daemon, s: &Session) -> bool {
    let st = stats(d, s);
    let quiet = s.last_output.lock().unwrap().is_none_or(|t| t.elapsed() > TURN_OVER_QUIET);
    !s.pane.is_exited() && st.in_flight == 0 && !matches!(st.activity, Some(Activity::Working | Activity::NeedsPermission(_))) && quiet
}

/// Idle, and not waiting on subagents or background commands its last turn left running.
fn finished(d: &Daemon, s: &Session) -> bool {
    idle(d, s) && d.proxy.stats.session(&s.id).waiting() == (0, 0)
}

fn state(d: &Daemon) -> Response {
    let groups = d.groups.lock().unwrap().clone();
    let prs = d.prs.lock().unwrap().clone();
    let previews = d.previews.lock().unwrap().clone();
    let group_of = |id: &str| groups.iter().find(|g| g.members.iter().any(|m| m.session == id)).map(|g| g.id.clone());
    let live = d.sessions.lock().unwrap().clone();
    let sessions = live
        .iter()
        .map(|s| {
            let st = stats(d, s);
            let (context_tokens, context_limit) = context_use(s, &st);
            let label = s.label.lock().unwrap().clone();
            let tasks = session_tasks(&st, &s.cwd, s.pane.is_exited());
            let waiting = st.waiting();
            let serving = s.servers.lock().unwrap().clone();
            // A server isn't work to wait on: once it's all that runs, the turn is over.
            let serving_waited = serving.iter().filter(|x| st.waits_on(&x.task)).count();
            SessionInfo {
                id: s.id.clone(),
                name: s.name.clone(),
                agent_id: s.agent_id.clone(),
                title: label.clone().or_else(|| s.pane.title()),
                exited: s.pane.is_exited(),
                exit_code: s.pane.exit_code(),
                output_ms_ago: s.last_output.lock().unwrap().map(|t| t.elapsed().as_millis() as u64),
                bells: s.pane.shared.bells.load(Ordering::Relaxed),
                requests: st.requests,
                in_flight: st.in_flight,
                input_tokens: st.usage.total_input(),
                output_tokens: st.usage.output,
                last_model: st.last_model,
                tier: st.tier,
                activity: st.activity.map(|a| match a {
                    Activity::Working => "working".into(),
                    Activity::Done => match (waiting.0, waiting.1.saturating_sub(serving_waited)) {
                        (0, 0) if serving_waited > 0 => format!("server:{}", ports(&serving)),
                        (0, 0) => "done".into(),
                        (agents, commands) => format!("waiting:{}", waiting_words(agents, commands)),
                    },
                    Activity::NeedsPermission(what) => format!("needs:{what}"),
                }),
                group: group_of(&s.id),
                error: st.last_error,
                cwd: if s.host.is_some() { s.cwd.display().to_string() } else { real(&s.cwd) },
                host: s.host.clone(),
                pr: prs.get(&s.id).cloned(),
                auto: s.auto.lock().unwrap().pr.clone(),
                previews: previews.iter().filter(|p| p.session == s.id).map(|p| p.info()).collect(),
                local_url: s.local_url.lock().unwrap().clone(),
                controls: s.controls.clone(),
                pending: s.pending.lock().unwrap().clone(),
                agent_mode: st.agent_mode.as_deref().and_then(|m| controls::reported_mode(&s.agent_id, m)),
                context_tokens,
                context_limit,
                scheduled: s.scheduled.clone(),
                started_by: s.started_by.clone(),
                messaged_by: s.messaged_by.lock().unwrap().clone(),
                label,
                pinned: s.pinned.load(Ordering::Relaxed),
                tasks,
                inside: s.inside.lock().unwrap().found.clone(),
                shell_cwd: s.pane.shared.cwd.lock().unwrap().clone(),
                last_exit: *s.pane.shared.last_exit.lock().unwrap(),
                servers: serving.into_iter().map(|x| ipc::ServerInfo { task: x.task, command: x.command, ports: x.ports }).collect(),
            }
        })
        .collect();
    let quotas = ["anthropic", "chatgpt"]
        .iter()
        .filter_map(|p| {
            d.proxy.stats.quota(p).map(|q| QuotaInfo {
                provider: p.to_string(),
                windows: q.windows.into_iter().map(|(name, w)| WindowInfo { name, utilization: w.utilization, resets_at: w.resets_at }).collect(),
            })
        })
        .collect();
    Response::State { sessions, quotas }
}

// ---- Persistence: sessions survive dinod restarts (and reboots) by resuming each agent. ----

#[derive(Serialize, Deserialize, Clone)]
struct SavedSession {
    id: String,
    name: String,
    launcher: String,
    args: Vec<String>,
    cwd: String,
    started_at: u64,
    agent_session: Option<String>,
    #[serde(default)]
    auto: AutoState,
    #[serde(default)]
    controls: Controls,
    #[serde(default)]
    scheduled: Option<String>,
    #[serde(default)]
    started_by: Option<String>,
    #[serde(default)]
    messaged_by: Option<String>,
    label: Option<String>,
    #[serde(default)]
    pinned: bool,
    #[serde(default)]
    host: Option<String>,
    /// Its program exited on its own: it comes back ended, its last screen up, until resumed.
    #[serde(default)]
    ended: bool,
    #[serde(default)]
    exit_code: Option<u32>,
}

fn saved_path() -> PathBuf {
    dino_core::config_dir().join("sessions.json")
}

fn load_saved() -> Vec<SavedSession> {
    std::fs::read(saved_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// Where an ended session's last screen is kept, to show it again after dinod restarts.
fn screens_dir() -> PathBuf {
    dino_core::config_dir().join("screens")
}

fn load_screen(id: &str) -> Vec<u8> {
    std::fs::read(screens_dir().join(id)).unwrap_or_default()
}

/// Write the sessions to disk: running ones, and ended ones with their last screen, which stay
/// until the user resumes or removes them.
fn save(d: &Daemon) {
    let sessions = d.sessions.lock().unwrap().clone();
    let saved: Vec<SavedSession> = sessions.iter().map(|s| snapshot(s)).collect();
    let dir = screens_dir();
    for s in sessions.iter().filter(|s| s.pane.is_exited() && !s.screen_saved.load(Ordering::Relaxed)) {
        use std::os::unix::fs::OpenOptionsExt;
        let _ = std::fs::create_dir_all(&dir);
        let written = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(dir.join(&s.id))
            .and_then(|mut f| io::Write::write_all(&mut f, &s.pane.replay(REPLAY_HISTORY)));
        s.screen_saved.store(written.is_ok(), Ordering::Relaxed);
    }
    // A screen whose session was resumed or removed goes.
    for f in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let name = f.file_name().to_string_lossy().into_owned();
        if !sessions.iter().any(|s| s.id == name && s.pane.is_exited()) {
            let _ = std::fs::remove_file(f.path());
        }
    }
    let tmp = saved_path().with_extension("json.tmp");
    if std::fs::write(&tmp, serde_json::to_vec_pretty(&saved).unwrap_or_default()).is_ok() {
        let _ = std::fs::rename(tmp, saved_path());
    }
}

/// What it takes to bring `s` back.
fn snapshot(s: &Session) -> SavedSession {
    let mut agent_session = s.agent_session.lock().unwrap();
    if agent_session.is_none() {
        *agent_session = conversation_of(s);
    }
    SavedSession {
        id: s.id.clone(),
        name: s.name.clone(),
        launcher: s.launcher.clone(),
        args: s.args.clone(),
        cwd: s.cwd.display().to_string(),
        started_at: s.started_at,
        agent_session: agent_session.clone(),
        auto: s.auto.lock().unwrap().clone(),
        controls: s.controls.clone(),
        scheduled: s.scheduled.clone(),
        started_by: s.started_by.clone(),
        messaged_by: s.messaged_by.lock().unwrap().clone(),
        label: s.label.lock().unwrap().clone(),
        pinned: s.pinned.load(Ordering::Relaxed),
        host: s.host.clone(),
        ended: s.pane.is_exited(),
        exit_code: s.pane.exit_code(),
    }
}

/// Run session `id` with `controls` from now on: stop its agent and resume the conversation
/// with them, under the same id. Attached clients see the socket drop without an exit and
/// reattach (see `dino attach`), so the terminal carries on.
fn restart(d: &Daemon, id: &str, controls: Controls) -> anyhow::Result<()> {
    let (s, mut saved) = {
        let sessions = d.sessions.lock().unwrap();
        let s = sessions.iter().find(|s| s.id == id).cloned().ok_or_else(|| anyhow::anyhow!("no session {id}"))?;
        let saved = snapshot(&s);
        (s, saved)
    };
    // Dropping the subscribers ends each client's stream without the exit the dying agent would send.
    s.subscribers.lock().unwrap().clear();
    s.pane.kill();
    d.proxy.stats.restarted(id);
    if saved.controls.model != controls.model {
        d.proxy.stats.reset_context(id);
    }
    saved.controls = controls;
    saved.ended = false;
    saved.exit_code = None;
    let (cols, rows) = s.pane.size();
    let launch = Launch { cols, rows, restore: Some(saved.clone()), ..Launch::new(&saved.launcher, saved.args.clone(), Some(saved.cwd.clone())) };
    let spawned = spawn(d, launch);
    // The new one takes the old one's place, so the session never drops out of the list.
    take_place(d, &s);
    save(d);
    spawned.map(|_| ())
}

/// The session just spawned with `old`'s id replaces `old` in the list, at its position.
fn take_place(d: &Daemon, old: &Arc<Session>) {
    let mut sessions = d.sessions.lock().unwrap();
    let i = sessions.iter().position(|o| Arc::ptr_eq(o, old));
    let j = sessions.iter().rposition(|o| o.id == old.id && !Arc::ptr_eq(o, old));
    if let (Some(i), Some(j)) = (i, j) {
        let new = sessions.remove(j);
        sessions[if j < i { i - 1 } else { i }] = new;
    }
}

/// Start session `id`'s program again after it ended: the agent resumes its conversation, a
/// shell starts afresh, in the same place and with any controls chosen meanwhile.
fn resume(d: &Daemon, id: &str) -> anyhow::Result<()> {
    let s = d.sessions.lock().unwrap().iter().find(|s| s.id == id).cloned().ok_or_else(|| anyhow::anyhow!("no session {id}"))?;
    anyhow::ensure!(s.pane.is_exited(), "{} is still running", s.name);
    let controls = s.pending.lock().unwrap().take().unwrap_or_else(|| s.controls.clone());
    restart(d, id, controls)
}

/// Apply controls asked for mid-turn, now that the turn is over.
fn apply_pending(d: &Daemon) {
    let ready: Vec<(String, Controls)> = d
        .sessions
        .lock()
        .unwrap()
        .clone()
        .iter()
        .filter(|s| !s.pane.is_exited() && s.pending.lock().unwrap().is_some() && restartable(d, s))
        .filter_map(|s| s.pending.lock().unwrap().take().map(|c| (s.id.clone(), c)))
        .collect();
    for (id, controls) in ready {
        if let Err(e) = restart(d, &id, controls) {
            eprintln!("restart {id}: {e}");
        }
    }
}

fn restore(d: &Daemon, saved: Vec<SavedSession>) {
    let max_id = saved.iter().filter_map(|s| s.id.parse::<u64>().ok()).max().unwrap_or(0);
    d.next_id.fetch_max(max_id + 1, Ordering::Relaxed);
    for s in saved {
        if let Err(e) = spawn(d, Launch { restore: Some(s.clone()), ..Launch::new(&s.launcher, s.args.clone(), Some(s.cwd.clone())) }) {
            eprintln!("restore {} ({}): {e}", s.id, s.name);
        }
    }
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// `prompt` for `agent_id` to start on: most take it as their last argument.
fn prompt_args(agent_id: &str, prompt: String) -> Vec<String> {
    match agent(agent_id) {
        Some(a) => a.prompt_args(prompt),
        None => vec![prompt],
    }
}

/// The conversation `s`'s agent is on, looked at now, for agents that name it only as they go.
fn conversation_of(s: &Session) -> Option<String> {
    s.host.is_none().then(|| agent(&s.agent_id)?.conversation_of(s.pane.pid()?)).flatten()
}

/// `s` is on this Mac, and dino follows its agent's own record of its turns (see `codex`).
fn watched(s: &Session) -> bool {
    s.host.is_none() && agent(&s.agent_id).is_some_and(|a| a.status_source() == StatusSource::Rollout)
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

// ---- Continue anything: sessions dino didn't start. ----

/// Found sessions minus the ones dino itself is running, or that run inside its shells.
fn discover(d: &Daemon, cloud: bool, running_only: bool) -> Vec<FoundSession> {
    let sessions = d.sessions.lock().unwrap().clone();
    let inside: Vec<FoundSession> = sessions.iter().filter_map(|s| s.inside.lock().unwrap().found.clone()).collect();
    // dino's own conversations, live or archived, are listed as dino sessions already.
    let mut ours: Vec<String> = sessions.iter().filter_map(|s| s.agent_session.lock().unwrap().clone()).collect();
    ours.extend(d.archived.lock().unwrap().iter().filter_map(|a| a.saved.agent_session.clone()));
    ours.extend(inside.iter().map(|f| f.session_id.clone()).filter(|id| !id.is_empty()));
    let in_shell = |f: &FoundSession| f.pid.is_some() && inside.iter().any(|i| i.pid == f.pid);
    let running: Vec<FoundSession> = found::running().into_iter().filter(|f| !ours.contains(&f.session_id) && !in_shell(f)).collect();
    let mut out = if running_only { vec![] } else { dino_core::history::finished(&running) };
    out.retain(|f| !ours.contains(&f.session_id));
    out.splice(0..0, running);
    if cloud {
        out.extend(found::cloud(&|id| d.launcher(id).map(|l| PathBuf::from(&l.program))));
    }
    out
}

/// Hand a session over to dino. A running one is left to finish its current turn, stopped, and
/// resumed here with the same conversation, folder and flags; its old terminal gets a note.
fn adopt(d: &Daemon, f: FoundSession, cwd: Option<String>) -> anyhow::Result<String> {
    let Some(a) = agent(&f.agent) else { anyhow::bail!("don't know how to continue {} sessions yet", f.agent) };
    let launcher = a.id().to_string();
    if f.source == Source::Cloud {
        return spawn(d, Launch::new(&launcher, a.cloud_args(&f.session_id), cwd.or(f.cwd.clone())));
    }

    let mut tty = None;
    if let (Source::Running, Some(pid)) = (&f.source, f.pid) {
        wait_until_idle(a, pid, std::time::Duration::from_secs(180))?;
        tty = tty_of(pid);
        stop(pid)?;
    }

    let id = d.next_id.fetch_add(1, Ordering::Relaxed).to_string();
    let restore = SavedSession {
        id: id.clone(),
        name: session_name(&f.title),
        launcher: launcher.clone(),
        args: f.args.clone(),
        cwd: f.cwd.clone().unwrap_or_else(|| home().display().to_string()),
        started_at: now_secs(),
        agent_session: Some(f.session_id.clone()),
        auto: AutoState::default(),
        // It keeps whatever the conversation ran with.
        controls: Controls::default(),
        scheduled: None,
        started_by: None,
        messaged_by: None,
        label: None,
        pinned: false,
        host: None,
        ended: false,
        exit_code: None,
    };
    let id = spawn(d, Launch { restore: Some(restore.clone()), ..Launch::new(&launcher, restore.args.clone(), Some(restore.cwd.clone())) })?;
    if let Some(tty) = tty {
        // Tell whoever looks at the old tab where the conversation went.
        let note = format!("\r\n\x1b[38;2;117;179;64m▲▲ dino\x1b[0m  \"{}\" continues in dino (session {id}). Open dino, or run: dino attach {id}\r\n", f.title);
        let _ = std::fs::OpenOptions::new().write(true).open(&tty).and_then(|mut t| io::Write::write_all(&mut t, note.as_bytes()));
    }
    Ok(id)
}

/// Notice agents started by hand in dino's shells, and when they exit back to the prompt.
fn watch_shells(d: &Daemon) {
    let shells: Vec<Arc<Session>> = d.sessions.lock().unwrap().iter().filter(|s| s.agent_id == "shell" && s.host.is_none() && !s.pane.is_exited()).cloned().collect();
    for s in shells {
        let fg = s.pane.foreground().filter(|fg| Some(*fg) != s.pane.pid());
        let due = {
            let mut i = s.inside.lock().unwrap();
            if fg.is_none() {
                // Agents set the title, and change it again on the way out: put the shell's back.
                if i.found.is_some() {
                    *s.pane.shared.title.lock().unwrap() = i.before.clone().flatten();
                }
                // Taken at the prompt: an agent can retitle before a poll sees it start.
                *i = Inside { before: Some(s.pane.title()), ..Inside::default() };
            } else if i.before.is_none() {
                i.before = Some(s.pane.title());
            }
            fg.is_some() && (i.fg != fg || i.checked.is_none_or(|t| t.elapsed() >= INSIDE_RECHECK))
        };
        let Some(fg) = fg.filter(|_| due) else { continue };
        let found = found::inside(fg);
        let mut i = s.inside.lock().unwrap();
        let before = i.before.take();
        *i = Inside { fg: Some(fg), checked: Some(Instant::now()), found, before };
    }
}

/// Continue the agent someone started by hand in shell `id` as a dino session in the shell's
/// place: same id and row, the agent's folder and flags, its conversation resumed. The shell goes.
fn take_over(d: &Daemon, id: &str) -> anyhow::Result<()> {
    let s = d.sessions.lock().unwrap().iter().find(|s| s.id == id).cloned().ok_or_else(|| anyhow::anyhow!("no session {id}"))?;
    anyhow::ensure!(s.agent_id == "shell" && s.host.is_none(), "{id} isn't a shell on this Mac");
    // Looked at now, not as of the last poll: it may have exited, or named its conversation since.
    let f = s.pane.foreground().and_then(found::inside).ok_or_else(|| anyhow::anyhow!("no agent is running in {id}"))?;
    let Some(a) = agent(&f.agent) else { anyhow::bail!("dino can't continue {} sessions yet", f.agent) };
    anyhow::ensure!(!f.session_id.is_empty(), "the agent hasn't started a conversation yet; send it a prompt first");
    let pid = f.pid.ok_or_else(|| anyhow::anyhow!("no agent is running in {id}"))?;
    wait_until_idle(a, pid, std::time::Duration::from_secs(180))?;
    stop(pid)?;
    let restore = SavedSession {
        id: id.to_string(),
        name: session_name(&f.title),
        launcher: f.agent.clone(),
        args: f.args.clone(),
        cwd: f.cwd.clone().unwrap_or_else(|| real(&s.cwd)),
        started_at: now_secs(),
        agent_session: Some(f.session_id.clone()),
        auto: AutoState::default(),
        // It keeps whatever the conversation ran with.
        controls: Controls::default(),
        scheduled: s.scheduled.clone(),
        started_by: s.started_by.clone(),
        messaged_by: s.messaged_by.lock().unwrap().clone(),
        label: s.label.lock().unwrap().clone(),
        pinned: s.pinned.load(Ordering::Relaxed),
        host: None,
        ended: false,
        exit_code: None,
    };
    let (cols, rows) = s.pane.size();
    spawn(d, Launch { cols, rows, restore: Some(restore.clone()), ..Launch::new(&restore.launcher, restore.args.clone(), Some(restore.cwd.clone())) })?;
    // In the shell's place first, so clients reattaching by id find the agent.
    take_place(d, &s);
    // As in `restart`: clients see the stream drop without an exit, and reattach.
    s.subscribers.lock().unwrap().clear();
    s.pane.kill();
    d.proxy.stats.restarted(id);
    save(d);
    Ok(())
}

/// Don't cut a turn in half, for agents that say when they're on one.
fn wait_until_idle(a: &dyn dino_core::agent::Agent, pid: u32, max: std::time::Duration) -> anyhow::Result<()> {
    let deadline = Instant::now() + max;
    while a.busy(pid) == Some(true) {
        anyhow::ensure!(Instant::now() < deadline, "the session is still working; try again when its turn finishes");
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    Ok(())
}

fn tty_of(pid: u32) -> Option<String> {
    let out = std::process::Command::new("ps").args(["-o", "tty=", "-p", &pid.to_string()]).output().ok()?;
    let tty = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!tty.is_empty() && tty != "??").then(|| format!("/dev/{tty}"))
}

/// SIGTERM, then SIGKILL if it hasn't exited after a few seconds.
fn stop(pid: u32) -> anyhow::Result<()> {
    let alive = || std::process::Command::new("kill").args(["-0", &pid.to_string()]).status().is_ok_and(|s| s.success());
    let _ = std::process::Command::new("kill").args(["-TERM", &pid.to_string()]).status();
    let deadline = Instant::now() + std::time::Duration::from_secs(5);
    while alive() && Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    if alive() {
        let _ = std::process::Command::new("kill").args(["-KILL", &pid.to_string()]).status();
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    if alive() {
        anyhow::bail!("couldn't stop process {pid}");
    }
    Ok(())
}

/// "Pineapple memory word" → "pineapple-memory"
fn session_name(title: &str) -> String {
    let words: Vec<String> = title
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect();
    let mut name = String::new();
    for w in words {
        if !name.is_empty() && name.len() + w.len() + 1 > 16 {
            break;
        }
        if !name.is_empty() {
            name.push('-');
        }
        name.push_str(&w);
    }
    if name.is_empty() { "session".into() } else { name.chars().take(16).collect() }
}

fn kill(d: &Daemon, id: &str) -> bool {
    d.proxy.forget_remote(id);
    let mut sessions = d.sessions.lock().unwrap();
    match sessions.iter().position(|s| s.id == id) {
        Some(i) => {
            sessions.remove(i).pane.kill();
            dino_core::agent::qwen::forget(id);
            d.previews.lock().unwrap().retain(|p| {
                if p.session == id {
                    p.stop();
                }
                p.session != id
            });
            true
        }
        None => false,
    }
}

fn session_cwd(d: &Daemon, id: &str) -> anyhow::Result<PathBuf> {
    Ok(local_session(d, id)?.cwd.clone())
}

/// Session `id`, if it runs on this Mac: what needs its checkout (changes, PRs, previews) doesn't
/// work over SSH.
fn local_session(d: &Daemon, id: &str) -> anyhow::Result<Arc<Session>> {
    let s = d.sessions.lock().unwrap().iter().find(|s| s.id == id).cloned().ok_or_else(|| anyhow::anyhow!("no session {id}"))?;
    if let Some(host) = &s.host {
        anyhow::bail!("Not available for sessions on {host}");
    }
    Ok(s)
}

/// Start (or restart) one of the session's dev servers. One run of each name per session.
fn preview_start(d: &Daemon, id: &str, name: &str) -> anyhow::Result<()> {
    let cwd = session_cwd(d, id)?;
    let config = dino_core::preview::configs(&cwd)?
        .into_iter()
        .find(|c| c.name == name)
        .ok_or_else(|| anyhow::anyhow!("no dev server named {name} in .dino/launch.json or .claude/launch.json"))?;
    let mut previews = d.previews.lock().unwrap();
    if let Some(i) = previews.iter().position(|p| p.session == id && p.config.name == name) {
        if previews[i].running() {
            return Ok(());
        }
        previews.remove(i);
    }
    previews.push(preview::Server::start(id, config)?);
    Ok(())
}

// ---- Fan-out: one prompt, several agents, each in its own worktree; keep the best. ----

#[derive(Serialize, Deserialize, Clone)]
struct Group {
    id: String,
    prompt: String,
    repo: PathBuf,
    /// The commit every worktree started from (the user's checkout, uncommitted edits included).
    base: String,
    members: Vec<Member>,
}

#[derive(Serialize, Deserialize, Clone)]
struct Member {
    session: String,
    launcher: String,
    branch: String,
    worktree: PathBuf,
}

fn groups_path() -> PathBuf {
    dino_core::config_dir().join("groups.json")
}

fn load_groups() -> Vec<Group> {
    std::fs::read(groups_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save_groups(groups: &[Group]) {
    let tmp = groups_path().with_extension("json.tmp");
    if std::fs::write(&tmp, serde_json::to_vec_pretty(groups).unwrap_or_default()).is_ok() {
        let _ = std::fs::rename(tmp, groups_path());
    }
}

fn fanout(d: &Daemon, prompt: &str, launchers: &[String], cwd: Option<String>) -> anyhow::Result<String> {
    let prompt = prompt.trim();
    anyhow::ensure!(!prompt.is_empty(), "fan-out needs a prompt");
    let mut picked: Vec<LauncherInfo> = vec![];
    for short in launchers {
        let l = d.allowed_launcher(short)?;
        anyhow::ensure!(l.agent_id != "shell", "a shell can't take a prompt");
        if !picked.iter().any(|p| p.short == l.short) {
            picked.push(l);
        }
    }
    anyhow::ensure!(!picked.is_empty(), "pick at least one agent");

    let dir = work_dir(cwd.as_deref());
    let repo = worktree::repo_root(&dir)?;
    let base = worktree::snapshot(&repo)?;
    let id = format!("{}-{}", session_name(prompt), &new_uuid()[..4]);
    let mut group = Group { id: id.clone(), prompt: prompt.into(), repo: repo.clone(), base: base.clone(), members: vec![] };
    let prefix = Settings::load().worktrees.prefix();
    for l in picked {
        let branch = format!("{prefix}{id}/{}", l.short);
        let wt = worktree::add(&repo, &format!("{id}/{}", l.short), &branch, &base)?;
        // Same folder inside the worktree as the user was in inside the repo.
        let cwd = wt.join(dir.strip_prefix(&repo).unwrap_or(std::path::Path::new("")));
        carry_trust(&l, carried_trust(&l, &dir, &repo).as_deref(), &wt);
        let session = spawn(d, Launch {
            name: Some(format!("{}·{}", l.short, &id[id.len() - 4..])),
            prompt: Some(prompt.into()),
            ..Launch::new(&l.short, vec![], Some(cwd.display().to_string()))
        })?;
        group.members.push(Member { session, launcher: l.short.clone(), branch, worktree: wt });
    }
    let mut groups = d.groups.lock().unwrap();
    groups.push(group);
    save_groups(&groups);
    Ok(id)
}

fn real(p: &Path) -> String {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()).to_string_lossy().into_owned()
}

/// Every repo a session runs in (or a fan-out came from), and each extra folder, once.
fn tree(d: &Daemon, folders: Vec<String>) -> Vec<ipc::RepoInfo> {
    let mut dirs: Vec<String> = d.sessions.lock().unwrap().iter().filter(|s| s.host.is_none()).map(|s| real(&s.cwd)).collect();
    dirs.extend(d.groups.lock().unwrap().iter().map(|g| real(Path::new(&g.repo))));
    // A session's worktree stays after the session ends, until the user closes it.
    let made: Vec<String> = d.worktrees.lock().unwrap().iter().map(|w| real(&w.path)).collect();
    dirs.extend(made.iter().cloned());
    let fanned: HashSet<String> =
        d.groups.lock().unwrap().iter().flat_map(|g| g.members.iter().map(|m| real(&m.worktree))).collect();
    let owners = subagent_owners(d);
    dirs.extend(folders.iter().map(|f| real(Path::new(f))));
    // Shallowest first, so a folder comes before the folders inside it.
    dirs.sort_by_key(|d| d.len());
    dirs.dedup();
    let inside = |dir: &str, p: &str| dir == p || dir.starts_with(&format!("{p}/"));
    let mut repos: Vec<ipc::RepoInfo> = Vec::new();
    let mut plain: Vec<String> = Vec::new();
    for dir in dirs {
        if repos.iter().any(|r| r.worktrees.iter().any(|w| inside(&dir, &w.path))) {
            continue;
        }
        match worktree::list(Path::new(&dir)) {
            Ok(mut w) if !w.is_empty() => {
                // Compared with what the main checkout has out.
                let base = w[0].branch.clone().unwrap_or_else(|| "HEAD".into());
                let base = if base == "HEAD" { worktree::head(Path::new(&w[0].path)) } else { base };
                // Each summary waits on git, so they're read side by side.
                std::thread::scope(|sc| {
                    let reads: Vec<_> = w
                        .iter()
                        .skip(1)
                        .map(|w| {
                            let (path, branch, base) = (real(Path::new(&w.path)), w.branch.clone(), &base);
                            // Fan-out members show their own stat.
                            (!fanned.contains(&path)).then(|| sc.spawn(move || summary(d, &path, branch.as_deref(), base)))
                        })
                        .collect();
                    for (w, read) in w.iter_mut().skip(1).zip(reads) {
                        let path = real(Path::new(&w.path));
                        w.dino = made.contains(&path);
                        if let Some(read) = read {
                            w.git = read.join().unwrap_or_default();
                            w.owner = owners.iter().find(|o| o.0 == path).map(|o| o.1.clone());
                        }
                    }
                });
                let path = w[0].path.clone();
                repos.push(ipc::RepoInfo { name: base_name(&path), path, worktrees: w });
            }
            // A plain folder stands for everything under it, except repos, which get their own node.
            _ if plain.iter().any(|p| inside(&dir, p)) => {}
            _ => plain.push(dir),
        }
    }
    repos.extend(plain.into_iter().map(|path| ipc::RepoInfo { name: base_name(&path), path, worktrees: Vec::new() }));
    repos.sort_by_key(|r| r.name.to_lowercase());
    repos
}

/// A worktree's git summary, read again when older than a few seconds.
fn summary(d: &Daemon, path: &str, branch: Option<&str>, base: &str) -> Option<worktree::Summary> {
    const FRESH: std::time::Duration = std::time::Duration::from_secs(8);
    if let Some((at, s)) = d.summaries.lock().unwrap().get(path) {
        if at.elapsed() < FRESH {
            return s.clone();
        }
    }
    let s = worktree::summary(Path::new(path), branch, base).ok();
    d.summaries.lock().unwrap().insert(path.to_string(), (Instant::now(), s.clone()));
    s
}

/// Its task list and background work, for the Tasks pane. Nothing runs in a session that ended.
fn session_tasks(st: &dino_proxy::SessionStats, cwd: &Path, exited: bool) -> ipc::SessionTasks {
    let here = real(cwd);
    ipc::SessionTasks {
        todos: st
            .todos
            .iter()
            .map(|t| ipc::TodoInfo { id: t.id.clone(), subject: t.subject.clone(), status: t.status.clone(), active: t.active.clone() })
            .collect(),
        subagents: st
            .subagents
            .iter()
            .map(|a| ipc::SubagentInfo {
                id: a.id.clone(),
                agent_type: a.agent_type.clone(),
                description: a.description.clone(),
                running: a.running && !exited,
                started: a.started,
                finished: a.finished,
                worktree: a.cwd.as_deref().map(|c| real(Path::new(c))).filter(|w| *w != here),
            })
            .collect(),
        background: st
            .background
            .iter()
            .map(|b| ipc::BackgroundInfo {
                id: b.id.clone(),
                kind: b.kind.clone(),
                description: b.description.clone(),
                command: b.command.clone(),
                running: b.running && !exited,
                started: b.started,
                finished: b.finished,
            })
            .collect(),
    }
}

/// A subagent that runs in a worktree of its own: which session started it and for what.
#[derive(Serialize, Deserialize, Clone, PartialEq)]
struct SubagentWorktree {
    id: String,
    session: String,
    /// Symlinks resolved, as `tree` compares paths.
    worktree: String,
    description: Option<String>,
    agent_type: Option<String>,
    running: bool,
}

fn subagents_path() -> PathBuf {
    dino_core::config_dir().join("subagents.json")
}

/// A restart stops every agent, so none is running any more; worktrees removed since are gone.
fn load_subagents() -> Vec<SubagentWorktree> {
    let all: Vec<SubagentWorktree> =
        std::fs::read(subagents_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    all.into_iter().filter(|a| Path::new(&a.worktree).exists()).map(|a| SubagentWorktree { running: false, ..a }).collect()
}

fn save_subagents(all: &[SubagentWorktree]) {
    let tmp = subagents_path().with_extension("json.tmp");
    if std::fs::write(&tmp, serde_json::to_vec_pretty(all).unwrap_or_default()).is_ok() {
        let _ = std::fs::rename(tmp, subagents_path());
    }
}

/// Worktree path → who made it: takes in what the sessions' hooks reported since last time.
fn subagent_owners(d: &Daemon) -> Vec<(String, worktree::Owner)> {
    let sessions: Vec<(String, bool)> = d.sessions.lock().unwrap().iter().map(|s| (s.id.clone(), !s.pane.is_exited())).collect();
    let mut all = d.subagents.lock().unwrap();
    let before = all.clone();
    for (id, _) in &sessions {
        for a in d.proxy.stats.session(id).subagents {
            // Only ones in a worktree of their own matter here; others run in the session's.
            let Some(cwd) = a.cwd.as_deref() else { continue };
            let rec = SubagentWorktree {
                id: a.id.clone(),
                session: id.clone(),
                worktree: real(Path::new(cwd)),
                description: a.description,
                agent_type: a.agent_type,
                running: a.running,
            };
            match all.iter_mut().find(|o| o.id == rec.id) {
                Some(o) => *o = rec,
                None if Path::new(cwd).file_name().is_some_and(|n| n.to_string_lossy() == format!("agent-{}", rec.id)) => {
                    all.push(rec)
                }
                None => {}
            }
        }
    }
    if *all != before {
        save_subagents(&all);
    }
    all.iter()
        .map(|a| {
            let alive = sessions.iter().any(|(id, live)| *id == a.session && *live);
            let owner = worktree::Owner {
                session: a.session.clone(),
                description: a.description.clone(),
                agent_type: a.agent_type.clone(),
                running: a.running && alive,
            };
            (a.worktree.clone(), owner)
        })
        .collect()
}

/// The subagent that made `worktree`.
fn subagent_of_worktree(d: &Daemon, worktree: &str) -> Option<ipc::SubagentView> {
    let path = real(Path::new(worktree));
    let (session, id) = {
        subagent_owners(d);
        let all = d.subagents.lock().unwrap();
        let a = all.iter().find(|a| a.worktree == path)?;
        (a.session.clone(), a.id.clone())
    };
    read_subagent(d, &session, &id)
}

/// Subagent `id` of `session`, and what it has said and done: read from its agent's transcript
/// each time, so a view that asks again follows it live. Also after its session is gone, while
/// dino remembers its worktree.
fn read_subagent(d: &Daemon, session: &str, id: &str) -> Option<ipc::SubagentView> {
    let s = d.sessions.lock().unwrap().iter().find(|s| s.id == session).cloned();
    let known = s.as_ref().and_then(|s| {
        let st = d.proxy.stats.session(session);
        let exited = s.pane.is_exited();
        let info = session_tasks(&st, &s.cwd, exited).subagents.into_iter().find(|a| a.id == id)?;
        let output = st.subagents.iter().find(|a| a.id == id).and_then(|a| a.output.clone());
        Some((info, output))
    });
    let (view, output) = match known {
        Some((a, output)) => (
            ipc::SubagentView {
                id: a.id,
                session: session.into(),
                worktree: a.worktree,
                agent_type: a.agent_type,
                description: a.description,
                running: a.running,
                task: None,
                conversation: None,
            },
            output,
        ),
        None => {
            let a = d.subagents.lock().unwrap().iter().find(|a| a.id == id && a.session == session).cloned()?;
            let view = ipc::SubagentView {
                id: a.id,
                session: a.session,
                worktree: Some(a.worktree),
                agent_type: a.agent_type,
                description: a.description,
                running: false,
                task: None,
                conversation: None,
            };
            (view, None)
        }
    };
    // Where the Agent tool said it writes; else under its session's conversation; else anywhere.
    let parent = s.as_ref().and_then(|s| s.agent_session.lock().unwrap().clone());
    let transcript = output
        .map(PathBuf::from)
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl") && p.exists())
        .or_else(|| parent.as_deref().and_then(|p| dino_core::transcript::claude_subagent_path(Some(p), id)))
        .or_else(|| dino_core::transcript::claude_subagent_path(None, id));
    let Some(transcript) = transcript else { return Some(view) };
    // Subagents dino records are Claude's (its SubagentStart hooks).
    let conversation = agent("claude").and_then(|a| dino_core::history::page(a, &transcript, None));
    Some(ipc::SubagentView { task: dino_core::history::subagent_task(&transcript), conversation, ..view })
}

fn base_name(path: &str) -> String {
    Path::new(path).file_name().map_or(path.to_string(), |n| n.to_string_lossy().into_owned())
}

fn groups(d: &Daemon) -> Vec<ipc::GroupInfo> {
    let groups = d.groups.lock().unwrap().clone();
    groups
        .into_iter()
        .map(|g| ipc::GroupInfo {
            members: g
                .members
                .iter()
                .map(|m| ipc::MemberInfo {
                    session: m.session.clone(),
                    launcher: m.launcher.clone(),
                    branch: m.branch.clone(),
                    worktree: m.worktree.display().to_string(),
                    stat: dino_core::worktree::stat(&m.worktree, &g.base).ok(),
                })
                .collect(),
            id: g.id,
            prompt: g.prompt,
            repo: g.repo.display().to_string(),
        })
        .collect()
}

fn find_member(d: &Daemon, session: &str) -> anyhow::Result<(Group, Member)> {
    let groups = d.groups.lock().unwrap();
    groups
        .iter()
        .find_map(|g| g.members.iter().find(|m| m.session == session).map(|m| (g.clone(), m.clone())))
        .ok_or_else(|| anyhow::anyhow!("session {session} isn't part of a fan-out"))
}

fn member_diff(d: &Daemon, session: &str) -> anyhow::Result<(ipc::DiffStat, String)> {
    let (g, m) = find_member(d, session)?;
    Ok((dino_core::worktree::stat(&m.worktree, &g.base)?, dino_core::worktree::diff(&m.worktree, &g.base)?))
}

/// What `id` changed: a fan-out member since its fan-out began (edits it committed included),
/// any other session since the last commit of the checkout it runs in.
fn changes(d: &Daemon, id: &str) -> anyhow::Result<Response> {
    Ok(match changes_base(d, id)? {
        Ok((dir, base, label)) => Response::Changes { root: real(&dir), files: worktree::changes(&dir, &base)?, base: label, note: None },
        Err((cwd, note)) => Response::Changes { root: real(&cwd), base: String::new(), files: vec![], note: Some(note) },
    })
}

/// Where `id`'s changes are and what they're compared with: (checkout, base, base in words).
/// Not in a repo: its cwd and why.
fn changes_base(d: &Daemon, id: &str) -> anyhow::Result<Result<(PathBuf, String, String), (PathBuf, String)>> {
    let cwd = local_session(d, id)?.cwd.clone();
    Ok(match (find_member(d, id), session_worktree(d, &cwd)) {
        (Ok((g, m)), _) => Ok((m.worktree, g.base, "where the fan-out started".to_string())),
        (Err(_), Some(w)) => Ok((w.path, w.base, "where the worktree started".to_string())),
        (Err(_), None) => match worktree::repo_root(&cwd) {
            Ok(root) => {
                let head = worktree::head(&root);
                Ok((root, head, "the last commit".to_string()))
            }
            Err(e) => Err((cwd, e.to_string())),
        },
    })
}

/// Claude's review of what `changes` shows for `id`.
fn review(d: &Daemon, id: &str) -> anyhow::Result<Vec<dino_core::review::Finding>> {
    let (dir, base, _) = changes_base(d, id)?.map_err(|(_, note)| anyhow::anyhow!(note))?;
    dino_core::review::run(id, &dir, &base)
}

/// The fan-out group session `id` belongs to.
fn group_of(d: &Daemon, id: &str) -> Option<String> {
    d.groups.lock().unwrap().iter().find(|g| g.members.iter().any(|m| m.session == id)).map(|g| g.id.clone())
}

/// The worktree dino made that `dir` is in: its commits count as changes too.
fn session_worktree(d: &Daemon, dir: &Path) -> Option<SessionWorktree> {
    d.worktrees.lock().unwrap().iter().find(|w| dir.starts_with(&w.path)).cloned()
}

// ---- Pull requests: one per session branch, found by polling gh. ----

fn pr_session(d: &Daemon, id: &str) -> anyhow::Result<Arc<Session>> {
    local_session(d, id)
}

/// Ask the agent to fix its PR's failed checks, with their logs.
fn pr_fix(d: &Daemon, id: &str) -> anyhow::Result<()> {
    let s = pr_session(d, id)?;
    anyhow::ensure!(!s.pane.is_exited(), "{id} has exited");
    let branch = pr::branch(&s.cwd).ok_or_else(|| anyhow::anyhow!("Not on a branch"))?;
    let msg = pr::fix_message(&s.cwd, &branch)?;
    send_input(&s, &msg, true);
    // Asked by hand: no automatic fix for the same push.
    if let Some(pr) = d.prs.lock().unwrap().get(id) {
        s.auto.lock().unwrap().fixed = Some((pr.number, pr.head.clone()));
    }
    Ok(())
}

/// A session's PR automation: the flags the user sets, and what it has done.
#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(default)]
struct AutoState {
    pr: ipc::AutoPr,
    /// The PR and head commit a fix was last asked for: one per push.
    fixed: Option<(u32, String)>,
    /// The head commit a merge last failed on: tried again after the next push.
    merge_failed: Option<String>,
}

/// How long a merged or closed PR's session stays before it's archived (when the setting says
/// so): long enough to see it happen.
const CLOSE_AFTER_MERGE: std::time::Duration = std::time::Duration::from_secs(20);

/// How long after the user last typed in a session an automatic fix waits.
const LEAVE_THE_USER: std::time::Duration = std::time::Duration::from_secs(10);

/// Look up the PR from each live session's branch, and take the automatic steps that are due.
/// Sessions on the same branch share one lookup; on the default branch, detached, or without gh
/// there's none. Failures just mean no PR.
fn refresh_prs(d: &Daemon) {
    let _one = d.pr_poll.lock().unwrap();
    if dino_core::which("gh").is_none() {
        d.prs.lock().unwrap().clear();
        return;
    }
    let live: Vec<Arc<Session>> = d.sessions.lock().unwrap().iter().filter(|s| !s.pane.is_exited() && s.host.is_none()).cloned().collect();
    let mut looked_up: HashMap<(PathBuf, String), Option<ipc::PrInfo>> = HashMap::new();
    let mut found = HashMap::new();
    let mut with_pr = vec![];
    for s in live {
        let Ok(root) = worktree::repo_root(&s.cwd) else { continue };
        let Some(branch) = pr::branch(&root) else { continue };
        let pr = looked_up
            .entry((root.clone(), branch.clone()))
            .or_insert_with(|| (branch != pr::default_branch(&root)).then(|| pr::view(&root, &branch).ok()).flatten());
        if let Some(pr) = pr {
            found.insert(s.id.clone(), pr.clone());
            with_pr.push((s, root, branch, pr.clone()));
        }
    }
    let old = std::mem::replace(&mut *d.prs.lock().unwrap(), found);
    // One automatic step per PR, however many sessions share its branch.
    let mut acted = HashSet::new();
    for (s, root, branch, pr) in with_pr {
        if pr.is_done() && old.get(&s.id).is_some_and(|was| was.number == pr.number && was.is_open()) {
            pr_done(d, &s.id, &pr);
        }
        if !acted.contains(&pr.url) && auto_pr(d, &s, &root, &branch, &pr) {
            acted.insert(pr.url.clone());
        }
    }
    archive_done_prs(d);
}

/// Take the session's automatic PR step, if one is due. True when it took one.
fn auto_pr(d: &Daemon, s: &Session, root: &Path, branch: &str, pr: &ipc::PrInfo) -> bool {
    let a = s.auto.lock().unwrap().clone();
    if a.pr.merge && pr.mergeable() && a.merge_failed.as_deref() != Some(pr.head.as_str()) {
        let result = pr::merge(root, branch);
        let mut a = s.auto.lock().unwrap();
        match result {
            Ok(now) => {
                a.pr.note = None;
                drop(a);
                pr_done(d, &s.id, &now);
            }
            Err(e) => {
                a.pr.note = Some(format!("Couldn't merge: {e}"));
                a.merge_failed = Some(pr.head.clone());
            }
        }
        return true;
    }
    // A shell has no one to read the logs.
    let this_push = Some((pr.number, pr.head.clone()));
    if !a.pr.fix || !pr.failing() || s.agent_id == "shell" || a.fixed == this_push {
        return false;
    }
    let fixes = match &a.fixed {
        Some((n, _)) if *n != pr.number => 0,
        _ => a.pr.fixes,
    };
    if fixes >= ipc::MAX_AUTO_FIXES {
        s.auto.lock().unwrap().pr.note = Some(format!("Stopped after {fixes} fixes: checks still fail"));
        return false;
    }
    // Not mid-turn, not while it waits on you, not while you type: the next poll will do.
    let typing = s.poked.lock().unwrap().is_some_and(|t| t.elapsed() < LEAVE_THE_USER);
    if typing || !idle(d, s) {
        return false;
    }
    let msg = pr::fix_message(root, branch);
    if let Ok(msg) = &msg {
        send_input(s, msg, true);
    }
    let mut a = s.auto.lock().unwrap();
    a.fixed = this_push;
    match msg {
        Ok(_) => {
            a.pr.fixes = fixes + 1;
            a.pr.note = None;
        }
        Err(e) => a.pr.note = Some(format!("Couldn't ask for a fix: {e}")),
    }
    true
}

/// The session's PR merged or closed: when the setting says so, the session is archived once its
/// turn is over.
fn pr_done(d: &Daemon, id: &str, pr: &ipc::PrInfo) {
    d.prs.lock().unwrap().insert(id.to_string(), pr.clone());
    if pr.is_done() && Settings::load().policies.close_merged {
        d.closing.lock().unwrap().entry(id.to_string()).or_insert_with(Instant::now);
    }
}

/// Archive sessions whose PR merged or closed a little while ago. After a merge, the worktree dino
/// made goes too when nothing in it would be lost; after a close without merging it stays, since
/// the work never landed. Waits for their turn to end.
fn archive_done_prs(d: &Daemon) {
    if !Settings::load().policies.close_merged {
        d.closing.lock().unwrap().clear();
        return;
    }
    let due: Vec<String> = d.closing.lock().unwrap().iter().filter(|(_, t)| t.elapsed() >= CLOSE_AFTER_MERGE).map(|(id, _)| id.clone()).collect();
    for id in due {
        let state = d.prs.lock().unwrap().get(&id).map(|pr| pr.state.clone());
        let s = d.sessions.lock().unwrap().iter().find(|s| s.id == id && !s.pane.is_exited()).cloned();
        let w = s.as_ref().and_then(|s| session_worktree(d, &s.cwd));
        let (Some(s), Some(w)) = (s, w) else {
            // Gone, or not in a worktree.
            d.closing.lock().unwrap().remove(&id);
            continue;
        };
        let Some(state) = state.clone().filter(|st| st == "merged" || st == "closed") else {
            // Reopened. A failed lookup (no PR at all) just waits.
            if state.is_some() {
                d.closing.lock().unwrap().remove(&id);
            }
            continue;
        };
        let target = real(&w.path);
        if !sessions_in(d, &target).iter().all(|o| o.pane.is_exited() || finished(d, o)) {
            continue;
        }
        d.closing.lock().unwrap().remove(&id);
        // Pinned sessions stay until the user archives them.
        if sessions_in(d, &target).iter().any(|o| o.pinned.load(Ordering::Relaxed)) {
            continue;
        }
        let merged = state == "merged";
        if merged && !pr::nothing_to_lose(&w.path) {
            s.auto.lock().unwrap().pr.note = Some("Kept open after the merge: its worktree has work that isn't pushed".into());
            continue;
        }
        if let Err(e) = lifecycle::archive_pr_done(d, &target, merged) {
            s.auto.lock().unwrap().pr.note = Some(format!("Couldn't archive after the PR {state}: {e}"));
        }
    }
}

/// After a PR action, off the request: other sessions on the same branch see it without waiting for the poll.
fn refresh_prs_soon(d: &Arc<Daemon>) {
    let d = d.clone();
    std::thread::spawn(move || refresh_prs(&d));
}

/// A paste, then Return a moment later: sent together, some agents take the Return as part of it.
fn send_input(s: &Session, text: &str, submit: bool) {
    s.pane.paste(text);
    if submit {
        std::thread::sleep(std::time::Duration::from_millis(150));
        s.pane.write(b"\r".to_vec());
    }
}

/// The winner's changes land in the user's checkout, uncommitted; the whole group closes.
fn keep(d: &Daemon, session: &str) -> anyhow::Result<()> {
    let (g, m) = find_member(d, session)?;
    dino_core::worktree::apply(&m.worktree, &g.base, &g.repo)?;
    close_group(d, &g.id)
}

fn close_group(d: &Daemon, id: &str) -> anyhow::Result<()> {
    let group = {
        let mut groups = d.groups.lock().unwrap();
        let i = groups.iter().position(|g| g.id == id).ok_or_else(|| anyhow::anyhow!("no fan-out {id}"))?;
        let g = groups.remove(i);
        save_groups(&groups);
        g
    };
    for m in &group.members {
        kill(d, &m.session);
    }
    save(d);
    for m in &group.members {
        dino_core::worktree::remove(&group.repo, &m.worktree, &m.branch);
        let _ = trust::claude_forget(&m.worktree);
    }
    Ok(())
}

/// Where a worktree is made from, symlinks resolved like git's paths (`/tmp` is `/private/tmp`),
/// so the folder the user was in maps to the same one in the worktree.
fn work_dir(cwd: Option<&str>) -> PathBuf {
    let dir = cwd.map(PathBuf::from).or_else(|| std::env::current_dir().ok()).unwrap_or_default();
    std::fs::canonicalize(&dir).unwrap_or(dir)
}

/// Where `l`'s agent is trusted in `repo`, from `dir` up, when the policies carry trust into worktrees.
fn carried_trust(l: &LauncherInfo, dir: &Path, repo: &Path) -> Option<PathBuf> {
    Settings::load().policies.worktree_trust.then(|| agent(&l.agent_id)?.trusted_in(dir, repo)).flatten()
}

/// Trust the same folder in worktree `wt`, so the agent starts there without asking again.
fn carry_trust(l: &LauncherInfo, rel: Option<&Path>, wt: &Path) {
    let (Some(a), Some(rel)) = (agent(&l.agent_id), rel) else { return };
    if let Err(e) = a.trust(&trust::join(wt, rel)) {
        eprintln!("dinod: couldn't trust {} for {}: {e}", wt.display(), l.label);
    }
}

// ---- A session in its own worktree: fan-out's isolation for one agent, kept until closed. ----

/// A worktree dino made for a session. It outlives the session: when to close it is the user's call.
#[derive(Serialize, Deserialize, Clone)]
struct SessionWorktree {
    path: PathBuf,
    branch: String,
    /// The repo's main checkout, which holds every worktree.
    repo: PathBuf,
    /// The checkout it came from, where its changes are applied.
    checkout: PathBuf,
    /// The commit its changes count from.
    base: String,
}

fn worktrees_path() -> PathBuf {
    dino_core::config_dir().join("worktrees.json")
}

fn load_worktrees() -> Vec<SessionWorktree> {
    let all: Vec<SessionWorktree> = std::fs::read(worktrees_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    // Ones removed by hand are gone.
    all.into_iter().filter(|w| w.path.exists()).collect()
}

fn save_worktrees(worktrees: &[SessionWorktree]) {
    let tmp = worktrees_path().with_extension("json.tmp");
    if std::fs::write(&tmp, serde_json::to_vec_pretty(worktrees).unwrap_or_default()).is_ok() {
        let _ = std::fs::rename(tmp, worktrees_path());
    }
}

fn spawn_in_worktree(d: &Daemon, launch: Launch) -> anyhow::Result<String> {
    anyhow::ensure!(launch.host.is_none(), "Worktrees are made on this Mac; for a session on {}, start one in a folder there", launch.host.as_deref().unwrap_or_default());
    let l = d.allowed_launcher(&launch.launcher)?;
    let dir = work_dir(launch.cwd.as_deref());
    let checkout = worktree::repo_root(&dir)?;
    // A scheduled run's branch says which task made it; anything else is named after the agent.
    let base = launch.scheduled.as_deref().and_then(worktree::slug).unwrap_or_else(|| l.short.clone());
    let name = format!("{base}-{}", &new_uuid()[..4]);
    let branch = format!("{}{name}", Settings::load().worktrees.prefix());
    let (wt, base) = worktree::start(&checkout, &name, &branch)?;
    let repo = worktree::list(&wt)?.into_iter().next().map_or_else(|| checkout.clone(), |w| PathBuf::from(w.path));
    carry_trust(&l, carried_trust(&l, &dir, &checkout).as_deref(), &wt);
    // Same folder inside the worktree as the user was in inside the checkout.
    let cwd = wt.join(dir.strip_prefix(&checkout).unwrap_or(Path::new("")));
    match spawn(d, Launch { cwd: Some(cwd.display().to_string()), ..launch }) {
        Ok(id) => {
            let mut worktrees = d.worktrees.lock().unwrap();
            worktrees.push(SessionWorktree { path: wt, branch, repo, checkout, base });
            save_worktrees(&worktrees);
            Ok(id)
        }
        Err(e) => {
            worktree::remove(&repo, &wt, &branch);
            let _ = trust::claude_forget(&wt);
            Err(e)
        }
    }
}

/// The sessions running in `dir` (a resolved path) or a folder inside it.
fn sessions_in(d: &Daemon, dir: &str) -> Vec<Arc<Session>> {
    let inside = |s: &Session| {
        if s.host.is_some() {
            return false;
        }
        let cwd = real(&s.cwd);
        cwd == dir || cwd.starts_with(&format!("{dir}/"))
    };
    d.sessions.lock().unwrap().iter().filter(|s| inside(s)).cloned().collect()
}

fn remove_worktree(d: &Daemon, path: &str, apply: bool) -> anyhow::Result<()> {
    let target = real(Path::new(path));
    let w = d.worktrees.lock().unwrap().iter().find(|w| real(&w.path) == target).cloned();
    let w = w.ok_or_else(|| anyhow::anyhow!("{path} isn't a worktree dino made for a session"))?;
    if apply {
        worktree::apply(&w.path, &w.base, &w.checkout)?;
    }
    for s in sessions_in(d, &target) {
        kill(d, &s.id);
    }
    save(d);
    worktree::remove(&w.repo, &w.path, &w.branch);
    let _ = trust::claude_forget(&w.path);
    let mut worktrees = d.worktrees.lock().unwrap();
    worktrees.retain(|o| o.path != w.path);
    save_worktrees(&worktrees);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
        let since = Instant::now();
        while !done() {
            assert!(since.elapsed() < std::time::Duration::from_secs(10), "timed out waiting for {what}");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    fn shell_daemon() -> Arc<Daemon> {
        let shell = LauncherInfo { short: "shell".into(), agent_id: "shell".into(), label: "Shell (sh)".into(), program: "/bin/sh".into(), knobs: Default::default() };
        new_daemon(Proxy::start(HashMap::new()).unwrap(), vec![shell])
    }

    fn session(d: &Daemon, id: &str) -> Arc<Session> {
        d.sessions.lock().unwrap().iter().find(|s| s.id == id).cloned().unwrap()
    }

    /// A session whose program exits stays, ended, across a dinod restart, and resumes in place.
    #[test]
    fn ended_sessions_are_kept_and_resume() {
        let home = std::env::temp_dir().join(format!("dino-resume-test-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        // SAFETY: the only test in this crate that reads dino's config.
        unsafe { std::env::set_var("DINO_HOME", &home) };

        let d = shell_daemon();
        let id = spawn(&d, Launch::new("shell", vec![], Some(home.display().to_string()))).unwrap();
        let s = session(&d, &id);
        s.pane.write(b"echo last-words-$((6*7)); exit 3\r".to_vec());
        wait_for("the shell to exit", || s.pane.is_exited() && s.pane.exit_code().is_some());
        assert_eq!(s.pane.exit_code(), Some(3));
        assert_eq!(String::from_utf8(ended_note(&d, &s)).unwrap(), "Shell (sh) exited with code 3 · Enter starts a new one");

        // Controls chosen meanwhile wait for the resume: nothing starts on its own.
        *s.pending.lock().unwrap() = Some(Controls::default());
        apply_pending(&d);
        assert!(Arc::ptr_eq(&session(&d, &id), &s));
        assert!(s.pending.lock().unwrap().is_some());

        save(&d);
        let saved = load_saved();
        assert_eq!(saved.len(), 1);
        assert!(saved[0].ended);
        assert_eq!(saved[0].exit_code, Some(3));
        assert!(screens_dir().join(&id).exists());

        // dinod restarts: it comes back ended, its last screen up.
        let d2 = shell_daemon();
        restore(&d2, saved);
        let back = session(&d2, &id);
        assert!(back.pane.is_exited());
        assert_eq!(back.pane.exit_code(), Some(3));
        assert!(back.pane.text(100).contains("last-words-42"));

        resume(&d2, &id).unwrap();
        let live = session(&d2, &id);
        assert!(!live.pane.is_exited());
        assert_eq!(d2.sessions.lock().unwrap().len(), 1);
        assert!(resume(&d2, &id).is_err(), "only an ended session resumes");
        save(&d2);
        assert!(!load_saved()[0].ended);
        assert!(!screens_dir().join(&id).exists());

        // Removed: attached clients are told it's gone, not that it ended.
        kill(&d2, &id);
        assert!(ended_note(&d2, &live).is_empty());
        kill(&d, &id);
        let _ = std::fs::remove_dir_all(&home);
    }
}
