//! Who keeps the Mac awake, and keeping it awake while agents work.
//!
//! macOS sleeps when idle unless some process holds a power assertion against it. Claude Code
//! starts `caffeinate` while it works; other agents don't, so their sessions could let the Mac
//! sleep mid-task. While `machine.awake_while_working` is on, dinod holds an assertion of its own
//! (`PreventUserIdleSystemSleep`, named for what it's for) whenever any agent is working, and
//! also, as it always has, for scheduled automations and while the lid is kept awake. The process
//! going away releases it, however dinod ends.
//!
//! dinod also reads every process's assertions from powerd, and says whose dino session each is
//! under. It reads them only when powerd says they changed (a notification, checked on the lid
//! tick dinod already runs), so an idle Mac costs nothing more.

use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use dino_core::ipc::AwakeHolder;
use dino_core::procinfo;

use crate::Daemon;

type CFRef = *const c_void;
type IOReturn = i32;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOPMCopyAssertionsByProcess(out: *mut CFRef) -> IOReturn;
    fn IOPMAssertionCreateWithName(kind: CFRef, level: u32, name: CFRef, id: *mut u32) -> IOReturn;
    fn IOPMAssertionSetProperty(id: u32, key: CFRef, value: CFRef) -> IOReturn;
    fn IOPMAssertionRelease(id: u32) -> IOReturn;
    /// Asks powerd to post `ANY_CHANGE` on every assertion change; `pmset -g assertionslog` does.
    fn IOPMAssertionNotify(name: *const c_char, request: i32) -> IOReturn;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringCreateWithCString(alloc: CFRef, s: *const c_char, encoding: u32) -> CFRef;
    fn CFStringGetCString(s: CFRef, buf: *mut c_char, size: isize, encoding: u32) -> u8;
    fn CFNumberGetValue(n: CFRef, kind: isize, out: *mut c_void) -> u8;
    fn CFDateGetAbsoluteTime(d: CFRef) -> f64;
    fn CFDictionaryGetCount(d: CFRef) -> isize;
    fn CFDictionaryGetKeysAndValues(d: CFRef, keys: *mut CFRef, values: *mut CFRef);
    fn CFDictionaryGetValue(d: CFRef, key: CFRef) -> CFRef;
    fn CFArrayGetCount(a: CFRef) -> isize;
    fn CFArrayGetValueAtIndex(a: CFRef, i: isize) -> CFRef;
    fn CFGetTypeID(cf: CFRef) -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFNumberGetTypeID() -> usize;
    fn CFDateGetTypeID() -> usize;
    fn CFArrayGetTypeID() -> usize;
    fn CFDictionaryGetTypeID() -> usize;
    fn CFRelease(cf: CFRef);
}

unsafe extern "C" {
    fn notify_register_check(name: *const c_char, token: *mut i32) -> u32;
    fn notify_check(token: i32, changed: *mut i32) -> u32;
}

const UTF8: u32 = 0x0800_0100;
const SINT64: isize = 4;
const LEVEL_ON: u32 = 255;
const NOTIFY_REGISTER: i32 = 1;
const ANY_CHANGE: &CStr = c"com.apple.system.powermanagement.assertions.anychange";
/// Seconds from 1970 to 2001, where CFDate counts from.
const CF_EPOCH: f64 = 978_307_200.0;
/// Without powerd's notification, how often the list is read instead.
const READ_EVERY: Duration = Duration::from_secs(30);

/// The assertion types that keep the Mac from sleeping when idle (a display kept on does too).
const KEEPS_AWAKE: [&str; 5] = ["PreventUserIdleSystemSleep", "PreventSystemSleep", "NoIdleSleepAssertion", "PreventUserIdleDisplaySleep", "NoDisplaySleepAssertion"];

