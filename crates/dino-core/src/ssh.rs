//! Sessions on other machines, over SSH. dinod runs `ssh -t host …` in the session's terminal,
//! so passwords, passphrases and host keys are answered right there, and the agent runs on the
//! remote machine in the folder asked for. Connections share one master per host (in dino's own
//! control directory), so a second session or a reconnect doesn't log in again.
//!
//! Claude's status hooks come back through a reverse tunnel: a port on the remote machine's
//! loopback forwards to dinod's hook-only listener, which takes a per-session token (see
//! `dino_proxy::Proxy::remote_hook_url`). API traffic goes straight from the remote machine;
//! dino's proxy (with its keys) is never reachable from there.

use std::path::{Path, PathBuf};

use crate::config_dir;

/// Hosts from `~/.ssh/config` (and what it includes) that name one machine: no wildcards.
pub fn config_hosts() -> Vec<String> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else { return vec![] };
    let mut out = vec![];
    read_config(&home.join(".ssh/config"), &home, 0, &mut out);
    out
}

fn read_config(path: &Path, home: &Path, depth: usize, out: &mut Vec<String>) {
    if depth > 4 {
        return;
    }
    let Ok(text) = std::fs::read_to_string(path) else { return };
    let (hosts, includes) = parse_config(&text);
    for h in hosts {
        if !out.contains(&h) {
            out.push(h);
        }
    }
    for inc in includes {
        for p in expand_include(&inc, home) {
            read_config(&p, home, depth + 1, out);
        }
    }
}

/// `Host` names without patterns, and `Include` arguments, in file order.
fn parse_config(text: &str) -> (Vec<String>, Vec<String>) {
    let (mut hosts, mut includes) = (vec![], vec![]);
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        // `Keyword value` or `Keyword=value`.
        let Some(split) = line.find(|c: char| c.is_whitespace() || c == '=') else { continue };
        let (key, rest) = line.split_at(split);
        let rest = rest.trim_start_matches(|c: char| c.is_whitespace() || c == '=');
        let words = rest.split_whitespace().map(|w| w.trim_matches('"').to_string());
        if key.eq_ignore_ascii_case("host") {
            hosts.extend(words.filter(|w| !w.is_empty() && !w.contains(['*', '?', '!'])));
        } else if key.eq_ignore_ascii_case("include") {
            includes.extend(words);
        }
    }
    (hosts, includes)
}

/// An `Include` argument's files: relative ones are in `~/.ssh`; `*` matches in the file name.
fn expand_include(arg: &str, home: &Path) -> Vec<PathBuf> {
    let path = match arg.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None if arg.starts_with('/') => PathBuf::from(arg),
        None => home.join(".ssh").join(arg),
    };
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if !name.contains('*') {
        return vec![path];
    }
    let Some(dir) = path.parent() else { return vec![] };
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| wildcard(&name, &e.file_name().to_string_lossy()))
        .map(|e| e.path())
        .collect();
    found.sort();
    found
}

fn wildcard(pattern: &str, name: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == name,
        Some((head, tail)) => {
            name.starts_with(head) && (0..=name.len() - head.len()).any(|i| name.is_char_boundary(head.len() + i) && wildcard(tail, &name[head.len() + i..]))
        }
    }
}

/// Where connection masters live; `%C` keeps each socket path short and unique per host.
pub fn control_dir() -> PathBuf {
    config_dir().join("ssh")
}

/// What to run on the remote machine.
pub enum Program<'a> {
    /// Claude Code with `session` as its conversation id; `resume` continues it if the remote
    /// machine has its transcript (a session that never got a prompt has none).
    Claude { session: &'a str, resume: bool },
    /// An agent by its command name.
    Agent { bin: &'a str, name: &'a str },
    /// The user's login shell there.
    Shell,
}

/// `ssh`'s arguments for a session on `host`. `tunnel` is (port on the remote loopback, local
/// port) for hooks; `command` comes from `remote_command`.
pub fn ssh_args(host: &str, tunnel: Option<(u16, u16)>, command: &str) -> Vec<String> {
    let mut args: Vec<String> = vec!["-t".into()];
    let opts = [
        "ControlMaster=auto".to_string(),
        format!("ControlPath={}/%C", control_dir().display()),
        "ControlPersist=10m".into(),
        "ServerAliveInterval=30".into(),
        "ServerAliveCountMax=4".into(),
        // Quiet about "Connection closed"; prompts and real errors still show.
        "LogLevel=ERROR".into(),
        // A taken port only costs the status hooks, never the session.
        "ExitOnForwardFailure=no".into(),
    ];
    for o in opts {
        args.extend(["-o".into(), o]);
    }
    if let Some((remote, local)) = tunnel {
        args.extend(["-R".into(), format!("127.0.0.1:{remote}:127.0.0.1:{local}")]);
    }
    args.extend(["--".into(), host.into(), command.into()]);
    args
}

