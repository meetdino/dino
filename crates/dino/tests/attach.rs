//! `dino attach --fresh`, as the app's panes, the quick terminal and tmux windows run it.

use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// A folder of its own, and whatever dinod got started in it stopped.
struct Dir(PathBuf);

impl Drop for Dir {
    fn drop(&mut self) {
        let sock = self.0.join("dino/dinod.sock");
        if let Ok(mut s) = UnixStream::connect(&sock) {
            let _ = dino_core::ipc::write_json(&mut s, &dino_core::ipc::Request::Shutdown);
            let _ = dino_core::ipc::read_frame(&mut s);
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

/// Ghostty runs a pane's command under login(1), which sets HOME to the user's own: a dinod a pane
/// started would run a second dino (a test's own HOME and DINO_HOME) in the user's home, and
/// resume its sessions there (#23). Starting dinod is for what made the pane.
#[test]
fn a_panes_attach_never_starts_dinod() {
    let dir = Dir(std::env::temp_dir().join(format!("dino-attach-test-{}", std::process::id())));
    let dino_home = dir.0.join("dino");
    std::fs::create_dir_all(dir.0.join("home")).unwrap();
    std::fs::create_dir_all(&dino_home).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_dino"))
        .args(["attach", "--fresh", "1"])
        .env_clear()
        .env("HOME", dir.0.join("home"))
        .env("DINO_HOME", &dino_home)
        .env("PATH", "/usr/bin:/bin")
        .env("TERM", "xterm-256color")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("dinod isn't running"), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(!dino_home.join("dinod.sock").exists(), "a pane's attach started dinod");
    assert!(!dino_home.join("dinod.log").exists(), "a pane's attach started dinod");
}
