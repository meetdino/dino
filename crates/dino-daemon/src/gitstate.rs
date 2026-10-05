//! Each worktree's git summary for the sidebar's tree, kept until something in it changes.
//!
//! A repo can have a thousand worktrees (agents make one per task, and few get cleaned up).
//! Reading every one's git state every few seconds was thousands of git runs a minute, a thread
//! and a git process for each worktree at once. Instead macOS says which folders changed
//! (FSEvents, one stream per repo), and a worktree is read again only when files in it changed,
//! its HEAD or the base branch moved (`git worktree list` says, for all of them in one run), or a
//! remote branch moved (then only its unpushed count). At most `workers()` git runs at once.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::{Duration, Instant};

use dino_core::worktree;

use super::Daemon;
use crate::fsevents::{Folders, LOST, ROOT_CHANGED};

/// Without a watch (macOS wouldn't give one), a summary is read again when older than this.
pub(crate) const FRESH: Duration = Duration::from_secs(8);
/// A worktree that keeps changing is read again at most this often.
const SETTLE: Duration = Duration::from_secs(4);
/// A repo's watch is dropped when its tree hasn't been asked for in this long. An idle watch
/// costs nothing, and dropping it means reading every worktree again when the app next asks.
const UNUSED: Duration = Duration::from_secs(24 * 3600);
/// A repo's worktrees are listed again at least this often, whatever the watch says.
const RELIST: Duration = Duration::from_secs(5 * 60);

/// What's kept between reads of the tree: each repo's watch, and its worktrees as last listed.
#[derive(Default)]
pub(crate) struct State {
    /// Main checkout → its watch.
    watches: Mutex<HashMap<String, RepoWatch>>,
    /// A folder asked about → its repo's worktrees, as listed then.
    lists: Mutex<HashMap<String, Listed>>,
    /// Worktrees read (their own git runs) and repos listed so far, for tests and the budget.
    pub reads: AtomicUsize,
    pub listings: AtomicUsize,
}

struct Listed {
    worktrees: Vec<worktree::Worktree>,
    /// Their paths, symlinks resolved (a thousand of them is thousands of lookups each time).
    paths: Vec<String>,
    main: String,
    /// The watch and its `listed` count when it was read; None when there was no watch yet.
    stamp: Option<(u64, u64)>,
    at: Instant,
}

impl State {
    /// Every repo's worktrees are listed again at the next read.
    pub(crate) fn relist(&self) {
        self.lists.lock().unwrap().values_mut().for_each(|l| l.stamp = None);
    }
}

/// The worktrees of the repo containing `dir`, main checkout first (`worktree::list`), with their
/// paths symlinks resolved: as listed last time while nothing that would change them has, else
/// listed again.
pub(crate) fn list(d: &Daemon, dir: &str) -> anyhow::Result<(Vec<worktree::Worktree>, Vec<String>)> {
    let watch_now = |main: &str| {
        let w = d.git.watches.lock().unwrap();
        w.get(main).map(|w| (w.id, w.marks.listed.load(Ordering::Relaxed)))
    };
    let mut lists = d.git.lists.lock().unwrap();
    if let Some(l) = lists.get(dir) {
        if l.at.elapsed() < RELIST && l.stamp.is_some() && l.stamp == watch_now(&l.main) {
            return Ok((l.worktrees.clone(), l.paths.clone()));
        }
    }
    let known = lists.get(dir).map(|l| l.main.clone());
    lists.retain(|_, l| l.at.elapsed() < RELIST);
    drop(lists);
    // Read before listing: a change while git lists is seen next time.
    let stamp = known.as_deref().and_then(watch_now);
    d.git.listings.fetch_add(1, Ordering::Relaxed);
    let w = worktree::list(Path::new(dir))?;
    let paths: Vec<String> = w.iter().map(|w| super::real(Path::new(&w.path))).collect();
    if let Some(main) = paths.first().cloned() {
        let stamp = if known.as_deref() == Some(main.as_str()) { stamp } else { None };
        let l = Listed { worktrees: w.clone(), paths: paths.clone(), main, stamp, at: Instant::now() };
        d.git.lists.lock().unwrap().insert(dir.to_string(), l);
    }
    Ok((w, paths))
}

