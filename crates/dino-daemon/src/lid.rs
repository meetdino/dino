//! Keeping agents running with the lid closed, when Settings → Power says so.
//!
//! dinod turns system sleep off (`pmset -a disablesleep 1`, through the sudoers rule
//! [`dino_core::power`] installs) only while [`power::step`] holds, and back on the moment it
//! doesn't: work done, unplugged, battery low, too hot, too long, setting off, dinod stopped. A
//! marker file says sleep is off because of this dinod. If dinod dies without turning it back
//! on, a small watchdog process that outlives it does; if the Mac restarts, a launch daemon does.
//! Sleep someone else turned off (a hand-run `sudo pmset`) is left alone.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use dino_core::ipc::PowerInfo;
use dino_core::power::{self, Inputs, Off};
use dino_core::settings::Settings;
use dino_proxy::Activity;

use crate::{Daemon, now_secs, schedule};

/// How often the battery and temperature are read, and only while it matters.
const POWER_EVERY: Duration = Duration::from_secs(15);
/// After turning sleep off failed (no permission yet), how long before trying again.
const RETRY: Duration = Duration::from_secs(60);

#[derive(Default)]
pub(crate) struct Lid {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// Sleep is off because of us, since then.
    held: Option<(Instant, u64)>,
    /// Sleep was already off, by someone else.
    external: bool,
    latched: Option<Off>,
    watchdog: Option<Child>,
    read_at: Option<Instant>,
    adapter: Option<bool>,
    battery: Option<u8>,
    thermal: bool,
    note: Option<(String, u64)>,
    error: Option<String>,
    failed_at: Option<Instant>,
}

impl Lid {
    pub(crate) fn holding(&self) -> bool {
        self.state.lock().unwrap().held.is_some()
    }

    pub(crate) fn info(&self) -> PowerInfo {
        let s = self.state.lock().unwrap();
        PowerInfo {
            holding: s.held.is_some(),
            since: s.held.map(|h| h.1),
            external: s.external,
            note: s.note.as_ref().map(|n| n.0.clone()),
            note_at: s.note.as_ref().map(|n| n.1),
            ready: None,
            error: s.error.clone(),
            awake: vec![],
        }
    }
}

impl Daemon {
    /// The lid, and who keeps the Mac awake.
    pub(crate) fn power_info(&self) -> PowerInfo {
        PowerInfo { awake: self.awake.holders(), ..self.lid.info() }
    }
}

fn marker() -> PathBuf {
    dino_core::config_dir().join("lid-awake")
}

/// `pmset`, or what `DINO_PMSET` names instead (tests): reading as the user, writing through sudo.
fn pmset(args: &[&str]) -> std::io::Result<std::process::Output> {
    let mut c = match std::env::var_os("DINO_PMSET") {
        Some(p) => Command::new(p),
        None if args.first() == Some(&"-g") => Command::new(power::PMSET),
        None => {
            let mut c = Command::new("/usr/bin/sudo");
            c.args(["-n", power::PMSET]);
            c
        }
    };
    c.args(args).stdin(Stdio::null()).output()
}

fn read(args: &[&str]) -> String {
    pmset(args).map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
}

