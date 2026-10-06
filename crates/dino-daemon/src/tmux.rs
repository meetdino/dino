//! A real tmux running in one of dino's shells. tmux stays in charge: dino never puts its own
//! `tmux` on PATH or edits a config. It asks the tmux client's own server, with read-only format
//! queries, what the client is showing, so the tab can follow the active pane: its folder, what
//! runs in it, a name. Closing the tab detaches the client and leaves the server as it was.
//!
//! Every question to a server goes through [`ask`], which gives up after [`ANSWER`]: a server that
//! is stopped, stuck in its config or otherwise not answering never holds dinod up, and isn't asked
//! again for [`QUIET`].

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use dino_core::procinfo;

/// How long a server has to answer one command. tmux answers in milliseconds; one that hasn't by
/// then is stopped, busy in its config (a `run-shell`), or on a socket nothing serves.
pub const ANSWER: Duration = Duration::from_millis(1500);
/// How long a server that didn't answer is left alone before it's asked again.
const QUIET: Duration = Duration::from_secs(20);

/// Servers that didn't answer, by socket, and when.
static STUCK: Mutex<Option<HashMap<PathBuf, Instant>>> = Mutex::new(None);

/// `bin -S socket args`: its output when it succeeded in time. `None` when it failed, didn't
/// answer within [`ANSWER`] (it's stopped then, and the server is left alone for [`QUIET`]), or the
/// server is one that recently didn't.
pub fn ask(bin: &Path, socket: &Path, args: &[&str]) -> Option<String> {
    if stuck(socket) {
        return None;
    }
    let mut cmd = Command::new(bin);
    // `-S` names the server; TMUX (dinod started from inside tmux) would only add confusion.
    cmd.arg("-S").arg(socket).args(args).env_remove("TMUX");
    match timed(&mut cmd, ANSWER) {
        Some((ok, out)) => ok.then_some(out),
        None => {
            STUCK.lock().unwrap().get_or_insert_default().insert(socket.to_path_buf(), Instant::now());
            None
        }
    }
}

/// The server on `socket` didn't answer within the last [`QUIET`].
pub fn stuck(socket: &Path) -> bool {
    let mut stuck = STUCK.lock().unwrap();
    let map = stuck.get_or_insert_default();
    map.retain(|_, at| at.elapsed() < QUIET);
    map.contains_key(socket)
}