/// A worktree's summary, and what it was read at.
pub(crate) struct Known {
    pub summary: Option<worktree::Summary>,
    pub at: Instant,
    stamp: Option<Stamp>,
}

impl Known {
    /// Read without a stamp (on its own, not for the tree): good for `FRESH`.
    pub(crate) fn unstamped(summary: Option<worktree::Summary>) -> Known {
        Known { summary, at: Instant::now(), stamp: None }
    }
}

/// What a summary was read at: it holds while all of these do.
#[derive(Clone, PartialEq, Debug)]
struct Stamp {
    head: String,
    base: String,
    /// The repo's watch, and the changes it had seen in this worktree.
    watch: u64,
    seen: u64,
    /// Changes it had seen to the repo's remote branches.
    remotes: u64,
}

/// One repo's watch: which worktree each change was in.
pub(crate) struct RepoWatch {
    id: u64,
    marks: Arc<Marks>,
    /// The worktrees it files changes under, to know when they change.
    worktrees: Vec<String>,
    roots: Vec<PathBuf>,
    /// None while the repo has no worktrees but its main checkout: no folders to watch.
    _folders: Option<Folders>,
    _git: Option<Folders>,
    used: Instant,
}

#[derive(Default)]
struct Marks {
    lookup: RwLock<Lookup>,
    /// Worktree → changes seen in it.
    seen: Mutex<HashMap<String, u64>>,
    /// Changes the watch may have missed (FSEvents dropped some): everything is read again.
    lost: AtomicU64,
    remotes: AtomicU64,
    /// Changes that may change what `git worktree list` says: a worktree made or removed, a HEAD
    /// or branch moved. Listing a thousand worktrees takes git a third of a second.
    listed: AtomicU64,
}

#[derive(Default)]
struct Lookup {
    worktrees: HashSet<String>,
    /// A worktree's git dir (`<common>/worktrees/<name>`) → the worktree.
    gitdirs: HashMap<String, String>,
    common: String,
    /// The folders watched.
    roots: HashSet<String>,
}

impl Marks {
    /// Changes in folders (not files: a build writing thousands is a few events).
    fn folders(&self, batch: &[(PathBuf, u32)]) {
        let lookup = self.lookup.read().unwrap();
        let mut seen = self.seen.lock().unwrap();
        for (p, flags) in batch {
            if flags & LOST != 0 {
                self.lost.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            let p = p.to_string_lossy();
            match within(&lookup.worktrees, p.trim_end_matches('/')) {
                Some(w) => {
                    *seen.entry(w.to_string()).or_default() += 1;
                    // The worktree's own folder went (or came back): watched on its own.
                    if flags & ROOT_CHANGED != 0 {
                        self.listed.fetch_add(1, Ordering::Relaxed);
                    }
                }
                // The folder holding worktrees: one was made or removed.
                None if lookup.roots.contains(p.trim_end_matches('/')) => {
                    self.listed.fetch_add(1, Ordering::Relaxed);
                }
                None => {}
            }
        }
    }

    /// Changes to files in the repo's git dir: a worktree's index, HEAD or reflog; remote branches.
    fn git(&self, batch: &[(PathBuf, u32)]) {
        let lookup = self.lookup.read().unwrap();
        let mut seen = self.seen.lock().unwrap();
        for (p, flags) in batch {
            if flags & LOST != 0 {
                self.lost.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            let p = p.to_string_lossy();
            let Some(rest) = p.strip_prefix(lookup.common.as_str()).and_then(|r| r.strip_prefix('/')) else { continue };
            // A lock comes and goes with every git run that writes; what it guards changes too.
            if rest.ends_with(".lock") || rest.starts_with("objects/") {
                continue;
            }
            if rest == "packed-refs" {
                self.remotes.fetch_add(1, Ordering::Relaxed);
                self.listed.fetch_add(1, Ordering::Relaxed);
            } else if rest.starts_with("refs/remotes/") {
                self.remotes.fetch_add(1, Ordering::Relaxed);
            } else if rest == "HEAD" || rest.starts_with("refs/heads/") {
                self.listed.fetch_add(1, Ordering::Relaxed);
            } else if let Some(r) = rest.strip_prefix("worktrees/") {
                let (name, file) = r.split_once('/').unwrap_or((r, ""));
                if matches!(file, "" | "HEAD" | "gitdir" | "locked") {
                    self.listed.fetch_add(1, Ordering::Relaxed);
                }
                if let Some(w) = lookup.gitdirs.get(&format!("{}/worktrees/{name}", lookup.common)) {
                    *seen.entry(w.clone()).or_default() += 1;
                }
            }
        }
    }
}

/// The worktree in `set` that `p` is in (or is), if any, without allocating.
fn within<'a>(set: &HashSet<String>, mut p: &'a str) -> Option<&'a str> {
    loop {
        if set.contains(p) {
            return Some(p);
        }
        match p.rfind('/') {
            Some(i) if i > 0 => p = &p[..i],
            _ => return None,
        }
    }
}

