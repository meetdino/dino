//! Files changing under a folder, from macOS's FSEvents: the kernel tells dinod, so nothing is
//! polled and an idle folder costs nothing. Changes collect in the watch until taken.

use std::ffi::{CString, c_char, c_void};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

type CFRef = *const c_void;
type Stream = *mut c_void;

#[repr(C)]
struct Context {
    version: isize,
    info: *mut c_void,
    retain: Option<extern "C" fn(*const c_void) -> *const c_void>,
    release: Option<extern "C" fn(*const c_void)>,
    describe: Option<extern "C" fn(*const c_void) -> CFRef>,
}

type Callback = extern "C" fn(stream: *const c_void, info: *mut c_void, count: usize, paths: *mut c_void, flags: *const u32, ids: *const u64);

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    fn FSEventStreamCreate(alloc: CFRef, callback: Callback, context: *const Context, paths: CFRef, since: u64, latency: f64, flags: u32) -> Stream;
    fn FSEventStreamSetDispatchQueue(stream: Stream, queue: *mut c_void);
    fn FSEventStreamStart(stream: Stream) -> u8;
    #[cfg(test)]
    fn FSEventStreamFlushSync(stream: Stream);
    fn FSEventStreamStop(stream: Stream);
    fn FSEventStreamInvalidate(stream: Stream);
    fn FSEventStreamRelease(stream: Stream);
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringCreateWithCString(alloc: CFRef, s: *const c_char, encoding: u32) -> CFRef;
    fn CFArrayCreate(alloc: CFRef, values: *const CFRef, count: isize, callbacks: *const c_void) -> CFRef;
    fn CFRelease(cf: CFRef);
    static kCFTypeArrayCallBacks: c_void;
}

unsafe extern "C" {
    fn dispatch_queue_create(label: *const c_char, attr: *const c_void) -> *mut c_void;
}

const SINCE_NOW: u64 = u64::MAX;
const NO_DEFER: u32 = 0x2;
const WATCH_ROOT: u32 = 0x4;
const FILE_EVENTS: u32 = 0x10;
const UTF8: u32 = 0x0800_0100;
/// At most this many changed paths are kept between looks; more only says "many".
const KEEP: usize = 500;

/// What changed since it was last taken, and when the last change came.
#[derive(Default)]
pub(crate) struct Changes {
    pub paths: Vec<PathBuf>,
    pub last: Option<Instant>,
    pub more: bool,
}

/// Each batch of changes: the paths, with FSEvents' flags for each.
type OnChanges = Box<dyn Fn(&[(PathBuf, u32)]) + Send + Sync>;

/// Events were lost (FSEvents' MustScanSubDirs, UserDropped, KernelDropped): anything under the
/// watched paths may have changed.
pub(crate) const LOST: u32 = 0x1 | 0x2 | 0x4;
/// A watched folder itself was moved or removed (FSEvents' RootChanged).
pub(crate) const ROOT_CHANGED: u32 = 0x20;

struct Shared {
    on: OnChanges,
}

pub(crate) struct Watch {
    stream: Stream,
    changes: Arc<Mutex<Changes>>,
    pub root: PathBuf,
}

// SAFETY: the stream is only started, stopped and released through `Watch`, which FSEvents allows
// from any thread; the callback reaches `Shared` only, which is Sync.
unsafe impl Send for Watch {}
unsafe impl Sync for Watch {}

/// Folders watched together, each batch of changes handed to a callback as it comes: what changed
/// is folders (not each file), so a build writing thousands of files is a few events.
pub(crate) struct Folders {
    stream: Stream,
}

// SAFETY: as for `Watch`.
unsafe impl Send for Folders {}
unsafe impl Sync for Folders {}

fn queue() -> *mut c_void {
    struct Q(*mut c_void);
    // SAFETY: a dispatch queue is thread-safe; this one lives as long as dinod.
    unsafe impl Send for Q {}
    unsafe impl Sync for Q {}
    static QUEUE: std::sync::OnceLock<Q> = std::sync::OnceLock::new();
    QUEUE.get_or_init(|| Q(unsafe { dispatch_queue_create(c"dino.automations.files".as_ptr(), std::ptr::null()) })).0
}

// The stream's own reference to `Shared`, taken and given back by FSEvents itself: it gives it back
// once no callback can come (after the last one ends, even when the stream is released during it),
// and also when it fails to make the stream at all (no paths given, say). So `stream` never gives
// back what FSEvents took: that was a double free whenever a stream couldn't be made.
extern "C" fn retain(info: *const c_void) -> *const c_void {
    // SAFETY: `info` is `Arc::as_ptr` of a `Shared` that `stream` holds while FSEvents retains it,
    // and that this reference keeps alive after.
    unsafe { Arc::increment_strong_count(info as *const Shared) };
    info
}

