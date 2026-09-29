//! Dev servers started for a session's preview: hidden processes in the session's folder, stopped
//! with it. Their output is kept (the tail) for the app to show, and read for the port.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use dino_core::preview::{PreviewConfig, PreviewInfo, find_port};

/// Output kept per server.
const LOG_LIMIT: usize = 256 * 1024;

pub struct Server {
    pub session: String,
    pub config: PreviewConfig,
    /// Also its process group.
    pid: i32,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    log: String,
    port: Option<u16>,
    exit: Option<String>,
}

impl Server {
    pub fn start(session: &str, config: PreviewConfig) -> anyhow::Result<Arc<Server>> {
        let (program, args) = config.argv.split_first().ok_or_else(|| anyhow::anyhow!("{} has nothing to run", config.name))?;
        let mut cmd = Command::new(program);
        // Output goes to a pipe, not a terminal: Python would hold its "Serving on port …" in a
        // buffer, and the port would never show. The config can still say otherwise.
        cmd.env("PYTHONUNBUFFERED", "1");
        cmd.args(args).current_dir(&config.cwd).envs(&config.env).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        // Servers pick their port from PORT; say so when the config names one.
        if let Some(port) = config.port {
            cmd.env("PORT", port.to_string());
        }
        // Its own process group, so stopping it stops what it started (npm → vite → esbuild).
        cmd.process_group(0);
        let mut child = cmd.spawn().map_err(|e| anyhow::anyhow!("couldn't start {}: {e}", config.argv.join(" ")))?;
        let server = Arc::new(Server { session: session.into(), pid: child.id() as i32, state: Mutex::new(State { port: config.port, ..State::default() }), config });
        for out in [child.stdout.take().map(|o| Box::new(o) as Box<dyn Read + Send>), child.stderr.take().map(|e| Box::new(e) as Box<dyn Read + Send>)].into_iter().flatten() {
            let s = server.clone();
            std::thread::spawn(move || s.read(out));
        }
        let s = server.clone();
        std::thread::spawn(move || {
            let status = child.wait();
            let why = match status {
                Ok(st) if st.success() => "Exited".to_string(),
                Ok(st) => match st.code() {
                    Some(code) => format!("Exited with code {code}"),
                    None => "Stopped".to_string(),
                },
                Err(e) => e.to_string(),
            };
            s.state.lock().unwrap().exit = Some(why);
        });
        Ok(server)
    }

    fn read(&self, mut out: Box<dyn Read + Send>) {
        let mut buf = [0u8; 8192];
        while let Ok(n) = out.read(&mut buf) {
            if n == 0 {
                break;
            }
            let text = strip_ansi(&String::from_utf8_lossy(&buf[..n]));
            let mut st = self.state.lock().unwrap();
            if st.port.is_none() {
                // The address may straddle two reads: look at the tail too.
                let tail_from = st.log.len().saturating_sub(64);
                let tail_from = (tail_from..st.log.len()).find(|i| st.log.is_char_boundary(*i)).unwrap_or(st.log.len());
                st.port = find_port(&format!("{}{text}", &st.log[tail_from..]));
            }
            st.log.push_str(&text);
            if st.log.len() > LOG_LIMIT {
                let cut = st.log.len() - LOG_LIMIT / 2;
                let cut = (cut..st.log.len()).find(|i| st.log.is_char_boundary(*i)).unwrap_or(0);
                st.log.drain(..cut);
            }
        }
    }

    pub fn running(&self) -> bool {
        self.state.lock().unwrap().exit.is_none()
    }

    pub fn stop(&self) {
        if self.running() {
            // SAFETY: signals the group this server leads; it's ours, and still running.
            unsafe { libc::kill(-self.pid, libc::SIGTERM) };
        }
    }

    pub fn log(&self) -> String {
        self.state.lock().unwrap().log.clone()
    }

    pub fn info(&self) -> PreviewInfo {
        let st = self.state.lock().unwrap();
        let url = st.port.map(|p| match &self.config.url {
            Some(u) if u.contains("://") => u.clone(),
            Some(u) => format!("http://localhost:{p}/{}", u.trim_start_matches('/')),
            None => format!("http://localhost:{p}/"),
        });
        PreviewInfo { name: self.config.name.clone(), running: st.exit.is_none(), url, exit: st.exit.clone() }
    }
}

/// Terminal colors and cursor moves out of server output, which the app shows as plain text.
pub(crate) fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.next() {
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    while let Some(c) = chars.next() {
                        if c == '\u{7}' || (c == '\u{1b}' && chars.next_if_eq(&'\\').is_some()) {
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\r' if chars.peek() != Some(&'\n') => out.push('\n'),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_colors() {
        assert_eq!(strip_ansi("\u{1b}[32m  ➜\u{1b}[39m  Local: \u{1b}[36mhttp://localhost:\u{1b}[1m5173\u{1b}[22m/\u{1b}[39m"), "  ➜  Local: http://localhost:5173/");
        assert_eq!(strip_ansi("\u{1b}]8;;http://x\u{7}link\u{1b}]8;;\u{1b}\\"), "link");
    }

    #[test]
    fn runs_reads_port_and_stops() {
        let config = PreviewConfig {
            name: "t".into(),
            argv: vec!["sh".into(), "-c".into(), "echo 'listening on http://localhost:4321'; sleep 30".into()],
            cwd: std::env::temp_dir(),
            env: Default::default(),
            port: None,
            url: None,
            source: String::new(),
        };
        let s = Server::start("1", config).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while s.info().url.is_none() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(s.info().url.as_deref(), Some("http://localhost:4321/"));
        assert!(s.running());
        s.stop();
        while s.running() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(!s.running());
        assert!(s.log().contains("listening"));
    }
}
