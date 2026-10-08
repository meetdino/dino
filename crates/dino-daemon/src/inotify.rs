//! Files changing under a folder, from Linux's inotify: `fsevents` on Linux, with the same API. The
//! kernel tells dinod, so nothing is polled and an idle folder costs nothing. Changes collect in
//! the watch until taken.
//!
//! inotify watches one folder at a time, not a tree as FSEvents does. Each stream therefore watches
//! every folder under its roots, adding folders as they're made, on one thread that reads them all
//! and hands changes on in batches, at most about once a latency, as FSEvents does. Folders that
//! hold nothing worth watching and can hold a great many folders are left out: `node_modules`, a git
//! dir's `objects`, and caches tagged with `CACHEDIR.TAG` (Cargo's `target/`). When the kernel drops
//! events, or runs out of watches (`fs.inotify.max_user_watches`), the batch says [`LOST`], and what
//! uses it reads everything again.

use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// At most this many changed paths are kept between looks; more only says "many".
const KEEP: usize = 500;
/// More than this many changes waiting for the next batch: the batch says [`LOST`] instead.
const PENDING_MAX: usize = 10_000;
const LATENCY: Duration = Duration::from_secs(1);

/// What changed since it was last taken, and when the last change came.
#[derive(Default)]
pub(crate) struct Changes {
    pub paths: Vec<PathBuf>,
    pub last: Option<Instant>,
    pub more: bool,
}

/// Each batch of changes: the paths, with flags for each ([`LOST`], [`ROOT_CHANGED`] or 0).
type OnChanges = Box<dyn Fn(&[(PathBuf, u32)]) + Send + Sync>;

/// Events were lost (the kernel's queue overflowed, or no watches were left): anything under the
/// watched paths may have changed. The same bits as FSEvents' flags, which `fsevents` passes on.
pub(crate) const LOST: u32 = 0x1 | 0x2 | 0x4;
/// A watched folder itself was moved or removed.
pub(crate) const ROOT_CHANGED: u32 = 0x20;

const MASK: u32 = libc::IN_MODIFY
    | libc::IN_ATTRIB
    | libc::IN_CLOSE_WRITE
    | libc::IN_MOVED_FROM
    | libc::IN_MOVED_TO
    | libc::IN_CREATE
    | libc::IN_DELETE
    | libc::IN_DELETE_SELF
    | libc::IN_MOVE_SELF
    | libc::IN_ONLYDIR
    | libc::IN_DONT_FOLLOW
    | libc::IN_EXCL_UNLINK;

/// What the stream's thread and its owner share.
struct Control {
    /// The eventfd that wakes the thread: to stop, or to flush.
    wake: libc::c_int,
    stop: AtomicBool,
    #[cfg_attr(not(test), allow(dead_code))]
    asked: AtomicU64,
    /// Flushes done, for `flush` to wait on.
    done: Mutex<u64>,
    #[cfg_attr(not(test), allow(dead_code))]
    flushed: Condvar,
}

impl Control {
    fn poke(&self) {
        let one: u64 = 1;
        // SAFETY: an eventfd this Control owns, and 8 bytes from a local.
        unsafe { libc::write(self.wake, &one as *const u64 as *const libc::c_void, 8) };
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        // SAFETY: the eventfd, closed once, after the thread and the owner are done with it.
        unsafe { libc::close(self.wake) };
    }
}

/// A started stream: its thread, until dropped.
struct Stream {
    control: Arc<Control>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.control.stop.store(true, Ordering::SeqCst);
        self.control.poke();
        if let Some(t) = self.thread.take() {
            // Dropped from its own callback: the thread ends once the callback returns.
            if t.thread().id() != std::thread::current().id() {
                let _ = t.join();
            }
        }
    }
}

pub(crate) struct Watch {
    _stream: Stream,
    changes: Arc<Mutex<Changes>>,
    pub root: PathBuf,
}

/// Folders watched together, each batch of changes handed to a callback as it comes: what changed
/// is folders (not each file), so a build writing thousands of files is a few events.
pub(crate) struct Folders {
    #[cfg_attr(not(test), allow(dead_code))]
    stream: Stream,
}

/// A folder not worth watching, or descending into.
fn skipped(dir: &Path) -> bool {
    let name = dir.file_name().map(|n| n.as_bytes()).unwrap_or_default();
    name == b"node_modules" || (name == b"objects" && dir.parent().is_some_and(|p| p.join("HEAD").is_file())) || dir.join("CACHEDIR.TAG").is_file()
}

