//! `dino claude-token set`, then straight away `dino new claude` (#179): the session starts with
//! the token, not before dinod has found out Claude Code here isn't signed in on its own.

use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// A folder of its own, and whatever dinod got started in it stopped.
struct Dir(PathBuf);

impl Drop for Dir {
    fn drop(&mut self) {
        let sock = self.0.join("dino").join(dino_core::ipc::SOCKET_NAME);
        if let Ok(mut s) = UnixStream::connect(&sock) {
            let _ = dino_core::ipc::write_json(&mut s, &dino_core::ipc::Request::Shutdown);
            let _ = dino_core::ipc::read_frame(&mut s);
            for _ in 0..100 {
                if UnixStream::connect(&sock).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn dino(dir: &Path, args: &[&str], stdin: &str) -> Output {
    use std::io::Write;
    let path = format!("{}:/usr/bin:/bin", dir.join("bin").display());
    let mut child = Command::new(env!("CARGO_BIN_EXE_dino"))
        .args(args)
        .env_clear()
        .env("HOME", dir.join("home"))
        .env("DINO_HOME", dir.join("dino"))
        .env("CLAUDE_CONFIG_DIR", dir.join("home/.claude"))
        .env("PATH", path)
        .env("SHELL", "/bin/sh")
        .env("TERM", "xterm-256color")
        .current_dir(dir.join("home"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn the_first_session_after_setting_the_token_is_signed_in() {
    let dir = Dir(PathBuf::from("/tmp").join(format!("dino-token-test-{}", std::process::id())));
    for d in ["bin", "home/.claude", "dino"] {
        std::fs::create_dir_all(dir.0.join(d)).unwrap();
    }
    // A Claude Code that isn't signed in here, and takes a moment to say so (as the real one
    // does); as a session it notes whether it got the token. Anything else dinod asks it
    // (`--version`, `mcp add`) it answers at once.
    let seen = dir.0.join("seen");
    let claude = dir.0.join("bin/claude");
    std::fs::write(
        &claude,
        format!(
            "#!/bin/sh\ncase \"$*\" in\n\
             'auth status'*) sleep 1; echo '{{\"loggedIn\":false}}' ;;\n\
             *--session-id*) if [ -n \"$CLAUDE_CODE_OAUTH_TOKEN\" ]; then echo signed-in > '{s}'; else echo signed-out > '{s}'; fi; sleep 30 ;;\n\
             esac\n",
            s = seen.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();

    let token = format!("sk-ant-oat01-{}", "a".repeat(60));
    let set = dino(&dir.0, &["claude-token", "set"], &token);
    assert!(set.status.success(), "{}", String::from_utf8_lossy(&set.stderr));
    let new = dino(&dir.0, &["new", "claude"], "");
    assert!(new.status.success(), "{}", String::from_utf8_lossy(&new.stderr));

    let deadline = Instant::now() + Duration::from_secs(15);
    let got = loop {
        if let Ok(s) = std::fs::read_to_string(&seen) {
            break s;
        }
        assert!(Instant::now() < deadline, "the session's Claude Code never started");
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(got.trim(), "signed-in");
}
