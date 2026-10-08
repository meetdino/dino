//! What `pgrep` and `lsof` would say about local processes, asked of the kernel directly: each of
//! those takes ~50 ms to start, and discovery asks every few seconds.

#[cfg(target_os = "macos")]
use std::ffi::{c_void, CStr};
#[cfg(target_os = "macos")]
use std::mem::{size_of, size_of_val};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux::{all_pids, name_of};
#[cfg(target_os = "linux")]
pub use linux::{alive, args_and_env, children_of, cwd_of, exe_of, now_ns, open_fds, open_file, parent_of, process, rusage, started};

/// Every pid on the system, like `ps -A`.
#[cfg(target_os = "macos")]
fn all_pids() -> Vec<libc::c_int> {
    let mut pids = vec![0 as libc::c_int; 4096];
    loop {
        let bytes = (pids.len() * size_of::<libc::c_int>()) as libc::c_int;
        let n = unsafe { libc::proc_listallpids(pids.as_mut_ptr() as *mut c_void, bytes) };
        if n <= 0 {
            return vec![];
        }
        // A full buffer may have cut the list short.
        if (n as usize) < pids.len() {
            pids.truncate(n as usize);
            break;
        }
        pids.resize(pids.len() * 2, 0);
    }
    pids.retain(|&pid| pid > 0);
    pids
}

#[cfg(target_os = "macos")]
fn name_of(pid: libc::c_int, buf: &mut [u8; 64]) -> Option<&[u8]> {
    let n = unsafe { libc::proc_name(pid, buf.as_mut_ptr() as *mut c_void, buf.len() as u32) };
    (n > 0).then(|| &buf[..n as usize])
}

/// A process's name, as `ps -o comm` shows it without the path: `vim`, `nvim`, `htop`.
pub fn name(pid: u32) -> Option<String> {
    let mut buf = [0u8; 64];
    name_of(pid as libc::c_int, &mut buf).map(|n| String::from_utf8_lossy(n).into_owned())
}

/// Pids whose process name is exactly `name`, like `pgrep -x`.
pub fn pids_named(name: &str) -> Vec<u32> {
    let mut buf = [0u8; 64];
    all_pids().into_iter().filter(|&pid| name_of(pid, &mut buf) == Some(name.as_bytes())).map(|pid| pid as u32).collect()
}

/// Pids in `procs` whose process name is exactly `name`, in order.
pub fn named_in(procs: &Procs, name: &str) -> Vec<u32> {
    let mut out: Vec<u32> = procs.values().filter(|p| p.name == name).map(|p| p.pid).collect();
    out.sort_unstable();
    out
}

/// Node processes in `procs` that renamed themselves `title` (`process.title`): the kernel still
/// names them `node`, and only their arguments say what they are.
pub fn node_titled_in(procs: &Procs, title: &str) -> Vec<u32> {
    named_in(procs, "node").into_iter().filter(|&pid| args_and_env(pid).is_some_and(|(args, _)| args.first().is_some_and(|a| a.trim_end() == title))).collect()
}

/// Each process this user can see with its name and working directory, like `lsof -d cwd`:
/// what's working in a folder right now.
pub fn working_dirs() -> Vec<(u32, String, String)> {
    let mut buf = [0u8; 64];
    all_pids()
        .into_iter()
        .filter_map(|pid| {
            let cwd = cwd_of(pid as u32)?;
            let name = String::from_utf8_lossy(name_of(pid, &mut buf)?).into_owned();
            Some((pid as u32, name, cwd))
        })
        .collect()
}

/// A process as discovery sees it, from one kernel call: like a line of `ps -A -o pid,ppid,tty,lstart,comm`.
#[derive(Debug, Clone, PartialEq)]
pub struct Proc {
    pub pid: u32,
    pub parent: u32,
    /// When it started, in microseconds since the epoch: with the pid, which process this is
    /// (a pid is reused once its process is gone).
    pub started_us: u64,
    /// It has a controlling terminal (a person's terminal tab, a tmux pane, a pty).
    pub tty: bool,
    /// Its name as the kernel keeps it (its program's file name, up to 32 bytes).
    pub name: String,
}

