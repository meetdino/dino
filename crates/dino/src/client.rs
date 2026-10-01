//! Talking to dinod: start it on demand, make requests, attach terminal streams.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dino_core::ipc::{self, Request, Response};
use dino_term::{Pane, Transport};

/// Connect to dinod, starting it in the background if it isn't running.
pub fn connect() -> io::Result<UnixStream> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let path = ipc::socket_path();
    if let Ok(s) = UnixStream::connect(&path) {
        return Ok(s);
    }
    // Private: what dinod logs names sessions, paths and errors.
    let log = std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(dino_core::config_dir().join("dinod.log"))?;
    // `mode` is only for a new one: an older log may be open to others.
    let _ = log.set_permissions(std::fs::Permissions::from_mode(0o600));
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
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match UnixStream::connect(&path) {
            Ok(s) => return Ok(s),
            Err(e) if Instant::now() > deadline => return Err(e),
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

/// Keystrokes and resizes from a client pane, sent to dinod.
struct SocketTransport(Mutex<UnixStream>);

impl Transport for SocketTransport {
    fn write(&self, bytes: Vec<u8>) {
        let _ = ipc::write_frame(&mut *self.0.lock().unwrap(), ipc::DATA, &bytes);
    }
    fn resize(&self, cols: u16, rows: u16) {
        let _ = ipc::write_frame(&mut *self.0.lock().unwrap(), ipc::RESIZE, &ipc::resize_payload(cols, rows));
    }
}

fn start_attach(id: &str, cols: u16, rows: u16) -> io::Result<UnixStream> {
    attach_on(connect()?, id, cols, rows, false)
}

/// `wait`: if the session has ended, answer once it's resumed.
fn attach_on(mut s: UnixStream, id: &str, cols: u16, rows: u16, wait: bool) -> io::Result<UnixStream> {
    ipc::write_json(&mut s, &Request::Attach { id: id.into(), cols, rows, wait })?;
    let (_, payload) = ipc::read_frame(&mut s)?;
    match serde_json::from_slice(&payload).map_err(io::Error::other)? {
        Response::Ok => Ok(s),
        Response::Error { message } => Err(io::Error::other(message)),
        other => Err(io::Error::other(format!("unexpected {other:?}"))),
    }
}

/// A local emulator mirroring session `id`, kept live by a reader thread.
pub fn attach_pane(id: &str, cols: u16, rows: u16) -> io::Result<Arc<Pane>> {
    let stream = start_attach(id, cols, rows)?;
    let mut reader = stream.try_clone()?;
    let pane = Pane::remote(Arc::new(SocketTransport(Mutex::new(stream))), cols, rows);
    let weak = Arc::downgrade(&pane);
    std::thread::spawn(move || {
        while let Ok((kind, payload)) = ipc::read_frame(&mut reader) {
            let Some(pane) = weak.upgrade() else { return };
            match kind {
                ipc::DATA => pane.feed(&payload),
                ipc::EXIT => break,
                _ => {}
            }
        }
        if let Some(pane) = weak.upgrade() {
            pane.mark_exited();
        }
    });
    Ok(pane)
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
pub fn attach_raw(id: &str) -> anyhow::Result<()> {
    let (cols, rows) = crossterm::terminal::size()?;
    let stream = start_attach(id, cols, rows)?;
    let mut reader = stream.try_clone()?;
    let writer = Arc::new(Mutex::new(stream));
    crossterm::terminal::enable_raw_mode()?;
    // The session's program has ended: keys don't go to it, Enter resumes it.
    let ended = Arc::new(AtomicBool::new(false));

    let (w, end, sid) = (writer.clone(), ended.clone(), id.to_string());
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
            if end.load(Ordering::Relaxed) {
                if buf[..n].contains(&b'\r') && end.swap(false, Ordering::Relaxed) {
                    // The reader below is already waiting for it to come back.
                    if let Ok(Response::Error { message }) = request(&Request::Resume { id: sid.clone() }) {
                        end.store(true, Ordering::Relaxed);
                        let _ = write!(io::stdout(), "\r\n\x1b[2m{message}\x1b[0m\r\n");
                        let _ = io::stdout().flush();
                    }
                }
                continue;
            }
            // A failed write means the socket dropped; the reader below reconnects, so keep going.
            let _ = ipc::write_frame(&mut *w.lock().unwrap(), ipc::DATA, &buf[..n]);
        }
    });
    // Poll for size changes rather than wiring up SIGWINCH.
    let w = writer.clone();
    std::thread::spawn(move || {
        let mut last = (cols, rows);
        loop {
            std::thread::sleep(Duration::from_millis(200));
            if let Ok(size) = crossterm::terminal::size() {
                if size != last {
                    last = size;
                    let _ = ipc::write_frame(&mut *w.lock().unwrap(), ipc::RESIZE, &ipc::resize_payload(size.0, size.1));
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
        ended.store(false, Ordering::Relaxed);
        held = Some((Vec::new(), Instant::now()));
    }
    crossterm::terminal::disable_raw_mode()?;
    // Leave the terminal usable: undo modes the app may have left on.
    stdout.write_all(b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?2004l\x1b[?1049l\x1b[?25h\x1b[<u\x1b[0m\r\n")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::visible;

    #[test]
    fn only_drawn_text_is_visible() {
        assert!(!visible(b"\r\n\r\n\x1b[?2026h\x1b[H\x1b[2J\x1b[>4m\x1b]0;\xe2\x9c\xb3 Pelican\x07\x1b]2;x\x1b\\\x1b7\x1b8"));
        assert!(visible(b"\x1b[1m\xe2\x96\x90\xe2\x96\x9b Claude Code"));
        assert!(visible(b"\x1b[31m$ "));
    }
}