extern "C" fn release(info: *const c_void) {
    // SAFETY: gives back the reference `retain` took, once per retain.
    unsafe { Arc::decrement_strong_count(info as *const Shared) };
}

extern "C" fn changed(_stream: *const c_void, info: *mut c_void, count: usize, paths: *mut c_void, flags: *const u32, _ids: *const u64) {
    // SAFETY: FSEvents passes back the context's `info` (a live `Shared`: the stream holds a
    // reference until no callback can come) and, without kFSEventStreamCreateFlagUseCFTypes,
    // `count` C strings and as many flags.
    let shared = unsafe { &*(info as *const Shared) };
    let paths = unsafe { std::slice::from_raw_parts(paths as *const *const c_char, count) };
    let flags = unsafe { std::slice::from_raw_parts(flags, count) };
    let batch: Vec<(PathBuf, u32)> = paths
        .iter()
        .zip(flags)
        .map(|(&p, &f)| (PathBuf::from(unsafe { std::ffi::CStr::from_ptr(p) }.to_string_lossy().into_owned()), f))
        .collect();
    (shared.on)(&batch);
}

/// A started stream over `roots` (canonical), or why not.
fn stream(roots: &[PathBuf], latency: f64, flags: u32, on: OnChanges) -> anyhow::Result<Stream> {
    let what = || roots.first().map_or_else(|| "no folders".to_string(), |r| r.display().to_string());
    let shared = Arc::new(Shared { on });
    let paths = roots.iter().map(|r| CString::new(r.to_string_lossy().as_bytes())).collect::<Result<Vec<_>, _>>()?;
    // SAFETY: plain CoreFoundation/FSEvents calls with valid arguments. The strings and the array
    // are released once the stream has its own copies. FSEvents retains `shared` through the
    // context's callbacks for as long as it needs it, made or not; ours drops on return.
    unsafe {
        let mut strings = Vec::new();
        for (c, r) in paths.iter().zip(roots) {
            let s = CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), UTF8);
            if s.is_null() {
                strings.iter().for_each(|&s| CFRelease(s));
                anyhow::bail!("can't watch {}", r.display());
            }
            strings.push(s);
        }
        let arr = CFArrayCreate(std::ptr::null(), strings.as_ptr(), strings.len() as isize, &kCFTypeArrayCallBacks);
        strings.iter().for_each(|&s| CFRelease(s));
        anyhow::ensure!(!arr.is_null(), "can't watch {}", what());
        let ctx = Context { version: 0, info: Arc::as_ptr(&shared) as *mut c_void, retain: Some(retain), release: Some(release), describe: None };
        let stream = FSEventStreamCreate(std::ptr::null(), changed, &ctx, arr, SINCE_NOW, latency, flags);
        CFRelease(arr);
        anyhow::ensure!(!stream.is_null(), "macOS won't watch {}", what());
        FSEventStreamSetDispatchQueue(stream, queue());
        if FSEventStreamStart(stream) == 0 {
            FSEventStreamInvalidate(stream);
            FSEventStreamRelease(stream);
            anyhow::bail!("macOS won't watch {}", what());
        }
        Ok(stream)
    }
}

fn drop_stream(stream: Stream) {
    // SAFETY: a stream `stream` started, stopped and freed once.
    unsafe {
        FSEventStreamStop(stream);
        FSEventStreamInvalidate(stream);
        FSEventStreamRelease(stream);
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
        let stream = stream(std::slice::from_ref(&root), 1.0, NO_DEFER | WATCH_ROOT | FILE_EVENTS, Box::new(on))?;
        Ok(Watch { stream, changes, root })
    }

    /// When the last change came, without taking anything.
    pub(crate) fn last(&self) -> Option<Instant> {
        self.changes.lock().unwrap().last
    }

    pub(crate) fn take(&self) -> Changes {
        std::mem::take(&mut *self.changes.lock().unwrap())
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        drop_stream(self.stream);
    }
}

impl Folders {
    /// Watch `roots` (canonical) and everything under them; `on` gets the folders that changed,
    /// about once a second at most while they keep changing.
    pub(crate) fn new(roots: &[PathBuf], on: impl Fn(&[(PathBuf, u32)]) + Send + Sync + 'static) -> anyhow::Result<Folders> {
        Ok(Folders { stream: stream(roots, 1.0, NO_DEFER | WATCH_ROOT, Box::new(on))? })
    }

    /// `new`, with each file that changed rather than its folder.
    pub(crate) fn files(roots: &[PathBuf], on: impl Fn(&[(PathBuf, u32)]) + Send + Sync + 'static) -> anyhow::Result<Folders> {
        Ok(Folders { stream: stream(roots, 1.0, NO_DEFER | WATCH_ROOT | FILE_EVENTS, Box::new(on))? })
    }

