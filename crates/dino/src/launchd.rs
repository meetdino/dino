//! Starting dinod through launchd. macOS privacy permissions (Screen Recording, Accessibility,
//! automation…) go to a process's responsible code. A dinod forked off whatever started it passed
//! that on to every shell and agent under it, so what they could do depended on who happened to
//! start dinod: the app, a terminal, an app since quit. Run by launchd from the launch agent the app
//! registers (SMAppService, app/Sources/Dino/LaunchAgent.swift), dinod and everything it starts are
//! the app's. With no registration yet, an installed app's agent goes in ~/Library/LaunchAgents,
//! naming the app (AssociatedBundleIdentifiers); with no app, dinod starts as it always did.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// What the app wrote in `dino_core::launchd_record()` about its launch agent.
struct Record {
    label: String,
    /// SMAppService's status: "enabled", "requiresApproval", "notRegistered", "notFound".
    state: String,
    app: PathBuf,
    bundle: String,
}

fn record() -> Option<Record> {
    let text = std::fs::read_to_string(dino_core::launchd_record()).ok()?;
    let get = |k: &str| text.lines().find_map(|l| l.strip_prefix(k)?.strip_prefix('=')).map(str::to_string);
    Some(Record { label: get("label")?, state: get("state").unwrap_or_default(), app: get("app")?.into(), bundle: get("bundle").unwrap_or_default() })
}

fn domain() -> String {
    // SAFETY: no arguments; it can't fail.
    format!("gui/{}", unsafe { libc::getuid() })
}

fn launchctl(args: &[&str]) -> bool {
    Command::new("/bin/launchctl").args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

fn loaded(label: &str) -> bool {
    launchctl(&["print", &format!("{}/{label}", domain())])
}

/// Ask launchd to start dinod, when there's a launch agent for it: true if it was asked. The
/// caller waits for the socket either way, and starts dinod itself if launchd's doesn't come.
pub fn start() -> bool {
    match record() {
        // Registered and allowed: the app's own agent. One the CLI installed before is retired.
        Some(r) if r.state == "enabled" && r.app.join("Contents/Info.plist").is_file() => {
            if !r.bundle.is_empty() {
                remove_legacy(&r.bundle);
            }
            kickstart(&r.label)
        }
        // Waiting for approval in System Settings → General → Login Items, or turned off there:
        // the user's call, not something to get around with another agent.
        Some(r) if r.state == "requiresApproval" && r.app.join("Contents/Info.plist").is_file() => false,
        // No app has registered one for this dino (or could): an installed app's, from
        // ~/Library/LaunchAgents.
        _ => app().is_some_and(|(app, bundle)| legacy(&app, &bundle)),
    }
}

/// The dino app this CLI belongs to: the one it's inside, else (for the user's own dino, not an
/// isolated `$DINO_HOME`) one in an Applications folder. Its path and bundle identifier.
fn app() -> Option<(PathBuf, String)> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    let inside = exe.to_str().and_then(|p| p.find(".app/Contents/Helpers/")).map(|i| PathBuf::from(&exe.to_str().unwrap_or_default()[..i + 4]));
    let candidates: Vec<PathBuf> = match inside {
        Some(app) => vec![app],
        None if std::env::var_os("DINO_HOME").is_none() => {
            let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
            vec!["/Applications/Dino.app".into(), home.join("Applications/Dino.app")]
        }
        None => vec![],
    };
    candidates.into_iter().find_map(|app| {
        // Only an app that carries dinod: that's what the agent runs.
        if !app.join("Contents/Helpers/dino").is_file() {
            return None;
        }
        let out = Command::new("/usr/libexec/PlistBuddy").args(["-c", "Print :CFBundleIdentifier"]).arg(app.join("Contents/Info.plist")).stderr(Stdio::null()).output().ok()?;
        let bundle = String::from_utf8(out.stdout).ok()?.trim().to_string();
        (out.status.success() && !bundle.is_empty()).then_some((app, bundle))
    })
}

/// The CLI's launch agent for `bundle`'s dinod: its own label, apart from the app's (which
/// SMAppService registers), and one per `$DINO_HOME`.
fn legacy_label(bundle: &str) -> String {
    format!("{bundle}.dinod-cli{}", home_suffix())
}

/// "" for the user's own dino, ".<8 hex>" naming an isolated `$DINO_HOME`.
fn home_suffix() -> String {
    if std::env::var_os("DINO_HOME").is_none() {
        return String::new();
    }
    // FNV-1a: stable across builds and Rust versions, unlike `DefaultHasher`.
    let h = dino_core::config_dir().as_os_str().as_encoded_bytes().iter().fold(0xcbf29ce484222325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x100000001b3));
    format!(".{:08x}", h as u32)
}

