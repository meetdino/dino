//! Talking to dinod: start it on demand, make requests, attach terminal streams.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dino_core::ipc::{self, Request, Response};

/// Connect to dinod, starting it in the background if it isn't running.
pub fn connect() -> io::Result<UnixStream> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
    let path = ipc::socket_path();
    if let Ok(s) = UnixStream::connect(&path) {
        return Ok(s);
    }
    // On a new Mac dino's folder isn't there yet: made here, private, before dinod's log goes in it.
    let dir = dino_core::config_dir();
    if !dir.exists() {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
    }
    // Private: what dinod logs names sessions, paths and errors.
    let log = std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(dir.join("dinod.log"))?;
    // `mode` is only for a new one: an older log may be open to others.
    let _ = log.set_permissions(std::fs::Permissions::from_mode(0o600));
    // Through launchd when there's a launch agent for it, so what dinod runs has the app's
    // permissions (see launchd.rs). launchd may wait up to 2 s to start it again (ThrottleInterval).
    #[cfg(target_os = "macos")]
    let asked = crate::launchd::start();
    // Through systemd when `dino service install` put a unit in.
    #[cfg(target_os = "linux")]
    let asked = crate::systemd::start();
    if asked {
        if let Some(s) = wait_for(&path, Duration::from_secs(15)) {
            return Ok(s);
        }
        // launchd couldn't run it (an app since deleted, say): as below, and the lock keeps a
        // late one from launchd from being a second dinod.
    }
    let mut cmd = Command::new(std::env::current_exe()?);
    cmd.arg("daemon").stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log);
    // Own session, so it outlives this terminal.
    unsafe {
        cmd.pre_exec(|| {
            libc_setsid();
            Ok(())
        });
    }
    cmd.spawn()?;
    wait_for(&path, Duration::from_secs(5)).map_or_else(|| UnixStream::connect(&path), Ok)
}

/// A connection to dinod once it's listening at `path`, within `wait`.
fn wait_for(path: &std::path::Path, wait: Duration) -> Option<UnixStream> {
    let deadline = Instant::now() + wait;
    loop {
        match UnixStream::connect(path) {
            Ok(s) => return Some(s),
            Err(_) if Instant::now() > deadline => return None,
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

fn libc_setsid() {
    unsafe extern "C" {
        fn setsid() -> i32;
    }
    unsafe {
        setsid();
    }
}

/// A long-lived control connection for repeated requests.
pub struct Control(UnixStream);

impl Control {
    pub fn open() -> io::Result<Self> {
        connect().map(Control)
    }

    /// Connect only if dinod is already running; never starts it.
    pub fn open_existing() -> io::Result<Self> {
        UnixStream::connect(ipc::socket_path()).map(Control)
    }

    pub fn request(&mut self, req: &Request) -> io::Result<Response> {
        ipc::write_json(&mut self.0, req)?;
        let (_, payload) = ipc::read_frame(&mut self.0)?;
        serde_json::from_slice(&payload).map_err(io::Error::other)
    }
}

pub fn request(req: &Request) -> io::Result<Response> {
    Control::open()?.request(req)
}

/// The dinod `dino attach` attaches to. In a surface dino made for the session (`--fresh`: an app
/// pane, the quick terminal, a tmux window) only a running one, waited for as a reattach waits:
/// what made the surface starts dinod, with its own environment (the app's `dino ping`, launchd).
/// Ghostty runs a pane's command under login(1), which sets HOME to the user's own, so a dinod
/// started from a pane for a second dino (a test's own HOME and DINO_HOME) would resume its
/// sessions in the user's home.
fn attach_connection(fresh: bool) -> io::Result<UnixStream> {
    if !fresh {
        return connect();
    }
    wait_for(&ipc::socket_path(), Duration::from_secs(5)).ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "dinod isn't running"))
}

/// How much scrollback the terminal this runs in keeps, in bytes, when it says
/// (`DINO_SCROLLBACK_LIMIT`, Ghostty's `scrollback-limit` in a dino pane): the session's replay
/// brings that much rather than a short one.
fn scrollback() -> Option<u64> {
    std::env::var("DINO_SCROLLBACK_LIMIT").ok()?.parse().ok()
}

/// `wait`: if the session has ended, answer once it's resumed.
fn attach_on(mut s: UnixStream, id: &str, cols: u16, rows: u16, wait: bool) -> io::Result<UnixStream> {
    ipc::write_json(&mut s, &Request::Attach { id: id.into(), cols, rows, wait, scrollback: scrollback() })?;
    let (_, payload) = ipc::read_frame(&mut s)?;
    match serde_json::from_slice(&payload).map_err(io::Error::other)? {
        Response::Ok => Ok(s),
        Response::Error { message } => Err(io::Error::other(message)),
        other => Err(io::Error::other(format!("unexpected {other:?}"))),
    }
}

/// Whether terminal output draws any text: more than escape sequences, titles and blank lines.
fn visible(bytes: &[u8]) -> bool {
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            0x1b => {
                i += 1;
                match bytes.get(i) {
                    // CSI: parameters up to a final byte.
                    Some(b'[') => {
                        i += 1;
                        while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                            i += 1;
                        }
                    }
                    // OSC, DCS, APC…: up to BEL or ST.
                    Some(b']' | b'P' | b'_' | b'^') => {
                        while i < bytes.len() && bytes[i] != 0x07 && !(bytes[i] == b'\\' && bytes[i - 1] == 0x1b) {
                            i += 1;
                        }
                    }
                    _ => {}
                }
            }
            b if b.is_ascii_whitespace() || b.is_ascii_control() => {}
            _ => return true,
        }
        i += 1;
    }
    false
}

