//! dinod: owns agent sessions (PTY + terminal state), the proxy and the router. Clients (the TUI,
//! `dino attach` inside a Ghostty surface, the future app) talk to it over a Unix socket; agents
//! keep running when every client goes away.

use std::collections::HashMap;
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
use dino_core::{detect_agents, load_keys, proxy_wiring, trust, user_shell, worktree};
use dino_proxy::{Activity, Proxy};
use dino_term::{Pane, SpawnSpec};

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
    last_output: Arc<Mutex<Option<Instant>>>,
    attached: AtomicUsize,
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
                    Response::Ok
                }
                Err(e) => Response::Error { message: e.to_string() },
            },
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
            Request::Shutdown => {
                // Saved first: `dino stop` pauses sessions, the next dinod resumes them.
                save(d);
                ipc::write_json(&mut stream, &Response::Ok)?;
                for s in d.sessions.lock().unwrap().drain(..) {
                    s.pane.kill();
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
}

impl Launch {
    fn new(launcher: &str, args: Vec<String>, cwd: Option<String>) -> Self {
        Self { launcher: launcher.into(), args, cwd, cols: 120, rows: 40, ..Default::default() }
    }
}

fn spawn(d: &Daemon, launch: Launch) -> anyhow::Result<String> {
    let Launch { launcher, args, cwd, cols, rows, restore, name, prompt } = launch;
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
    let (subs, last) = (subscribers.clone(), last_output.clone());
    let pane = Pane::spawn(spec, cols.max(20), rows.max(5), move |bytes| {
        if !bytes.is_empty() {
            *last.lock().unwrap() = Some(Instant::now());
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
        attached: AtomicUsize::new(0),
    }));
    Ok(id)
}

/// Stream a session to a client until it detaches or the session ends.
fn attach(d: &Daemon, s: &Session, mut stream: UnixStream, cols: u16, rows: u16) -> io::Result<()> {
    ipc::write_json(&mut stream, &Response::Ok)?;
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
            ipc::DATA => s.pane.write(payload),
            ipc::RESIZE => {
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

fn state(d: &Daemon) -> Response {
    let groups = d.groups.lock().unwrap().clone();
    let group_of = |id: &str| groups.iter().find(|g| g.members.iter().any(|m| m.session == id)).map(|g| g.id.clone());
    let sessions = d
        .sessions
        .lock()
        .unwrap()
        .iter()
        .map(|s| {
            let st = d.proxy.stats.session(&s.id);
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
            true
        }
        None => false,
    }
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
                for w in &mut w {
                    w.dino = made.contains(&real(Path::new(&w.path)));
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
    let cwd = d.sessions.lock().unwrap().iter().find(|s| s.id == id).map(|s| s.cwd.clone());
    let cwd = cwd.ok_or_else(|| anyhow::anyhow!("no session {id}"))?;
    let (dir, base, label) = match (find_member(d, id), session_worktree(d, &cwd)) {
        (Ok((g, m)), _) => (m.worktree, g.base, "where the fan-out started".to_string()),
        (Err(_), Some(w)) => (w.path, w.base, "where the worktree started".to_string()),
        (Err(_), None) => match worktree::repo_root(&cwd) {
            Ok(root) => {
                let head = worktree::head(&root);
                (root, head, "the last commit".to_string())
            }
            Err(e) => return Ok(Response::Changes { root: real(&cwd), base: String::new(), files: vec![], note: Some(e.to_string()) }),
        },
    };
    Ok(Response::Changes { root: real(&dir), files: worktree::changes(&dir, &base)?, base: label, note: None })
}

/// The worktree dino made that `dir` is in: its commits count as changes too.
fn session_worktree(d: &Daemon, dir: &Path) -> Option<SessionWorktree> {
    d.worktrees.lock().unwrap().iter().find(|w| dir.starts_with(&w.path)).cloned()
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

fn remove_worktree(d: &Daemon, path: &str, apply: bool) -> anyhow::Result<()> {
    let target = real(Path::new(path));
    let w = d.worktrees.lock().unwrap().iter().find(|w| real(&w.path) == target).cloned();
    let w = w.ok_or_else(|| anyhow::anyhow!("{path} isn't a worktree dino made for a session"))?;
    if apply {
        worktree::apply(&w.path, &w.base, &w.checkout)?;
    }
    let inside: Vec<String> = d
        .sessions
        .lock()
        .unwrap()
        .iter()
        .filter(|s| {
            let cwd = real(&s.cwd);
            cwd == target || cwd.starts_with(&format!("{target}/"))
        })
        .map(|s| s.id.clone())
        .collect();
    for id in &inside {
        kill(d, id);
    }
    save(d);
    worktree::remove(&w.repo, &w.path, &w.branch);
    let _ = trust::claude_forget(&w.path);
    let mut worktrees = d.worktrees.lock().unwrap();
    worktrees.retain(|o| o.path != w.path);
    save_worktrees(&worktrees);
    Ok(())
}