#[cfg(target_os = "macos")]
fn bsdinfo(pid: u32) -> Option<libc::proc_bsdinfo> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let n = unsafe { libc::proc_pidinfo(pid as libc::c_int, libc::PROC_PIDTBSDINFO, 0, &mut info as *mut _ as *mut c_void, size) };
    (n == size).then_some(info)
}

/// `PROC_FLAG_CONTROLT` from sys/proc_info.h: the process has a controlling terminal.
#[cfg(target_os = "macos")]
const PROC_FLAG_CONTROLT: u32 = 0x80;

/// One process, as [`processes`] lists it; `None` once it's gone.
#[cfg(target_os = "macos")]
pub fn process(pid: u32) -> Option<Proc> {
    let info = bsdinfo(pid)?;
    let name = unsafe { CStr::from_ptr(info.pbi_name.as_ptr()) }.to_string_lossy();
    let name = if name.is_empty() { unsafe { CStr::from_ptr(info.pbi_comm.as_ptr()) }.to_string_lossy() } else { name };
    Some(Proc {
        pid,
        parent: info.pbi_ppid,
        started_us: info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec,
        tty: info.pbi_flags & PROC_FLAG_CONTROLT != 0 && info.e_tdev != u32::MAX,
        name: name.into_owned(),
    })
}

/// Process `pid` is still the one that started at `started_us` (see [`Proc`]), and hasn't ended:
/// not a zombie waiting for its parent.
#[cfg(target_os = "macos")]
pub fn alive(pid: u32, started_us: u64) -> bool {
    bsdinfo(pid).is_some_and(|i| i.pbi_start_tvsec * 1_000_000 + i.pbi_start_tvusec == started_us && i.pbi_status != libc::SZOMB as u32)
}

/// Processes by pid, as [`processes`] lists them.
pub type Procs = std::collections::HashMap<u32, Proc>;

/// Every process this user can see, by pid. A few microseconds each: no `ps` started.
pub fn processes() -> Procs {
    all_pids().into_iter().filter_map(|pid| process(pid as u32)).map(|p| (p.pid, p)).collect()
}

/// When a process started, in seconds since the epoch, like `ps -o lstart`.
#[cfg(target_os = "macos")]
pub fn started(pid: u32) -> Option<u64> {
    bsdinfo(pid).map(|i| i.pbi_start_tvsec)
}

/// The terminal session a process is in (the pid of the process that started it, `setsid`), like
/// `ps -o sess`.
pub fn session_of(pid: u32) -> Option<u32> {
    let sid = unsafe { libc::getsid(pid as libc::pid_t) };
    (sid > 0).then_some(sid as u32)
}

/// A process's parent, like `ps -o ppid`. Another user's process (the root-owned `login` that
/// Terminal, iTerm2 and Ghostty put above the shell) gives no full BSD info, only the short one.
#[cfg(target_os = "macos")]
pub fn parent_of(pid: u32) -> Option<u32> {
    bsdinfo(pid).map(|i| i.pbi_ppid).or_else(|| {
        let mut info: libc::proc_bsdshortinfo = unsafe { std::mem::zeroed() };
        let size = size_of::<libc::proc_bsdshortinfo>() as libc::c_int;
        let n = unsafe { libc::proc_pidinfo(pid as libc::c_int, libc::PROC_PIDT_SHORTBSDINFO, 0, &mut info as *mut _ as *mut c_void, size) };
        (n == size).then_some(info.pbsi_ppid)
    })
}

/// The program a process runs, by its full path (`node` for a Node CLI), like lsof's `txt` entry.
#[cfg(target_os = "macos")]
pub fn exe_of(pid: u32) -> Option<String> {
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let n = unsafe { libc::proc_pidpath(pid as libc::c_int, buf.as_mut_ptr() as *mut c_void, buf.len() as u32) };
    (n > 0).then(|| String::from_utf8_lossy(&buf[..n as usize]).into_owned())
}

