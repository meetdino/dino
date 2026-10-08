//! Claude Code's statusline, which is where it reports the context window (size and use). dino
//! wraps a statusline the user already has with `dino statusline`: it passes the report on to dino,
//! then runs their command and prints its output unchanged. Without one, nothing is wrapped: any
//! statusline, even an empty one, takes the place of Claude Code's "? for shortcuts" hint. A
//! Claude dino starts says where to report in its environment; one typed into a dino shell, in the
//! settings its shell gives it (`--settings`), whose hooks report there already.

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
    /// settings, then the user's, in Claude's config folder `config` (see `claude_config`).
    fn for_project(project: &Path, config: &Path) -> Self {
        Self::new(project, config, Path::new(crate::models::CLAUDE_MANAGED).parent().unwrap_or(Path::new("/")))
    }

    fn new(project: &Path, config: &Path, managed_dir: &Path) -> Self {
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
            config.join("settings.json"),
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

/// The user's own statusline command for `project`, for `dino statusline` to run: in the session
/// it reports for, whose config folder its environment says.
pub fn user_command(project: &Path) -> Option<String> {
    Layers::for_project(project, &crate::claude_config::home()).effective()?.0["command"].as_str().map(String::from)
}

/// The environment variable that tells `dino statusline` where to report: the hook URL holds the
/// proxy's secret, so it stays off the statusline's command line, which every user can read.
pub const HOOK_ENV: &str = "DINO_HOOK_URL";

/// The `statusLine` setting for a Claude session in `project`: the user's own, run through
/// `dino statusline` (which reports to `HOOK_ENV`), with their padding and refresh interval.
/// `config` is the session's own config folder, if it has one (see `claude_config`). `None` when
/// they have none, or when it is managed (a managed setting can't be overridden anyway).
pub fn wrapper(project: &Path, config: Option<&Path>, dino: &Path) -> Option<String> {
    let config = config.map_or_else(crate::claude_config::home, Path::to_path_buf);
    wrap(Layers::for_project(project, &config).effective(), dino, None)
}

/// `wrapper`, for a Claude typed into a dino shell, given `settings` (the file its shell gives it
/// with `--settings`): `dino statusline` reports where that file's hooks do. Its path is on the
/// statusline's command line; the URL, and the proxy's secret, stay in the file, which is the
/// user's alone.
pub fn shell_wrapper(project: &Path, config: Option<&Path>, dino: &Path, settings: &Path) -> Option<String> {
    let config = config.map_or_else(crate::claude_config::home, Path::to_path_buf);
    wrap(Layers::for_project(project, &config).effective(), dino, Some(settings))
}

fn wrap(effective: Option<(Value, bool)>, dino: &Path, settings: Option<&Path>) -> Option<String> {
    let (mut setting, managed) = effective?;
    if managed {
        return None;
    }
    let from = settings.map(|f| format!(" {HOOKS_FLAG} {}", shell_quote(&f.display().to_string()))).unwrap_or_default();
    setting["command"] = format!("{} statusline{from}", shell_quote(&dino.display().to_string())).into();
    Some(setting.to_string())
}

/// `dino statusline --hooks <file>`: report where the hooks in Claude settings file `file` do.
pub const HOOKS_FLAG: &str = "--hooks";

/// Where the HTTP hooks in Claude settings `json` report (all to one place, see `hook_settings`).
fn hooks_url(json: &str) -> Option<String> {
    let v: Value = serde_json::from_str(json).ok()?;
    v["hooks"].as_object()?.values().flat_map(|e| e.as_array().into_iter().flatten()).flat_map(|m| m["hooks"].as_array().into_iter().flatten()).find(|h| h["type"] == "http").and_then(|h| h["url"].as_str()).map(String::from)
}

/// `dino statusline [<hook_url> | --hooks <settings file>]`: pass what Claude Code gives the
/// statusline on to dino (at `hook_url`, where the file's hooks report, else `HOOK_ENV`), then run
/// the user's own statusline with the same input; what it prints and its exit status are the
/// statusline's. Failing to reach dino changes nothing the user sees.
pub fn run(args: &[String]) -> i32 {
    let given = match args {
        [flag, file, ..] if flag == HOOKS_FLAG => std::fs::read_to_string(file).ok().and_then(|j| hooks_url(&j)),
        [url, ..] => Some(url.clone()),
        [] => None,
    };
    let from_env = std::env::var(HOOK_ENV).ok().filter(|u| !u.is_empty());
    let hook_url = given.as_deref().or(from_env.as_deref());
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
        Layers::new(&d.join("project"), &d.join("home/.claude"), &d.join("managed")).effective()
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
        assert_eq!(wrap(effective(&d), dino, None), None, "nothing to wrap");
        write(&d, "home/.claude/settings.json", r#"{"statusLine":{"type":"command","command":"~/bin/sl.sh","padding":0,"refreshInterval":10}}"#);
        let wrapped: Value = serde_json::from_str(&wrap(effective(&d), dino, None).unwrap()).unwrap();
        // No URL: the one it reports to holds the proxy's secret, and comes from its environment.
        assert_eq!(wrapped["command"], r#"'/Applications/Dino app/dino' statusline"#);
        assert_eq!((wrapped["padding"].as_u64(), wrapped["refreshInterval"].as_u64(), wrapped["type"].as_str()), (Some(0), Some(10), Some("command")));
        // Typed into a dino shell: where to report is in the settings file its shell gives it.
        let typed: Value = serde_json::from_str(&wrap(effective(&d), dino, Some(Path::new("/x/run/7/claude-hooks.json"))).unwrap()).unwrap();
        assert_eq!(typed["command"], r#"'/Applications/Dino app/dino' statusline --hooks '/x/run/7/claude-hooks.json'"#);
        assert_eq!(typed["refreshInterval"].as_u64(), Some(10));
        write(&d, "managed/managed-settings.json", r#"{"statusLine":{"type":"command","command":"org.sh"}}"#);
        assert_eq!(wrap(effective(&d), dino, None), None, "a managed statusline can't be replaced");
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// The hooks a shell gives a Claude typed there say where its statusline reports.
    #[test]
    fn reports_where_the_shells_hooks_do() {
        let url = "http://127.0.0.1:4100/k/secret/s/7/hook";
        let settings = crate::claude_hook_settings(url, Some(r#"{"type":"command","command":"x"}"#.into()));
        assert_eq!(hooks_url(&settings).as_deref(), Some(url));
        assert_eq!(hooks_url(r#"{"hooks":{}}"#), None);
        assert_eq!(hooks_url("not json"), None);
    }

    /// A session with a config folder of its own (`CLAUDE_CONFIG_DIR`) has its user settings there.
    #[test]
    fn the_users_own_is_in_the_sessions_config_folder() {
        let d = dir("config");
        let dino = Path::new("/usr/local/bin/dino");
        let own = d.join("work-account");
        std::fs::create_dir_all(&own).unwrap();
        write(&d, "work-account/settings.json", r#"{"statusLine":{"type":"command","command":"work.sh","padding":1}}"#);
        assert!(wrapper(&d.join("project"), Some(&own), dino).is_some_and(|w| w.contains("statusline") && w.contains(r#""padding":1"#)));
        assert_eq!(wrapper(&d.join("project"), Some(&d.join("home/.claude")), dino), None, "not the other folder's");
        std::fs::remove_dir_all(&d).unwrap();
    }
}
