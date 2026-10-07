//! Help must be side-effect-free, even for commands that normally start or stop dinod.

use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const COMMANDS: &[&str] = &[
    "ls",
    "status",
    "new",
    "attach",
    "resume",
    "kill",
    "fork",
    "rm",
    "found",
    "continue",
    "stats",
    "automations",
    "automation",
    "login",
    "logout",
    "sync",
    "claude-token",
    "fallback",
    "power",
    "permissions",
    "build-cache",
    "init",
    "shell",
    "ai",
    "mcp",
    "ping",
    "stop",
    "daemon",
    "search",
    "version",
];

struct DinoHome {
    root: PathBuf,
    config: PathBuf,
}

impl DinoHome {
    fn new() -> Self {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = PathBuf::from("/tmp").join(format!("dino-help-test-{}-{nonce}", std::process::id()));
        let config = root.join("dino");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::create_dir_all(root.join("home")).unwrap();
        Self { root, config }
    }
}

impl Drop for DinoHome {
    fn drop(&mut self) {
        let sock = self.config.join(dino_core::ipc::SOCKET_NAME);
        if let Ok(mut stream) = UnixStream::connect(&sock) {
            let _ = dino_core::ipc::write_json(&mut stream, &dino_core::ipc::Request::Shutdown);
            let _ = dino_core::ipc::read_frame(&mut stream);
            for _ in 0..100 {
                if UnixStream::connect(&sock).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn run_dino(args: &[&str], home: &DinoHome) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_dino"))
        .args(args)
        .env_clear()
        .env("HOME", home.root.join("home"))
        .env("DINO_HOME", &home.config)
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let output = child.wait_with_output().unwrap();
            panic!("`dino {}` did not exit; stdout: {}; stderr: {}", args.join(" "), String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn every_command_help_exits_without_starting_or_stopping_dinod() {
    let home = DinoHome::new();
    for command in COMMANDS {
        for flag in ["-h", "--help"] {
            let output = run_dino(&[command, flag], &home);
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(output.status.success(), "`dino {command} {flag}` failed: {}", String::from_utf8_lossy(&output.stderr));
            assert!(!stdout.trim().is_empty(), "`dino {command} {flag}` printed no help");
            assert!(stdout.contains("dino"), "`dino {command} {flag}` printed: {stdout}");
            assert!(!home.config.join("dinod.sock").exists(), "`dino {command} {flag}` started or stopped dinod");
            assert!(!home.config.join("dinod.log").exists(), "`dino {command} {flag}` started dinod");
        }
    }

    let output = run_dino(&["--help"], &home);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Every command accepts `-h` or `--help`"));
    assert!(!home.config.join("dinod.sock").exists());
}

#[test]
fn stop_help_does_not_connect_to_an_existing_service() {
    let home = DinoHome::new();
    let socket = home.config.join(dino_core::ipc::SOCKET_NAME);
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let output = run_dino(&["stop", "--help"], &home);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(String::from_utf8_lossy(&output.stdout).contains("dino stop"));
    assert!(listener.accept().is_err(), "`dino stop --help` connected to the existing service");
}
