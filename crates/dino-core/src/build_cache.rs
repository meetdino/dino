//! One compiler cache for every session on this Mac (Settings → Workspaces → Worktrees → Build
//! Cache): each worktree keeps its own `target/`, and what one worktree compiled, every other one
//! gets from the cache instead of compiling it again.
//!
//! Cargo's own hook does it, nothing else: sessions dinod starts get `RUSTC_WRAPPER`, a small
//! script of dino's (see [`script`]) that runs `dino rustc-wrapper`, which compiles through sccache
//! when it can help and runs rustc itself whenever it can't ([`wrap`]). dinod runs the one sccache
//! server, on a socket of its own, so an sccache the user runs themselves is never touched.
//!
//! It never fails a build: no sccache, a server that's gone or broken, a cache folder it can't
//! write, and an sccache that crashes all end in plain rustc. A wrapper the repo or the user chose
//! (`build.rustc-wrapper` in a Cargo config, `RUSTC_WRAPPER` or `SCCACHE_*` of their own) is used
//! instead, as Cargo would without dino.

use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::config_dir;

/// The variables dino's script hands `dino rustc-wrapper` for sccache, and only for it: the
/// session itself never has them, so an sccache the user runs by hand keeps its own settings.
pub const SCCACHE_VARS: [&str; 4] = ["SCCACHE_SERVER_UDS", "SCCACHE_DIR", "SCCACHE_CACHE_SIZE", "SCCACHE_IGNORE_SERVER_IO_ERROR"];

/// Where sccache keeps the cache: with the Mac's other caches, or inside an isolated dino's folder.
pub fn cache_dir() -> PathBuf {
    if std::env::var_os("DINO_HOME").is_some() {
        return config_dir().join("build-cache/sccache");
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    #[cfg(target_os = "macos")]
    return home.join("Library/Caches/dino/sccache");
    // `$XDG_CACHE_HOME`, else ~/.cache.
    #[cfg(not(target_os = "macos"))]
    return crate::xdg_dir("XDG_CACHE_HOME").unwrap_or_else(|| home.join(".cache")).join("dino/sccache");
}

/// The socket dino's sccache server listens on: in `run/` beside dinod's own socket, which
/// `dino rustc-wrapper` finds from it.
pub fn socket() -> PathBuf {
    config_dir().join("run/sccache.sock")
}

/// A Unix socket's path must fit `sun_path` (104 bytes on macOS, with its NUL).
pub fn socket_fits(path: &Path) -> bool {
    path.as_os_str().len() < 104
}

/// The script sessions get as `RUSTC_WRAPPER`.
pub fn script_path() -> PathBuf {
    config_dir().join("build-cache/rustc-wrapper")
}

/// The server's log.
pub fn server_log() -> PathBuf {
    config_dir().join("build-cache/server.log")
}

/// sccache on this Mac: on the `PATH`, else where Homebrew and `cargo install` put it.
pub fn find_sccache() -> Option<PathBuf> {
    crate::which("sccache")
}

/// How to install it, as Settings and `dino build-cache` offer it (never run without asking).
pub fn install_command() -> &'static str {
    if crate::which("brew").is_some() { "brew install sccache" } else { "cargo install sccache --locked" }
}

/// Whether `vars` (an environment) already says how to wrap rustc or set up sccache: then dino
/// adds nothing, as if it weren't there.
pub fn user_has_own<'a>(mut vars: impl Iterator<Item = (&'a str, &'a str)>) -> bool {
    vars.any(|(k, _)| k == "RUSTC_WRAPPER" || k == "CARGO_BUILD_RUSTC_WRAPPER" || k.starts_with("SCCACHE_"))
}

/// `SCCACHE_CACHE_SIZE` for a limit of `gb`.
pub fn size_value(gb: u32) -> String {
    format!("{gb}G")
}