#[derive(Default)]
pub(crate) struct Awake {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// dinod's own assertion and what it's named.
    ours: Option<(u32, String)>,
    /// Agents working, as of the last tick.
    working: usize,
    /// Automations are scheduled and `keep_awake` is on.
    scheduled: bool,
    /// powerd's change notification: `None` before the first look, `Some(None)` without one.
    token: Option<Option<i32>>,
    read_at: Option<Instant>,
    /// Read again at the next look: dinod's own changed.
    stale: bool,
    holders: Vec<AwakeHolder>,
}

/// What dinod's own assertion is named for, if it should hold one.
fn reason(working: usize, awake_while_working: bool, lid: bool, scheduled: bool) -> Option<String> {
    if working > 0 && awake_while_working {
        Some(format!("dino: {working} {} working", if working == 1 { "agent" } else { "agents" }))
    } else if lid {
        Some("dino: keeping agents running with the lid closed".into())
    } else if scheduled {
        Some("dino: automations scheduled".into())
    } else {
        None
    }
}

impl Awake {
    /// Every holder now, dinod's own first.
    pub(crate) fn holders(&self) -> Vec<AwakeHolder> {
        self.state.lock().unwrap().holders.clone()
    }

    pub(crate) fn set_scheduled(&self, on: bool) {
        self.state.lock().unwrap().scheduled = on;
    }
}

fn cfstring(s: &str) -> CFRef {
    let c = CString::new(s.replace('\0', "")).unwrap_or_default();
    // SAFETY: a NUL-terminated string, copied by CF.
    unsafe { CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), UTF8) }
}

/// Takes, renames or lets go of dinod's own assertion for what `working` and the settings say.
pub(crate) fn apply(d: &Daemon, settings: &dino_core::settings::Settings) {
    let lid = d.lid.holding();
    let mut s = d.awake.state.lock().unwrap();
    let want = reason(s.working, settings.machine.awake_while_working, lid, s.scheduled);
    match (&s.ours, want) {
        (Some((_, had)), Some(name)) if *had == name => {}
        (Some((id, _)), Some(name)) => {
            let id = *id;
            let (key, value) = (cfstring("AssertName"), cfstring(&name));
            // SAFETY: our own assertion id; both strings released after.
            unsafe {
                IOPMAssertionSetProperty(id, key, value);
                CFRelease(key);
                CFRelease(value);
            }
            s.ours = Some((id, name));
            s.stale = true;
        }
        (None, Some(name)) => {
            let (kind, label) = (cfstring("PreventUserIdleSystemSleep"), cfstring(&name));
            let mut id = 0u32;
            // SAFETY: as above; `id` is written on success.
            let r = unsafe { IOPMAssertionCreateWithName(kind, LEVEL_ON, label, &mut id) };
            unsafe {
                CFRelease(kind);
                CFRelease(label);
            }
            if r == 0 {
                s.ours = Some((id, name));
            } else {
                eprintln!("dinod: couldn't keep the Mac awake: IOReturn {r:#x}");
            }
            s.stale = true;
        }
        (Some((id, _)), None) => {
            // SAFETY: our own assertion, released once.
            unsafe { IOPMAssertionRelease(*id) };
            s.ours = None;
            s.stale = true;
        }
        (None, None) => {}
    }
}

/// On the lid tick: dinod's own assertion for `working` agents, then the holders if they changed.
pub(crate) fn tick(d: &Daemon, working: usize, settings: &dino_core::settings::Settings) {
    d.awake.state.lock().unwrap().working = working;
    apply(d, settings);
    let read = {
        let mut s = d.awake.state.lock().unwrap();
        let token = *s.token.get_or_insert_with(register);
        let notified = token.is_some_and(|t| {
            let mut changed = 0;
            // SAFETY: a token notify gave us; reads shared memory, no syscall.
            unsafe { notify_check(t, &mut changed) };
            changed != 0
        });
        let due = s.read_at.is_none() || token.is_none() && s.read_at.is_some_and(|t| t.elapsed() >= READ_EVERY);
        let read = notified || due || std::mem::take(&mut s.stale);
        if read {
            s.read_at = Some(Instant::now());
        }
        read
    };
    if read {
        let roots = session_roots(d);
        let holders = read_holders(&roots);
        d.awake.state.lock().unwrap().holders = holders;
    }
}

