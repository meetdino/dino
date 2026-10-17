//! `dino kill`, as an agent asked to close sessions runs it: the session goes to Archived, by its
//! name, to bring back on its conversation, rather than out of dino for good (`dino rm` deletes).

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use dino_core::ipc::{self, Request, Response};

/// A folder of its own, and whatever dinod got started in it stopped.
struct Dir(PathBuf);

impl Drop for Dir {
    fn drop(&mut self) {
        let sock = self.0.join("dino").join(ipc::SOCKET_NAME);
        if let Ok(mut s) = UnixStream::connect(&sock) {
            let _ = ipc::write_json(&mut s, &Request::Shutdown);
            let _ = ipc::read_frame(&mut s);
            for _ in 0..100 {
                if UnixStream::connect(&sock).is_err() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn dino(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_dino"))
        .args(args)
        .env_clear()
        .env("HOME", dir.join("home"))
        .env("ZDOTDIR", dir.join("home"))
        .env("HISTFILE", "/dev/null")
        .env("DINO_HOME", dir.join("dino"))
        .env("PATH", "/usr/bin:/bin")
        .env("SHELL", "/bin/sh")
        .env("TERM", "xterm-256color")
        .current_dir(dir.join("home"))
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn ask(dir: &Path, r: &Request) -> Response {
    let mut s = UnixStream::connect(dir.join("dino").join(ipc::SOCKET_NAME)).unwrap();
    ipc::write_json(&mut s, r).unwrap();
    let (_, body) = ipc::read_frame(&mut s).unwrap();
    serde_json::from_slice(&body).unwrap()
}

#[test]
fn a_session_killed_is_archived_by_its_name() {
    let dir = Dir(PathBuf::from("/tmp").join(format!("dino-kill-test-{}", std::process::id())));
    for d in ["home/Library", "dino"] {
        std::fs::create_dir_all(dir.0.join(d)).unwrap();
    }
    // Nothing started here registers with launchd.
    std::fs::write(dir.0.join("home/Library/LaunchAgents"), "").unwrap();
    let new = dino(&dir.0, &["new", "shell"]);
    assert!(new.status.success(), "{}", String::from_utf8_lossy(&new.stderr));
    let id = String::from_utf8_lossy(&new.stdout).trim().to_string();
    assert!(matches!(ask(&dir.0, &Request::Rename { id: id.clone(), name: "release notes".into() }), Response::Ok));

    let kill = dino(&dir.0, &["kill", &id]);
    assert!(kill.status.success(), "{}", String::from_utf8_lossy(&kill.stderr));

    let Response::State { sessions, .. } = ask(&dir.0, &Request::State) else { panic!("no state") };
    assert!(!sessions.iter().any(|s| s.id == id), "still running");
    let Response::Archived { sessions } = ask(&dir.0, &Request::Archived) else { panic!("no archived list") };
    let a = sessions.iter().find(|a| a.id == id).expect("not in Archived");
    assert_eq!(a.label.as_deref(), Some("release notes"));
}
