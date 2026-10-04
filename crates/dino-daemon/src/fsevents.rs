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

struct Shared {
    changes: Mutex<Changes>,
    /// Told after each batch of changes (wakes the scheduler).
    poke: Box<dyn Fn() + Send + Sync>,
}

pub(crate) struct Watch {
    stream: Stream,
    shared: Arc<Shared>,
    pub root: PathBuf,
}

// SAFETY: the stream is only started, stopped and released through `Watch`, which FSEvents allows
// from any thread; the callback reaches `Shared` only, which is Sync.
unsafe impl Send for Watch {}
unsafe impl Sync for Watch {}

fn queue() -> *mut c_void {
    struct Q(*mut c_void);
    // SAFETY: a dispatch queue is thread-safe; this one lives as long as dinod.
    unsafe impl Send for Q {}
    unsafe impl Sync for Q {}
    static QUEUE: std::sync::OnceLock<Q> = std::sync::OnceLock::new();
    QUEUE.get_or_init(|| Q(unsafe { dispatch_queue_create(c"dino.automations.files".as_ptr(), std::ptr::null()) })).0
}

extern "C" fn release(info: *const c_void) {
    // SAFETY: `info` is the `Arc<Shared>` given to the stream in `watch`, released once, here.
    unsafe { drop(Arc::from_raw(info as *const Shared)) };
}

extern "C" fn changed(_stream: *const c_void, info: *mut c_void, count: usize, paths: *mut c_void, _flags: *const u32, _ids: *const u64) {
    // SAFETY: FSEvents passes back the context's `info` (a live `Shared`: the stream holds a
    // reference until it's released) and, without kFSEventStreamCreateFlagUseCFTypes, `count`
    // C strings.
    let shared = unsafe { &*(info as *const Shared) };
    let paths = unsafe { std::slice::from_raw_parts(paths as *const *const c_char, count) };
    {
        let mut c = shared.changes.lock().unwrap();
        for &p in paths {
            let p = PathBuf::from(unsafe { std::ffi::CStr::from_ptr(p) }.to_string_lossy().into_owned());
            if c.paths.len() >= KEEP {
                c.more = true;
            } else if !c.paths.contains(&p) {
                c.paths.push(p);
            }
        }
        c.last = Some(Instant::now());
    }
    (shared.poke)();
}

impl Watch {
    /// Watch `root` and everything under it.
    pub(crate) fn new(root: &Path, poke: impl Fn() + Send + Sync + 'static) -> anyhow::Result<Watch> {
        let root = std::fs::canonicalize(root)?;
        let c = CString::new(root.to_string_lossy().as_bytes())?;
        let shared = Arc::new(Shared { changes: Mutex::default(), poke: Box::new(poke) });
        // SAFETY: plain CoreFoundation/FSEvents calls with valid arguments; the array and string
        // are released once the stream has its own copies.
        unsafe {
            let s = CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), UTF8);
            anyhow::ensure!(!s.is_null(), "can't watch {}", root.display());
            let arr = CFArrayCreate(std::ptr::null(), &s, 1, &kCFTypeArrayCallBacks);
            CFRelease(s);
            let ctx = Context { version: 0, info: Arc::into_raw(shared.clone()) as *mut c_void, retain: None, release: Some(release), describe: None };
            let stream = FSEventStreamCreate(std::ptr::null(), changed, &ctx, arr, SINCE_NOW, 1.0, NO_DEFER | WATCH_ROOT | FILE_EVENTS);
            CFRelease(arr);
            if stream.is_null() {
                release(ctx.info);
                anyhow::bail!("macOS won't watch {}", root.display());
            }
            FSEventStreamSetDispatchQueue(stream, queue());
            if FSEventStreamStart(stream) == 0 {
                FSEventStreamInvalidate(stream);
                FSEventStreamRelease(stream);
                anyhow::bail!("macOS won't watch {}", root.display());
            }
            Ok(Watch { stream, shared, root })
        }
    }

    /// When the last change came, without taking anything.
    pub(crate) fn last(&self) -> Option<Instant> {
        self.shared.changes.lock().unwrap().last
    }

    pub(crate) fn take(&self) -> Changes {
        std::mem::take(&mut *self.shared.changes.lock().unwrap())
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        // SAFETY: the stream this watch started, stopped and freed once.
        unsafe {
            FSEventStreamStop(self.stream);
            FSEventStreamInvalidate(self.stream);
            FSEventStreamRelease(self.stream);
        }
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
        let w = Watch::new(&dir, move || drop(tx.lock().unwrap().send(()))).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        std::fs::write(dir.join("a.txt"), "hi").unwrap();
        rx.recv_timeout(std::time::Duration::from_secs(10)).expect("no change seen");
        let c = w.take();
        assert!(c.paths.iter().any(|p| p.ends_with("a.txt")), "{:?}", c.paths);
        drop(w);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