/// powerd's change notification, or `None` when it can't be had (then the list is read every
/// [`READ_EVERY`]).
fn register() -> Option<i32> {
    let mut token = 0;
    // SAFETY: a static NUL-terminated name and a local out-parameter.
    unsafe {
        if IOPMAssertionNotify(ANY_CHANGE.as_ptr(), NOTIFY_REGISTER) != 0 || notify_register_check(ANY_CHANGE.as_ptr(), &mut token) != 0 {
            eprintln!("dinod: no notification of power assertion changes; reading them every {}s", READ_EVERY.as_secs());
            return None;
        }
        // The first check always says changed.
        let mut changed = 0;
        notify_check(token, &mut changed);
    }
    Some(token)
}

/// dinod stopping: let the Mac sleep again (exiting would, too).
pub(crate) fn stop(d: &Daemon) {
    let mut s = d.awake.state.lock().unwrap();
    if let Some((id, _)) = s.ours.take() {
        // SAFETY: our own assertion, released once.
        unsafe { IOPMAssertionRelease(id) };
    }
}

/// Each session's own process, by pid, with the session's id.
fn session_roots(d: &Daemon) -> Vec<(u32, String)> {
    d.sessions.lock().unwrap().iter().filter(|s| !s.pane.is_exited()).filter_map(|s| Some((s.pane.pid()?, s.id.clone()))).collect()
}

/// The session `pid` runs under: the first of its ancestors that is a session's process.
fn session_of(pid: u32, roots: &[(u32, String)]) -> Option<String> {
    let mut at = pid;
    for _ in 0..64 {
        if let Some((_, id)) = roots.iter().find(|(p, _)| *p == at) {
            return Some(id.clone());
        }
        at = procinfo::parent_of(at).filter(|&p| p > 1 && p != at)?;
    }
    None
}

/// Part of macOS rather than something the user started.
fn is_system(exe: Option<&str>) -> bool {
    exe.is_some_and(|e| ["/System/", "/usr/libexec/", "/usr/sbin/", "/sbin/"].iter().any(|p| e.starts_with(p)))
}

/// What people call the process at `exe`: the app it's part of ("Google Chrome" for a helper
/// deep inside it), else its file name.
fn display_name(exe: Option<&str>, fallback: &str) -> String {
    if let Some(e) = exe {
        if let Some(app) = e.split('/').find(|c| c.ends_with(".app")) {
            return app.trim_end_matches(".app").to_string();
        }
        if let Some(file) = e.rsplit('/').next().filter(|f| !f.is_empty()) {
            return file.to_string();
        }
    }
    fallback.to_string()
}

/// One assertion, as powerd lists it.
#[derive(Debug, Clone, Default, PartialEq)]
struct Raw {
    pid: u32,
    process: String,
    on_behalf: Option<u32>,
    kind: String,
    name: String,
    level: i64,
    since: Option<u64>,
}