/// The script: `dino rustc-wrapper` with dino's sccache settings when both programs are still
/// there, else rustc as is. With `cache` none (turned off), always rustc as is: sessions started
/// while it was on keep `RUSTC_WRAPPER`, and turning it off reaches them at once.
pub fn script(cache: Option<(&Path, &Path, u32)>) -> String {
    let mut s = String::from("#!/bin/sh\n# dino's build cache (Settings → Workspaces → Worktrees, `dino build-cache`): rustc through\n# sccache, or rustc as is whenever the cache can't help. Written by dinod.\n");
    if let Some((dino, sccache, gb)) = cache {
        let q = |p: &Path| quote(&p.display().to_string());
        s.push_str(&format!(
            "[ -x {d} ] && [ -x {sc} ] && SCCACHE_SERVER_UDS={sock} SCCACHE_DIR={dir} SCCACHE_CACHE_SIZE={size} SCCACHE_IGNORE_SERVER_IO_ERROR=1 exec {d} rustc-wrapper {sc} \"$@\"\n",
            d = q(dino),
            sc = q(sccache),
            sock = q(&socket()),
            dir = q(&cache_dir()),
            size = size_value(gb),
        ));
    }
    s.push_str("exec \"$@\"\n");
    s
}

/// `s` in single quotes for sh.
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// What a rustc call goes through.
#[derive(Debug, PartialEq)]
enum Route {
    /// dino's sccache.
    Cache,
    /// rustc as is.
    Plain,
    /// rustc as is: the server isn't up. dinod is asked to start it again, for the calls after.
    NoServer,
    /// The wrapper a Cargo config names (`build.rustc-wrapper`), resolved as Cargo resolves it.
    Theirs(PathBuf),
}