/// A process's arguments (its own name first) and environment, as the kernel keeps them, like
/// `ps -o args` and `ps -E`. A program that renames itself (a Node CLI setting its title) has
/// written over its arguments; its environment stays as it started.
#[cfg(target_os = "macos")]
pub fn args_and_env(pid: u32) -> Option<(Vec<String>, Vec<String>)> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
    let mut size: libc::size_t = 0;
    if unsafe { libc::sysctl(mib.as_mut_ptr(), 3, std::ptr::null_mut(), &mut size, std::ptr::null_mut(), 0) } != 0 || size < 4 {
        return None;
    }
    let mut buf = vec![0u8; size];
    if unsafe { libc::sysctl(mib.as_mut_ptr(), 3, buf.as_mut_ptr() as *mut c_void, &mut size, std::ptr::null_mut(), 0) } != 0 {
        return None;
    }
    buf.truncate(size);
    procargs(&buf)
}

/// `KERN_PROCARGS2`'s layout: argc, the executable's path, padding, argv, then the environment.
#[cfg(target_os = "macos")]
fn procargs(buf: &[u8]) -> Option<(Vec<String>, Vec<String>)> {
    let argc = i32::from_ne_bytes(buf.get(..4)?.try_into().ok()?).max(0) as usize;
    let rest = &buf[4..];
    // Past the executable's path and the NULs after it.
    let start = rest.iter().position(|&b| b == 0)?;
    let rest = &rest[start..];
    let rest = &rest[rest.iter().position(|&b| b != 0)?..];
    let mut strings = rest.split(|&b| b == 0).map(|s| String::from_utf8_lossy(s).into_owned());
    let args: Vec<String> = strings.by_ref().take(argc).collect();
    // A process that renamed itself zeroed what its arguments took: NULs up to its environment.
    let env = strings.skip_while(|s| s.is_empty()).take_while(|s| !s.is_empty()).collect();
    Some((args, env))
}

/// Process `pid` is a `dino daemon`: a dinod.
pub fn is_dinod(pid: u32, name: &str) -> bool {
    name == "dino" && args_and_env(pid).is_some_and(|(args, _)| args.get(1).is_some_and(|a| a == "daemon"))
}

/// The process runs under a dinod (this one, a test build's, a second user's dino): that dinod
/// owns it and lists it as its own session, so it isn't one found "on this Mac". Its parents are
/// looked at up to launchd, in `procs` (see [`processes`]).
pub fn under_a_dinod(procs: &Procs, pid: u32) -> bool {
    let mut at = procs.get(&pid).map(|p| p.parent);
    for _ in 0..64 {
        let Some(p) = at.filter(|&p| p > 1).and_then(|p| procs.get(&p)) else { return false };
        if is_dinod(p.pid, &p.name) {
            return true;
        }
        at = Some(p.parent);
    }
    false
}

/// What a process costs the Mac, from `proc_pid_rusage`, as Activity Monitor counts it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rusage {
    /// Its physical footprint: Activity Monitor's Memory column, `footprint`'s total.
    pub footprint: u64,
    /// CPU time it has used, user and system, in nanoseconds.
    pub cpu_ns: u64,
    /// CPU time of its children that ended and that it waited for (theirs counted in turn), in
    /// nanoseconds: a short-lived compiler a build ran shows up here once it's gone.
    pub children_ns: u64,
    /// When it started, on the clock `now_ns` reads: tells a reused pid from the process before.
    pub started_ns: u64,
}

/// A process's cost now; none once it's gone (or it isn't this user's).
#[cfg(target_os = "macos")]
pub fn rusage(pid: u32) -> Option<Rusage> {
    let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
    let got = unsafe { libc::proc_pid_rusage(pid as libc::c_int, libc::RUSAGE_INFO_V4, &mut info as *mut _ as *mut libc::rusage_info_t) };
    (got == 0).then(|| Rusage {
        footprint: info.ri_phys_footprint,
        cpu_ns: ticks_to_ns(info.ri_user_time + info.ri_system_time),
        children_ns: ticks_to_ns(info.ri_child_user_time + info.ri_child_system_time),
        started_ns: ticks_to_ns(info.ri_proc_start_abstime),
    })
}