/// What the stream's thread keeps: the inotify descriptor and the folder each watch is on.
struct Tree {
    fd: libc::c_int,
    dirs: HashMap<libc::c_int, PathBuf>,
    roots: Vec<PathBuf>,
    files: bool,
    /// No watches were left for some folder: said once, as [`LOST`].
    full: bool,
}

impl Tree {
    /// Watch `dir` and the folders under it. `found` gets the files and folders under it (not
    /// `dir` itself), for a folder that came with things already in it.
    fn add(&mut self, dir: &Path, mut found: Option<&mut Vec<PathBuf>>) -> bool {
        let mut stack = vec![dir.to_path_buf()];
        let mut any = false;
        while let Some(d) = stack.pop() {
            if skipped(&d) {
                continue;
            }
            let Ok(c) = CString::new(d.as_os_str().as_bytes()) else { continue };
            // SAFETY: a live inotify descriptor and a NUL-terminated path.
            let wd = unsafe { libc::inotify_add_watch(self.fd, c.as_ptr(), MASK) };
            if wd < 0 {
                if std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOSPC) && !self.full {
                    self.full = true;
                    eprintln!("dinod: out of inotify watches (fs.inotify.max_user_watches) under {}; changes there may be missed", dir.display());
                }
                continue;
            }
            any = true;
            self.dirs.insert(wd, d.clone());
            let Ok(entries) = std::fs::read_dir(&d) else { continue };
            for e in entries.flatten() {
                let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
                if let Some(f) = found.as_deref_mut() {
                    f.push(e.path());
                }
                if is_dir {
                    stack.push(e.path());
                }
            }
        }
        any
    }

    /// Stop watching `dir` and everything under it (moved away).
    fn remove(&mut self, dir: &Path) {
        let gone: Vec<libc::c_int> = self.dirs.iter().filter(|(_, d)| d.starts_with(dir)).map(|(&wd, _)| wd).collect();
        for wd in gone {
            self.dirs.remove(&wd);
            // SAFETY: a watch of this descriptor.
            unsafe { libc::inotify_rm_watch(self.fd, wd) };
        }
    }

    /// Read what's waiting, without blocking, into `pending`.
    fn read(&mut self, pending: &mut Pending) {
        #[repr(C, align(8))]
        struct Buf([u8; 64 * 1024]);
        let mut buf = Buf([0; 64 * 1024]);
        loop {
            // SAFETY: reads into a local buffer of the length given.
            let n = unsafe { libc::read(self.fd, buf.0.as_mut_ptr() as *mut libc::c_void, buf.0.len()) };
            if n <= 0 {
                return;
            }
            let mut at = 0usize;
            let head = std::mem::size_of::<libc::inotify_event>();
            while at + head <= n as usize {
                // SAFETY: the kernel wrote whole events; unaligned read of the header.
                let ev: libc::inotify_event = unsafe { std::ptr::read_unaligned(buf.0.as_ptr().add(at) as *const libc::inotify_event) };
                let name_bytes = &buf.0[at + head..(at + head + ev.len as usize).min(n as usize)];
                let name = &name_bytes[..name_bytes.iter().position(|&b| b == 0).unwrap_or(name_bytes.len())];
                at += head + ev.len as usize;
                self.event(ev.wd, ev.mask, std::ffi::OsStr::from_bytes(name), pending);
            }
        }
    }

    fn event(&mut self, wd: libc::c_int, mask: u32, name: &std::ffi::OsStr, pending: &mut Pending) {
        if mask & libc::IN_Q_OVERFLOW != 0 {
            pending.push(self.roots[0].clone(), LOST);
            return;
        }
        if mask & libc::IN_IGNORED != 0 {
            self.dirs.remove(&wd);
            return;
        }
        let Some(dir) = self.dirs.get(&wd).cloned() else { return };
        if mask & (libc::IN_DELETE_SELF | libc::IN_MOVE_SELF) != 0 {
            // A folder under a root is reported by its parent, as it goes.
            if self.roots.contains(&dir) {
                pending.push(dir.clone(), ROOT_CHANGED);
                if mask & libc::IN_MOVE_SELF != 0 {
                    self.remove(&dir);
                }
            }
            return;
        }
        let path = if name.is_empty() { dir.clone() } else { dir.join(name) };
        if mask & libc::IN_ISDIR != 0 {
            if mask & libc::IN_MOVED_FROM != 0 {
                self.remove(&path);
            } else if mask & (libc::IN_CREATE | libc::IN_MOVED_TO) != 0 {
                let mut inside = vec![];
                self.add(&path, self.files.then_some(&mut inside));
                for p in inside {
                    pending.push(p, 0);
                }
            }
        }
        if self.full {
            self.full = false;
            pending.push(self.roots[0].clone(), LOST);
        }
        pending.push(if self.files { path } else { dir }, 0);
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        // SAFETY: the inotify descriptor, closed once.
        unsafe { libc::close(self.fd) };
    }
}

