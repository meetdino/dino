//! dinod as a systemd user service on Linux: launchd.rs's counterpart. `dino service install`
//! writes a unit to `$XDG_CONFIG_HOME/systemd/user` and enables it, so dinod starts with the
//! user's systemd (at boot, with lingering on) and runs the automations it schedules with nobody
//! logged in. Once the unit is there, a client starts dinod through systemd rather than forking
//! it. Without one, dinod starts as it does on macOS without the app: forked by the first `dino`
//! that needs it.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The unit's name: `dinod.service` for the user's own dino, `dinod-<8 hex>.service` for an
/// isolated `$DINO_HOME` (as launchd's label is).
fn unit_name() -> String {
    format!("dinod{}.service", home_suffix())
}

/// "" for the user's own dino, "-<8 hex>" naming an isolated `$DINO_HOME`.
fn home_suffix() -> String {
    if std::env::var_os("DINO_HOME").is_none() {
        return String::new();
    }
    // FNV-1a: stable across builds and Rust versions, unlike `DefaultHasher`.
    let h = dino_core::config_dir().as_os_str().as_encoded_bytes().iter().fold(0xcbf29ce484222325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x100000001b3));
    format!("-{:08x}", h as u32)
}

fn unit_dir() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    dino_core::xdg_dir("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config")).join("systemd/user")
}

fn unit_path() -> PathBuf {
    unit_dir().join(unit_name())
}

fn systemctl(args: &[&str]) -> bool {
    Command::new("systemctl").arg("--user").args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

/// Quote a value for a unit file's `Environment=` or `ExecStart=`: double quotes, with `\`, `"`,
/// `%` (a specifier) and `$` (a variable in ExecStart) escaped.
fn quoted(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '%' => out.push_str("%%"),
            '$' => out.push_str("$$"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The unit for `program`'s dinod. `DINO_LAUNCHD` tells dinod a service manager started it, as
/// launchd's agent does: its output goes to dinod.log, and it takes the login shell's `PATH`.
/// `KillMode=process` (launchd's AbandonProcessGroup): systemd stops dinod alone, and dinod ends
/// its sessions as it always does (they resume when it's back), while what dino leaves running on
/// purpose (a tmux server a session started, say) isn't killed with it.
fn unit(name: &str, program: &Path) -> String {
    let home = match std::env::var_os("DINO_HOME") {
        Some(_) => format!("\nEnvironment=DINO_HOME={}", quoted(&dino_core::config_dir().to_string_lossy())),
        None => String::new(),
    };
    format!(
        "[Unit]
Description=dino's background service (dinod): agent sessions, hooks and automations
Documentation=https://github.com/meetdino/dino

[Service]
Type=simple
ExecStart={program} daemon
Environment={env}={name}{home}
Restart=on-failure
RestartSec=2
KillMode=process

[Install]
WantedBy=default.target
",
        program = quoted(&program.to_string_lossy()),
        env = dino_core::LAUNCHD_ENV,
    )
}

/// Ask systemd to start dinod, when its unit is installed: true if it was asked. The caller waits
/// for the socket either way, and starts dinod itself if systemd's doesn't come.
pub fn start() -> bool {
    unit_path().is_file() && systemctl(&["start", &unit_name()])
}

pub const USAGE: &str = "usage: dino service install|uninstall|status

Run dinod, dino's background service, as a systemd user service:
  install    write ~/.config/systemd/user/dinod.service, enable it and start it
  uninstall  stop it, disable it and remove the unit
  status     say whether it's installed and running

On a server, `loginctl enable-linger $USER` keeps it running while you're logged out.";

/// `dino service install|uninstall|status`.
pub fn run(args: &[String]) -> anyhow::Result<()> {
    let name = unit_name();
    let path = unit_path();
    match args.first().map(String::as_str) {
        Some("install") => {
            let exe = std::env::current_exe()?.canonicalize()?;
            let text = unit(&name, &exe);
            std::fs::create_dir_all(unit_dir())?;
            let changed = std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str());
            if changed {
                std::fs::write(&path, &text)?;
            }
            anyhow::ensure!(systemctl(&["daemon-reload"]), "systemctl --user daemon-reload failed: is a systemd user session running? (`systemctl --user status`)");
            // A dinod already running outside systemd keeps its socket: stopped first, so the
            // service's takes over. Its sessions carry on and are picked up again.
            if !systemctl(&["is-active", "--quiet", &name]) && dino_core::ipc::socket_path().exists() && crate::client::Control::open_existing().is_ok() {
                let _ = crate::client::request(&dino_core::ipc::Request::Shutdown);
            }
            anyhow::ensure!(systemctl(&["enable", &name]), "systemctl --user enable {name} failed");
            let started = if changed { systemctl(&["restart", &name]) } else { systemctl(&["start", &name]) };
            anyhow::ensure!(started, "systemctl --user start {name} failed: see `journalctl --user -u {name}` and {}", dino_core::config_dir().join("dinod.log").display());
            println!("Installed {} and started it.", path.display());
            if !lingering() {
                println!("To keep it running while you're logged out (a server), run: loginctl enable-linger $USER");
            }
        }
        Some("uninstall") => {
            if !path.exists() {
                println!("dino's service isn't installed.");
                return Ok(());
            }
            systemctl(&["disable", "--now", &name]);
            std::fs::remove_file(&path)?;
            systemctl(&["daemon-reload"]);
            println!("Removed {}. dinod now starts when a dino command needs it.", path.display());
        }
        Some("status") | None => {
            let installed = path.is_file();
            let active = installed && systemctl(&["is-active", "--quiet", &name]);
            let rows = vec![
                ("Unit", if installed { path.display().to_string() } else { "not installed: `dino service install`".into() }),
                (
                    "Running",
                    if active {
                        "yes, under systemd".to_string()
                    } else if crate::client::Control::open_existing().is_ok() {
                        "yes, started by a dino command".into()
                    } else {
                        "no".into()
                    },
                ),
                ("Lingering", if lingering() { "on: runs while you're logged out".into() } else { "off: `loginctl enable-linger $USER` to run while logged out".into() }),
            ];
            print!("{}", crate::out::fields(&rows));
        }
        Some(_) => println!("{USAGE}"),
    }
    Ok(())
}

/// The user's systemd stays up while they're logged out (`loginctl enable-linger`).
fn lingering() -> bool {
    let user = std::env::var("USER").unwrap_or_default();
    Path::new("/var/lib/systemd/linger").join(user).exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_runs_dinod_with_its_home_and_leaves_agents_running() {
        let u = unit("dinod-1234abcd.service", Path::new("/opt/dino it's/50% \"x\"/dino"));
        assert!(u.contains("ExecStart=\"/opt/dino it's/50%% \\\"x\\\"/dino\" daemon\n"), "{u}");
        assert!(u.contains("Environment=DINO_LAUNCHD=dinod-1234abcd.service\n"));
        assert!(u.contains("KillMode=process\n") && u.contains("Restart=on-failure\n"));
        // Tests run with a `$DINO_HOME`: the unit serves that one, under a name of its own.
        assert_eq!(u.contains("Environment=DINO_HOME="), std::env::var_os("DINO_HOME").is_some());
        assert_eq!(unit_name().len() > "dinod.service".len(), std::env::var_os("DINO_HOME").is_some());
    }
}