/// The folders to watch for `worktrees` of the repo whose main checkout is `main`: a folder
/// holding several of them (`.claude/worktrees`), else each one, never the main checkout or one
/// above it (its own changes don't matter here).
fn roots(main: &str, worktrees: &[String]) -> Vec<PathBuf> {
    let parent = |w: &str| w.rfind('/').map(|i| w[..i].to_string()).unwrap_or_default();
    let mut count: HashMap<String, usize> = HashMap::new();
    for w in worktrees {
        *count.entry(parent(w)).or_default() += 1;
    }
    let covers = |dir: &str, p: &str| p == dir || p.strip_prefix(dir).is_some_and(|r| r.starts_with('/'));
    let mut roots: Vec<String> = worktrees
        .iter()
        .map(|w| {
            let p = parent(w);
            if count[&p] >= 2 && !p.is_empty() && !covers(&p, main) { p } else { w.clone() }
        })
        .collect();
    roots.sort_by_key(|r| r.len());
    roots.dedup();
    let mut out: Vec<String> = Vec::new();
    for r in roots {
        if !out.iter().any(|o| covers(o, &r)) {
            out.push(r);
        }
    }
    out.sort();
    out.into_iter().map(PathBuf::from).collect()
}

/// A stream over `roots` filing changes in `marks`: none for no roots (FSEvents makes none).
fn folders(roots: &[PathBuf], marks: &Arc<Marks>) -> anyhow::Result<Option<Folders>> {
    if roots.is_empty() {
        return Ok(None);
    }
    let marks = marks.clone();
    Ok(Some(Folders::new(roots, move |b| marks.folders(b))?))
}