/// Changes waiting for the next batch.
#[derive(Default)]
struct Pending {
    batch: Vec<(PathBuf, u32)>,
    seen: HashSet<PathBuf>,
    lost: bool,
}

impl Pending {
    fn push(&mut self, path: PathBuf, flags: u32) {
        if flags != 0 {
            self.batch.push((path, flags));
        } else if self.batch.len() >= PENDING_MAX {
            if !self.lost {
                self.lost = true;
                self.batch.push((path, LOST));
            }
        } else if self.seen.insert(path.clone()) {
            self.batch.push((path, 0));
        }
    }

    fn take(&mut self) -> Vec<(PathBuf, u32)> {
        self.seen.clear();
        self.lost = false;
        std::mem::take(&mut self.batch)
    }
}

/// A started stream over `roots` (canonical), or why not.
fn stream(roots: &[PathBuf], files: bool, on: OnChanges) -> anyhow::Result<Stream> {
    anyhow::ensure!(!roots.is_empty(), "no folders to watch");
    // SAFETY: plain syscalls; each descriptor is owned by what it's put in.
    let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
    anyhow::ensure!(fd >= 0, "can't watch {}: {}", roots[0].display(), std::io::Error::last_os_error());
    let mut tree = Tree { fd, dirs: HashMap::new(), roots: roots.to_vec(), files, full: false };
    let wake = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
    anyhow::ensure!(wake >= 0, "can't watch {}: {}", roots[0].display(), std::io::Error::last_os_error());
    let control = Arc::new(Control { wake, stop: AtomicBool::new(false), asked: AtomicU64::new(0), done: Mutex::new(0), flushed: Condvar::new() });
    let mut any = false;
    for r in roots {
        any |= tree.add(r, None);
    }
    anyhow::ensure!(any, "Linux won't watch {}", roots[0].display());
    let c = control.clone();
    let thread = std::thread::Builder::new().name("dinod-inotify".into()).spawn(move || run(tree, &c, on))?;
    Ok(Stream { control, thread: Some(thread) })
}

/// The stream's thread: waits for changes, a flush or the stop, and hands batches on.
fn run(mut tree: Tree, c: &Control, on: OnChanges) {
    let mut pending = Pending::default();
    let mut sent: Option<Instant> = None;
    loop {
        let wait = if pending.batch.is_empty() { -1 } else { sent.map_or(0, |s| LATENCY.saturating_sub(s.elapsed()).as_millis() as libc::c_int) };
        let mut fds = [libc::pollfd { fd: tree.fd, events: libc::POLLIN, revents: 0 }, libc::pollfd { fd: c.wake, events: libc::POLLIN, revents: 0 }];
        // SAFETY: two pollfds of our own.
        unsafe { libc::poll(fds.as_mut_ptr(), 2, wait) };
        if c.stop.load(Ordering::SeqCst) {
            return;
        }
        if fds[1].revents != 0 {
            let mut n: u64 = 0;
            // SAFETY: drains the eventfd into a local.
            unsafe { libc::read(c.wake, &mut n as *mut u64 as *mut libc::c_void, 8) };
        }
        tree.read(&mut pending);
        let asked = c.asked.load(Ordering::SeqCst);
        let flush = asked > *c.done.lock().unwrap();
        // The first change after a quiet spell goes at once; more wait out the latency.
        if !pending.batch.is_empty() && (flush || sent.is_none_or(|s| s.elapsed() >= LATENCY)) {
            let batch = pending.take();
            sent = Some(Instant::now());
            on(&batch);
            if c.stop.load(Ordering::SeqCst) {
                return;
            }
        } else if pending.batch.is_empty() && sent.is_some_and(|s| s.elapsed() >= LATENCY) {
            sent = None;
        }
        if flush {
            *c.done.lock().unwrap() = asked;
            c.flushed.notify_all();
        }
    }
}

