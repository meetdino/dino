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
use std::time::Instant;

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

    let keys = load_keys();
    let free_tier = keys.contains_key("NVIDIA_API_KEY");
    let daemon = Arc::new(Daemon {
        proxy: Proxy::start(keys)?,
        launchers: launchers(free_tier),
        sessions: Mutex::default(),
        next_id: AtomicU64::new(1),
        next_sub: AtomicU64::new(1),
    });
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
            Request::New { launcher, args, cwd, cols, rows } => match spawn(d, &launcher, args, cwd, cols, rows) {
                Ok(id) => Response::Created { id },
                Err(e) => Response::Error { message: e.to_string() },
            },
            Request::Kill { id } => {
                let mut sessions = d.sessions.lock().unwrap();
                match sessions.iter().position(|s| s.id == id) {
                    Some(i) => {
                        sessions.remove(i).pane.kill();
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

fn spawn(d: &Daemon, launcher: &str, args: Vec<String>, cwd: Option<String>, cols: u16, rows: u16) -> anyhow::Result<String> {
    let l = d.launchers.iter().find(|l| l.short == launcher).ok_or_else(|| anyhow::anyhow!("unknown agent {launcher}"))?;
    let id = d.next_id.fetch_add(1, Ordering::Relaxed).to_string();
    let (env, mut wired_args) = proxy_wiring(&l.agent_id, Config::load().route, &|provider| d.proxy.base_url(&id, provider));
    wired_args.extend(args);
    let spec = SpawnSpec {
        program: l.program.clone(),
        args: wired_args,
        cwd: cwd.map(PathBuf::from).or_else(|| std::env::current_dir().ok()),
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
    let n = sessions.iter().filter(|s| s.name == l.short || s.name.starts_with(&format!("{}-", l.short))).count();
    let name = if n == 0 { l.short.clone() } else { format!("{}-{}", l.short, n + 1) };
    sessions.push(Arc::new(Session {
        id: id.clone(),
        name,
        agent_id: l.agent_id.clone(),
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
