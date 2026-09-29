//! dinod: owns agent sessions (PTY + terminal state), the proxy and the router. Clients (the TUI,
//! `dino attach` inside a Ghostty surface, the future app) talk to it over a Unix socket; agents
//! keep running when every client goes away.

use std::collections::{HashMap, HashSet};
use std::io;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use dino_core::found::{self, FoundSession, Source};
use dino_core::ipc::{self, LauncherInfo, QuotaInfo, Request, Response, SessionInfo, WindowInfo};
use dino_core::settings::{self, Settings};
use dino_core::{detect_agents, load_keys, pr, proxy_wiring, trust, user_shell, worktree};
use dino_proxy::{Activity, Proxy, SessionStats};
use dino_term::{Pane, SpawnSpec};

mod preview;
mod schedule;

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
    /// The scheduled task that started it, by name.
    scheduled: Option<String>,
}

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

    /// The launchers to offer: the allowed ones, the default (Claude Code unless set) first.
    fn offered(&self) -> Vec<LauncherInfo> {
        let p = Settings::load().policies;
        let mut out: Vec<LauncherInfo> = self.launchers.read().unwrap().iter().filter(|l| p.allows(&l.short)).cloned().collect();
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
    sessions: Mutex<Vec<Arc<Session>>>,
    groups: Mutex<Vec<Group>>,
    worktrees: Mutex<Vec<SessionWorktree>>,
    /// Session id → the PR from its branch, as of the last poll.
    prs: Mutex<HashMap<String, ipc::PrInfo>>,
    /// One PR poll at a time, so an automatic step is never taken twice.
    pr_poll: Mutex<()>,
    /// Sessions whose PR merged, to close once they have nothing to lose; since when.
    closing: Mutex<HashMap<String, Instant>>,
    /// Dev servers started for previews, each tied to a session.
    previews: Mutex<Vec<Arc<preview::Server>>>,
    /// Subagents that run in a worktree of their own, and whose session started them.
    subagents: Mutex<Vec<SubagentWorktree>>,
    /// Worktree path → its git summary and when it was read; git is too slow for every tree poll.
    summaries: Mutex<HashMap<String, (Instant, Option<worktree::Summary>)>>,
    schedule: schedule::Scheduler,
    next_id: AtomicU64,
    next_sub: AtomicU64,
}

pub fn run() -> anyhow::Result<()> {
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
    let daemon = Arc::new(Daemon {
        proxy,
        launchers: RwLock::new(launchers(free_tier)),
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
        next_id: AtomicU64::new(1),
        next_sub: AtomicU64::new(1),
    });
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
                refresh_prs(&d);
                std::thread::sleep(std::time::Duration::from_secs(30));
            }
        });
    }
    schedule::start(&daemon);
    eprintln!("dinod listening on {}", path.display());
    for stream in listener.incoming().flatten() {
        let daemon = daemon.clone();
        std::thread::spawn(move || {
            let _ = serve(&daemon, stream);
        });
    }
    Ok(())
}

