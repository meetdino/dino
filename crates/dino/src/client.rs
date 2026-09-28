//! Talking to dinod: start it on demand, make requests, attach terminal streams.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dino_core::ipc::{self, Request, Response};
use dino_term::{Pane, Transport};

/// Connect to dinod, starting it in the background if it isn't running.
pub fn connect() -> io::Result<UnixStream> {
    let path = ipc::socket_path();
    if let Ok(s) = UnixStream::connect(&path) {
        return Ok(s);
    }
    let log = std::fs::OpenOptions::new().create(true).append(true).open(dino_core::config_dir().join("dinod.log"))?;
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
    let mut s = connect()?;
    ipc::write_json(&mut s, &Request::Attach { id: id.into(), cols, rows })?;
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

/// `dino attach <id>`: relay between this terminal and the session. Exits when the session ends.
/// Meant to run inside a real terminal surface (Ghostty), which does all the rendering.
pub fn attach_raw(id: &str) -> anyhow::Result<()> {
    let (cols, rows) = crossterm::terminal::size()?;
    let stream = start_attach(id, cols, rows)?;
    let mut reader = stream.try_clone()?;
    let writer = Arc::new(Mutex::new(stream));
    crossterm::terminal::enable_raw_mode()?;

    let w = writer.clone();
    std::thread::spawn(move || {
        let mut stdin = io::stdin();
        let mut buf = [0u8; 8192];
        while let Ok(n) = stdin.read(&mut buf) {
            if n == 0 || ipc::write_frame(&mut *w.lock().unwrap(), ipc::DATA, &buf[..n]).is_err() {
                break;
            }
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
                    if ipc::write_frame(&mut *w.lock().unwrap(), ipc::RESIZE, &ipc::resize_payload(size.0, size.1)).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let mut stdout = io::stdout();
    while let Ok((kind, payload)) = ipc::read_frame(&mut reader) {
        match kind {
            ipc::DATA => {
                stdout.write_all(&payload)?;
                stdout.flush()?;
            }
            ipc::EXIT => break,
            _ => {}
        }
    }
    crossterm::terminal::disable_raw_mode()?;
    // Leave the terminal usable: undo modes the app may have left on.
    stdout.write_all(b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?2004l\x1b[?1049l\x1b[?25h\x1b[<u\x1b[0m\r\n")?;
    Ok(())
}
