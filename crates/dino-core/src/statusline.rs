//! Claude Code's statusline, which is where it reports the context window (size and use). dino
//! wraps a statusline the user already has with `dino statusline`: it passes the report on to dino,
//! then runs their command and prints its output unchanged. Without one, nothing is wrapped: any
//! statusline, even an empty one, takes the place of Claude Code's "? for shortcuts" hint.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::Value;

/// Where a statusline setting came from, most specific first.
struct Layers {
    managed: Vec<PathBuf>,
    rest: Vec<PathBuf>,
}

impl Layers {
    /// Claude Code's own order: managed settings win, then the project's local and shared
    /// settings, then the user's.
    fn for_project(project: &Path) -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        Self::new(project, &home, Path::new("/Library/Application Support/ClaudeCode"))
    }

    fn new(project: &Path, home: &Path, managed_dir: &Path) -> Self {
        // `managed-settings.d/*.json` go on top of `managed-settings.json`, later names winning.
        let mut drop_ins: Vec<PathBuf> = std::fs::read_dir(managed_dir.join("managed-settings.d"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .collect();
        drop_ins.sort();
        drop_ins.reverse();
        drop_ins.push(managed_dir.join("managed-settings.json"));
        let rest = vec![
            project.join(".claude/settings.local.json"),
            project.join(".claude/settings.json"),
            home.join(".claude/settings.json"),
        ];
        Self { managed: drop_ins, rest }
    }

    /// The statusline setting in effect and whether it is managed, unless hooks are all off
    /// (which turns the statusline off too).
    fn effective(&self) -> Option<(Value, bool)> {
        let files: Vec<(Value, bool)> = self
            .managed
            .iter()
            .map(|p| (p, true))
            .chain(self.rest.iter().map(|p| (p, false)))
            .filter_map(|(p, managed)| Some((serde_json::from_str::<Value>(&std::fs::read_to_string(p).ok()?).ok()?, managed)))
            .collect();
        if files.iter().find_map(|(v, _)| v["disableAllHooks"].as_bool()) == Some(true) {
            return None;
        }
        files.into_iter().find_map(|(v, managed)| {
            let sl = &v["statusLine"];
            let command = sl["command"].as_str().is_some_and(|c| !c.trim().is_empty());
            (sl["type"] == "command" && command).then(|| (sl.clone(), managed))
        })
    }
}

/// The user's own statusline command for `project`, for `dino statusline` to run.
pub fn user_command(project: &Path) -> Option<String> {
    Layers::for_project(project).effective()?.0["command"].as_str().map(String::from)
}

/// The `statusLine` setting for a Claude session in `project`: the user's own, run through
/// `dino statusline <hook_url>`, with their padding and refresh interval. `None` when they have
/// none, or when it is managed (a managed setting can't be overridden anyway).
pub fn wrapper(project: &Path, dino: &Path, hook_url: &str) -> Option<String> {
    wrap(Layers::for_project(project).effective(), dino, hook_url)
}

fn wrap(effective: Option<(Value, bool)>, dino: &Path, hook_url: &str) -> Option<String> {
    let (mut setting, managed) = effective?;
    if managed {
        return None;
    }
    setting["command"] = format!("{} statusline {}", shell_quote(&dino.display().to_string()), shell_quote(hook_url)).into();
    Some(setting.to_string())
}

/// `dino statusline <hook_url>`: pass what Claude Code gives the statusline on to dino, then run
/// the user's own statusline with the same input; what it prints and its exit status are the
/// statusline's. Failing to reach dino changes nothing the user sees.
pub fn run(hook_url: Option<&str>) -> i32 {
    let mut input = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut input);
    let report = hook_url.map(|url| {
        let (url, body) = (url.to_string(), input.clone());
        std::thread::spawn(move || post(&url, &body))
    });
    let v: serde_json::Value = serde_json::from_slice(&input).unwrap_or_default();
    // The project whose settings Claude Code read: where it was started.
    let project = v["workspace"]["project_dir"].as_str().or(v["cwd"].as_str()).map(PathBuf::from).or_else(|| std::env::current_dir().ok());
    let status = project.and_then(|p| user_command(&p)).map_or(0, |command| run_command(&command, &input));
    if let Some(r) = report {
        let _ = r.join();
    }
    status
}

/// `command` in `sh`, `input` on its stdin, its output going straight to ours.
fn run_command(command: &str, input: &[u8]) -> i32 {
    let Ok(mut child) = Command::new("/bin/sh").arg("-c").arg(command).stdin(Stdio::piped()).spawn() else { return 1 };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input);
    }
    child.wait().ok().and_then(|s| s.code()).unwrap_or(1)
}

/// POST `body` to dino's hook URL (`http://127.0.0.1:<port>/…`), briefly: the statusline waits.
fn post(url: &str, body: &[u8]) -> Option<()> {
    let rest = url.strip_prefix("http://")?;
    let (host, path) = rest.split_at(rest.find('/')?);
    let addr = std::net::ToSocketAddrs::to_socket_addrs(host).ok()?.next()?;
    let mut stream = std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(200)).ok()?;
    stream.set_write_timeout(Some(Duration::from_millis(300))).ok()?;
    stream.set_read_timeout(Some(Duration::from_millis(300))).ok()?;
    let head = format!("POST {path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
    stream.write_all(head.as_bytes()).ok()?;
    stream.write_all(body).ok()?;
    // Wait for the answer so dinod has it before we exit; its content doesn't matter.
    let _ = stream.read(&mut [0; 64]);
    Some(())
}

