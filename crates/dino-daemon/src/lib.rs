//! dinod: owns agent sessions (PTY + terminal state), the proxy and the router. Clients (the TUI,
//! `dino attach` inside a Ghostty surface, the future app) talk to it over a Unix socket; agents
//! keep running when every client goes away.

use std::collections::HashMap;
use std::io;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use dino_core::ipc::{self, LauncherInfo, QuotaInfo, Request, Response, SessionInfo, WindowInfo};
use dino_core::{Config, detect_agents, load_keys, proxy_wiring, user_shell};
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

struct Daemon {
    proxy: Proxy,
    launchers: Vec<LauncherInfo>,
    sessions: Mutex<Vec<Arc<Session>>>,
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
    let daemon = Arc::new(Daemon {
        proxy: Proxy::start(keys)?,
        launchers: launchers(free_tier),
        sessions: Mutex::default(),
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
            Request::Launchers => Response::Launchers { launchers: d.launchers.clone() },
            Request::New { launcher, args, cwd, cols, rows } => match spawn(d, &launcher, args, cwd, cols, rows, None) {
                Ok(id) => {
                    save(d);
                    Response::Created { id }
                }
                Err(e) => Response::Error { message: e.to_string() },
            },
            Request::Kill { id } => {
                let mut sessions = d.sessions.lock().unwrap();
                match sessions.iter().position(|s| s.id == id) {
                    Some(i) => {
                        sessions.remove(i).pane.kill();
                        drop(sessions);
                        save(d);
                        Response::Ok
                    }
                    None => Response::Error { message: format!("no session {id}") },
                }
            }
            Request::Attach { id, cols, rows } => {
                let session = d.sessions.lock().unwrap().iter().find(|s| s.id == id).cloned();
                return match session {
                    Some(s) => attach(d, &s, stream, cols, rows),
                    None => ipc::write_json(&mut stream, &Response::Error { message: format!("no session {id}") }),
                };
            }
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

fn spawn(d: &Daemon, launcher: &str, args: Vec<String>, cwd: Option<String>, cols: u16, rows: u16, restore: Option<SavedSession>) -> anyhow::Result<String> {
    let l = d.launchers.iter().find(|l| l.short == launcher).ok_or_else(|| anyhow::anyhow!("unknown agent {launcher}"))?;
    let id = match &restore {
        Some(r) => r.id.clone(),
        None => d.next_id.fetch_add(1, Ordering::Relaxed).to_string(),
    };
    let cwd = cwd.map(PathBuf::from).or_else(|| std::env::current_dir().ok()).unwrap_or_default();
    let (env, mut wired_args) = proxy_wiring(&l.agent_id, Config::load().route, &|provider| d.proxy.base_url(&id, provider));

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
    let name = match &restore {
        Some(r) => r.name.clone(),
        None => {
            let n = sessions.iter().filter(|s| s.name == l.short || s.name.starts_with(&format!("{}-", l.short))).count();
            if n == 0 { l.short.clone() } else { format!("{}-{}", l.short, n + 1) }
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
        if let Err(e) = spawn(d, &s.launcher.clone(), s.args.clone(), Some(s.cwd.clone()), 120, 40, Some(s.clone())) {
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