/// Run `cmd` (stdin empty, stderr dropped) for at most `most`: whether it succeeded and its output,
/// or `None` when it couldn't start or had to be stopped.
fn timed(cmd: &mut Command, most: Duration) -> Option<(bool, String)> {
    let deadline = Instant::now() + most;
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    // Read on the side, so a long answer can't fill the pipe while this waits.
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut out = vec![];
        let _ = stdout.read_to_end(&mut out);
        let _ = tx.send(out);
    });
    let out = rx.recv_timeout(most).ok();
    loop {
        match child.try_wait() {
            // Done, though something it started may still hold its output (none then).
            Ok(Some(status)) => return Some((status.success(), out.map(|o| String::from_utf8_lossy(&o).into_owned()).unwrap_or_default())),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            // Still running (never reaped, so the pid is still its own): stop it.
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// What a tmux client in the foreground of a dino shell is showing.
#[derive(Clone, Debug, PartialEq)]
pub struct View {
    /// The tmux the client runs, so the server is asked by its own version.
    pub bin: PathBuf,
    /// The server's socket.
    pub socket: PathBuf,
    /// The client's terminal, which names it to its server.
    pub tty: String,
    /// `session:window.pane` of the active pane.
    pub target: String,
    /// `session:window name`, for the tab.
    pub label: String,
    /// The active pane's folder, as tmux knows it: from its process, else (a process macOS won't
    /// let tmux look at, like `top`) from the pane's own OSC 7.
    pub path: Option<String>,
    /// Something other than a shell runs in the active pane.
    pub busy: bool,
}

/// Whether process `pid` is a tmux (in a shell's foreground: a client, attached or on its way).
pub fn is_tmux_process(pid: u32) -> bool {
    procinfo::name(pid).is_some_and(|n| is_tmux(&n))
}

/// The tmux and the server socket of `fg`, when it's a tmux client. Its arguments don't change, so
/// this is looked up once per client; [`view`] then asks the server.
pub fn client(fg: u32) -> Option<(PathBuf, PathBuf)> {
    let (bin, args) = command_of(fg)?;
    if bin.file_name()? != "tmux" {
        return None;
    }
    let socket = socket(&args)?;
    Some((bin, socket))
}

/// What tmux client `fg` (of `bin` on `socket`) shows now; `None` when its server can't be asked
/// (gone, or an older tmux without these formats).
pub fn view(fg: u32, bin: &Path, socket: &Path) -> Option<View> {
    // The server's answer for the client with this pid: what its session shows.
    const FMT: &str = "#{client_pid}\t#{client_tty}\t#{session_name}\t#{window_index}\t#{pane_index}\t#{window_name}\t#{?pane_current_path,#{pane_current_path},#{pane_path}}\t#{pane_current_command}";
    let text = ask(bin, socket, &["list-clients", "-F", FMT])?;
    let line = text.lines().find(|l| l.split('\t').next() == Some(&fg.to_string()))?;
    parse(line).map(|(tty, target, label, path, busy)| View { bin: bin.into(), socket: socket.into(), tty, target, label, path, busy })
}

/// One `list-clients` line: tty, target, label, folder and whether the pane is busy.
fn parse(line: &str) -> Option<(String, String, String, Option<String>, bool)> {
    let f: Vec<&str> = line.split('\t').collect();
    let [_, tty, session, window, pane, name, path, command] = f[..] else { return None };
    let path = (!path.is_empty()).then(|| std::fs::canonicalize(path).map_or(path.to_string(), |p| p.display().to_string()));
    let label = if versioned(name) { session.to_string() } else { format!("{session}:{name}") };
    Some((tty.into(), format!("{session}:{window}.{pane}"), label, path, !is_shell(command)))
}

/// A window tmux named after a program whose file is its version (Claude Code's is
/// `…/versions/2.1.288`): a name that says nothing.
fn versioned(name: &str) -> bool {
    !name.is_empty() && name.contains('.') && name.chars().all(|c| c.is_ascii_digit() || c == '.')
}

/// `session:window` with a window named only by a version number named by its agent instead.
fn named(label: &str, agent: &str) -> String {
    match label.split_once(':') {
        Some((session, window)) if versioned(window) => format!("{session}:{agent}"),
        _ => label.to_string(),
    }
}

/// A login shell or a plain one: the pane is at a prompt (or waiting on a job it put back).
fn is_shell(command: &str) -> bool {
    matches!(command.trim_start_matches('-'), "" | "sh" | "bash" | "zsh" | "fish" | "dash" | "ksh" | "tcsh" | "csh" | "nu" | "elvish" | "xonsh")
}

impl View {
    /// The window showing, as `session:index name`: how alerts say where they came from.
    pub fn window(&self) -> String {
        let window = self.target.split('.').next().unwrap_or(&self.target);
        let name = self.label.split_once(':').map_or("", |(_, n)| n);
        format!("{window} {name}")
    }
}

/// The windows of the client's session that rang their bell since someone last looked at them, as
/// `session:index name`. tmux passes every window's bell on to its client, but not which window.
pub fn rang(v: &View) -> Vec<String> {
    let session = v.target.split(':').next().unwrap_or_default();
    let Some(out) = ask(&v.bin, &v.socket, &["list-windows", "-t", session, "-F", "#{window_bell_flag}\t#{session_name}:#{window_index} #{window_name}"]) else { return vec![] };
    out.lines().filter_map(|l| l.strip_prefix("1\t")).map(String::from).collect()
}

/// Detach the client: its server and everything in it keep running.
pub fn detach(v: &View) {
    ask(&v.bin, &v.socket, &["detach-client", "-t", &v.tty]);
}

// ---- Agents started by hand in tmux panes (any server on this Mac, dino's or not) ----

/// One pane of a server, from `list-panes -a`.
#[derive(Clone, Debug, PartialEq)]
struct Pane {
    pid: u32,
    id: String,
    target: String,
    label: String,
    attached: bool,
}

/// A tmux server as last asked: its tmux, socket and panes. Asked again after [`FRESH`].
struct Server {
    bin: PathBuf,
    socket: PathBuf,
    panes: Vec<Pane>,
    at: std::time::Instant,
}

/// How long a server's pane list is reused: the sidebar asks every few seconds.
const FRESH: std::time::Duration = std::time::Duration::from_millis(2500);

/// Servers by their pid. Only ever read from: nothing here changes a server.
static SERVERS: std::sync::Mutex<Option<std::collections::HashMap<u32, Server>>> = std::sync::Mutex::new(None);

/// Fill in where each running agent that lives under a tmux server is: its pane. One look at the
/// process table and one `list-panes -a` per server (reused for [`FRESH`]); nothing at all when no
/// agent runs in tmux.
pub fn place(found: &mut [dino_core::found::FoundSession]) {
    let tmuxed: Vec<usize> = (0..found.len()).filter(|&i| found[i].terminal.as_deref() == Some("tmux") && found[i].pid.is_some()).collect();
    if tmuxed.is_empty() {
        return;
    }
    let table = procinfo::processes();
    let parent = |p: u32| table.get(&p).map(|pr| pr.parent);
    // Each agent's ancestors, and the nearest that is a tmux: the server its pane belongs to.
    let mut placed = vec![];
    for i in tmuxed {
        let mut chain = vec![found[i].pid.unwrap()];
        while let Some(pp) = parent(*chain.last().unwrap()).filter(|&pp| pp > 1 && chain.len() < 32) {
            chain.push(pp);
        }
        if let Some(&server) = chain.iter().find(|p| table.get(p).is_some_and(|pr| is_tmux(&pr.name))) {
            placed.push((i, chain, server));
        }
    }
    let wanted: std::collections::HashSet<u32> = placed.iter().map(|(_, _, s)| *s).collect();
    // Servers not asked lately are asked again, without holding the list: one may be slow to answer.
    let stale: Vec<(u32, Option<(PathBuf, PathBuf)>)> = {
        let mut servers = SERVERS.lock().unwrap();
        let servers = servers.get_or_insert_default();
        servers.retain(|pid, _| wanted.contains(pid));
        wanted.iter().filter(|p| servers.get(p).is_none_or(|s| s.at.elapsed() >= FRESH)).map(|p| (*p, servers.get(p).map(|s| (s.bin.clone(), s.socket.clone())))).collect()
    };
    for (server, known) in stale {
        let Some((bin, socket)) = known.or_else(|| locate(server)) else { continue };
        let panes = panes(&bin, &socket).unwrap_or_default();
        SERVERS.lock().unwrap().get_or_insert_default().insert(server, Server { bin, socket, panes, at: std::time::Instant::now() });
    }
    for (i, chain, server) in placed {
        let pane = {
            let servers = SERVERS.lock().unwrap();
            let Some(srv) = servers.as_ref().and_then(|s| s.get(&server)) else { continue };
            chain.iter().find_map(|c| srv.panes.iter().find(|p| p.pid == *c)).map(|p| (p.clone(), srv.bin.clone(), srv.socket.clone()))
        };
        let Some((p, bin, socket)) = pane else { continue };
        found[i].tmux = Some(dino_core::found::TmuxPlace {
            socket: socket.display().to_string(),
            pane: p.id.clone(),
            target: p.target.clone(),
            label: named(&p.label, &found[i].agent),
            attached: p.attached,
        });
        found[i].terminal = Some(format!("tmux {}", named(&p.label, &found[i].agent)));
        // An agent's own status says busy, not that it's asking: its dialog on screen does.
        if ["claude", "codex"].contains(&found[i].agent.as_str()) && capture(&bin, &socket, &p.id).is_some_and(|t| dino_core::found::asking(&found[i].agent, &t)) {
            found[i].status = Some("needs".into());
        }
    }
}

/// The tmux and socket of server `pid`: from the arguments it was started with, else (a socket
/// under another `TMUX_TMPDIR`) the socket in tmux's usual folders whose server is this one.
fn locate(pid: u32) -> Option<(PathBuf, PathBuf)> {
    let (bin, args) = command_of(pid)?;
    if let Some(s) = socket(&args).filter(|s| server_pid(&bin, s) == Some(pid)) {
        return Some((bin, s));
    }
    let uid = unsafe { libc::getuid() };
    let dirs = [std::env::var_os("TMUX_TMPDIR").map(PathBuf::from), std::env::var_os("TMPDIR").map(PathBuf::from), Some("/tmp".into()), Some("/private/tmp".into())];
    for dir in dirs.into_iter().flatten() {
        for e in std::fs::read_dir(dir.join(format!("tmux-{uid}"))).into_iter().flatten().flatten() {
            let s = e.path();
            if server_pid(&bin, &s) == Some(pid) {
                return Some((bin, s));
            }
        }
    }
    None
}

fn server_pid(bin: &Path, socket: &Path) -> Option<u32> {
    ask(bin, socket, &["display-message", "-p", "#{pid}"])?.trim().parse().ok()
}

fn panes(bin: &Path, socket: &Path) -> Option<Vec<Pane>> {
    const FMT: &str = "#{pane_pid}\t#{pane_id}\t#{session_name}:#{window_index}.#{pane_index}\t#{session_name}:#{window_name}\t#{session_attached}";
    Some(ask(bin, socket, &["list-panes", "-a", "-F", FMT])?.lines().filter_map(parse_pane).collect())
}

fn parse_pane(line: &str) -> Option<Pane> {
    let f: Vec<&str> = line.split('\t').collect();
    let [pid, id, target, label, attached] = f[..] else { return None };
    Some(Pane { pid: pid.parse().ok()?, id: id.into(), target: target.into(), label: label.into(), attached: attached.parse::<u32>().is_ok_and(|n| n > 0) })
}

/// What pane `pane` shows now, as text.
fn capture(bin: &Path, socket: &Path, pane: &str) -> Option<String> {
    ask(bin, socket, &["capture-pane", "-p", "-J", "-t", pane])
}

/// The tmux of the server on `socket`, as last seen by [`place`].
fn bin_for(socket: &str) -> Option<PathBuf> {
    SERVERS.lock().unwrap().as_ref()?.values().find(|s| s.socket == Path::new(socket)).map(|s| s.bin.clone())
}

/// What pane `pane` of the server on `socket` shows, for a look without taking it over.
pub fn screen(socket: &str, pane: &str) -> Option<String> {
    capture(&bin_for(socket)?, Path::new(socket), pane)
}

/// Bring pane `pane` to the front in a client attached to its server: the one on its session if
/// any, else the one used last (switched to that session). Returns that client's terminal, `None`
/// when nobody is attached (nothing changes then).
pub fn show(socket: &str, pane: &str) -> Option<String> {
    let bin = bin_for(socket)?;
    let tmux = |args: &[&str]| ask(&bin, Path::new(socket), args);
    // Sessions by id: a name can hold anything, `:` and `.` too, which a target would misread.
    let session = tmux(&["display-message", "-p", "-t", pane, "#{session_id}"])?.trim().to_string();
    let clients = tmux(&["list-clients", "-F", "#{client_activity}\t#{client_tty}\t#{session_id}"])?;
    let mut clients: Vec<(u64, String, String)> = clients
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            let [at, tty, s] = f[..] else { return None };
            Some((at.parse().unwrap_or(0), tty.to_string(), s.to_string()))
        })
        .collect();
    // On the pane's session first, then the most recently used.
    clients.sort_by_key(|(at, _, s)| (*s == session, *at));
    let (_, tty, on) = clients.pop()?;
    if on != session {
        tmux(&["switch-client", "-c", &tty, "-t", &session])?;
    }
    tmux(&["select-window", "-t", pane])?;
    tmux(&["select-pane", "-t", pane])?;
    Some(tty)
}