fn launchers(free_tier: bool) -> Vec<LauncherInfo> {
    let mut out = vec![];
    for d in detect_agents() {
        let program: String = d.path.to_string_lossy().into();
        if d.kind.id == "claude" && free_tier {
            out.push(LauncherInfo { short: "free".into(), agent_id: "claude-free".into(), label: "Claude Code · free models".into(), program: program.clone() });
        }
        out.push(LauncherInfo { short: d.kind.id.into(), agent_id: d.kind.id.into(), label: d.kind.name.into(), program });
    }
    let shell = user_shell();
    let shell_name = shell.rsplit('/').next().unwrap_or("shell").to_string();
    out.push(LauncherInfo { short: "shell".into(), agent_id: "shell".into(), label: format!("Shell ({shell_name})"), program: shell });
    out
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
            Request::AllLaunchers => Response::Launchers { launchers: d.launchers.read().unwrap().clone() },
            Request::Settings => Response::Settings { settings: Settings::load() },
            Request::SetSettings { settings } => match settings.save() {
                Ok(()) => {
                    d.proxy.set_budget(settings.policies.session_token_budget);
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
                    let keys = load_keys();
                    *d.launchers.write().unwrap() = launchers(keys.contains_key("NVIDIA_API_KEY"));
                    d.proxy.set_keys(keys);
                    Response::Ok
                }
                Err(e) => Response::Error { message: e.to_string() },
            },
            Request::New { launcher, args, cwd, cols, rows, worktree } => {
                let launch = Launch { cols, rows, ..Launch::new(&launcher, args, cwd) };
                match if worktree { spawn_in_worktree(d, launch) } else { spawn(d, launch) } {
                    Ok(id) => {
                        save(d);
                        Response::Created { id }
                    }
                    Err(e) => Response::Error { message: e.to_string() },
                }
            }
            Request::Kill { id } => {
                if kill(d, &id) {
                    save(d);
                    Response::Ok
                } else {
                    Response::Error { message: format!("no session {id}") }
                }
            }
            Request::Attach { id, cols, rows } => {
                let session = d.sessions.lock().unwrap().iter().find(|s| s.id == id).cloned();
                return match session {
                    Some(s) => attach(d, &s, stream, cols, rows),
                    None => ipc::write_json(&mut stream, &Response::Error { message: format!("no session {id}") }),
                };
            }
            Request::Found { cloud } => Response::Found { sessions: discover(d, cloud) },
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
                        merged(d, &id, &pr);
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
    /// The scheduled task starting it, by name.
    scheduled: Option<String>,
}

impl Launch {
    fn new(launcher: &str, args: Vec<String>, cwd: Option<String>) -> Self {
        Self { launcher: launcher.into(), args, cwd, cols: 120, rows: 40, ..Default::default() }
    }
}

fn spawn(d: &Daemon, launch: Launch) -> anyhow::Result<String> {
    let Launch { launcher, args, cwd, cols, rows, restore, name, prompt, scheduled } = launch;
    let launcher = launcher.as_str();
    // Sessions already running come back even if the policies changed since; new ones must be allowed.
    let l = match restore {
        Some(_) => d.launcher(launcher).ok_or_else(|| anyhow::anyhow!("unknown agent {launcher}"))?,
        None => d.allowed_launcher(launcher)?,
    };
    let id = match &restore {
        Some(r) => r.id.clone(),
        None => d.next_id.fetch_add(1, Ordering::Relaxed).to_string(),
    };
    let cwd = cwd.map(PathBuf::from).or_else(|| std::env::current_dir().ok()).unwrap_or_default();
    let (env, mut wired_args) = proxy_wiring(&l.agent_id, Settings::load().routing.proxy, &|provider| d.proxy.base_url(&id, provider));

    // Resume the agent's own conversation when we know it; otherwise start one we can resume later.
    let mut agent_session = restore.as_ref().and_then(|r| r.agent_session.clone());
    match l.agent_id.as_str() {
        "claude" | "claude-free" => {
            let uuid = agent_session.get_or_insert_with(new_uuid).clone();
            // Claude only saves a transcript after the first prompt; resuming an unused id fails.
            if restore.is_some() && claude_transcript_exists(&uuid) {
                wired_args.extend(["--resume".into(), uuid]);
            } else {
                wired_args.extend(["--session-id".into(), uuid]);
            }
        }
        "codex" => {
            if let Some(sid) = &agent_session {
                wired_args.insert(0, "resume".into());
                wired_args.push(sid.clone());
            }
        }
        _ => {}
    }
    wired_args.extend(args.iter().cloned());
    // Claude and Codex both take an opening prompt as their last argument.
    wired_args.extend(prompt);
    let spec = SpawnSpec {
        program: l.program.clone(),
        args: wired_args,
        cwd: Some(cwd.clone()),
        env: env.into_iter().collect::<HashMap<_, _>>(),
    };

    let subscribers: Arc<Mutex<Vec<(u64, Sender<Vec<u8>>)>>> = Arc::default();
    let last_output: Arc<Mutex<Option<Instant>>> = Arc::default();
    let poked: Arc<Mutex<Option<Instant>>> = Arc::default();
    let last_write: Arc<Mutex<Option<Instant>>> = Arc::default();
    let local_url: Arc<Mutex<Option<String>>> = Arc::default();
    let (subs, last, poke, write, url) = (subscribers.clone(), last_output.clone(), poked.clone(), last_write.clone(), local_url.clone());
    let mut tail = String::new();
    let pane = Pane::spawn(spec, cols.max(20), rows.max(5), move |bytes| {
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
    })?;

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
        scheduled: restore.as_ref().map_or(scheduled, |r| r.scheduled.clone()),
    }));
    Ok(id)
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