/// `dino rustc-wrapper <sccache> <rustc> <args…>`, as dino's script runs it for Cargo: the
/// compile through sccache, or rustc itself whenever sccache can't do it. Returns the exit code;
/// on the plain path it replaces itself with rustc.
pub fn wrap(args: &[OsString]) -> i32 {
    let [sccache, rustc, rest @ ..] = args else {
        eprintln!("usage: dino rustc-wrapper <sccache> <rustc> [args…] (run by Cargo as RUSTC_WRAPPER)");
        return 2;
    };
    let env = |k: &str| std::env::var_os(k);
    let starts: Vec<PathBuf> = cargo_dir().into_iter().chain(env("PWD").map(PathBuf::from)).chain(std::env::current_dir().ok()).collect();
    let cargo_home = env("CARGO_HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(env("HOME").unwrap_or_default()).join(".cargo"));
    let route = route(&starts, &cargo_home, env("CARGO_BUILD_RUSTC_WRAPPER"), [env("CARGO_INCREMENTAL"), env("CARGO_BUILD_INCREMENTAL")], env("SCCACHE_SERVER_UDS").as_deref());
    match route {
        Route::Theirs(w) => plain(&w, std::iter::once(rustc).chain(rest)),
        Route::Plain => plain(Path::new(rustc), rest.iter()),
        Route::NoServer => {
            if let Some(socket) = env("SCCACHE_SERVER_UDS") {
                ask_for_server(Path::new(&socket));
            }
            plain(Path::new(rustc), rest.iter())
        }
        Route::Cache => match cached(Path::new(sccache), rustc, rest) {
            Some(code) => code,
            None => plain(Path::new(rustc), rest.iter()),
        },
    }
}

/// Where Cargo looked for its config: the folder it runs in. Not this call's own folder (a
/// dependency's is the dependency's, and so is its `PWD`): the nearest `cargo` above it, which
/// is its parent, or its grandparent for a build script's own probe of rustc. Else the parent's.
fn cargo_dir() -> Option<PathBuf> {
    use crate::procinfo;
    // SAFETY: no arguments; it can't fail.
    let parent = unsafe { libc::getppid() } as u32;
    let mut pid = parent;
    for _ in 0..4 {
        if procinfo::name(pid).is_some_and(|n| n == "cargo") {
            return procinfo::cwd_of(pid).map(PathBuf::from);
        }
        match procinfo::parent_of(pid) {
            Some(p) if p > 1 => pid = p,
            _ => break,
        }
    }
    procinfo::cwd_of(parent).map(PathBuf::from)
}

fn route(starts: &[PathBuf], cargo_home: &Path, env_wrapper: Option<OsString>, incremental: [Option<OsString>; 2], socket: Option<&OsStr>) -> Route {
    // The environment's own `build.rustc-wrapper`, set after the session started (a shell's rc).
    if let Some(w) = env_wrapper {
        return if w.is_empty() { Route::Plain } else { Route::Theirs(resolve(&w.to_string_lossy(), None)) };
    }
    if let Some((w, base)) = configured_wrapper(starts, cargo_home) {
        return if w.is_empty() { Route::Plain } else { Route::Theirs(resolve(&w, Some(&base))) };
    }
    // sccache refuses outright with incremental compilation forced on through the environment.
    if incremental.iter().flatten().any(|v| v == "1") {
        return Route::Plain;
    }
    // Only dinod starts the server: one started from here would run with this session's limits
    // (an agent's sandbox), for every session. No server, no cache.
    match socket.map(Path::new) {
        Some(s) if marked_hung(s) => Route::NoServer,
        Some(s) if std::os::unix::net::UnixStream::connect(s).is_ok() => Route::Cache,
        Some(_) => Route::NoServer,
        None => Route::Plain,
    }
}

/// The `build.rustc-wrapper` Cargo would use, run from one of `dirs`: the nearest
/// `.cargo/config.toml` (or `.cargo/config`) that sets it, in a dir or above, else `$CARGO_HOME`'s.
/// With the folder its relative path is relative to (the one holding `.cargo`).
fn configured_wrapper(dirs: &[PathBuf], cargo_home: &Path) -> Option<(String, PathBuf)> {
    let read = |file: PathBuf, base: &Path| -> Option<(String, PathBuf)> {
        let text = std::fs::read_to_string(&file).ok()?;
        let doc: toml::Table = text.parse().ok()?;
        let w = doc.get("build")?.get("rustc-wrapper")?.as_str()?.to_string();
        Some((w, base.to_path_buf()))
    };
    let at = |cargo_dir: &Path, base: &Path| {
        // Cargo reads `config` over `config.toml` when both are there.
        read(cargo_dir.join("config"), base).or_else(|| read(cargo_dir.join("config.toml"), base))
    };
    dirs.iter()
        .find_map(|dir| dir.ancestors().find_map(|d| at(&d.join(".cargo"), d)))
        .or_else(|| at(cargo_home, cargo_home.parent().unwrap_or(cargo_home)))
}

/// A wrapper as Cargo finds it: a bare name on the `PATH` (left to exec), a path with a slash
/// relative to the folder holding the config's `.cargo`.
fn resolve(w: &str, base: Option<&Path>) -> PathBuf {
    match base {
        Some(b) if w.contains('/') && !w.starts_with('/') => b.join(w),
        _ => PathBuf::from(w),
    }
}

/// Ask the dinod whose server listens at `socket` (see [`socket`]) to start it again (it ended, or
/// was ended), without waiting: at most every few seconds from all of a build's rustc calls
/// together, by a mark's age. The dinod is the one that wrote the script, whatever this process's
/// environment says (`DINO_HOME` may not have come this far).
fn ask_for_server(socket: &Path) {
    let Some(run) = socket.parent() else { return };
    let Some(dinod) = run.parent().map(|c| c.join(crate::ipc::SOCKET_NAME)) else { return };
    let mark = run.join("sccache.asked");
    if fresh(&mark, std::time::Duration::from_secs(10)) || std::fs::write(&mark, b"").is_err() {
        return;
    }
    if let Ok(mut s) = std::os::unix::net::UnixStream::connect(dinod) {
        let _ = crate::ipc::write_json(&mut s, &crate::ipc::Request::BuildCacheEnsure);
    }
}

/// Run `program` with `args` in place of this process, without dino's sccache settings.
fn plain<'a>(program: &Path, args: impl Iterator<Item = &'a OsString>) -> i32 {
    use std::os::unix::process::CommandExt;
    let mut cmd = Command::new(program);
    cmd.args(args);
    for v in SCCACHE_VARS {
        cmd.env_remove(v);
    }
    let e = cmd.exec();
    eprintln!("dino: couldn't run {}: {e}", program.display());
    // What a shell says for a command it can't run.
    if e.kind() == std::io::ErrorKind::NotFound { 127 } else { 126 }
}