/// The kernel's clock (`mach_absolute_time`, stopped while the Mac sleeps) in nanoseconds: what
/// `Rusage::started_ns` is on.
#[cfg(target_os = "macos")]
pub fn now_ns() -> u64 {
    unsafe extern "C" {
        fn mach_absolute_time() -> u64;
    }
    ticks_to_ns(unsafe { mach_absolute_time() })
}

/// `proc_pid_rusage` counts time in the kernel's ticks: nanoseconds on Intel, 1/24 µs on Apple silicon.
#[cfg(target_os = "macos")]
fn ticks_to_ns(ticks: u64) -> u64 {
    static TIMEBASE: std::sync::OnceLock<(u64, u64)> = std::sync::OnceLock::new();
    /// `mach_timebase_info_data_t`; libc's is deprecated in favour of a crate dino doesn't use.
    #[repr(C)]
    struct Timebase {
        numer: u32,
        denom: u32,
    }
    unsafe extern "C" {
        fn mach_timebase_info(info: *mut Timebase) -> libc::c_int;
    }
    let (numer, denom) = *TIMEBASE.get_or_init(|| {
        let mut tb = Timebase { numer: 0, denom: 0 };
        if unsafe { mach_timebase_info(&mut tb) } != 0 || tb.numer == 0 || tb.denom == 0 {
            return (1, 1);
        }
        (tb.numer as u64, tb.denom as u64)
    });
    (ticks as u128 * numer as u128 / denom as u128) as u64
}

/// A process's children, by their parent: like `pgrep -P`.
#[cfg(target_os = "macos")]
pub fn children_of(pid: u32) -> Vec<u32> {
    let mut kids = vec![0 as libc::c_int; 64];
    loop {
        let bytes = (kids.len() * size_of::<libc::c_int>()) as libc::c_int;
        // A count of pids, unlike the rest of libproc.
        let n = unsafe { libc::proc_listchildpids(pid as libc::c_int, kids.as_mut_ptr() as *mut c_void, bytes) };
        if n <= 0 {
            return vec![];
        }
        if (n as usize) < kids.len() {
            kids.truncate(n as usize);
            break;
        }
        kids.resize(kids.len() * 4, 0);
    }
    kids.into_iter().filter(|&k| k > 0).map(|k| k as u32).collect()
}

/// A process and everything under it, each with its parent, the process first. Found by parent,
/// so what moved to a process group or session of its own (a job, a daemonizing server) is in it;
/// what its parent left behind for launchd isn't.
pub fn tree(root: u32) -> Vec<(u32, u32)> {
    let mut out = vec![(root, parent_of(root).unwrap_or(0))];
    let mut i = 0;
    // Bounded, should a pid be reused into its own subtree between two looks.
    while i < out.len() && out.len() < 4096 {
        let at = out[i].0;
        for kid in children_of(at) {
            if !out.iter().any(|&(p, _)| p == kid) {
                out.push((kid, at));
            }
        }
        i += 1;
    }
    out
}

/// A process's working directory, like lsof's `cwd` entry.
#[cfg(target_os = "macos")]
pub fn cwd_of(pid: u32) -> Option<String> {
    let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let size = size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
    let n = unsafe { libc::proc_pidinfo(pid as libc::c_int, libc::PROC_PIDVNODEPATHINFO, 0, &mut info as *mut _ as *mut c_void, size) };
    (n == size).then(|| path(&info.pvi_cdir)).flatten()
}

/// `struct vnode_fdinfowithpath` from sys/proc_info.h; libc has only the path half.
#[cfg(target_os = "macos")]
#[repr(C)]
struct VnodeFdInfoWithPath {
    pfi: [u64; 3],
    pvip: libc::vnode_info_path,
}

#[cfg(target_os = "macos")]
const PROC_PIDFDVNODEPATHINFO: libc::c_int = 2;