fn legacy_plist_path(label: &str) -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    home.join("Library/LaunchAgents").join(format!("{label}.plist"))
}

/// Install (or update) the CLI's launch agent for `app`'s dinod and start it.
fn legacy(app: &Path, bundle: &str) -> bool {
    let label = legacy_label(bundle);
    let path = legacy_plist_path(&label);
    let plist = legacy_plist(&label, &app.join("Contents/Helpers/dino"), bundle);
    if std::fs::read_to_string(&path).ok().as_deref() != Some(plist.as_str()) {
        // Not answering on the socket, so not serving anything: replaced by the new one.
        if loaded(&label) {
            launchctl(&["bootout", &format!("{}/{label}", domain())]);
        }
        if path.parent().is_none_or(|d| std::fs::create_dir_all(d).is_err()) || std::fs::write(&path, &plist).is_err() {
            return false;
        }
    }
    if !loaded(&label) && !launchctl(&["bootstrap", &domain(), &path.to_string_lossy()]) {
        // Turned off in Login Items, say: nothing left behind, and dinod starts as before.
        let _ = std::fs::remove_file(&path);
        return false;
    }
    kickstart(&label)
}

/// Start job `label` (already running: nothing to do). launchd's dinod gets this process's `PATH`,
/// as one it started itself would (the app passes the login shell's): launchd's own is
/// /usr/bin:/bin:/usr/sbin:/sbin.
fn kickstart(label: &str) -> bool {
    if let Some(path) = std::env::var_os("PATH") {
        use std::os::unix::fs::OpenOptionsExt;
        let file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(dino_core::launchd_path_file());
        let _ = file.and_then(|mut f| std::io::Write::write_all(&mut f, path.as_encoded_bytes()));
    }
    launchctl(&["kickstart", &format!("{}/{label}", domain())])
}

/// The CLI's agent, once the app has registered its own: unloaded (dinod isn't answering, so it
/// isn't running one) and deleted.
fn remove_legacy(bundle: &str) {
    let label = legacy_label(bundle);
    let path = legacy_plist_path(&label);
    if !path.exists() {
        return;
    }
    if loaded(&label) {
        launchctl(&["bootout", &format!("{}/{label}", domain())]);
    }
    let _ = std::fs::remove_file(path);
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// As scripts/release.sh writes the app's, but with an absolute `Program` (`BundleProgram` is
/// SMAppService's alone).
fn legacy_plist(label: &str, program: &Path, bundle: &str) -> String {
    let home = match std::env::var_os("DINO_HOME") {
        Some(_) => format!("\n        <key>DINO_HOME</key><string>{}</string>", xml(&dino_core::config_dir().to_string_lossy())),
        None => String::new(),
    };
    let (label, program, bundle) = (xml(label), xml(&program.to_string_lossy()), xml(bundle));
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>{label}</string>
    <key>Program</key><string>{program}</string>
    <key>ProgramArguments</key><array><string>dino</string><string>daemon</string></array>
    <key>AssociatedBundleIdentifiers</key><array><string>{bundle}</string></array>
    <key>EnvironmentVariables</key>
    <dict>
        <key>{env}</key><string>{label}</string>{home}
    </dict>
    <key>KeepAlive</key><dict><key>Crashed</key><true/></dict>
    <key>ThrottleInterval</key><integer>2</integer>
    <key>ProcessType</key><string>Interactive</string>
    <key>AbandonProcessGroup</key><true/>
</dict>
</plist>
"#,
        env = dino_core::LAUNCHD_ENV
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_plist_names_the_app_and_its_home() {
        let p = legacy_plist("dev.dino.app.dinod-cli", Path::new("/Applications/Dino & Co.app/Contents/Helpers/dino"), "dev.dino.app");
        assert!(p.contains("<key>Program</key><string>/Applications/Dino &amp; Co.app/Contents/Helpers/dino</string>"));
        assert!(p.contains("<key>AssociatedBundleIdentifiers</key><array><string>dev.dino.app</string></array>"));
        assert!(p.contains("<key>DINO_LAUNCHD</key><string>dev.dino.app.dinod-cli</string>"));
        // Tests run with a `$DINO_HOME`: the agent serves that one.
        assert_eq!(p.contains("<key>DINO_HOME</key>"), std::env::var_os("DINO_HOME").is_some());
        assert!(!p.contains("RunAtLoad"), "dinod starts when dino is used, not at login");
    }
}