/// The repo's watch, made or brought up to date with `worktrees`; None when macOS won't watch.
fn watch(d: &Daemon, main: &str, worktrees: &[String]) -> Option<(u64, Arc<Marks>)> {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let mut all = d.git.watches.lock().unwrap();
    all.retain(|_, w| w.used.elapsed() < UNUSED);
    if let Some(w) = all.get_mut(main) {
        w.used = Instant::now();
        if w.worktrees == worktrees {
            return Some((w.id, w.marks.clone()));
        }
    }
    let common = worktree::git_common_dir(Path::new(main)).map(|c| super::real(&c)).unwrap_or_default();
    let roots = roots(main, worktrees);
    let lookup = Lookup {
        worktrees: worktrees.iter().cloned().collect(),
        gitdirs: worktrees.iter().filter_map(|w| worktree::git_dir(Path::new(w)).map(|g| (super::real(&g), w.clone()))).collect(),
        common: common.clone(),
        roots: roots.iter().map(|r| r.to_string_lossy().into_owned()).collect(),
    };
    match all.get_mut(main) {
        // New worktrees: what's known of the others still holds.
        Some(w) => {
            w.marks.seen.lock().unwrap().retain(|k, _| lookup.worktrees.contains(k));
            *w.marks.lookup.write().unwrap() = lookup;
            if w.roots != roots {
                // The new stream starts before the old one stops, so no change falls between.
                let Ok(f) = folders(&roots, &w.marks) else {
                    all.remove(main);
                    return None;
                };
                w._folders = f;
                w.roots = roots;
            }
            w.worktrees = worktrees.to_vec();
            Some((w.id, w.marks.clone()))
        }
        None => {
            let marks = Arc::new(Marks { lookup: RwLock::new(lookup), ..Default::default() });
            let folders = folders(&roots, &marks).ok()?;
            let m = marks.clone();
            let git = (!common.is_empty()).then(|| Folders::files(&[PathBuf::from(&common)], move |b| m.git(b)).ok()).flatten();
            // Without the git dir watched, remote branches moving goes unseen: read on a timer.
            git.as_ref()?;
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            all.insert(
                main.to_string(),
                RepoWatch { id, marks: marks.clone(), worktrees: worktrees.to_vec(), roots, _folders: folders, _git: git, used: Instant::now() },
            );
            Some((id, marks))
        }
    }
}

/// How many git runs for summaries go at once, across every tree being read: enough to read a
/// repo's worth quickly, few enough to leave the Mac's cores to what the user is doing.
fn workers() -> usize {
    std::thread::available_parallelism().map_or(4, |n| (n.get() / 2).clamp(2, 6))
}

/// Taken while a git run for a summary goes; at most `workers()` at once.
struct Slot;

static SLOTS: (Mutex<usize>, Condvar) = (Mutex::new(0), Condvar::new());

impl Slot {
    fn take() -> Slot {
        let (n, cv) = &SLOTS;
        let mut n = cv.wait_while(n.lock().unwrap(), |n| *n >= workers()).unwrap();
        *n += 1;
        Slot
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let (n, cv) = &SLOTS;
        *n.lock().unwrap() -= 1;
        cv.notify_one();
    }
}

enum Job {
    /// Read it all.
    Full,
    /// Only remote branches moved: its unpushed count.
    Unpushed(worktree::Summary),
}

