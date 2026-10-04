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
    }
}