/// Paths of the files a process has open, like lsof's `n` entries for its descriptors.
pub fn open_files(pid: u32) -> Vec<String> {
    open_fds(pid).into_iter().map(|(_, p)| p).collect()
}

/// The path of the file open as descriptor `fd` of process `pid`, if that's a file.
#[cfg(target_os = "macos")]
pub fn open_file(pid: u32, fd: i32) -> Option<String> {
    let mut info: VnodeFdInfoWithPath = unsafe { std::mem::zeroed() };
    let size = size_of::<VnodeFdInfoWithPath>() as libc::c_int;
    let got = unsafe { libc::proc_pidfdinfo(pid as libc::c_int, fd, PROC_PIDFDVNODEPATHINFO, &mut info as *mut _ as *mut c_void, size) };
    if got == size { path(&info.pvip) } else { None }
}

/// The files a process has open, with their descriptors.
#[cfg(target_os = "macos")]
pub fn open_fds(pid: u32) -> Vec<(i32, String)> {
    let pid = pid as libc::c_int;
    let n = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
    if n <= 0 {
        return vec![];
    }
    // Room for a few opened in between.
    let mut fds = vec![libc::proc_fdinfo { proc_fd: 0, proc_fdtype: 0 }; n as usize / size_of::<libc::proc_fdinfo>() + 16];
    let bytes = (fds.len() * size_of::<libc::proc_fdinfo>()) as libc::c_int;
    let n = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, fds.as_mut_ptr() as *mut c_void, bytes) };
    fds.truncate(n.max(0) as usize / size_of::<libc::proc_fdinfo>());
    fds.iter().filter(|f| f.proc_fdtype == libc::PROX_FDTYPE_VNODE as u32).filter_map(|f| Some((f.proc_fd, open_file(pid as u32, f.proc_fd)?))).collect()
}