/// The holders that keep the Mac awake, from what powerd lists: dinod's own first, then by when
/// they started. `exe` and `session` look a pid up (the kernel, the sessions).
fn holders_of(raw: Vec<Raw>, me: u32, exe: impl Fn(u32) -> Option<String>, name: impl Fn(u32) -> Option<String>, session: impl Fn(u32) -> Option<String>) -> Vec<AwakeHolder> {
    let mut out: Vec<AwakeHolder> = raw
        .into_iter()
        .filter(|r| r.level == LEVEL_ON as i64 && KEEPS_AWAKE.contains(&r.kind.as_str()))
        .map(|r| {
            let own_exe = exe(r.pid);
            // A system process holding it for someone else (audio for a browser): that someone.
            let behalf = r.on_behalf.filter(|&p| p > 0 && p != r.pid && is_system(own_exe.as_deref()));
            let (pid, path) = match behalf {
                Some(p) => (p, exe(p)),
                None => (r.pid, own_exe),
            };
            let fallback = if behalf.is_some() { name(pid).unwrap_or_else(|| r.process.clone()) } else { r.process.clone() };
            let ours = r.pid == me;
            // Held for another process it isn't shown as (`caffeinate -w <pid>`): say whose.
            let for_whom = r.on_behalf.filter(|&p| p > 0 && p != pid).map(|p| format!("{}, for {} (pid {p})", r.name, display_name(exe(p).as_deref(), &name(p).unwrap_or_default())));
            AwakeHolder {
                pid,
                process: if ours { "dino".into() } else { display_name(path.as_deref(), &fallback) },
                name: for_whom.unwrap_or(r.name),
                kind: r.kind,
                since: r.since,
                // `caffeinate -w <pid>` holds it for that pid: either may be in a session.
                session: if ours { None } else { session(pid).or_else(|| r.on_behalf.and_then(&session)) },
                ours,
                system: !ours && is_system(path.as_deref()),
            }
        })
        .collect();
    out.sort_by_key(|h| (!h.ours, h.since.unwrap_or(u64::MAX), h.pid));
    out
}

fn read_holders(roots: &[(u32, String)]) -> Vec<AwakeHolder> {
    holders_of(read_raw(), std::process::id(), procinfo::exe_of, procinfo::name, |pid| session_of(pid, roots))
}

/// Every assertion from `IOPMCopyAssertionsByProcess`.
fn read_raw() -> Vec<Raw> {
    let mut dict: CFRef = std::ptr::null();
    // SAFETY: an out-parameter; we own what it returns and release it below.
    if unsafe { IOPMCopyAssertionsByProcess(&mut dict) } != 0 || dict.is_null() {
        return vec![];
    }
    let mut out = vec![];
    // SAFETY: every value is type-checked before it's read; nothing outlives `dict`.
    unsafe {
        if CFGetTypeID(dict) == CFDictionaryGetTypeID() {
            let n = CFDictionaryGetCount(dict).max(0) as usize;
            let mut keys = vec![std::ptr::null(); n];
            let mut values = vec![std::ptr::null(); n];
            CFDictionaryGetKeysAndValues(dict, keys.as_mut_ptr(), values.as_mut_ptr());
            for (k, list) in keys.into_iter().zip(values) {
                let Some(pid) = number(k) else { continue };
                if CFGetTypeID(list) != CFArrayGetTypeID() {
                    continue;
                }
                for i in 0..CFArrayGetCount(list) {
                    let a = CFArrayGetValueAtIndex(list, i);
                    if CFGetTypeID(a) != CFDictionaryGetTypeID() {
                        continue;
                    }
                    let get = |key: &str| {
                        let k = cfstring(key);
                        let v = CFDictionaryGetValue(a, k);
                        CFRelease(k);
                        v
                    };
                    out.push(Raw {
                        pid: pid as u32,
                        process: string(get("Process Name")).unwrap_or_default(),
                        on_behalf: number(get("AssertionOnBehalfOfPID")).map(|p| p as u32),
                        kind: string(get("AssertType")).unwrap_or_default(),
                        name: string(get("AssertName")).unwrap_or_default(),
                        level: number(get("AssertLevel")).unwrap_or(0),
                        since: date(get("AssertStartWhen")),
                    });
                }
            }
        }
        CFRelease(dict);
    }
    out
}

/// SAFETY (these three): `v` is null or a live CF object.
unsafe fn string(v: CFRef) -> Option<String> {
    if v.is_null() || unsafe { CFGetTypeID(v) != CFStringGetTypeID() } {
        return None;
    }
    let mut buf = vec![0 as c_char; 1024];
    (unsafe { CFStringGetCString(v, buf.as_mut_ptr(), buf.len() as isize, UTF8) } != 0).then(|| unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned())
}

unsafe fn number(v: CFRef) -> Option<i64> {
    if v.is_null() || unsafe { CFGetTypeID(v) != CFNumberGetTypeID() } {
        return None;
    }
    let mut n: i64 = 0;
    (unsafe { CFNumberGetValue(v, SINT64, &mut n as *mut i64 as *mut c_void) } != 0).then_some(n)
}