fn set_sleep_disabled(off: bool) -> Result<(), String> {
    let out = pmset(&["-a", "disablesleep", if off { "1" } else { "0" }]).map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else if String::from_utf8_lossy(&out.stderr).contains("password") {
        Err("dino doesn't have permission to keep the lid awake yet: set it up in Settings → Power".into())
    } else {
        Err(format!("pmset: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// How many agents are working, or waiting on their own background work, an agent typed into a
/// dino shell among them; and whether any agent session is open.
fn agents(d: &Daemon) -> (usize, bool) {
    let sessions = d.sessions.lock().unwrap().clone();
    let mut working = 0;
    let mut open = false;
    for s in sessions.iter().filter(|s| !s.pane.is_exited()) {
        let st = crate::stats(d, s);
        if s.agent_id == "shell" {
            // Only an agent reporting from the shell says it's working.
            working += usize::from(st.activity == Some(Activity::Working));
            continue;
        }
        open = true;
        let (agents, commands) = st.waiting();
        if st.activity == Some(Activity::Working) || agents + commands > 0 || st.in_flight > 0 {
            working += 1;
        }
    }
    (working, open)
}

/// Once a second or two: turn sleep off or back on as the setting and the Mac say, and keep the
/// Mac from idle sleep while agents work (see [`crate::awake`]).
pub(crate) fn tick(d: &Daemon) {
    let settings = Settings::load();
    let lid = &settings.machine.lid;
    let quiet = !lid.enabled && {
        let s = d.lid.state.lock().unwrap();
        s.held.is_none() && s.latched.is_none() && !s.external
    };
    // Who's working is only worth asking when something uses it.
    let (working, open) = if quiet && !settings.machine.awake_while_working { (0, false) } else { agents(d) };
    let changed = {
        let mut s = d.lid.state.lock().unwrap();
        if quiet {
            s.error = None;
            false
        } else {
            let working = working > 0;
            let wanted = lid.enabled && if lid.when == dino_core::settings::LidWhen::Open { open } else { working };
            if (wanted || s.held.is_some()) && s.read_at.is_none_or(|t| t.elapsed() >= POWER_EVERY) {
                let (adapter, battery) = power::battery(&read(&["-g", "batt"]));
                s.adapter = adapter;
                s.battery = battery;
                s.thermal = power::thermal(&read(&["-g", "therm"]));
                s.read_at = Some(Instant::now());
            }
            let inputs = Inputs { working, open, on_adapter: s.adapter, battery: s.battery, thermal: s.thermal, held_for: s.held.map(|h| h.0.elapsed()) };
            let mut latched = s.latched;
            let decision = power::step(lid, &inputs, &mut latched);
            s.latched = latched;
            match decision {
                Ok(()) if s.held.is_none() && !s.external => hold(&mut s),
                Err(off) if s.held.is_some() => {
                    release(&mut s, off.note());
                    true
                }
                Err(_) if s.external => {
                    s.external = false;
                    false
                }
                _ => false,
            }
        }
    };
    if changed {
        schedule::keep_awake(d);
    }
    crate::awake::tick(d, working, &settings);
}

/// Sleep off, unless it already was (someone else's): then leave it be.
fn hold(s: &mut State) -> bool {
    if s.failed_at.is_some_and(|t| t.elapsed() < RETRY) {
        return false;
    }
    if power::sleep_disabled(&read(&["-g"])) == Some(true) && !marker().exists() {
        s.external = true;
        return false;
    }
    match set_sleep_disabled(true) {
        Ok(()) => {
            let _ = std::fs::write(marker(), std::process::id().to_string());
            s.watchdog = spawn_watchdog();
            s.held = Some((Instant::now(), now_secs()));
            s.error = None;
            s.failed_at = None;
            true
        }
        Err(e) => {
            eprintln!("dinod: couldn't keep the lid awake: {e}");
            s.error = Some(e);
            s.failed_at = Some(Instant::now());
            false
        }
    }
}

/// Sleep back on, saying why when it's worth telling.
fn release(s: &mut State, note: Option<String>) {
    match set_sleep_disabled(false) {
        Ok(()) => s.error = None,
        Err(e) => {
            eprintln!("dinod: couldn't turn sleep back on: {e}");
            s.error = Some(e);
        }
    }
    let _ = std::fs::remove_file(marker());
    if let Some(mut w) = s.watchdog.take() {
        let _ = w.kill();
        let _ = w.wait();
    }
    s.held = None;
    if let Some(n) = note {
        s.note = Some((n, now_secs()));
    }
}

/// On start: sleep off from a dinod that died is turned back on (the next tick turns it off
/// again if it should be). Then the ticks.
pub(crate) fn start(d: std::sync::Arc<Daemon>) {
    if let Ok(pid) = std::fs::read_to_string(marker()) {
        let mut s = d.lid.state.lock().unwrap();
        release(&mut s, None);
        eprintln!("dinod: sleep was still off from dinod {}; turned it back on", pid.trim());
    }
    std::thread::spawn(move || {
        loop {
            tick(&d);
            std::thread::sleep(Duration::from_secs(2));
        }
    });
}

/// dinod is stopping: sleep back on.
pub(crate) fn stop(d: &Daemon) {
    let mut s = d.lid.state.lock().unwrap();
    if s.held.is_some() {
        release(&mut s, None);
    }
}

/// `dino lid-watchdog <pid>`: started while sleep is off, outlives dinod. When dinod exits, by
/// any means, and the marker still names it, turn sleep back on.
fn spawn_watchdog() -> Option<Child> {
    let exe = std::env::current_exe().ok()?;
    let mut c = Command::new(exe);
    c.args(["lid-watchdog", &std::process::id().to_string()]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    std::os::unix::process::CommandExt::process_group(&mut c, 0);
    c.spawn().inspect_err(|e| eprintln!("dinod: no lid watchdog: {e}")).ok()
}

pub fn watchdog(pid: i32) {
    wait_for_exit(pid);
    let ours = std::fs::read_to_string(marker()).is_ok_and(|m| m.trim() == pid.to_string());
    if ours && set_sleep_disabled(false).is_ok() {
        let _ = std::fs::remove_file(marker());
    }
}

/// Blocks until process `pid` exits (kqueue), or returns at once if it already has.
fn wait_for_exit(pid: i32) {
    // SAFETY: a kqueue of our own, one filter on `pid`, read into a local event.
    unsafe {
        let kq = libc::kqueue();
        if kq < 0 {
            while libc::kill(pid, 0) == 0 {
                std::thread::sleep(Duration::from_secs(1));
            }
            return;
        }
        let mut ev: libc::kevent = std::mem::zeroed();
        ev.ident = pid as usize;
        ev.filter = libc::EVFILT_PROC;
        ev.flags = libc::EV_ADD | libc::EV_ONESHOT;
        ev.fflags = libc::NOTE_EXIT;
        if libc::kevent(kq, &ev, 1, std::ptr::null_mut(), 0, std::ptr::null()) < 0 {
            // Already gone (ESRCH).
            libc::close(kq);
            return;
        }
        let mut out: libc::kevent = std::mem::zeroed();
        while libc::kevent(kq, std::ptr::null(), 0, &mut out, 1, std::ptr::null()) < 0 {}
        libc::close(kq);
    }
}

/// `Power { action }`: the state, and installing or removing the one-time permission.
pub(crate) fn serve(d: &Daemon, action: &str) -> Result<PowerInfo, String> {
    match action {
        "status" => {}
        "setup" | "remove" => {
            if action == "remove" {
                let mut s = d.lid.state.lock().unwrap();
                if s.held.is_some() {
                    release(&mut s, None);
                }
            }
            let user = user().ok_or("couldn't tell which user this is")?;
            if !power::valid_user(&user) {
                return Err(format!("“{user}” isn't a user name sudoers can take"));
            }
            run_as_admin(&power::setup_script(action == "setup", &user))?;
            d.lid.state.lock().unwrap().failed_at = None;
        }
        other => return Err(format!("unknown power action “{other}”")),
    }
    let mut info = d.power_info();
    info.ready = Some(ready());
    Ok(info)
}

/// The rule is in place: `sudo` would run `pmset -a disablesleep 1` without asking.
fn ready() -> bool {
    if std::env::var_os("DINO_PMSET").is_some() {
        return std::env::var_os("DINO_POWER_READY").is_some_and(|v| v == "1");
    }
    Command::new("/usr/bin/sudo")
        .args(["-n", "-l", power::PMSET, "-a", "disablesleep", "1"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Runs `script` as root after macOS asks for an administrator's password. `DINO_POWER_SETUP`
/// names a program to hand the script to instead (tests).
fn run_as_admin(script: &str) -> Result<(), String> {
    let out = match std::env::var_os("DINO_POWER_SETUP") {
        Some(p) => {
            let mut c = Command::new(p).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| e.to_string())?;
            std::io::Write::write_all(c.stdin.as_mut().ok_or("no stdin")?, script.as_bytes()).map_err(|e| e.to_string())?;
            drop(c.stdin.take());
            c.wait_with_output().map_err(|e| e.to_string())?
        }
        None => Command::new("/usr/bin/osascript").args(["-e", &power::osascript(script)]).stdin(Stdio::null()).output().map_err(|e| e.to_string())?,
    };
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(if err.contains("-128") { "Cancelled".into() } else { format!("Setting it up failed: {}", err.trim()) })
    }
}

fn user() -> Option<String> {
    // SAFETY: getpwuid returns a pointer into static storage, read at once.
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if pw.is_null() {
            return std::env::var("USER").ok();
        }
        Some(std::ffi::CStr::from_ptr((*pw).pw_name).to_string_lossy().into_owned())
    }
}