/// The compile through sccache: its exit code, or none when sccache itself failed (it couldn't
/// run, it crashed, or it said `sccache: error:`) and rustc should run as is instead. rustc's
/// own errors come back as they are. sccache hands back rustc's output only once it's done, so
/// holding its errors until then changes nothing for Cargo.
fn cached(sccache: &Path, rustc: &OsString, rest: &[OsString]) -> Option<i32> {
    let socket = std::env::var_os("SCCACHE_SERVER_UDS").map(PathBuf::from);
    let alive = {
        let (sccache, socket) = (sccache.to_path_buf(), socket.clone());
        move || socket.as_deref().is_none_or(|s| responsive(&sccache, s))
    };
    let code = cached_watched(sccache, rustc, rest, HUNG_AFTER, alive);
    if code.is_none()
        && let Some(s) = socket.as_deref().filter(|s| marked_hung(s))
    {
        ask_for_server(s);
    }
    code
}

/// How long a compile goes before dino checks the server still answers (a big crate's compile
/// takes minutes; a server that's stopped or stuck never answers), and how long a stuck server
/// is passed by, so the calls after don't each wait for it.
const HUNG_AFTER: std::time::Duration = std::time::Duration::from_secs(30);
const HUNG_FOR: std::time::Duration = std::time::Duration::from_secs(120);

/// Where a rustc call that found the server stuck says so, beside its socket.
pub fn hung_mark(socket: &Path) -> Option<PathBuf> {
    socket.parent().map(|r| r.join("sccache.hung"))
}

/// A rustc call found the server at `socket` stuck, moments ago.
pub fn marked_hung(socket: &Path) -> bool {
    hung_mark(socket).is_some_and(|m| fresh(&m, HUNG_FOR))
}

fn fresh(mark: &Path, within: std::time::Duration) -> bool {
    mark.metadata().and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().is_ok_and(|e| e < within))
}

/// Whether the server at `socket` answers sccache's own stats request within a few seconds.
pub fn responsive(sccache: &Path, socket: &Path) -> bool {
    let child = Command::new(sccache)
        .args(["--show-stats", "--stats-format=json"])
        .env("SCCACHE_SERVER_UDS", socket)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else { return false };
    let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(s)) => return s.success(),
            Ok(None) if std::time::Instant::now() < until => std::thread::sleep(std::time::Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

/// [`cached`], with a watch on the server: past `after`, and every `after` from then on, `alive`
/// says whether it still answers. When it doesn't, the compile is ended, the server marked stuck
/// (see [`hung_mark`]), and none comes back, so rustc runs as is.
fn cached_watched(sccache: &Path, rustc: &OsString, rest: &[OsString], after: std::time::Duration, alive: impl Fn() -> bool + Send + 'static) -> Option<i32> {
    let mut child = Command::new(sccache).arg(rustc).args(rest).stderr(Stdio::piped()).spawn().ok()?;
    let pid = child.id() as libc::pid_t;
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stuck = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let (done, stuck) = (done.clone(), stuck.clone());
        let socket = std::env::var_os("SCCACHE_SERVER_UDS").map(PathBuf::from);
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(after);
                if done.load(std::sync::atomic::Ordering::SeqCst) || alive() {
                    if done.load(std::sync::atomic::Ordering::SeqCst) {
                        return;
                    }
                    continue;
                }
                stuck.store(true, std::sync::atomic::Ordering::SeqCst);
                if let Some(m) = socket.as_deref().and_then(hung_mark) {
                    let _ = std::fs::write(m, b"");
                }
                // SAFETY: a signal to the child this process started and hasn't waited for yet.
                unsafe { libc::kill(pid, libc::SIGKILL) };
                return;
            }
        });
    }
    // Read on a thread of its own: once the compile is ended for a stuck server, whatever it
    // started may still hold its end open, and isn't waited for.
    let mut stderr = child.stderr.take()?;
    let reader = std::thread::spawn(move || {
        let mut err = vec![];
        stderr.read_to_end(&mut err).map(|_| err)
    });
    let status = child.wait();
    done.store(true, std::sync::atomic::Ordering::SeqCst);
    let status = status.ok()?;
    if stuck.load(std::sync::atomic::Ordering::SeqCst) {
        return None;
    }
    let err = reader.join().ok()?.ok()?;
    let code = status.code()?;
    if (code == SCCACHE_FAILED && failed(&err)) || COULDNT_RUN.contains(&code) {
        return None;
    }
    let _ = std::io::stderr().write_all(&err);
    Some(code)
}