unsafe fn date(v: CFRef) -> Option<u64> {
    if v.is_null() || unsafe { CFGetTypeID(v) != CFDateGetTypeID() } {
        return None;
    }
    let t = unsafe { CFDateGetAbsoluteTime(v) } + CF_EPOCH;
    (t > 0.0).then_some(t as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(pid: u32, process: &str, kind: &str, name: &str, since: u64) -> Raw {
        Raw { pid, process: process.into(), on_behalf: None, kind: kind.into(), name: name.into(), level: 255, since: Some(since) }
    }

    #[test]
    fn dinod_names_its_assertion_for_what_its_for() {
        assert_eq!(reason(0, true, false, false), None);
        assert_eq!(reason(1, true, false, false).as_deref(), Some("dino: 1 agent working"));
        assert_eq!(reason(2, true, true, true).as_deref(), Some("dino: 2 agents working"));
        // Setting off: no assertion for working agents, but the lid and automations keep theirs.
        assert_eq!(reason(2, false, false, false), None);
        assert_eq!(reason(2, false, true, false).as_deref(), Some("dino: keeping agents running with the lid closed"));
        assert_eq!(reason(0, true, false, true).as_deref(), Some("dino: automations scheduled"));
    }

    #[test]
    fn holders_say_who_and_whose_session() {
        let exe = |pid: u32| {
            Some(
                match pid {
                    552 => "/System/Library/CoreServices/powerd.bundle/powerd",
                    90 => "/usr/sbin/coreaudiod",
                    91 => "/Applications/Google Chrome.app/Contents/Frameworks/Google Chrome Framework.framework/Helpers/Google Chrome Helper.app/Contents/MacOS/Google Chrome Helper",
                    7 => "/usr/bin/caffeinate",
                    8 => "/usr/bin/caffeinate",
                    10 => "/Applications/Dino.app/Contents/MacOS/dino",
                    _ => return None,
                }
                .to_string(),
            )
        };
        let session = |pid: u32| (pid == 7).then(|| "3".to_string());
        let mut audio = raw(90, "coreaudiod", "PreventUserIdleSystemSleep", "com.apple.audio.context", 50);
        audio.on_behalf = Some(91);
        let mut waits = raw(8, "caffeinate", "PreventUserIdleSystemSleep", "caffeinate command-line tool", 40);
        waits.on_behalf = Some(77);
        let mut off = raw(9, "x", "PreventUserIdleSystemSleep", "off", 1);
        off.level = 0;
        let list = vec![
            raw(552, "powerd", "PreventUserIdleSystemSleep", "Powerd - Prevent sleep while display is on", 10),
            raw(7, "caffeinate", "PreventUserIdleSystemSleep", "caffeinate command-line tool", 30),
            raw(608, "WindowServer", "UserIsActive", "tickle", 5),
            raw(10, "dino", "PreventUserIdleSystemSleep", "dino: 2 agents working", 60),
            audio,
            waits,
            off,
        ];
        let got = holders_of(list, 10, exe, |p| (p == 91).then(|| "Google Chrome He".into()).or((p == 77).then(|| "dino".into())), session);
        let brief: Vec<_> = got.iter().map(|h| (h.pid, h.process.as_str(), h.session.as_deref(), h.ours, h.system)).collect();
        assert_eq!(
            brief,
            [
                (10, "dino", None, true, false),
                (552, "powerd", None, false, true),
                (7, "caffeinate", Some("3"), false, false),
                // `caffeinate -w 77`: shown as itself, in no session.
                (8, "caffeinate", None, false, false),
                // coreaudiod for a Chrome tab playing: Chrome.
                (91, "Google Chrome", None, false, false),
            ]
        );
        assert_eq!(got[3].name, "caffeinate command-line tool, for dino (pid 77)");
        assert_eq!(got[4].name, "com.apple.audio.context");
    }
}