#[cfg(target_os = "macos")]
fn path(v: &libc::vnode_info_path) -> Option<String> {
    let bytes = unsafe { std::slice::from_raw_parts(v.vip_path.as_ptr() as *const u8, size_of_val(&v.vip_path)) };
    let s = CStr::from_bytes_until_nul(bytes).ok()?.to_str().ok()?;
    (!s.is_empty()).then(|| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parent_of_another_users_process() {
        // launchd is root's: a parent from the short info, as for the `login` above a Terminal shell.
        assert_eq!(parent_of(1), Some(0));
    }

    #[test]
    fn sees_itself() {
        let me = std::process::id();
        let exe = std::env::current_exe().unwrap();
        // Process names are cut at 32 bytes; the test binary's is longer, so match what the kernel keeps.
        let name = exe.file_name().unwrap().to_str().unwrap();
        let name = &name[..name.len().min(32)];
        assert!(pids_named(name).contains(&me), "{name}");
        assert_eq!(super::name(me).as_deref(), Some(name));
        let cwd = std::env::current_dir().unwrap().canonicalize().unwrap();
        assert_eq!(cwd_of(me).map(std::path::PathBuf::from), Some(cwd.clone()));
        assert!(working_dirs().iter().any(|(pid, n, c)| *pid == me && name.starts_with(n.as_str()) && std::path::Path::new(c) == cwd));
        let f = std::env::temp_dir().join(format!("dino-procinfo-{me}"));
        let _open = std::fs::File::create(&f).unwrap();
        let f = f.canonicalize().unwrap();
        assert!(open_files(me).iter().any(|p| std::path::Path::new(p) == f), "{:?}", open_files(me));
        std::fs::remove_file(&f).unwrap();
        assert_eq!(parent_of(me), Some(std::os::unix::process::parent_id()));
        assert_eq!(exe_of(me).map(std::path::PathBuf::from), Some(exe.canonicalize().unwrap()));
        let (args, env) = args_and_env(me).unwrap();
        assert_eq!(args, std::env::args().collect::<Vec<_>>());
        assert!(env.iter().any(|e| e.starts_with("PATH=")));
    }

    #[test]
    fn rusage_counts_as_getrusage_does() {
        let me = std::process::id();
        let before = rusage(me).unwrap();
        // A child that ends and is waited for: its time moves to ours.
        let ok = std::process::Command::new("/bin/sh").args(["-c", "i=0; while [ $i -lt 200000 ]; do i=$((i+1)); done"]).status().unwrap();
        assert!(ok.success());
        let after = rusage(me).unwrap();
        let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
        assert_eq!(unsafe { libc::getrusage(libc::RUSAGE_CHILDREN, &mut ru) }, 0);
        let ns = |t: libc::timeval| t.tv_sec as u64 * 1_000_000_000 + t.tv_usec as u64 * 1000;
        let children = ns(ru.ru_utime) + ns(ru.ru_stime);
        let later = rusage(me).unwrap();
        assert!(after.children_ns > before.children_ns + 50_000_000, "{before:?} {after:?}");
        // Same clock, same units: getrusage rounds to microseconds and adds its time up a little
        // differently, a fraction of a percent apart over many children and a few percent on a
        // busy Mac (2.5% seen while the whole workspace's tests ran). What this guards is the
        // units: unconverted ticks would be 41× off on Apple silicon. Bracketed, as other tests
        // may be ending children of their own meanwhile.
        let near = |n: u64| n + n / 10 + 1_000_000;
        assert!(after.children_ns <= near(children) && children <= near(later.children_ns), "{} ≤ {children} ≤ {}", after.children_ns, later.children_ns);
        assert!(after.footprint > 0 && after.cpu_ns > 0 && after.started_ns < now_ns());
        assert_eq!(rusage(me).unwrap().started_ns, after.started_ns);
        let mut kid = std::process::Command::new("/bin/sleep").arg("5").spawn().unwrap();
        assert!(children_of(me).contains(&kid.id()));
        assert!(tree(me).contains(&(kid.id(), me)));
        assert_eq!(name(kid.id()).as_deref(), Some("sleep"));
        let _ = kid.kill();
        let _ = kid.wait();
    }

    #[test]
    fn reads_dinods_and_processes() {
        // A child of this process is under a dinod when this one is (a test run from a dino tab);
        // with no parents but launchd, it isn't.
        let mut child = std::process::Command::new("/bin/sleep").arg("5").spawn().unwrap();
        let mut procs = processes();
        assert_eq!(under_a_dinod(&procs, child.id()), under_a_dinod(&procs, std::process::id()));
        let mut orphan = procs[&child.id()].clone();
        orphan.parent = 1;
        procs.insert(orphan.pid, orphan);
        assert!(!under_a_dinod(&procs, child.id()));
        let procs = processes();
        let me = &procs[&std::process::id()];
        assert_eq!(me.parent, std::os::unix::process::parent_id());
        assert_eq!(procs[&child.id()].parent, me.pid);
        assert_eq!(procs[&child.id()].name, "sleep");
        assert!(procs[&child.id()].started_us >= me.started_us);
        let _ = child.kill();
        let _ = child.wait();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn reads_procargs() {
        // As the kernel lays out `dino daemon`'s arguments.
        let mut buf = 2i32.to_ne_bytes().to_vec();
        buf.extend(b"/x/dino\0\0\0\0/x/dino\0daemon\0HOME=/h\0DINO_HOME=/tmp/d\0\0\0");
        let (args, env) = procargs(&buf).unwrap();
        assert_eq!((args, env), (vec!["/x/dino".to_string(), "daemon".into()], vec!["HOME=/h".to_string(), "DINO_HOME=/tmp/d".into()]));
        // A Node CLI that set its title ("pi"): the rest of its arguments' room zeroed.
        let mut buf = 3i32.to_ne_bytes().to_vec();
        buf.extend(b"/opt/homebrew/bin/node\0\0pi\0\0\0\0\0\0\0\0\0\0\0\0_=/u/.nvm/bin/pi\0\0");
        let (args, env) = procargs(&buf).unwrap();
        assert_eq!((args, env), (vec!["pi".to_string(), String::new(), String::new()], vec!["_=/u/.nvm/bin/pi".to_string()]));
    }
}