/// Summaries of `w[1..]`, the worktrees of one repo (main checkout first, `paths` their real
/// paths), next to `base`: what's known where nothing changed, read again where something did.
/// `wanted`: whether a worktree gets one at all. `until`: no more reads after then; a worktree
/// never read before is then still to be read (true beside it), the next look reads it.
pub(crate) fn summaries(
    d: &Daemon,
    w: &[worktree::Worktree],
    paths: &[String],
    base: &str,
    wanted: impl Fn(&str) -> bool,
    until: Option<Instant>,
) -> Vec<(Option<worktree::Summary>, bool)> {
    let Some(first) = w.first() else { return Vec::new() };
    let base_tip = first.head.clone().unwrap_or_default();
    let watched = watch(d, &paths[0], &paths[1..]);
    let lost = watched.as_ref().map_or(0, |(_, m)| m.lost.load(Ordering::Relaxed));
    let remotes = watched.as_ref().map_or(0, |(_, m)| m.remotes.load(Ordering::Relaxed));
    let seen: HashMap<String, u64> = watched.as_ref().map(|(_, m)| m.seen.lock().unwrap().clone()).unwrap_or_default();
    let mut out: Vec<(Option<worktree::Summary>, bool)> = vec![(None, false); w.len() - 1];
    let mut jobs: Vec<(usize, Job, Option<Stamp>)> = Vec::new();
    {
        let mut known = d.summaries.lock().unwrap();
        for (i, (wt, path)) in w.iter().zip(paths).enumerate().skip(1) {
            if !wanted(path) {
                continue;
            }
            let k = known.get_mut(path.as_str());
            let stamp = match (&watched, &wt.head) {
                (Some((id, _)), Some(head)) if !base_tip.is_empty() => Some(Stamp {
                    head: head.clone(),
                    base: base_tip.clone(),
                    watch: *id,
                    seen: seen.get(path.as_str()).copied().unwrap_or(0) + lost,
                    remotes,
                }),
                _ => None,
            };
            let job = match (&k, &stamp) {
                (None, _) => Some(Job::Full),
                (Some(k), None) => (k.at.elapsed() >= FRESH).then_some(Job::Full),
                (Some(Known { stamp: None, at, .. }), Some(_)) => (at.elapsed() >= FRESH).then_some(Job::Full),
                (Some(k @ Known { stamp: Some(was), .. }), Some(now)) => {
                    if was == now {
                        None
                    } else if was.head != now.head || was.base != now.base || was.watch != now.watch {
                        Some(Job::Full)
                    } else if was.seen != now.seen {
                        // Still changing: read again once it has had a moment.
                        (k.at.elapsed() >= SETTLE).then_some(Job::Full)
                    } else {
                        match &k.summary {
                            Some(s) if s.ahead > 0 => Some(Job::Unpushed(s.clone())),
                            // Nothing of its own: nothing to push, wherever remote branches moved.
                            Some(_) => None,
                            None => Some(Job::Full),
                        }
                    }
                }
            };
            // What was known stands until it's read again; never read, it's to be read.
            out[i - 1] = match &k {
                Some(k) => (k.summary.clone(), false),
                None => (None, true),
            };
            match job {
                Some(j) => jobs.push((i, j, stamp)),
                None => {
                    if let Some(k) = k {
                        if let (Some(was), Some(now)) = (&mut k.stamp, stamp) {
                            if was.seen == now.seen {
                                *was = now;
                            }
                        }
                    }
                }
            }
        }
    }
    // What history says of them, for all at once: a few git runs instead of a few each.
    let full: Vec<&str> = jobs.iter().filter(|j| matches!(j.1, Job::Full)).filter_map(|j| w[j.0].head.as_deref()).collect();
    let batch = if full.is_empty() || base_tip.is_empty() { worktree::Batch::default() } else { worktree::batch(Path::new(&paths[0]), &full, base) };
    let pushes: Vec<(&str, u32)> =
        jobs.iter().filter_map(|j| match &j.1 { Job::Unpushed(s) => Some((w[j.0].head.as_deref()?, s.ahead)), Job::Full => None }).collect();
    let unpushed = if pushes.is_empty() { HashMap::new() } else { worktree::unpushed_all(Path::new(&paths[0]), &pushes, base) };
    // Read side by side, a few at a time.
    let next = AtomicUsize::new(0);
    let read: Mutex<Vec<(usize, Option<worktree::Summary>, Option<Stamp>)>> = Mutex::default();
    let threads = workers().min(jobs.len());
    std::thread::scope(|sc| {
        for _ in 0..threads {
            sc.spawn(|| {
                loop {
                    if until.is_some_and(|t| Instant::now() >= t) {
                        break;
                    }
                    let n = next.fetch_add(1, Ordering::Relaxed);
                    let Some((i, job, stamp)) = jobs.get(n) else { break };
                    let (wt, path) = (&w[*i], &paths[*i]);
                    let _slot = Slot::take();
                    if matches!(job, Job::Full) {
                        d.git.reads.fetch_add(1, Ordering::Relaxed);
                    }
                    let s = match (job, &wt.head) {
                        (Job::Full, Some(head)) if !base_tip.is_empty() => {
                            worktree::summary_of(Path::new(path), wt.branch.as_deref(), base, head, &base_tip, &batch).ok()
                        }
                        (Job::Full, _) => worktree::summary(Path::new(path), wt.branch.as_deref(), base).ok(),
                        (Job::Unpushed(s), head) => {
                            let mut s = s.clone();
                            s.unpushed = match head.as_deref().and_then(|h| unpushed.get(h)) {
                                Some(&n) => n,
                                None => worktree::unpushed(Path::new(path), base, s.ahead),
                            };
                            Some(s)
                        }
                    };
                    read.lock().unwrap().push((*i, s, stamp.clone()));
                }
            });
        }
    });
    let mut known = d.summaries.lock().unwrap();
    for (i, s, stamp) in read.into_inner().unwrap() {
        out[i - 1] = (s.clone(), false);
        known.insert(paths[i].clone(), Known { summary: s, at: Instant::now(), stamp });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watched_folders() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let p = |v: &[&str]| v.iter().map(PathBuf::from).collect::<Vec<_>>();
        // Claude Code's, inside the repo: their folder, not the main checkout.
        assert_eq!(roots("/r", &s(&["/r/.claude/worktrees/a", "/r/.claude/worktrees/b", "/r/x"])), p(&["/r/.claude/worktrees", "/r/x"]));
        // Next to the main checkout: each one, never the folder holding the main checkout too.
        assert_eq!(roots("/c/r", &s(&["/c/r-a", "/c/r-b"])), p(&["/c/r-a", "/c/r-b"]));
        // dino's own, in a folder of their own.
        assert_eq!(roots("/r", &s(&["/d/w/r/a", "/d/w/r/b", "/d/w/r/a/nested"])), p(&["/d/w/r"]));
    }

    #[test]
    fn changes_filed_under_their_worktree() {
        let marks = Marks::default();
        *marks.lookup.write().unwrap() = Lookup {
            worktrees: ["/r/.claude/worktrees/a".to_string(), "/r/.claude/worktrees/ab".to_string()].into(),
            gitdirs: [("/r/.git/worktrees/a".to_string(), "/r/.claude/worktrees/a".to_string())].into(),
            common: "/r/.git".into(),
            roots: ["/r/.claude/worktrees".to_string()].into(),
        };
        let ev = |p: &str, f: u32| (PathBuf::from(p), f);
        marks.folders(&[ev("/r/.claude/worktrees/a/src/", 0), ev("/r/.claude/worktrees/a", 0), ev("/r/.claude/worktrees/", 0), ev("/elsewhere/", 0)]);
        assert_eq!(marks.seen.lock().unwrap().get("/r/.claude/worktrees/a"), Some(&2));
        assert_eq!(marks.seen.lock().unwrap().get("/r/.claude/worktrees/ab"), None, "a prefix isn't a folder");
        marks.git(&[ev("/r/.git/worktrees/a/index", 0), ev("/r/.git/worktrees/a/index.lock", 0), ev("/r/.git/objects/ab/cdef", 0), ev("/r/.git/index", 0)]);
        assert_eq!(marks.seen.lock().unwrap().get("/r/.claude/worktrees/a"), Some(&3));
        assert_eq!(marks.remotes.load(Ordering::Relaxed), 0);
        marks.git(&[ev("/r/.git/refs/remotes/origin/x", 0), ev("/r/.git/packed-refs", 0)]);
        assert_eq!(marks.remotes.load(Ordering::Relaxed), 2);
        // The worktrees' folder changed (one came or went), and packed-refs did.
        assert_eq!(marks.listed.load(Ordering::Relaxed), 2);
        marks.git(&[ev("/r/.git/worktrees/a/HEAD", 0), ev("/r/.git/refs/heads/x", 0), ev("/r/.git/refs/heads/x.lock", 0), ev("/r/.git/worktrees/new", 0)]);
        assert_eq!(marks.listed.load(Ordering::Relaxed), 5, "a HEAD, a branch, a new worktree: not the lock");
        marks.folders(&[ev("/r/.claude/worktrees/", 0x1)]);
        assert_eq!(marks.lost.load(Ordering::Relaxed), 1);
    }
}