    /// Hands over now what macOS holds back for the latency: back once every change made before
    /// the call has reached `on`.
    #[cfg(test)]
    pub(crate) fn flush(&self) {
        // SAFETY: a started stream, not released while `self` lives; called off its queue.
        unsafe { FSEventStreamFlushSync(self.stream) }
    }
}

impl Drop for Folders {
    fn drop(&mut self) {
        drop_stream(self.stream);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sees_a_file_written() {
        let dir = std::env::temp_dir().join(format!("dino-fsevents-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let tx = Mutex::new(tx);
        let w = Watch::new(&dir, move || {
            let _ = tx.lock().unwrap().send(());
        })
        .unwrap();
        // The stream may start late on a loaded Mac, and the folder being made can come first:
        // write again until the file's change is seen.
        let mut paths = Vec::new();
        let seen = (0..30).any(|i| {
            std::fs::write(dir.join("a.txt"), format!("hi {i}")).unwrap();
            if rx.recv_timeout(std::time::Duration::from_secs(1)).is_ok() {
                paths.extend(w.take().paths);
            }
            paths.iter().any(|p| p.ends_with("a.txt"))
        });
        assert!(seen, "a.txt not seen in 30 s: {paths:?}");
        drop(w);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Streams made and dropped many at a time, from several threads, while files change under
    /// them (some dropped during a callback), and streams macOS won't make (no folders): what
    /// each one's callback holds is freed exactly once, after its last callback. A stream that
    /// couldn't be made once freed it twice, corrupting the heap (dinod crashed in a fresh repo).
    #[test]
    fn streams_made_and_dropped_while_files_change() {
        use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicUsize, Ordering};
        use std::time::Duration;

        /// What a callback holds: counted while alive, so a double free shows as a count below 0.
        struct Held(Arc<AtomicIsize>);
        impl Held {
            fn new(live: &Arc<AtomicIsize>) -> Held {
                live.fetch_add(1, Ordering::SeqCst);
                Held(live.clone())
            }
        }
        impl Drop for Held {
            fn drop(&mut self) {
                assert!(self.0.fetch_sub(1, Ordering::SeqCst) > 0, "freed twice");
            }
        }

        let dir = std::env::temp_dir().join(format!("dino-fsevents-churn-{}", std::process::id()));
        let (a, b) = (dir.join("a"), dir.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let roots = [std::fs::canonicalize(&a).unwrap(), std::fs::canonicalize(&b).unwrap()];
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
        // Callbacks that take a moment, so drops land during them too.
        let on = |live: &Arc<AtomicIsize>, calls: &Arc<AtomicUsize>| {
            let (held, calls) = (Held::new(live), calls.clone());
            move |batch: &[(PathBuf, u32)]| {
                let _ = &held;
                assert!(!batch.is_empty());
                calls.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(3));
            }
        };
        // One kept throughout: the changes are seen at all.
        let kept = Folders::files(&roots, on(&live, &calls)).unwrap();
        std::thread::scope(|sc| {
            for t in 0..4u64 {
                let (live, calls, roots) = (&live, &calls, &roots);
                sc.spawn(move || {
                    let mut held: Vec<Box<dyn Send>> = Vec::new();
                    for i in 0..60u64 {
                        assert!(Folders::new(&[], on(live, calls)).is_err(), "no folders, no stream");
                        let made: Box<dyn Send> = match (t + i) % 3 {
                            0 => Box::new(Folders::new(roots, on(live, calls)).unwrap()),
                            1 => Box::new(Folders::files(&roots[..1], on(live, calls)).unwrap()),
                            _ => {
                                let (h, c) = (Held::new(live), calls.clone());
                                Box::new(Watch::new(&roots[1], move || {
                                    let _ = &h;
                                    c.fetch_add(1, Ordering::SeqCst);
                                }).unwrap())
                            }
                        };
                        held.push(made);
                        // Dropped soon, a few together, or after a callback or two.
                        if i % 4 == 0 {
                            held.clear();
                        }
                        std::thread::sleep(Duration::from_millis((i * 7 + t * 13) % 40));
                    }
                });
            }
        });
        let since = Instant::now();
        while calls.load(Ordering::SeqCst) == 0 && since.elapsed() < Duration::from_secs(30) {
            std::thread::sleep(Duration::from_millis(50));
        }
        drop(kept);
        stop.store(true, Ordering::Relaxed);
        writer.join().unwrap();
        assert!(calls.load(Ordering::SeqCst) > 0, "no change seen in 30 s");
        // FSEvents lets go of each stream once its queue is done with it.
        let since = Instant::now();
        while live.load(Ordering::SeqCst) != 0 && since.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(live.load(Ordering::SeqCst), 0, "every callback's state freed once");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
