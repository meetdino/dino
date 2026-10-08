//! `procinfo` on Linux, from `/proc`: what libproc and `sysctl` give on macOS.

use std::path::Path;

use super::{Proc, Rusage};

/// Every pid on the system, like `ps -A`: `/proc`'s numbered entries.
pub(super) fn all_pids() -> Vec<libc::c_int> {
    let Ok(dir) = std::fs::read_dir("/proc") else { return vec![] };
    dir.filter_map(|e| e.ok()?.file_name().to_str()?.parse::<libc::c_int>().ok()).filter(|&p| p > 0).collect()
}

/// A process's name as macOS's `proc_name` gives it: its program's file name. The kernel keeps
/// only its first 15 bytes (`comm`); a name that long is completed from the program's path when
/// that is readable and starts with it.
pub(super) fn name_of(pid: libc::c_int, buf: &mut [u8; 64]) -> Option<&[u8]> {
    let stat = Stat::read(pid as u32)?;
    let mut name = stat.comm;
    if name.len() >= 15
        && let Some(exe) = exe_of(pid as u32)
        && let Some(base) = Path::new(&exe).file_name().and_then(|n| n.to_str())
        && base.starts_with(&name)
    {
        name = base.to_string();
    }
    let n = name.len().min(buf.len());
    buf[..n].copy_from_slice(&name.as_bytes()[..n]);
    (n > 0).then(|| &buf[..n])
}

/// The fields of `/proc/<pid>/stat` dino reads.
struct Stat {
    comm: String,
    state: u8,
    ppid: u32,
    tty_nr: i64,
    utime: u64,
    stime: u64,
    cutime: u64,
    cstime: u64,
    /// In clock ticks since boot.
    starttime: u64,
}

impl Stat {
    fn read(pid: u32) -> Option<Stat> {
        Stat::parse(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
    }

    fn parse(text: &str) -> Option<Stat> {
        // `pid (comm) state ppid …`: the name may hold spaces and parentheses, so it ends at the
        // last `)`.
        let open = text.find('(')?;
        let close = text.rfind(')')?;
        let comm = text.get(open + 1..close)?.to_string();
        let f: Vec<&str> = text.get(close + 1..)?.split_whitespace().collect();
        let num = |i: usize| f.get(i).and_then(|v| v.parse::<i64>().ok());
        Some(Stat {
            comm,
            state: *f.first()?.as_bytes().first()?,
            ppid: num(1)? as u32,
            tty_nr: num(4)?,
            utime: num(11)? as u64,
            stime: num(12)? as u64,
            cutime: num(13)? as u64,
            cstime: num(14)? as u64,
            starttime: num(19)? as u64,
        })
    }
}

fn ticks_per_sec() -> u64 {
    static HZ: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *HZ.get_or_init(|| {
        let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        if hz > 0 { hz as u64 } else { 100 }
    })
}

/// When the system booted, in seconds since the epoch (`btime` in `/proc/stat`): what a process's
/// start time counts from. Read once, so a process's start time stays the same between looks.
fn boot_secs() -> u64 {
    static BOOT: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *BOOT.get_or_init(|| std::fs::read_to_string("/proc/stat").ok().and_then(|s| s.lines().find_map(|l| l.strip_prefix("btime ")?.trim().parse().ok())).unwrap_or(0))
}

fn started_us_of(stat: &Stat) -> u64 {
    boot_secs() * 1_000_000 + stat.starttime * 1_000_000 / ticks_per_sec()
}

/// One process, as [`super::processes`] lists it; `None` once it's gone.
pub fn process(pid: u32) -> Option<Proc> {
    let stat = Stat::read(pid)?;
    let mut buf = [0u8; 64];
    let name = name_of(pid as libc::c_int, &mut buf).map(|n| String::from_utf8_lossy(n).into_owned()).unwrap_or_else(|| stat.comm.clone());
    Some(Proc { pid, parent: stat.ppid, started_us: started_us_of(&stat), tty: stat.tty_nr != 0, name })
}

/// Process `pid` is still the one that started at `started_us`, and hasn't ended: not a zombie
/// waiting for its parent.
pub fn alive(pid: u32, started_us: u64) -> bool {
    Stat::read(pid).is_some_and(|s| started_us_of(&s) == started_us && s.state != b'Z' && s.state != b'X')
}

/// When a process started, in seconds since the epoch, like `ps -o lstart`: rounded down once,
/// as macOS gives it, to be compared with a file's times. `btime` is itself rounded down, so
/// adding the start's whole seconds to it ran up to two seconds early, and a file made a second
/// before an agent started read as made after it (a resumed Codex's rollout as one it began).
pub fn started(pid: u32) -> Option<u64> {
    let stat = Stat::read(pid)?;
    let since_boot = stat.starttime as u128 * 1_000_000_000 / ticks_per_sec() as u128;
    Some(((booted_ns()? + since_boot) / 1_000_000_000) as u64)
}

/// When the system booted, in nanoseconds since the epoch: the clock now less the time since
/// boot, by the clock a process's start time counts on (suspend included).
fn booted_ns() -> Option<u128> {
    let ns = |clock| {
        let mut t: libc::timespec = unsafe { std::mem::zeroed() };
        if unsafe { libc::clock_gettime(clock, &mut t) } != 0 {
            return None;
        }
        Some(t.tv_sec as u128 * 1_000_000_000 + t.tv_nsec as u128)
    };
    let since_boot = ns(libc::CLOCK_BOOTTIME)?;
    ns(libc::CLOCK_REALTIME)?.checked_sub(since_boot)
}

/// A process's parent, like `ps -o ppid`.
pub fn parent_of(pid: u32) -> Option<u32> {
    Stat::read(pid).map(|s| s.ppid)
}

fn link(path: String) -> Option<String> {
    let p = std::fs::read_link(path).ok()?;
    let s = p.to_str()?;
    // A file deleted while open (a binary replaced by a rebuild, say) still names its path.
    Some(s.strip_suffix(" (deleted)").unwrap_or(s).to_string())
}

/// The program a process runs, by its full path (`node` for a Node CLI).
pub fn exe_of(pid: u32) -> Option<String> {
    link(format!("/proc/{pid}/exe"))
}

fn nul_separated(bytes: &[u8]) -> Vec<String> {
    let bytes = bytes.strip_suffix(&[0]).unwrap_or(bytes);
    if bytes.is_empty() {
        return vec![];
    }
    bytes.split(|&b| b == 0).map(|s| String::from_utf8_lossy(s).into_owned()).collect()
}

/// A process's arguments (its own name first) and environment, like `ps -o args` and `ps e`. A
/// program that renames itself (a Node CLI setting its title) has written over its arguments; its
/// environment stays as it started. Another user's environment isn't readable: then none.
pub fn args_and_env(pid: u32) -> Option<(Vec<String>, Vec<String>)> {
    let args = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    // None for a kernel thread, a zombie, or a process in the middle of exec.
    if args.is_empty() {
        return None;
    }
    let env = std::fs::read(format!("/proc/{pid}/environ")).unwrap_or_default();
    Some((nul_separated(&args), nul_separated(&env).into_iter().filter(|e| !e.is_empty()).collect()))
}

/// What a process costs, as `top` counts it: its resident memory and CPU time.
pub fn rusage(pid: u32) -> Option<Rusage> {
    let stat = Stat::read(pid)?;
    let statm = std::fs::read_to_string(format!("/proc/{pid}/statm")).ok()?;
    let resident: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(4096) as u64;
    let ns = |ticks: u64| ticks * 1_000_000_000 / ticks_per_sec();
    Some(Rusage { footprint: resident * page, cpu_ns: ns(stat.utime + stat.stime), children_ns: ns(stat.cutime + stat.cstime), started_ns: ns(stat.starttime) })
}

/// Nanoseconds since boot (`CLOCK_BOOTTIME`, which a process's start time counts on): what
/// `Rusage::started_ns` is on.
pub fn now_ns() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

/// A process's children, by their parent: like `pgrep -P`. From each of its threads' `children`
/// list, or by every process's parent where the kernel keeps none.
pub fn children_of(pid: u32) -> Vec<u32> {
    if let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) {
        let mut kids = vec![];
        let mut listed = false;
        for t in tasks.flatten() {
            if let Ok(text) = std::fs::read_to_string(t.path().join("children")) {
                listed = true;
                kids.extend(text.split_whitespace().filter_map(|k| k.parse::<u32>().ok()));
            }
        }
        if listed {
            kids.sort_unstable();
            kids.dedup();
            return kids;
        }
    } else {
        return vec![];
    }
    all_pids().into_iter().map(|p| p as u32).filter(|&p| parent_of(p) == Some(pid)).collect()
}