fn is_tmux(comm: &str) -> bool {
    comm.rsplit('/').next().is_some_and(|c| c == "tmux" || c.starts_with("tmux:"))
}

/// The executable and arguments of `pid`.
fn command_of(pid: u32) -> Option<(PathBuf, Vec<String>)> {
    let bin = PathBuf::from(procinfo::exe_of(pid)?);
    let args = procinfo::args_and_env(pid)?.0.into_iter().skip(1).collect();
    Some((bin, args))
}

/// The server's socket from the client's `-S path` or `-L name`, else tmux's default
/// (`$TMUX_TMPDIR`, else /tmp, `tmux-<uid>/default`).
fn socket(args: &[String]) -> Option<PathBuf> {
    let mut name = "default".to_string();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        // Flags come before the command; the first word that isn't one ends them.
        if !a.starts_with('-') {
            break;
        }
        let value = |i: usize| if a.len() > 2 { Some(a[2..].to_string()) } else { args.get(i + 1).cloned() };
        match &a[..a.len().min(2)] {
            "-S" => return value(i).map(PathBuf::from),
            "-L" => name = value(i)?,
            // Flags that take a value: skip it.
            "-c" | "-f" | "-T" if a.len() == 2 => i += 1,
            _ => {}
        }
        i += 1;
    }
    let dir = std::env::var_os("TMUX_TMPDIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"));
    let path = dir.join(format!("tmux-{}", unsafe { libc::getuid() })).join(name);
    path.exists().then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_that_hangs_is_stopped() {
        let t = Instant::now();
        assert_eq!(timed(Command::new("/bin/sleep").arg("10"), Duration::from_millis(200)), None);
        assert!(t.elapsed() < Duration::from_secs(2));
        assert_eq!(timed(Command::new("/bin/echo").arg("hi"), Duration::from_secs(5)), Some((true, "hi\n".into())));
        assert_eq!(timed(&mut Command::new("/usr/bin/false"), Duration::from_secs(5)), Some((false, String::new())));
        // More than a pipe holds: read while it runs, not after.
        let big = timed(Command::new("/bin/sh").args(["-c", "yes | head -c 300000"]), Duration::from_secs(5)).unwrap();
        assert_eq!(big.1.len(), 300000);
        assert_eq!(timed(&mut Command::new("/nonexistent/tmux"), Duration::from_secs(1)), None);
        // Done, with something it left behind still holding its output: not a command that hangs.
        let t = Instant::now();
        assert_eq!(timed(Command::new("/bin/sh").args(["-c", "sleep 3 & echo started"]), Duration::from_millis(300)), Some((true, String::new())));
        assert!(t.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn a_server_that_doesnt_answer_is_left_alone() {
        // A "tmux" whose server never answers, as a stopped one doesn't.
        let dir = std::env::temp_dir().join(format!("dino-tmux-stuck-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("tmux");
        std::fs::write(&bin, "#!/bin/sh\nexec sleep 30\n").unwrap();
        std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let socket = dir.join("default");
        let t = Instant::now();
        assert_eq!(ask(&bin, &socket, &["list-clients"]), None);
        assert!(t.elapsed() >= ANSWER && t.elapsed() < ANSWER + Duration::from_secs(1));
        assert!(stuck(&socket));
        // Not asked again for a while: an answer that won't come costs nothing more.
        let t = Instant::now();
        assert_eq!(ask(&bin, &socket, &["list-panes", "-a"]), None);
        assert!(t.elapsed() < Duration::from_millis(100));
        assert!(!stuck(&dir.join("other")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn finds_the_server_a_client_talks_to() {
        let s = |a: &[&str]| socket(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(s(&["-S", "/x/sock", "attach"]), Some(PathBuf::from("/x/sock")));
        assert_eq!(s(&["-S/x/sock"]), Some(PathBuf::from("/x/sock")));
        assert_eq!(s(&["-f", "/dev/null", "-S", "/y", "new"]), Some(PathBuf::from("/y")));
        // A `-S` after the command is the command's, not tmux's.
        assert_eq!(s(&["new", "-S", "/z"]).filter(|p| p == Path::new("/z")), None);
    }

    #[test]
    fn reads_a_process_command_line() {
        let mut child = Command::new("/bin/sleep").arg("30").spawn().unwrap();
        let got = command_of(child.id());
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(got, Some((PathBuf::from("/bin/sleep"), vec!["30".to_string()])));
        assert!(!is_tmux_process(std::process::id()));
    }

    #[test]
    fn reads_panes_and_dialogs() {
        let p = parse_pane("4242\t%3\tmain:1.0\tmain:claude\t1").unwrap();
        assert_eq!(p, Pane { pid: 4242, id: "%3".into(), target: "main:1.0".into(), label: "main:claude".into(), attached: true });
        assert!(!parse_pane("1\t%0\tw:0.0\tw:zsh\t0").unwrap().attached);
        assert!(parse_pane("garbage").is_none());
        assert!(is_tmux("/opt/homebrew/bin/tmux") && is_tmux("tmux") && !is_tmux("/bin/zsh") && !is_tmux("tmuxinator"));
    }

    #[test]
    fn reads_what_the_client_shows() {
        let (tty, target, label, path, busy) = parse("123\t/dev/ttys004\tmain\t2\t1\tvim\t/tmp\tvim").unwrap();
        assert_eq!((tty.as_str(), target.as_str(), label.as_str(), busy), ("/dev/ttys004", "main:2.1", "main:vim", true));
        // Claude Code's program file is its version: that says nothing as a window name.
        assert_eq!(parse("1\t/dev/ttys001\twork\t0\t0\t2.1.288\t/tmp\t2.1.288").unwrap().2, "work");
        assert_eq!(named("work:2.1.288", "claude"), "work:claude");
        assert_eq!(named("main:vim", "claude"), "main:vim");
        assert_eq!(path, std::fs::canonicalize("/tmp").ok().map(|p| p.display().to_string()));
        assert!(!parse("1\tt\tm\t0\t0\tzsh\t\t-zsh").unwrap().4);
        let v = View { bin: "tmux".into(), socket: "/s".into(), tty, target, label, path, busy };
        assert_eq!(v.window(), "main:2 vim");
        assert_eq!(parse("1\tt\tm\t0\t0\tzsh\t\t-zsh").unwrap().3, None);
    }
}
