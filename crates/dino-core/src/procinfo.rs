//! What `pgrep` and `lsof` would say about local processes, asked of the kernel directly: each of
//! those takes ~50 ms to start, and discovery asks every few seconds.

use std::ffi::{c_void, CStr};
use std::mem::{size_of, size_of_val};

/// Every pid on the system, like `ps -A`.
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

fn name_of(pid: libc::c_int, buf: &mut [u8; 64]) -> Option<&[u8]> {
    let n = unsafe { libc::proc_name(pid, buf.as_mut_ptr() as *mut c_void, buf.len() as u32) };
    (n > 0).then(|| &buf[..n as usize])
}

/// Pids whose process name is exactly `name`, like `pgrep -x`.
pub fn pids_named(name: &str) -> Vec<u32> {
    let mut buf = [0u8; 64];
    all_pids().into_iter().filter(|&pid| name_of(pid, &mut buf) == Some(name.as_bytes())).map(|pid| pid as u32).collect()
}

/// Node processes that renamed themselves `title` (`process.title`): the kernel still names them
/// `node`, and only their arguments say what they are.
pub fn node_titled(title: &str) -> Vec<u32> {
    pids_named("node").into_iter().filter(|&pid| args_and_env(pid).is_some_and(|(args, _)| args.first().is_some_and(|a| a.trim_end() == title))).collect()
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

/// When a process started, in seconds since the epoch, like `ps -o lstart`.
pub fn started(pid: u32) -> Option<u64> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let n = unsafe { libc::proc_pidinfo(pid as libc::c_int, libc::PROC_PIDTBSDINFO, 0, &mut info as *mut _ as *mut c_void, size) };
    (n == size).then_some(info.pbi_start_tvsec)
}

/// A process's parent, like `ps -o ppid`.
pub fn parent_of(pid: u32) -> Option<u32> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let n = unsafe { libc::proc_pidinfo(pid as libc::c_int, libc::PROC_PIDTBSDINFO, 0, &mut info as *mut _ as *mut c_void, size) };
    (n == size).then_some(info.pbi_ppid)
}

/// The program a process runs, by its full path (`node` for a Node CLI), like lsof's `txt` entry.
pub fn exe_of(pid: u32) -> Option<String> {
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let n = unsafe { libc::proc_pidpath(pid as libc::c_int, buf.as_mut_ptr() as *mut c_void, buf.len() as u32) };
    (n > 0).then(|| String::from_utf8_lossy(&buf[..n as usize]).into_owned())
}

/// A process's arguments (its own name first) and environment, as the kernel keeps them, like
/// `ps -o args` and `ps -E`. A program that renames itself (a Node CLI setting its title) has
/// written over its arguments; its environment stays as it started.
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

/// The process runs under another dinod (a test build's, a second user's dino): that dinod owns
/// it, and this one leaves it alone. Its parents are looked at up to launchd; this dinod (or the
/// command asking) isn't another.
pub fn under_another_dinod(pid: u32) -> bool {
    let me = std::process::id();
    let mut buf = [0u8; 64];
    let mut at = parent_of(pid);
    for _ in 0..32 {
        let Some(p) = at.filter(|&p| p > 1) else { return false };
        if p == me {
            return false;
        }
        if name_of(p as libc::c_int, &mut buf) == Some(b"dino".as_slice()) && args_and_env(p).is_some_and(|(args, _)| args.get(1).is_some_and(|a| a == "daemon")) {
            return true;
        }
        at = parent_of(p);
    }
    false
}

/// A process's working directory, like lsof's `cwd` entry.
pub fn cwd_of(pid: u32) -> Option<String> {
    let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let size = size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
    let n = unsafe { libc::proc_pidinfo(pid as libc::c_int, libc::PROC_PIDVNODEPATHINFO, 0, &mut info as *mut _ as *mut c_void, size) };
    (n == size).then(|| path(&info.pvi_cdir)).flatten()
}

/// `struct vnode_fdinfowithpath` from sys/proc_info.h; libc has only the path half.
#[repr(C)]
struct VnodeFdInfoWithPath {
    pfi: [u64; 3],
    pvip: libc::vnode_info_path,
}

const PROC_PIDFDVNODEPATHINFO: libc::c_int = 2;

/// Paths of the files a process has open, like lsof's `n` entries for its descriptors.
pub fn open_files(pid: u32) -> Vec<String> {
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
    let mut out = vec![];
    for fd in fds.iter().filter(|f| f.proc_fdtype == libc::PROX_FDTYPE_VNODE as u32) {
        let mut info: VnodeFdInfoWithPath = unsafe { std::mem::zeroed() };
        let size = size_of::<VnodeFdInfoWithPath>() as libc::c_int;
        let got = unsafe { libc::proc_pidfdinfo(pid, fd.proc_fd, PROC_PIDFDVNODEPATHINFO, &mut info as *mut _ as *mut c_void, size) };
        if got == size {
            out.extend(path(&info.pvip));
        }
    }
    out
}

fn path(v: &libc::vnode_info_path) -> Option<String> {
    let bytes = unsafe { std::slice::from_raw_parts(v.vip_path.as_ptr() as *const u8, size_of_val(&v.vip_path)) };
    let s = CStr::from_bytes_until_nul(bytes).ok()?.to_str().ok()?;
    (!s.is_empty()).then(|| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sees_itself() {
        let me = std::process::id();
        let exe = std::env::current_exe().unwrap();
        // Process names are cut at 32 bytes; the test binary's is longer, so match what the kernel keeps.
        let name = exe.file_name().unwrap().to_str().unwrap();
        let name = &name[..name.len().min(32)];
        assert!(pids_named(name).contains(&me), "{name}");
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
    fn only_another_dinods_agents_are_its_own() {
        // A child of this process: no dinod above it, and this one isn't another.
        let mut child = std::process::Command::new("/bin/sleep").arg("5").spawn().unwrap();
        assert!(!under_another_dinod(child.id()));
        let _ = child.kill();
        let _ = child.wait();
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