/// A process's working directory, like lsof's `cwd` entry.
pub fn cwd_of(pid: u32) -> Option<String> {
    link(format!("/proc/{pid}/cwd"))
}

/// The path of the file open as descriptor `fd` of process `pid`, if that's a file (not a socket
/// or a pipe).
pub fn open_file(pid: u32, fd: i32) -> Option<String> {
    link(format!("/proc/{pid}/fd/{fd}")).filter(|p| p.starts_with('/'))
}

/// The files a process has open, with their descriptors.
pub fn open_fds(pid: u32) -> Vec<(i32, String)> {
    let Ok(dir) = std::fs::read_dir(format!("/proc/{pid}/fd")) else { return vec![] };
    let mut fds: Vec<(i32, String)> = dir.filter_map(|e| e.ok()?.file_name().to_str()?.parse::<i32>().ok()).filter_map(|fd| Some((fd, open_file(pid, fd)?))).collect();
    fds.sort_unstable();
    fds
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_stat_with_any_name() {
        let s = Stat::parse("42 (a (b) c) S 7 42 42 34816 42 4194304 1 2 3 4 15 6 17 8 20 0 1 0 1234 0 0").unwrap();
        assert_eq!((s.comm.as_str(), s.state, s.ppid, s.tty_nr), ("a (b) c", b'S', 7, 34816));
        assert_eq!((s.utime, s.stime, s.cutime, s.cstime, s.starttime), (15, 6, 17, 8, 1234));
    }

    /// Within the clock tick it's counted in, never `btime`'s rounding (up to two seconds) early.
    #[test]
    fn a_process_started_when_it_did() {
        let now = || std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
        for _ in 0..20 {
            let before = now() - std::time::Duration::from_millis(1000 / ticks_per_sec() + 1);
            let mut child = std::process::Command::new("sleep").arg("5").spawn().unwrap();
            let after = now();
            let started = started(child.id());
            child.kill().unwrap();
            child.wait().unwrap();
            let started = started.unwrap();
            assert!(started >= before.as_secs() && started <= after.as_secs(), "{started} not in {before:?}..{after:?}");
            std::thread::sleep(std::time::Duration::from_millis(37));
        }
    }

    #[test]
    fn splits_arguments_as_the_kernel_keeps_them() {
        assert_eq!(nul_separated(b"/x/dino\0daemon\0"), vec!["/x/dino".to_string(), "daemon".into()]);
        // A Node CLI that set its title ("pi"): the rest of its arguments' room zeroed.
        assert_eq!(nul_separated(b"pi\0\0\0"), vec!["pi".to_string(), String::new(), String::new()]);
        assert!(nul_separated(b"").is_empty());
    }
}