/// Stream a session to a client until it detaches or the session ends.
fn attach(d: &Daemon, s: &Session, mut stream: UnixStream, cols: u16, rows: u16) -> io::Result<()> {
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
    let writer = std::thread::spawn(move || {
        if exited_already {
            let _ = ipc::write_frame(&mut out, ipc::EXIT, &[]);
            return;
        }
        while let Ok(bytes) = rx.recv() {
            let kind = if bytes.is_empty() { ipc::EXIT } else { ipc::DATA };
            if ipc::write_frame(&mut out, kind, &bytes).is_err() || kind == ipc::EXIT {
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
    if st.activity == Some(Activity::Working) && st.in_flight == 0 && quiet {
        d.proxy.stats.end_turn(&s.id);
        return d.proxy.stats.session(&s.id);
    }
    st
}

/// Between turns: not working, not waiting on the user, and quiet.
fn idle(d: &Daemon, s: &Session) -> bool {
    let st = stats(d, s);
    let quiet = s.last_output.lock().unwrap().is_none_or(|t| t.elapsed() > TURN_OVER_QUIET);
    !s.pane.is_exited() && st.in_flight == 0 && !matches!(st.activity, Some(Activity::Working | Activity::NeedsPermission(_))) && quiet
}

fn state(d: &Daemon) -> Response {
    let groups = d.groups.lock().unwrap().clone();
    let prs = d.prs.lock().unwrap().clone();
    let previews = d.previews.lock().unwrap().clone();
    let group_of = |id: &str| groups.iter().find(|g| g.members.iter().any(|m| m.session == id)).map(|g| g.id.clone());
    let sessions = d
        .sessions
        .lock()
        .unwrap()
        .iter()
        .map(|s| {
            let st = stats(d, s);
            SessionInfo {
                id: s.id.clone(),
                name: s.name.clone(),
                agent_id: s.agent_id.clone(),
                title: s.pane.title(),
                exited: s.pane.is_exited(),
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
                    Activity::Done => "done".into(),
                    Activity::NeedsPermission(what) => format!("needs:{what}"),
                }),
                group: group_of(&s.id),
                error: st.last_error,
                cwd: real(&s.cwd),
                pr: prs.get(&s.id).cloned(),
                auto: s.auto.lock().unwrap().pr.clone(),
                previews: previews.iter().filter(|p| p.session == s.id).map(|p| p.info()).collect(),
                local_url: s.local_url.lock().unwrap().clone(),
                scheduled: s.scheduled.clone(),
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
    scheduled: Option<String>,
}

fn saved_path() -> PathBuf {
    dino_core::config_dir().join("sessions.json")
}

fn load_saved() -> Vec<SavedSession> {
    std::fs::read(saved_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// Write live sessions to disk. Sessions whose agent exited on its own are dropped: the user
/// ended them.
fn save(d: &Daemon) {
    let sessions = d.sessions.lock().unwrap().clone();
    // Snapshot first: a session's own lock must not be held while reading the others.
    let claimed: Vec<String> = sessions.iter().filter_map(|o| o.agent_session.lock().unwrap().clone()).collect();
    let saved: Vec<SavedSession> = sessions
        .iter()
        .filter(|s| !s.pane.is_exited())
        .map(|s| {
            let mut agent_session = s.agent_session.lock().unwrap();
            if agent_session.is_none() && s.agent_id == "codex" {
                *agent_session = find_codex_session(&s.cwd, s.started_at, &claimed);
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
                scheduled: s.scheduled.clone(),
            }
        })
        .collect();
    let tmp = saved_path().with_extension("json.tmp");
    if std::fs::write(&tmp, serde_json::to_vec_pretty(&saved).unwrap_or_default()).is_ok() {
        let _ = std::fs::rename(tmp, saved_path());
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

/// Random v4 UUID, for Claude's `--session-id`.
fn new_uuid() -> String {
    let mut b = [0u8; 16];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = io::Read::read_exact(&mut f, &mut b);
    }
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

/// Claude keeps transcripts at `~/.claude/projects/<dir>/<uuid>.jsonl`.
fn claude_transcript_exists(uuid: &str) -> bool {
    let projects = home().join(".claude/projects");
    std::fs::read_dir(projects).into_iter().flatten().flatten().any(|dir| dir.path().join(format!("{uuid}.jsonl")).exists())
}

/// The Codex rollout this session created: our `dino` provider, same cwd, started after launch.
/// Codex offers no way to choose the id up front, so we find it in `~/.codex/sessions/Y/M/D/`.
fn find_codex_session(cwd: &std::path::Path, started_at: u64, claimed: &[String]) -> Option<String> {
    let root = home().join(".codex/sessions");
    let mut best: Option<(String, String)> = None; // (timestamp, id): earliest after launch wins
    for year in read_dirs(&root) {
        for month in read_dirs(&year) {
            for day in read_dirs(&month) {
                for f in std::fs::read_dir(&day).into_iter().flatten().flatten() {
                    let modified = f.metadata().ok().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(UNIX_EPOCH).ok());
                    if modified.is_none_or(|m| m.as_secs() + 5 < started_at) {
                        continue;
                    }
                    let Some(meta) = first_line(&f.path()).and_then(|l| serde_json::from_str::<serde_json::Value>(&l).ok()) else { continue };
                    let p = &meta["payload"];
                    let (Some(id), Some(ts)) = (p["id"].as_str(), p["timestamp"].as_str()) else { continue };
                    if p["model_provider"] != "dino" || p["cwd"].as_str() != Some(&cwd.display().to_string()) || claimed.iter().any(|c| c == id) {
                        continue;
                    }
                    if best.as_ref().is_none_or(|(t, _)| ts < t.as_str()) {
                        best = Some((ts.to_string(), id.to_string()));
                    }
                }
            }
        }
    }
    best.map(|(_, id)| id)
}

fn read_dirs(p: &std::path::Path) -> Vec<PathBuf> {
    std::fs::read_dir(p).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect()
}

fn first_line(p: &std::path::Path) -> Option<String> {
    use std::io::BufRead;
    std::io::BufReader::new(std::fs::File::open(p).ok()?).lines().next()?.ok()
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

// ---- Continue anything: sessions dino didn't start. ----

/// Found sessions minus the ones dino itself is running.
fn discover(d: &Daemon, cloud: bool) -> Vec<FoundSession> {
    let ours: Vec<String> = d.sessions.lock().unwrap().iter().filter_map(|s| s.agent_session.lock().unwrap().clone()).collect();
    let running: Vec<FoundSession> = found::running().into_iter().filter(|f| !ours.contains(&f.session_id)).collect();
    let mut out = found::recent(25, &running);
    out.retain(|f| !ours.contains(&f.session_id));
    out.splice(0..0, running);
    if cloud {
        let has = |short: &str| d.launcher(short).map(|l| PathBuf::from(&l.program));
        out.extend(found::cloud(has("codex").as_deref(), has("claude").is_some()));
    }
    out
}

/// Hand a session over to dino. A running one is left to finish its current turn, stopped, and
/// resumed here with the same conversation, folder and flags; its old terminal gets a note.
fn adopt(d: &Daemon, f: FoundSession, cwd: Option<String>) -> anyhow::Result<String> {
    let launcher = match f.agent.as_str() {
        "claude" | "codex" => f.agent.clone(),
        other => anyhow::bail!("don't know how to continue {other} sessions yet"),
    };
    if f.source == Source::Cloud {
        // Claude web sessions teleport into a checkout; Codex cloud tasks open in its cloud browser.
        let args = match f.agent.as_str() {
            "claude" if f.session_id.is_empty() => vec!["--teleport".to_string()],
            "claude" => vec!["--teleport".into(), f.session_id.clone()],
            _ => vec!["cloud".into()],
        };
        return spawn(d, Launch::new(&launcher, args, cwd.or(f.cwd.clone())));
    }

    let mut tty = None;
    if let (Source::Running, Some(pid)) = (&f.source, f.pid) {
        if f.agent == "claude" {
            wait_until_idle(pid, std::time::Duration::from_secs(180))?;
        }
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
        scheduled: None,
    };
    let id = spawn(d, Launch { restore: Some(restore.clone()), ..Launch::new(&launcher, restore.args.clone(), Some(restore.cwd.clone())) })?;
    if let Some(tty) = tty {
        // Tell whoever looks at the old tab where the conversation went.
        let note = format!("\r\n\x1b[38;2;117;179;64m▲▲ dino\x1b[0m  \"{}\" continues in dino (session {id}). Open dino, or run: dino attach {id}\r\n", f.title);
        let _ = std::fs::OpenOptions::new().write(true).open(&tty).and_then(|mut t| io::Write::write_all(&mut t, note.as_bytes()));
    }
    Ok(id)
}

/// Claude reports `busy`/`idle` in `~/.claude/sessions/<pid>.json`; don't cut a turn in half.
fn wait_until_idle(pid: u32, max: std::time::Duration) -> anyhow::Result<()> {
    let path = home().join(format!(".claude/sessions/{pid}.json"));
    let deadline = Instant::now() + max;
    loop {
        let status = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v["status"].as_str().map(String::from));
        match status.as_deref() {
            Some("busy") if Instant::now() < deadline => std::thread::sleep(std::time::Duration::from_millis(300)),
            Some("busy") => anyhow::bail!("the session is still working; try again when its turn finishes"),
            _ => return Ok(()),
        }
    }
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
    let mut sessions = d.sessions.lock().unwrap();
    match sessions.iter().position(|s| s.id == id) {
        Some(i) => {
            sessions.remove(i).pane.kill();
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
    d.sessions.lock().unwrap().iter().find(|s| s.id == id).map(|s| s.cwd.clone()).ok_or_else(|| anyhow::anyhow!("no session {id}"))
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
    let claude_trusted = carried_trust(&dir, &repo);
    let id = format!("{}-{}", session_name(prompt), &new_uuid()[..4]);
    let mut group = Group { id: id.clone(), prompt: prompt.into(), repo: repo.clone(), base: base.clone(), members: vec![] };
    for l in picked {
        let branch = format!("dino/{id}/{}", l.short);
        let wt = worktree::add(&repo, &format!("{id}/{}", l.short), &branch, &base)?;
        // Same folder inside the worktree as the user was in inside the repo.
        let cwd = wt.join(dir.strip_prefix(&repo).unwrap_or(std::path::Path::new("")));
        carry_trust(&l, claude_trusted.as_deref(), &wt);
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
    let mut dirs: Vec<String> = d.sessions.lock().unwrap().iter().map(|s| real(&s.cwd)).collect();
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
                for w in w.iter_mut().skip(1) {
                    let path = real(Path::new(&w.path));
                    w.dino = made.contains(&path);
                    // Fan-out members show their own stat.
                    if !fanned.contains(&path) {
                        w.git = summary(d, &path, w.branch.as_deref(), &base);
                        w.owner = owners.iter().find(|o| o.0 == path).map(|o| o.1.clone());
                    }
                }
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
    let cwd = d.sessions.lock().unwrap().iter().find(|s| s.id == id).map(|s| s.cwd.clone());
    let cwd = cwd.ok_or_else(|| anyhow::anyhow!("no session {id}"))?;
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

/// The worktree dino made that `dir` is in: its commits count as changes too.
fn session_worktree(d: &Daemon, dir: &Path) -> Option<SessionWorktree> {
    d.worktrees.lock().unwrap().iter().find(|w| dir.starts_with(&w.path)).cloned()
}

// ---- Pull requests: one per session branch, found by polling gh. ----

fn pr_session(d: &Daemon, id: &str) -> anyhow::Result<Arc<Session>> {
    d.sessions.lock().unwrap().iter().find(|s| s.id == id).cloned().ok_or_else(|| anyhow::anyhow!("no session {id}"))
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

/// How long a merged PR's session stays before it's closed (when the setting says so): long
/// enough to see it merged.
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
    let live: Vec<Arc<Session>> = d.sessions.lock().unwrap().iter().filter(|s| !s.pane.is_exited()).cloned().collect();
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
        if pr.state == "merged" && old.get(&s.id).is_some_and(|was| was.number == pr.number && was.is_open()) {
            merged(d, &s.id, &pr);
        }
        if !acted.contains(&pr.url) && auto_pr(d, &s, &root, &branch, &pr) {
            acted.insert(pr.url.clone());
        }
    }
    close_merged(d);
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
                merged(d, &s.id, &now);
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

/// The session's PR is merged: when the setting says so, the session closes once it's safe.
fn merged(d: &Daemon, id: &str, pr: &ipc::PrInfo) {
    d.prs.lock().unwrap().insert(id.to_string(), pr.clone());
    if pr.state == "merged" && Settings::load().policies.close_merged {
        d.closing.lock().unwrap().entry(id.to_string()).or_insert_with(Instant::now);
    }
}

/// Close sessions whose PR merged a little while ago, with the worktree dino made for them.
/// Waits for their turn to end; never removes a worktree with work in it.
fn close_merged(d: &Daemon) {
    if !Settings::load().policies.close_merged {
        d.closing.lock().unwrap().clear();
        return;
    }
    let due: Vec<String> = d.closing.lock().unwrap().iter().filter(|(_, t)| t.elapsed() >= CLOSE_AFTER_MERGE).map(|(id, _)| id.clone()).collect();
    for id in due {
        let still_merged = d.prs.lock().unwrap().get(&id).map(|pr| pr.state == "merged");
        let s = d.sessions.lock().unwrap().iter().find(|s| s.id == id && !s.pane.is_exited()).cloned();
        let w = s.as_ref().and_then(|s| session_worktree(d, &s.cwd));
        let (Some(s), Some(w), Some(true)) = (s, w, still_merged) else {
            // Gone, not in a worktree, or reopened. A failed lookup (None) just waits.
            if still_merged.is_some() {
                d.closing.lock().unwrap().remove(&id);
            }
            continue;
        };
        let target = real(&w.path);
        if !sessions_in(d, &target).iter().all(|o| o.pane.is_exited() || idle(d, o)) {
            continue;
        }
        d.closing.lock().unwrap().remove(&id);
        if !pr::nothing_to_lose(&w.path) {
            s.auto.lock().unwrap().pr.note = Some("Kept open after the merge: its worktree has work that isn't pushed".into());
            continue;
        }
        if let Err(e) = remove_worktree(d, &target, false) {
            s.auto.lock().unwrap().pr.note = Some(format!("Couldn't close after the merge: {e}"));
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

/// Where Claude is trusted in `repo`, from `dir` up, when the policies carry trust into worktrees.
fn carried_trust(dir: &Path, repo: &Path) -> Option<PathBuf> {
    Settings::load().policies.worktree_trust.then(|| trust::claude_trusted_in(dir, repo)).flatten()
}

/// Trust the same folder in worktree `wt`, so Claude starts there without asking again.
fn carry_trust(l: &LauncherInfo, rel: Option<&Path>, wt: &Path) {
    if let Some(rel) = rel.filter(|_| l.agent_id.starts_with("claude")) {
        if let Err(e) = trust::claude_trust(&trust::join(wt, rel)) {
            eprintln!("dinod: couldn't trust {} for Claude: {e}", wt.display());
        }
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
    let l = d.allowed_launcher(&launch.launcher)?;
    let dir = work_dir(launch.cwd.as_deref());
    let checkout = worktree::repo_root(&dir)?;
    let name = format!("{}-{}", l.short, &new_uuid()[..4]);
    let branch = format!("dino/{name}");
    let (wt, base) = worktree::start(&checkout, &name, &branch)?;
    let repo = worktree::list(&wt)?.into_iter().next().map_or_else(|| checkout.clone(), |w| PathBuf::from(w.path));
    carry_trust(&l, carried_trust(&dir, &checkout).as_deref(), &wt);
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