/// The code sccache exits with when it fails itself (rustc's own errors are 1, and 101 for a crash).
const SCCACHE_FAILED: i32 = 2;
/// A program that couldn't be run at all, as a shell says it (a file that isn't a program is
/// handed to sh, which gives up with 126): never rustc's.
const COULDNT_RUN: [i32; 2] = [126, 127];

/// sccache's own error, as it prints it, on a line of its own.
fn failed(stderr: &[u8]) -> bool {
    stderr.split(|&b| b == b'\n').any(|l| l.starts_with(b"sccache: error:"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dino-build-cache-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn dinod_is_found_from_the_servers_socket() {
        assert_eq!(socket().parent().and_then(Path::parent).map(|c| c.join(crate::ipc::SOCKET_NAME)), Some(crate::ipc::socket_path()));
    }

    #[test]
    fn users_own_setup_wins() {
        assert!(!user_has_own([("PATH", "/bin"), ("CARGO_HOME", "/x")].into_iter()));
        assert!(user_has_own([("RUSTC_WRAPPER", "sccache")].into_iter()));
        // Set empty, it still says "no wrapper", which dino's would override.
        assert!(user_has_own([("RUSTC_WRAPPER", "")].into_iter()));
        assert!(user_has_own([("CARGO_BUILD_RUSTC_WRAPPER", "x")].into_iter()));
        assert!(user_has_own([("SCCACHE_DIR", "/c")].into_iter()));
        assert!(user_has_own([("SCCACHE_BASEDIRS", "/w")].into_iter()));
    }

    #[test]
    fn script_runs_rustc_as_is_when_off_or_gone() {
        let off = script(None);
        assert!(off.starts_with("#!/bin/sh\n"));
        assert!(off.ends_with("exec \"$@\"\n"));
        assert!(!off.contains("sccache "), "off: no sccache at all");
        let on = script(Some((Path::new("/Apps/Dino it's.app/dino"), Path::new("/opt/homebrew/bin/sccache"), 7)));
        assert!(on.contains("[ -x '/Apps/Dino it'\\''s.app/dino' ] && [ -x '/opt/homebrew/bin/sccache' ] && "));
        assert!(on.contains("SCCACHE_CACHE_SIZE=7G SCCACHE_IGNORE_SERVER_IO_ERROR=1 exec '/Apps/Dino it'\\''s.app/dino' rustc-wrapper '/opt/homebrew/bin/sccache' \"$@\"\n"));
        // Either program gone: rustc as is.
        assert!(on.ends_with("exec \"$@\"\n"));
    }

    #[test]
    fn script_falls_back_in_sh() {
        let dir = tmp("script");
        let path = dir.join("rustc-wrapper");
        // dino's gone: the script runs what Cargo asked for as is.
        std::fs::write(&path, script(Some((&dir.join("no-dino"), Path::new("/bin/echo"), 1)))).unwrap();
        let out = Command::new("/bin/sh").arg(&path).args(["/bin/echo", "rustc", "-vV"]).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "rustc -vV\n");
        assert!(out.status.success());
        std::fs::write(&path, script(None)).unwrap();
        let out = Command::new("/bin/sh").arg(&path).args(["/usr/bin/false"]).output().unwrap();
        assert_eq!(out.status.code(), Some(1), "rustc's own exit code comes back");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_cargo_config_wrapper_wins() {
        let root = tmp("config");
        let home = root.join("home/.cargo");
        let repo = root.join("repo");
        let deep = repo.join("crates/a/src");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        let none = [None, None];
        let sock = None;
        assert_eq!(route(std::slice::from_ref(&deep), &home, None, none.clone(), sock), Route::Plain, "no socket: plain");
        // The user's own, in $CARGO_HOME.
        std::fs::write(home.join("config.toml"), "[build]\nrustc-wrapper = \"sccache\"\n").unwrap();
        assert_eq!(route(std::slice::from_ref(&deep), &home, None, none.clone(), sock), Route::Theirs("sccache".into()));
        // The repo's, nearer, wins; relative to the folder holding `.cargo`.
        std::fs::create_dir_all(repo.join(".cargo")).unwrap();
        std::fs::write(repo.join(".cargo/config.toml"), "[build]\nrustc-wrapper = \"tools/wrap\"\njobs = 2\n").unwrap();
        assert_eq!(route(std::slice::from_ref(&deep), &home, None, none.clone(), sock), Route::Theirs(repo.join("tools/wrap")));
        // `config` over `config.toml`; empty means no wrapper at all.
        std::fs::write(repo.join(".cargo/config"), "[build]\nrustc-wrapper = \"\"\n").unwrap();
        assert_eq!(route(std::slice::from_ref(&deep), &home, None, none.clone(), sock), Route::Plain);
        // A config that doesn't set it is passed over.
        std::fs::write(repo.join(".cargo/config"), "[build]\njobs = 1\n").unwrap();
        std::fs::remove_file(repo.join(".cargo/config.toml")).unwrap();
        assert_eq!(route(std::slice::from_ref(&deep), &home, None, none.clone(), sock), Route::Theirs("sccache".into()));
        // Any of the places Cargo looks: the dep's folder (cwd) has none, but where cargo ran (PWD) does.
        std::fs::write(repo.join(".cargo/config.toml"), "[build]\nrustc-wrapper = \"/abs/w\"\n").unwrap();
        let dep = root.join("registry/src/serde");
        std::fs::create_dir_all(&dep).unwrap();
        std::fs::remove_file(home.join("config.toml")).unwrap();
        assert_eq!(route(&[repo.clone(), dep.clone()], &home, None, none.clone(), sock), Route::Theirs("/abs/w".into()));
        assert_eq!(route(std::slice::from_ref(&dep), &home, None, none.clone(), sock), Route::Plain);
        // $CARGO_HOME's only after every place Cargo may have run from.
        std::fs::write(home.join("config.toml"), "[build]\nrustc-wrapper = \"sccache\"\n").unwrap();
        assert_eq!(route(&[dep.clone(), repo.clone()], &home, None, none.clone(), sock), Route::Theirs("/abs/w".into()));
        std::fs::remove_file(home.join("config.toml")).unwrap();
        // The environment's, over every file; empty: no wrapper.
        assert_eq!(route(std::slice::from_ref(&repo), &home, Some("w2".into()), none.clone(), sock), Route::Theirs("w2".into()));
        assert_eq!(route(std::slice::from_ref(&repo), &home, Some("".into()), none, sock), Route::Plain);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn needs_the_server_and_no_forced_incremental() {
        let root = tmp("server");
        let sock = root.join("s.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        let home = root.join("cargo");
        assert_eq!(route(std::slice::from_ref(&root), &home, None, [None, None], Some(sock.as_os_str())), Route::Cache);
        assert_eq!(route(std::slice::from_ref(&root), &home, None, [Some("1".into()), None], Some(sock.as_os_str())), Route::Plain);
        assert_eq!(route(std::slice::from_ref(&root), &home, None, [None, Some("1".into())], Some(sock.as_os_str())), Route::Plain);
        assert_eq!(route(std::slice::from_ref(&root), &home, None, [Some("0".into()), None], Some(sock.as_os_str())), Route::Cache);
        // Marked stuck by a call before (see `cached`): passed by for a while, then tried again.
        let mark = root.join("sccache.hung");
        std::fs::write(&mark, b"").unwrap();
        assert_eq!(route(std::slice::from_ref(&root), &home, None, [None, None], Some(sock.as_os_str())), Route::NoServer);
        let old = std::time::SystemTime::now() - HUNG_FOR - std::time::Duration::from_secs(1);
        std::fs::File::options().write(true).open(&mark).unwrap().set_modified(old).unwrap();
        assert_eq!(route(std::slice::from_ref(&root), &home, None, [None, None], Some(sock.as_os_str())), Route::Cache);
        // Gone (dinod is asked for it again), or never there.
        assert_eq!(route(std::slice::from_ref(&root), &home, None, [None, None], Some(root.join("other.sock").as_os_str())), Route::NoServer);
        assert_eq!(route(std::slice::from_ref(&root), &home, None, [None, None], None), Route::Plain);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn sccache_failures_are_told_from_rustc_errors() {
        assert!(failed(b"sccache: error: Server startup failed: cache dir\nsccache: caused by: x\n"));
        assert!(failed(b"warning: x\nsccache: error: failed to execute compile\n"));
        assert!(!failed(b"error[E0308]: mismatched types\n  --> src/lib.rs:1:1\n"));
        assert!(!failed(b"{\"message\":\"sccache: error: in a string\"}\n"));
        assert!(!failed(b"sccache: warning: The server looks like it shut down unexpectedly, compiling locally instead\n"));
    }

    #[test]
    fn cached_runs_rustc_itself_when_sccache_fails() {
        let dir = tmp("cached");
        let fake = |name: &str, body: &str| {
            use std::os::unix::fs::PermissionsExt;
            let p = dir.join(name);
            std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        };
        let rustc: OsString = "/usr/bin/true".into();
        // sccache's own failure: none, so rustc runs instead.
        let broken = fake("sc-broken", "echo 'sccache: error: Server startup failed' >&2; exit 2");
        assert_eq!(cached(&broken, &rustc, &[]), None);
        // Ended by a signal. SIGKILL, not a crash signal: those make macOS write a crash report
        // (ReportCrash, spindump) on every test run, and `cached` reads any signal the same way.
        let crashed = fake("sc-crash", "kill -KILL $$");
        assert_eq!(cached(&crashed, &rustc, &[]), None);
        // Not there, or not a program.
        assert_eq!(cached(&dir.join("missing"), &rustc, &[]), None);
        std::fs::write(dir.join("garbage"), [0u8, 1, 2, 3]).unwrap();
        assert_eq!(cached(&dir.join("garbage"), &rustc, &[]), None);
        // A file that isn't a program, marked as one: sh is handed it, and gives up.
        let garbage = fake("garbage-x", "");
        std::fs::write(&garbage, b"garbage\0\x01\x02").unwrap();
        assert_eq!(cached(&garbage, &rustc, &[]), None);
        let gone = fake("sc-127", "exit 127");
        assert_eq!(cached(&gone, &rustc, &[]), None);
        // rustc's own error, and success, come back as they are.
        let rustc_error = fake("sc-err", "echo 'error[E0425]: cannot find value' >&2; exit 1");
        assert_eq!(cached(&rustc_error, &rustc, &[]), Some(1));
        let exit2 = fake("sc-exit2", "echo 'error: rustc said so' >&2; exit 2");
        assert_eq!(cached(&exit2, &rustc, &[]), Some(2), "exit 2 without sccache's own error is the compiler's");
        let ok = fake("sc-ok", "exit 0");
        assert_eq!(cached(&ok, &rustc, &[]), Some(0));
        // A server that stops answering mid-compile: the compile is ended, and rustc runs instead.
        let slow = fake("sc-slow", "sleep 30; exit 0");
        let ms = std::time::Duration::from_millis;
        let started = std::time::Instant::now();
        assert_eq!(cached_watched(&slow, &rustc, &[], ms(100), || false), None);
        assert!(started.elapsed() < ms(5000), "ended, not waited for");
        // One that answers: a long compile is waited for.
        let long = fake("sc-long", "sleep 1; exit 0");
        assert_eq!(cached_watched(&long, &rustc, &[], ms(100), || true), Some(0));
        let _ = std::fs::remove_dir_all(dir);
    }
}