impl Watch {
    /// Watch `root` and everything under it.
    pub(crate) fn new(root: &Path, poke: impl Fn() + Send + Sync + 'static) -> anyhow::Result<Watch> {
        let root = std::fs::canonicalize(root)?;
        let changes: Arc<Mutex<Changes>> = Arc::default();
        let kept = changes.clone();
        let on = move |batch: &[(PathBuf, u32)]| {
            {
                let mut c = kept.lock().unwrap();
                for (p, _) in batch {
                    if c.paths.len() >= KEEP {
                        c.more = true;
                    } else if !c.paths.contains(p) {
                        c.paths.push(p.clone());
                    }
                }
                c.last = Some(Instant::now());
            }
            poke();
        };
        let stream = stream(std::slice::from_ref(&root), true, Box::new(on))?;
        Ok(Watch { _stream: stream, changes, root })
    }

    /// When the last change came, without taking anything.
    pub(crate) fn last(&self) -> Option<Instant> {
        self.changes.lock().unwrap().last
    }

    pub(crate) fn take(&self) -> Changes {
        std::mem::take(&mut *self.changes.lock().unwrap())
    }
}

impl Folders {
    /// Watch `roots` (canonical) and everything under them; `on` gets the folders that changed,
    /// about once a second at most while they keep changing.
    pub(crate) fn new(roots: &[PathBuf], on: impl Fn(&[(PathBuf, u32)]) + Send + Sync + 'static) -> anyhow::Result<Folders> {
        Ok(Folders { stream: stream(roots, false, Box::new(on))? })
    }

    /// `new`, with each file that changed rather than its folder.
    pub(crate) fn files(roots: &[PathBuf], on: impl Fn(&[(PathBuf, u32)]) + Send + Sync + 'static) -> anyhow::Result<Folders> {
        Ok(Folders { stream: stream(roots, true, Box::new(on))? })
    }

    /// Hands over now what's held back for the latency: back once every change made before the
    /// call has reached `on`.
    #[cfg(test)]
    pub(crate) fn flush(&self) {
        let c = &self.stream.control;
        let want = c.asked.fetch_add(1, Ordering::SeqCst) + 1;
        c.poke();
        let mut done = c.done.lock().unwrap();
        while *done < want && !c.stop.load(Ordering::SeqCst) {
            done = c.flushed.wait_timeout(done, Duration::from_secs(5)).unwrap().0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicIsize, AtomicUsize};

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dino-inotify-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::canonicalize(&dir).unwrap()
    }