/// `dino attach <id>`: relay between this terminal and the session. When its program ends the
/// last screen stays up, and Enter resumes it in place; exits once the session is removed.
/// Meant to run inside a real terminal surface (Ghostty), which does all the rendering.
pub fn attach_raw(id: &str, fresh: bool) -> anyhow::Result<()> {
    // Size changes come as SIGWINCH, taken by a thread of its own below: blocked here, before any
    // other thread starts, so none of those gets it. A handler of its own keeps it from being
    // dropped as ignored.
    extern "C" fn winched(_: libc::c_int) {}
    let winch = unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGWINCH);
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
        libc::signal(libc::SIGWINCH, winched as extern "C" fn(libc::c_int) as libc::sighandler_t);
        set
    };
    let (cols, rows) = crossterm::terminal::size()?;
    let stream = attach_on(attach_connection(fresh)?, id, cols, rows, false)?;
    let mut reader = stream.try_clone()?;
    let writer = Arc::new(Mutex::new(stream));
    crossterm::terminal::enable_raw_mode()?;
    // In a dino pane (`--fresh`), what ran before attach (login(1)'s "Last login: …") isn't the
    // session's: start from a clean screen and scrollback, then its own replay. In someone's own
    // terminal, their scrollback stays.
    if fresh {
        let mut out = io::stdout();
        let _ = out.write_all(b"\x1b[H\x1b[2J\x1b[3J");
        let _ = out.flush();
    }
    // This terminal reports its focus (mode 1004) to dinod, not to the program: the session's size
    // follows the client the user is looking at, and dinod tells the program if it asked. Kept on
    // whatever the program turns off. The terminal answers at once with the focus it has.
    let mut out = io::stdout();
    out.write_all(FOCUS_ON)?;
    out.flush()?;
    // As this terminal last reported it: 0 not yet, 1 lost, 2 gained. Told again after a reattach.
    let focus = Arc::new(AtomicU8::new(0));
    // The session's program has ended: keys don't go to it, Enter resumes it.
    let ended = Arc::new(AtomicBool::new(false));

    let (w, end, sid, fo) = (writer.clone(), ended.clone(), id.to_string(), focus.clone());
    std::thread::spawn(move || {
        let mut stdin = io::stdin();
        let mut buf = [0u8; 8192];
        loop {
            let n = match stdin.read(&mut buf) {
                Ok(n) => n,
                // A signal isn't the end of input; stopping here would leave a pane that ignores keys.
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            };
            if n == 0 {
                break;
            }
            let (keys, focused) = take_focus(&buf[..n]);
            if let Some(on) = focused {
                fo.store(if on { 2 } else { 1 }, Ordering::Relaxed);
                let _ = ipc::write_frame(&mut *w.lock().unwrap(), ipc::FOCUS, &[on as u8]);
            }
            if keys.is_empty() {
                continue;
            }
            if end.load(Ordering::Relaxed) {
                if keys.contains(&b'\r') && end.swap(false, Ordering::Relaxed) {
                    // The reader below is already waiting for it to come back.
                    if let Ok(Response::Error { message }) = attach_connection(fresh).and_then(|s| Control(s).request(&Request::Resume { id: sid.clone() })) {
                        end.store(true, Ordering::Relaxed);
                        let _ = write!(io::stdout(), "\r\n\x1b[2m{message}\x1b[0m\r\n");
                        let _ = io::stdout().flush();
                    }
                }
                continue;
            }
            // A failed write means the socket dropped; the reader below reconnects, so keep going.
            let _ = ipc::write_frame(&mut *w.lock().unwrap(), ipc::DATA, &keys);
        }
    });
    let w = writer.clone();
    std::thread::spawn(move || {
        // A run of size changes (a window dragged, the sidebar sliding in, a split moving) reaches
        // the program as its first and its settled size, not every frame between: an inline TUI
        // like Pi clears and repaints its whole screen on each one, which flickers. One change on
        // its own still goes at once.
        const SETTLE: Duration = Duration::from_millis(50);
        let mut last = (cols, rows);
        let mut sent = Instant::now().checked_sub(SETTLE).unwrap_or_else(Instant::now);
        loop {
            if let Ok(size) = crossterm::terminal::size() {
                if size != last {
                    last = size;
                    sent = Instant::now();
                    let _ = ipc::write_frame(&mut *w.lock().unwrap(), ipc::RESIZE, &ipc::resize_payload(size.0, size.1));
                }
            }
            let mut sig = 0;
            unsafe { libc::sigwait(&winch, &mut sig) };
            if sent.elapsed() < SETTLE {
                let mut seen = crossterm::terminal::size().ok();
                let mut still = Instant::now();
                while still.elapsed() < SETTLE {
                    std::thread::sleep(Duration::from_millis(10));
                    let now = crossterm::terminal::size().ok();
                    if now != seen {
                        seen = now;
                        still = Instant::now();
                    }
                }
            }
        }
    });

    let mut stdout = io::stdout();
    // After a reattach: what's arrived so far, held back (with the old screen left up) until the
    // agent has drawn something, so a restart doesn't blank the pane while it starts.
    let mut held: Option<(Vec<u8>, Instant)> = None;
    'session: loop {
        while let Ok((kind, payload)) = ipc::read_frame(&mut reader) {
            match kind {
                ipc::DATA => {
                    if let Some((buf, since)) = &mut held {
                        buf.extend_from_slice(&payload);
                        if !visible(buf) && since.elapsed() < Duration::from_secs(3) {
                            continue;
                        }
                        // The reattach replays the session's scrollback, so start from a clean screen.
                        stdout.write_all(b"\x1b[H\x1b[2J\x1b[3J")?;
                        stdout.write_all(buf)?;
                        held = None;
                    } else {
                        stdout.write_all(&payload)?;
                    }
                    if memchr::memmem::find(&payload, FOCUS_OFF).is_some() {
                        stdout.write_all(FOCUS_ON)?;
                    }
                    stdout.flush()?;
                }
                // Removed (killed, archived): nothing to come back to.
                ipc::EXIT if payload.is_empty() => break 'session,
                ipc::EXIT => {
                    // Kept: leave its last screen up, say so, and wait for it to be resumed.
                    ended.store(true, Ordering::Relaxed);
                    let note = String::from_utf8_lossy(&payload);
                    // What the program left on: mouse reporting, a hidden cursor, bracketed paste.
                    stdout.write_all(b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?2004l\x1b[?25h\x1b[<u\x1b[0m")?;
                    write!(stdout, "\r\n\x1b[2m── {note} ──\x1b[0m\r\n")?;
                    stdout.flush()?;
                    break;
                }
                _ => {}
            }
        }
        // The socket dropped but the session may live on (seen across sleep/wake). Reattach
        // rather than leave a pane that ignores keys; stop once dinod no longer knows the session.
        let mut started = Instant::now();
        let stream = loop {
            std::thread::sleep(Duration::from_millis(100));
            let (cols, rows) = crossterm::terminal::size().unwrap_or((cols, rows));
            // Ended: this blocks until it's resumed, by Enter here or from anywhere else.
            let wait = ended.load(Ordering::Relaxed);
            let tried = Instant::now();
            // Only a running dinod: if it's gone, so is the session, and starting one here would be a surprise.
            match UnixStream::connect(ipc::socket_path()).and_then(|s| attach_on(s, id, cols, rows, wait)) {
                Ok(s) => break s,
                Err(e) if e.kind() == io::ErrorKind::Other => break 'session,
                // Dropped after a long wait (dinod restarting): give it the full time again.
                Err(_) if tried.elapsed() > Duration::from_secs(1) => started = Instant::now(),
                Err(_) if started.elapsed() > Duration::from_secs(5) => break 'session,
                Err(_) => {}
            }
        };
        reader = stream.try_clone()?;
        *writer.lock().unwrap() = stream;
        if let f @ 1..=2 = focus.load(Ordering::Relaxed) {
            let _ = ipc::write_frame(&mut *writer.lock().unwrap(), ipc::FOCUS, &[(f == 2) as u8]);
        }
        ended.store(false, Ordering::Relaxed);
        held = Some((Vec::new(), Instant::now()));
    }
    crossterm::terminal::disable_raw_mode()?;
    // Leave the terminal usable: undo modes the app may have left on.
    stdout.write_all(b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?2004l\x1b[?1049l\x1b[?1004l\x1b[?25h\x1b[<u\x1b[0m\r\n")?;
    Ok(())
}

const FOCUS_ON: &[u8] = b"\x1b[?1004h";
const FOCUS_OFF: &[u8] = b"\x1b[?1004l";

/// Input from this terminal without its focus reports (`ESC [ I`, `ESC [ O`), and the focus the
/// last of them reported.
fn take_focus(input: &[u8]) -> (Vec<u8>, Option<bool>) {
    let mut keys = Vec::with_capacity(input.len());
    let mut focused = None;
    let mut i = 0;
    while i < input.len() {
        match input[i..] {
            [0x1b, b'[', f @ (b'I' | b'O'), ..] => {
                focused = Some(f == b'I');
                i += 3;
            }
            _ => {
                keys.push(input[i]);
                i += 1;
            }
        }
    }
    (keys, focused)
}

#[cfg(test)]
mod tests {
    use super::{take_focus, visible};

    #[test]
    fn focus_reports_are_taken_out_of_the_keys() {
        assert_eq!(take_focus(b"ab"), (b"ab".to_vec(), None));
        assert_eq!(take_focus(b"\x1b[I"), (vec![], Some(true)));
        assert_eq!(take_focus(b"a\x1b[Ob\x1b[Ic"), (b"abc".to_vec(), Some(true)));
        assert_eq!(take_focus(b"\x1b[I\x1b[O"), (vec![], Some(false)));
        // Keys that only look alike stay keys: arrows, Escape, SS3.
        assert_eq!(take_focus(b"\x1b[A\x1b\x1bOP\x1b["), (b"\x1b[A\x1b\x1bOP\x1b[".to_vec(), None));
    }

    #[test]
    fn only_drawn_text_is_visible() {
        assert!(!visible(b"\r\n\r\n\x1b[?2026h\x1b[H\x1b[2J\x1b[>4m\x1b]0;\xe2\x9c\xb3 Pelican\x07\x1b]2;x\x1b\\\x1b7\x1b8"));
        assert!(visible(b"\x1b[1m\xe2\x96\x90\xe2\x96\x9b Claude Code"));
        assert!(visible(b"\x1b[31m$ "));
    }
}