/// `s` as one word for `sh`.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dino-statusline-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        for sub in ["project/.claude", "home/.claude", "managed/managed-settings.d"] {
            std::fs::create_dir_all(d.join(sub)).unwrap();
        }
        d
    }

    fn write(d: &Path, file: &str, json: &str) {
        std::fs::write(d.join(file), json).unwrap();
    }

    fn effective(d: &Path) -> Option<(Value, bool)> {
        Layers::new(&d.join("project"), &d.join("home"), &d.join("managed")).effective()
    }

    fn command(d: &Path) -> Option<String> {
        effective(d).map(|(v, _)| v["command"].as_str().unwrap().to_string())
    }

    #[test]
    fn resolves_like_claude_code() {
        let d = dir("order");
        assert_eq!(effective(&d), None, "no statusline anywhere");
        write(&d, "home/.claude/settings.json", r#"{"statusLine":{"type":"command","command":"user.sh","padding":2,"refreshInterval":5}}"#);
        assert_eq!(command(&d).as_deref(), Some("user.sh"));
        write(&d, "project/.claude/settings.json", r#"{"statusLine":{"type":"command","command":"shared.sh"}}"#);
        assert_eq!(command(&d).as_deref(), Some("shared.sh"));
        // A local file without a statusline, or with a broken one, doesn't hide the others.
        write(&d, "project/.claude/settings.local.json", r#"{"statusLine":{"type":"command","command":"  "},"model":"opus"}"#);
        assert_eq!(command(&d).as_deref(), Some("shared.sh"));
        write(&d, "project/.claude/settings.local.json", r#"{"statusLine":{"type":"command","command":"local.sh"}}"#);
        assert_eq!(command(&d).as_deref(), Some("local.sh"));
        assert!(!effective(&d).unwrap().1, "the project's own");
        write(&d, "managed/managed-settings.json", r#"{"statusLine":{"type":"command","command":"org.sh"}}"#);
        assert_eq!(effective(&d), Some((serde_json::json!({"type":"command","command":"org.sh"}), true)));
        write(&d, "managed/managed-settings.d/50-x.json", r#"{"statusLine":{"type":"command","command":"drop-in.sh"}}"#);
        assert_eq!(command(&d).as_deref(), Some("drop-in.sh"));
        write(&d, "home/.claude/settings.json", r#"{"disableAllHooks":true}"#);
        assert_eq!(effective(&d), None, "hooks off turns the statusline off");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn passes_the_report_on() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/s/abc/hook", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let mut got = Vec::new();
            let mut buf = [0; 1024];
            while !String::from_utf8_lossy(&got).ends_with("}") {
                let n = conn.read(&mut buf).unwrap();
                got.extend_from_slice(&buf[..n]);
            }
            conn.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n").unwrap();
            String::from_utf8(got).unwrap()
        });
        assert_eq!(post(&url, br#"{"context_window":{}}"#), Some(()));
        let request = server.join().unwrap();
        assert!(request.starts_with("POST /s/abc/hook HTTP/1.1\r\n") && request.contains("Content-Length: 21\r\n"), "{request}");
        assert!(request.ends_with("\r\n\r\n{\"context_window\":{}}"));
        // Nobody listening: give up quietly.
        assert_eq!(post("http://127.0.0.1:1/s/abc/hook", b"{}"), None);
        assert_eq!(post("not a url", b"{}"), None);
    }

    #[test]
    fn runs_the_users_command_with_the_same_input() {
        let d = dir("run");
        let out = d.join("out");
        let command = format!("cat > {}; exit 3", out.display());
        assert_eq!(run_command(&command, br#"{"model":{}}"#), 3);
        assert_eq!(std::fs::read_to_string(&out).unwrap(), r#"{"model":{}}"#);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn wraps_the_users_own_keeping_its_options() {
        let d = dir("wrap");
        let dino = Path::new("/Applications/Dino app/dino");
        assert_eq!(wrap(effective(&d), dino, "http://x"), None, "nothing to wrap");
        write(&d, "home/.claude/settings.json", r#"{"statusLine":{"type":"command","command":"~/bin/sl.sh","padding":0,"refreshInterval":10}}"#);
        let wrapped: Value = serde_json::from_str(&wrap(effective(&d), dino, "http://127.0.0.1:9/s/it's/hook").unwrap()).unwrap();
        assert_eq!(wrapped["command"], r#"'/Applications/Dino app/dino' statusline 'http://127.0.0.1:9/s/it'\''s/hook'"#);
        assert_eq!((wrapped["padding"].as_u64(), wrapped["refreshInterval"].as_u64(), wrapped["type"].as_str()), (Some(0), Some(10), Some("command")));
        write(&d, "managed/managed-settings.json", r#"{"statusLine":{"type":"command","command":"org.sh"}}"#);
        assert_eq!(wrap(effective(&d), dino, "http://x"), None, "a managed statusline can't be replaced");
        std::fs::remove_dir_all(&d).unwrap();
    }
}
