//! dinod's side of the build cache (see `dino_core::build_cache`): which sessions get it, the one
//! sccache server every session's builds share, and what Settings and `dino build-cache` show.
//!
//! Nothing runs until a session that gets the cache starts: then dinod writes the wrapper script
//! and starts the server (once per Mac; one left by an earlier dinod is used as it is). The server
//! idles at no cost and stops with dinod. Turned off, the script runs rustc as is at once, in every
//! session, and the server stops.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use dino_core::build_cache;
use dino_core::ipc::{BuildCacheInfo, BuildCacheStats};
use dino_core::settings::Settings;

/// The server's pid while dinod knows it (it started it, or found it at its socket); 0 otherwise.
/// Read by dinod's signal handler, so it goes with dinod however dinod stops.
pub(crate) static SERVER_PID: AtomicI32 = AtomicI32::new(0);
/// The size limit the server runs with, in GB; 0 when not known.
static SERVER_GB: AtomicU32 = AtomicU32::new(0);
/// One start or stop at a time.
static BUSY: Mutex<()> = Mutex::new(());

/// The environment session dinod is starting gets for the cache, given the environment it has so
/// far (`env`, over dinod's own): `RUSTC_WRAPPER`, when it's on, sccache is installed, and nothing
/// there sets up a wrapper or sccache already. Writes the script first, so Cargo never meets a
/// wrapper that isn't there; the server starts meanwhile, and a build that comes first compiles
/// as it would without the cache.
pub(crate) fn session_env(settings: &Settings, env: &HashMap<String, String>) -> Option<(String, String)> {
    let bc = &settings.machine.build_cache;
    if !bc.enabled || own_setup(env).is_some() {
        return None;
    }
    let sccache = build_cache::find_sccache()?;
    let socket = build_cache::socket();
    if !build_cache::socket_fits(&socket) {
        return None;
    }
    let dino = std::env::current_exe().ok()?;
    let script = build_cache::script_path();
    if let Err(e) = write_script(&script, &build_cache::script(Some((&dino, &sccache, bc.size())))) {
        eprintln!("dinod: build cache: couldn't write {} ({e}); sessions build without it", script.display());
        return None;
    }
    let gb = bc.size();
    std::thread::spawn(move || ensure(&sccache, gb));
    Some(("RUSTC_WRAPPER".into(), script.display().to_string()))
}

/// What already sets up a wrapper or sccache: the session's own environment (Settings →
/// Repositories), or dinod's (the shell that started it).
fn own_setup(env: &HashMap<String, String>) -> Option<String> {
    let mine: Vec<(String, String)> = std::env::vars().collect();
    let found = env.iter().chain(mine.iter().map(|(k, v)| (k, v))).find(|(k, v)| build_cache::user_has_own(std::iter::once((k.as_str(), v.as_str()))));
    found.map(|(k, _)| format!("{k} is already set where dino starts sessions; they use that instead"))
}

/// Write the script if it says something else, as a whole (a build reading it meanwhile gets the
/// old one or the new one).
fn write_script(path: &Path, text: &str) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    if std::fs::read_to_string(path).is_ok_and(|t| t == text) && is_executable(path) {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    std::fs::rename(tmp, path)
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata().is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
}

/// The pid of the server at `socket`, if one answers there.
fn server_at(socket: &Path) -> Option<i32> {
    use std::os::fd::AsRawFd;
    let stream = std::os::unix::net::UnixStream::connect(socket).ok()?;
    let mut pid: libc::pid_t = 0;
    let mut len = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    // SAFETY: a live socket; `pid` and `len` are locals of the right size. LOCAL_PEERPID (2, at
    // SOL_LOCAL 0) is macOS's.
    let ok = unsafe { libc::getsockopt(stream.as_raw_fd(), 0, 2, (&mut pid as *mut libc::pid_t).cast(), &mut len) } == 0;
    Some(if ok && pid > 0 { pid } else { 0 })
}