    #[test]
    fn sees_a_file_written() {
        let dir = temp("file");
        let (tx, rx) = std::sync::mpsc::channel();
        let tx = Mutex::new(tx);
        let w = Watch::new(&dir, move || {
            let _ = tx.lock().unwrap().send(());
        })
        .unwrap();
        std::fs::write(dir.join("a.txt"), "hi").unwrap();
        rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(w.take().paths.iter().any(|p| p.ends_with("a.txt")));
        drop(w);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn folders_see_new_folders_and_their_roots_go() {
        let root = temp("folders");
        let got: Arc<Mutex<Vec<(PathBuf, u32)>>> = Arc::default();
        let g = got.clone();
        let f = Folders::new(std::slice::from_ref(&root), move |b| g.lock().unwrap().extend_from_slice(b)).unwrap();
        // A folder made after the watch started is watched too: its change names it.
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        f.flush();
        std::fs::write(root.join("a/b/x.txt"), "x").unwrap();
        f.flush();
        assert!(got.lock().unwrap().iter().any(|(p, fl)| *p == root.join("a/b") && *fl == 0), "{:?}", got.lock().unwrap());
        // node_modules and caches aren't.
        std::fs::create_dir_all(root.join("node_modules/m")).unwrap();
        f.flush();
        got.lock().unwrap().clear();
        std::fs::write(root.join("node_modules/m/y.js"), "y").unwrap();
        f.flush();
        assert!(got.lock().unwrap().is_empty(), "{:?}", got.lock().unwrap());
        // The root itself removed: the kernel says so once the folder is let go, which can be a
        // moment after `rmdir` returns.
        std::fs::remove_dir_all(&root).unwrap();
        let gone = || got.lock().unwrap().iter().any(|(p, fl)| *p == root && fl & ROOT_CHANGED != 0);
        let since = Instant::now();
        while !gone() && since.elapsed() < Duration::from_secs(5) {
            f.flush();
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(gone(), "{:?}", got.lock().unwrap());
    }

    #[test]
    fn files_see_what_a_new_folder_came_with() {
        let root = temp("moved");
        let outside = temp("outside");
        std::fs::create_dir_all(outside.join("m/n")).unwrap();
        std::fs::write(outside.join("m/n/z.txt"), "z").unwrap();
        let got: Arc<Mutex<Vec<(PathBuf, u32)>>> = Arc::default();
        let g = got.clone();
        let f = Folders::files(std::slice::from_ref(&root), move |b| g.lock().unwrap().extend_from_slice(b)).unwrap();
        std::fs::rename(outside.join("m"), root.join("m")).unwrap();
        f.flush();
        assert!(got.lock().unwrap().iter().any(|(p, _)| *p == root.join("m/n/z.txt")), "{:?}", got.lock().unwrap());
        // And changes in it after.
        std::fs::write(root.join("m/n/w.txt"), "w").unwrap();
        f.flush();
        assert!(got.lock().unwrap().iter().any(|(p, _)| *p == root.join("m/n/w.txt")), "{:?}", got.lock().unwrap());
        drop(f);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    /// Streams made and dropped many at a time, from several threads, some during a callback, and
    /// streams that can't be made (no folders): each callback's state freed exactly once.
    #[test]
    fn streams_made_and_dropped_while_files_change() {
        struct Held(Arc<AtomicIsize>);
        impl Drop for Held {
            fn drop(&mut self) {
                assert!(self.0.fetch_sub(1, Ordering::SeqCst) > 0, "freed twice");
            }
        }
        let held = |live: &Arc<AtomicIsize>| {
            live.fetch_add(1, Ordering::SeqCst);
            Held(live.clone())
        };
        let dir = temp("churn");
        let roots = [dir.join("a"), dir.join("b")];
        roots.iter().for_each(|r| std::fs::create_dir_all(r).unwrap());
        let live = Arc::new(AtomicIsize::new(0));
        let calls = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let writer = {
            let (roots, stop) = (roots.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut i = 0usize;
                while !stop.load(Ordering::Relaxed) {
                    let f = roots[i % 2].join(format!("{}.txt", i % 40));
                    std::fs::write(&f, format!("{i}")).unwrap();
                    if i % 7 == 0 {
                        let _ = std::fs::remove_file(&f);
                    }
                    i += 1;
                    std::thread::sleep(Duration::from_millis(1));
                }
            })
        };
        let on = |live: &Arc<AtomicIsize>, calls: &Arc<AtomicUsize>| {
            let (h, calls) = (held(live), calls.clone());
            move |batch: &[(PathBuf, u32)]| {
                let _ = &h;
                assert!(!batch.is_empty());
                calls.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(3));
            }
        };
        let kept = Folders::files(&roots, on(&live, &calls)).unwrap();
        std::thread::scope(|sc| {
            for t in 0..4u64 {
                let (live, calls, roots) = (&live, &calls, &roots);
                sc.spawn(move || {
                    let mut all: Vec<Box<dyn Send>> = Vec::new();
                    for i in 0..30u64 {
                        assert!(Folders::new(&[], on(live, calls)).is_err(), "no folders, no stream");
                        let made: Box<dyn Send> = match (t + i) % 3 {
                            0 => Box::new(Folders::new(roots, on(live, calls)).unwrap()),
                            1 => Box::new(Folders::files(&roots[..1], on(live, calls)).unwrap()),
                            _ => {
                                let (h, c) = (held(live), calls.clone());
                                Box::new(
                                    Watch::new(&roots[1], move || {
                                        let _ = &h;
                                        c.fetch_add(1, Ordering::SeqCst);
                                    })
                                    .unwrap(),
                                )
                            }
                        };
                        all.push(made);
                        if i % 4 == 0 {
                            all.clear();
                        }
                        std::thread::sleep(Duration::from_millis((i * 7 + t * 13) % 40));
                    }
                });
            }
        });
        drop(kept);
        stop.store(true, Ordering::Relaxed);
        writer.join().unwrap();
        assert!(calls.load(Ordering::SeqCst) > 0, "no change seen");
        assert_eq!(live.load(Ordering::SeqCst), 0, "every callback's state freed once");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