/// The command line `ssh` sends: through the user's login shell (so its PATH is there, even if
/// it's fish), into `folder`, then `program` with `args`, or a plain message if either is missing.
pub fn remote_command(host: &str, folder: &str, program: &Program, args: &[String]) -> String {
    let say = |msg: String| format!("{{ printf '\\n%s\\n\\n' {}; exit", quote(&msg));
    let mut script = String::from("PATH=\"$HOME/.local/bin:$PATH\"\n");
    let shown = if folder.is_empty() { "~" } else { folder };
    script += &format!("cd {} 2>/dev/null || {} 1; }}\n", folder_expr(folder), say(format!("dino: {host} has no folder {shown}")));
    let exec = match program {
        Program::Shell => "exec \"${SHELL:-/bin/sh}\" -l".to_string(),
        Program::Claude { session, resume } => {
            script += &missing_check("claude", "Claude Code", host, &say);
            let s = quote(session);
            if *resume {
                script += &format!("if ls \"$HOME\"/.claude/projects/*/{s}.jsonl >/dev/null 2>&1; then set -- --resume {s}; else set -- --session-id {s}; fi\n");
            } else {
                script += &format!("set -- --session-id {s}\n");
            }
            "exec claude \"$@\"".to_string()
        }
        Program::Agent { bin, name } => {
            script += &missing_check(bin, name, host, &say);
            format!("exec {}", quote(bin))
        }
    };
    script += &exec;
    for a in args {
        script.push(' ');
        script += &quote(a);
    }
    // The login shell only sets things up; the script itself is plain sh.
    format!("exec \"$SHELL\" -lc {}", quote(&format!("exec /bin/sh -c {}", quote(&script))))
}

fn missing_check(bin: &str, name: &str, host: &str, say: &dyn Fn(String) -> String) -> String {
    let hint = if bin == "claude" { "\nInstall it there with:  curl -fsSL https://claude.ai/install.sh | bash" } else { "" };
    let msg = format!("dino: {name} isn't installed on {host} (no `{bin}` on its PATH).{hint}\nThen start the session again.");
    format!("command -v {} >/dev/null 2>&1 || {} 127; }}\n", quote(bin), say(msg))
}

/// `~` and `~/…` are the remote home, which only the remote shell knows.
fn folder_expr(folder: &str) -> String {
    match folder {
        "" | "~" => "\"$HOME\"".into(),
        f => match f.strip_prefix("~/") {
            Some(rest) => format!("\"$HOME\"/{}", quote(rest)),
            None => quote(f),
        },
    }
}

/// Single-quoted for sh (and fish, which reads `'\''` the same way).
pub fn quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:@,+%".contains(c)) {
        return s.into();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// A port on the remote machine's loopback for the hook tunnel. Random: others may be taken.
pub fn pick_port() -> u16 {
    let mut b = [0u8; 2];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = std::io::Read::read_exact(&mut f, &mut b);
    }
    20000 + u16::from_be_bytes(b) % 30000
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn hosts_skip_patterns_and_follow_includes() {
        let (hosts, inc) = parse_config("Host devbox gpu-1 *.corp\n  HostName 10.0.0.2\nHost=\"build\"\n# Host old\nHost !bad ?x\ninclude conf.d/*\n");
        assert_eq!(hosts, ["devbox", "gpu-1", "build"]);
        assert_eq!(inc, ["conf.d/*"]);
        assert!(wildcard("*.conf", "work.conf"));
        assert!(wildcard("*", "anything"));
        assert!(!wildcard("*.conf", "work.txt"));
    }

    #[test]
    fn quoting() {
        assert_eq!(quote("plain-arg"), "plain-arg");
        assert_eq!(quote("it's"), "'it'\\''s'");
        assert_eq!(quote(""), "''");
    }

    #[test]
    fn ssh_args_forward_hooks_on_loopback() {
        let a = ssh_args("devbox", Some((30001, 5555)), "cmd");
        assert_eq!(a[0], "-t");
        assert!(a.windows(2).any(|w| w == ["-R", "127.0.0.1:30001:127.0.0.1:5555"]));
        assert_eq!(a[a.len() - 3..], ["--", "devbox", "cmd"]);
    }

    /// Runs the command as sshd would: through the user's shell, here with a made-up home.
    fn run(cmd: &str, home: &Path) -> (i32, String) {
        let out = Command::new("/bin/sh").arg("-c").arg(cmd).env("HOME", home).env("SHELL", "/bin/sh").env("PATH", "/usr/bin:/bin").output().unwrap();
        (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned())
    }

    #[test]
    fn remote_command_runs_in_the_folder_or_says_why_not() {
        let home = std::env::temp_dir().join(format!("dino-ssh-test-{}", std::process::id()));
        let bin = home.join(".local/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(home.join("my proj")).unwrap();
        // A stand-in agent that prints where it runs and what it got.
        let fake = bin.join("claude");
        std::fs::write(&fake, "#!/bin/sh\necho \"cwd=$(pwd) args=$*\"\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

        let claude = Program::Claude { session: "abc", resume: true };
        let (code, out) = run(&remote_command("devbox", "~/my proj", &claude, &["--model".into(), "it's".into()]), &home);
        assert_eq!(code, 0, "{out}");
        assert!(out.contains("my proj args=--session-id abc --model it's"), "{out}");

        // With a transcript there, it resumes.
        std::fs::create_dir_all(home.join(".claude/projects/x")).unwrap();
        std::fs::write(home.join(".claude/projects/x/abc.jsonl"), "").unwrap();
        let (_, out) = run(&remote_command("devbox", "~/my proj", &claude, &[]), &home);
        assert!(out.contains("args=--resume abc"), "{out}");

        let (code, out) = run(&remote_command("devbox", "/no/such", &claude, &[]), &home);
        assert_eq!(code, 1);
        assert!(out.contains("devbox has no folder /no/such"), "{out}");

        let codex = Program::Agent { bin: "codex", name: "Codex" };
        let (code, out) = run(&remote_command("devbox", "", &codex, &[]), &home);
        assert_eq!(code, 127);
        assert!(out.contains("Codex isn't installed on devbox"), "{out}");
        std::fs::remove_dir_all(&home).unwrap();
    }
}