/// The server, up: the one at the socket, else one dinod starts now with a limit of `gb`.
fn ensure(sccache: &Path, gb: u32) {
    let _busy = BUSY.lock().unwrap();
    let socket = build_cache::socket();
    if let Some(pid) = server_at(&socket) {
        // A rustc call found it stuck (it took the call and never answered): ended, and started
        // again, unless it answers now.
        let stuck = build_cache::marked_hung(&socket) && !build_cache::responsive(sccache, &socket);
        if let Some(mark) = build_cache::hung_mark(&socket) {
            let _ = std::fs::remove_file(mark);
        }
        if !stuck {
            if SERVER_PID.swap(pid, Ordering::SeqCst) != pid {
                // Left by an earlier dinod: its limit, from itself.
                SERVER_GB.store(stats(sccache).map_or(0, |s| (s.max_bytes >> 30) as u32), Ordering::SeqCst);
            }
            return;
        }
        eprintln!("dinod: build cache: sccache (pid {pid}) isn't answering; starting another");
        if pid > 0 {
            // SAFETY: a signal to the server's pid, which a stopped process takes too.
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
        let _ = SERVER_PID.compare_exchange(pid, 0, Ordering::SeqCst, Ordering::SeqCst);
    }
    if let Err(e) = start(sccache, &socket, gb) {
        eprintln!("dinod: build cache: sccache didn't start ({e:#}); sessions build without it");
    }
}

fn start(sccache: &Path, socket: &Path, gb: u32) -> anyhow::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    use std::os::unix::process::CommandExt;
    // A socket file nothing answers at is left from a server that died: it would refuse the bind.
    let _ = std::fs::remove_file(socket);
    if let Some(dir) = socket.parent() {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    }
    // What's compiled in them is the user's own code: theirs alone.
    let cache = build_cache::cache_dir();
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&cache)?;
    let log_path = build_cache::server_log();
    if let Some(dir) = log_path.parent() {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    }
    let log = std::fs::OpenOptions::new().create(true).write(true).truncate(true).mode(0o600).open(&log_path)?;
    let mut cmd = Command::new(sccache);
    cmd.env("SCCACHE_START_SERVER", "1")
        .env("SCCACHE_NO_DAEMON", "1")
        // Until dinod stops it: an idle server costs nothing, and builds find it up.
        .env("SCCACHE_IDLE_TIMEOUT", "0")
        .env("SCCACHE_SERVER_UDS", socket)
        .env("SCCACHE_DIR", &cache)
        .env("SCCACHE_CACHE_SIZE", build_cache::size_value(gb))
        .env_remove("RUSTC_WRAPPER")
        // Its own group: what's sent to dinod's (a Ctrl+C where it runs in a terminal) isn't for it.
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    let mut child = cmd.spawn()?;
    let pid = child.id() as i32;
    SERVER_PID.store(pid, Ordering::SeqCst);
    SERVER_GB.store(gb, Ordering::SeqCst);
    std::thread::spawn(move || {
        let status = child.wait();
        // Unless another has taken its place meanwhile.
        let _ = SERVER_PID.compare_exchange(pid, 0, Ordering::SeqCst, Ordering::SeqCst);
        let how = status.map_or_else(|e| e.to_string(), |s| s.to_string());
        eprintln!("dinod: build cache: sccache (pid {pid}) stopped: {how}");
    });
    // Up before the first compile asks, unless it's slow to start: that compile then runs as is.
    let started = Instant::now();
    while server_at(socket).is_none() {
        anyhow::ensure!(SERVER_PID.load(Ordering::SeqCst) == pid, "sccache exited; see {}", log_path.display());
        if started.elapsed() > Duration::from_secs(10) {
            anyhow::bail!("sccache isn't responding at {} yet", socket.display());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    eprintln!("dinod: build cache: sccache (pid {pid}) serving {} at {}, up to {gb} GB", cache.display(), socket.display());
    Ok(())
}

/// A rustc call found no server: start it again in the background, unless it's off now, or dinod
/// tried moments ago (a server that won't start isn't tried for every call of a build).
pub(crate) fn heal() {
    static TRIED: Mutex<Option<Instant>> = Mutex::new(None);
    let bc = Settings::load().machine.build_cache;
    if !bc.enabled {
        return;
    }
    {
        let mut tried = TRIED.lock().unwrap();
        if tried.is_some_and(|t| t.elapsed() < Duration::from_secs(30)) {
            return;
        }
        *tried = Some(Instant::now());
    }
    let Some(sccache) = build_cache::find_sccache() else { return };
    std::thread::spawn(move || ensure(&sccache, bc.size()));
}

/// Stop the server, as dinod stops or the cache is turned off: asked to, else ended.
pub(crate) fn stop() {
    let _busy = BUSY.lock().unwrap();
    stop_locked();
}

fn stop_locked() {
    let socket = build_cache::socket();
    let pid = server_at(&socket).filter(|&p| p > 0).unwrap_or_else(|| SERVER_PID.load(Ordering::SeqCst));
    if pid <= 0 {
        return;
    }
    if let Some(sccache) = build_cache::find_sccache() {
        let asked = Command::new(sccache).arg("--stop-server").env("SCCACHE_SERVER_UDS", &socket).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
        if let Ok(mut c) = asked {
            let until = Instant::now() + Duration::from_secs(3);
            while Instant::now() < until && matches!(c.try_wait(), Ok(None)) {
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = c.kill();
            let _ = c.wait();
        }
    }
    // Still there: ended.
    // SAFETY: plain signals to a pid; 0 checks it's there.
    if unsafe { libc::kill(pid, 0) } == 0 {
        unsafe { libc::kill(pid, libc::SIGTERM) };
    }
    SERVER_PID.store(0, Ordering::SeqCst);
    SERVER_GB.store(0, Ordering::SeqCst);
    let _ = std::fs::remove_file(socket);
}

/// After the settings changed, and as dinod starts: off (or locked off), the script runs rustc as
/// is and the server stops; on with another size, a server that runs starts again with it.
pub(crate) fn reconcile() {
    let bc = Settings::load().machine.build_cache;
    let script = build_cache::script_path();
    if !bc.enabled {
        if script.exists() {
            let _ = write_script(&script, &build_cache::script(None));
        }
        stop();
        return;
    }
    let Some(sccache) = build_cache::find_sccache() else { return };
    // Sessions started while it was off keep `RUSTC_WRAPPER`: back on, they get the cache again.
    if script.exists()
        && let Ok(dino) = std::env::current_exe()
    {
        let _ = write_script(&script, &build_cache::script(Some((&dino, &sccache, bc.size()))));
    }
    let running = server_at(&build_cache::socket()).is_some();
    if running && SERVER_GB.load(Ordering::SeqCst) != bc.size() {
        stop();
        ensure(&sccache, bc.size());
    }
}

/// What the server says: its size, limit and counts.
struct Stats {
    max_bytes: u64,
    size_bytes: Option<u64>,
    counts: BuildCacheStats,
}

fn stats(sccache: &Path) -> Option<Stats> {
    let out = Command::new(sccache)
        .args(["--show-stats", "--stats-format=json"])
        .env("SCCACHE_SERVER_UDS", build_cache::socket())
        .env("SCCACHE_DIR", build_cache::cache_dir())
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    parse_stats(&out.stdout)
}

fn parse_stats(json: &[u8]) -> Option<Stats> {
    let v: serde_json::Value = serde_json::from_slice(json).ok()?;
    let s = &v["stats"];
    let n = |k: &str| s[k].as_u64().unwrap_or(0);
    let sum = |k: &str| s[k]["counts"].as_object().map_or(0, |m| m.values().filter_map(|c| c.as_u64()).sum());
    let counts = BuildCacheStats {
        requests: n("compile_requests"),
        hits: sum("cache_hits"),
        misses: sum("cache_misses"),
        not_cacheable: n("requests_not_cacheable"),
        other: n("requests_not_compile") + n("requests_unsupported_compiler") + sum("cache_errors"),
    };
    Some(Stats { max_bytes: v["max_cache_size"].as_u64()?, size_bytes: v["cache_size"].as_u64(), counts })
}

/// Where the cache stands, for Settings and `dino build-cache`.
pub(crate) fn info() -> BuildCacheInfo {
    let bc = Settings::load().machine.build_cache;
    let sccache = build_cache::find_sccache();
    let socket = build_cache::socket();
    let running = server_at(&socket).is_some();
    let unused = if !bc.enabled || sccache.is_none() {
        None
    } else if !build_cache::socket_fits(&socket) {
        Some(format!("dino's folder path is too long for sccache ({})", socket.display()))
    } else {
        own_setup(&HashMap::new())
    };
    let version = sccache.as_deref().and_then(version);
    let stats = sccache.as_deref().filter(|_| running).and_then(stats);
    BuildCacheInfo {
        enabled: bc.enabled,
        sccache: sccache.map(|p| p.display().to_string()),
        version,
        install: build_cache::install_command().into(),
        unused,
        running,
        dir: build_cache::cache_dir().display().to_string(),
        max_bytes: stats.as_ref().map_or(u64::from(bc.size()) << 30, |s| s.max_bytes),
        size_bytes: stats.as_ref().and_then(|s| s.size_bytes),
        stats: stats.map(|s| s.counts),
    }
}

/// `sccache --version`'s number, remembered per program.
fn version(sccache: &Path) -> Option<String> {
    static SEEN: Mutex<Option<(PathBuf, String)>> = Mutex::new(None);
    let mut seen = SEEN.lock().unwrap();
    if let Some((_, v)) = seen.as_ref().filter(|(p, _)| p == sccache) {
        return Some(v.clone());
    }
    let out = Command::new(sccache).arg("--version").stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    let v = String::from_utf8_lossy(&out.stdout).trim().trim_start_matches("sccache ").to_string();
    (!v.is_empty()).then(|| {
        *seen = Some((sccache.to_path_buf(), v.clone()));
        v
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_from_sccache() {
        let json = br#"{"stats":{"compile_requests":309,"requests_unsupported_compiler":1,"requests_not_compile":3,"requests_not_cacheable":49,
            "cache_errors":{"counts":{"Rust":2}},"cache_hits":{"counts":{"Rust":150,"C/C++":4}},"cache_misses":{"counts":{"Rust":14}}},
            "cache_size":90461828,"max_cache_size":10737418240,"version":"0.18.0"}"#;
        let s = parse_stats(json).unwrap();
        assert_eq!(s.max_bytes, 10 << 30);
        assert_eq!(s.size_bytes, Some(90461828));
        assert_eq!(s.counts, BuildCacheStats { requests: 309, hits: 154, misses: 14, not_cacheable: 49, other: 6 });
        assert_eq!(s.counts.hit_rate().map(|r| r.round()), Some(92.0));
        assert!(parse_stats(b"not json").is_none());
        assert_eq!(BuildCacheStats::default().hit_rate(), None);
    }

    #[test]
    fn script_is_written_once_and_executable() {
        let dir = std::env::temp_dir().join(format!("dinod-build-cache-{}", std::process::id()));
        let path = dir.join("bc/rustc-wrapper");
        write_script(&path, "#!/bin/sh\nexec \"$@\"\n").unwrap();
        assert!(is_executable(&path));
        let before = path.metadata().unwrap().modified().unwrap();
        std::thread::sleep(Duration::from_millis(20));
        write_script(&path, "#!/bin/sh\nexec \"$@\"\n").unwrap();
        assert_eq!(path.metadata().unwrap().modified().unwrap(), before, "unchanged: not written again");
        write_script(&path, "#!/bin/sh\nexit 0\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "#!/bin/sh\nexit 0\n");
        let _ = std::fs::remove_dir_all(dir);
    }
}
