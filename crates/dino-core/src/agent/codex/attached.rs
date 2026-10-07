//! Which conversation each Codex in a terminal is on, when Codex's shared background server runs
//! them (`codex app-server --managed-daemon`, Codex 0.160.1). The process in the terminal then has
//! no file of its conversation open, and Codex keeps no record of which terminal runs which (asked
//! for in openai/codex#48880). What it does keep: the conversations its server has loaded, each a
//! lock file named for it (`~/.codex/thread-writer-locks/<id>.lock`, in the Codex home it runs
//! with: `$CODEX_HOME`), made as it loads it and gone once it lets it go; and when each began, in
//! its id (a UUIDv7). Each Codex home has a server of its own.
//!
//! A Codex begins its conversation as it starts, in its folder: its server has it loaded within a
//! fraction of a second, and keeps it loaded while the Codex is on it. One that starts on a Codex
//! home whose server isn't running yet starts that server first, installing it on a fresh home
//! (the first Codex after installing or upgrading Codex): it begins its conversation as the server
//! comes up, seconds later. So a Codex is on the conversation only it could have begun, worked out
//! for every Codex running on this Mac at once: one loaded within [`WITHIN`] after it started, or
//! after the server it waited for came up (never before), in its folder (one with no prompt
//! yet has no folder on record, and could be anyone's). Of the ways they could have begun what's
//! loaded, the ones where the most of them began one count: one that none began is some Codex's
//! that has quit, or a stranger's, the rarer case. What a Codex still in its first [`WITHIN`] (or
//! its server's) is on isn't said: it may not have begun its own yet.
//!
//! Two started at the same moment in one folder, before either had begun its own (or both waiting
//! for the server), could each be on either: dino can't tell, and says so. Nor can it once a conversation is loaded in a Codex's
//! folder since it started that no Codex began as it started: it may have moved to it (`/new`,
//! `/resume`, `/fork` in it).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

use crate::{history, procinfo};

/// How long after a Codex starts its server has begun its conversation, at most (milliseconds):
/// as the terminal's Codex connects (Codex 0.160.1: 85 to 400 ms with its server running, about a
/// second when it starts the server), more on a slow start. A conversation loaded later is no
/// Codex's first.
pub const WITHIN: u64 = 5_000;

/// How long after a Codex starts the server it waits for may come up, at most (milliseconds): it
/// starts one on a Codex home with none running, installing it first on a fresh home (Codex
/// 0.160.1: up 2 to 7 s after the Codex started), after waiting up to 30 s for an install already
/// under way. A server that comes up later is one restarted under Codexes already on their
/// conversations, not one they waited for.
pub const WAIT: u64 = 60_000;

/// Why dino can't tell, for people.
pub const TOGETHER: &str = "Another Codex started in this folder at the same moment, so dino can't tell which conversation is this one's.";
pub const MOVED: &str = "It may have moved to another conversation since it started (/new, /resume or /fork), and dino can't tell which one it's on.";

/// A Codex in a terminal.
#[derive(Debug, Clone)]
pub struct Tui {
    pub pid: u32,
    /// The Codex home it runs with (its `CODEX_HOME`), whose server it works with.
    pub home: PathBuf,
    /// When it started, in milliseconds since the epoch.
    pub started_ms: u64,
    /// The folder it works in (its `-C`, else its own), resolved.
    pub cwd: Option<PathBuf>,
    /// The conversation it's on for sure: one it has open itself (Codex before its shared server,
    /// or run with `--no-daemon`).
    pub open: Option<String>,
    /// The one its command line named (`codex resume <id>`): it began none.
    pub told: Option<String>,
    /// It may have begun none as it started: it was started to pick one (`codex resume`), or
    /// works with another server (`--remote`).
    pub picks: bool,
}

/// A conversation Codex's shared server has loaded.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub id: String,
    /// When it was loaded (its lock made), in milliseconds since the epoch.
    pub at_ms: u64,
    /// Its folder, once its rollout says (from its first prompt on).
    pub cwd: Option<PathBuf>,
    /// It has a rollout: there's a conversation to continue.
    pub written: bool,
}

/// What a Codex is on.
#[derive(Debug, Clone, PartialEq)]
pub enum Attached {
    /// Conversation `id`, as sure as dino can be; it has a rollout once it has a first prompt.
    On(String),
    /// dino can't tell: why, and the conversations (written ones) it may be on.
    Unsure { why: &'static str, maybe: Vec<String> },
    /// None dino can see.
    None,
}

/// What each of `tuis` (of one Codex home) is on, with its server having `loaded` loaded at
/// `now_ms`, up since `up_ms` (when known), `claimed` being others' for sure (dino's own
/// sessions'), and each Codex that holds its own open.
pub fn attach(tuis: &[Tui], loaded: &[Loaded], up_ms: Option<u64>, claimed: &[String], now_ms: u64) -> HashMap<u32, Attached> {
    let mut taken: HashSet<&str> = claimed.iter().map(String::as_str).collect();
    taken.extend(tuis.iter().filter_map(|t| t.open.as_deref().or(t.told.as_deref())));
    let pool: Vec<&Loaded> = loaded.iter().filter(|l| !taken.contains(l.id.as_str())).collect();
    // When each began its own: as it started, or as the server it waited for came up.
    let begins = |t: &Tui| [Some(t.started_ms), up_ms.filter(|&up| t.started_ms < up && up <= t.started_ms + WAIT)].into_iter().flatten();
    let begun_by = |t: &Tui, at_ms: u64| begins(t).any(|from| from < at_ms && at_ms <= from + WITHIN);
    let fits = |t: &Tui, l: &Loaded| begun_by(t, l.at_ms) && (l.cwd.is_none() || l.cwd == t.cwd);

    // The ones that began one as they started, and what each could have begun.
    let open: Vec<&Tui> = tuis.iter().filter(|t| t.open.is_none() && t.told.is_none()).collect();
    let fitting: Vec<Vec<usize>> = open.iter().map(|t| (0..pool.len()).filter(|&i| fits(t, pool[i])).collect()).collect();
    // Each that may have begun none: one that picks, and one still starting (it may not have yet).
    let starting = |t: &Tui| begins(t).any(|from| now_ms <= from + WITHIN);
    let optional: Vec<bool> = open.iter().map(|t| t.picks || starting(t)).collect();
    let picks = possible(&fitting, &optional);

    let mut out = HashMap::new();
    // Whatever one of them may have begun is accounted for; one loaded later that none began, in
    // a folder, is one some Codex there moved to.
    let accounted: HashSet<usize> = picks.iter().flatten().flatten().copied().collect();
    let written = |i: usize| pool[i].written.then(|| pool[i].id.clone());
    for (k, t) in open.iter().enumerate() {
        let maybe: BTreeSet<Option<usize>> = picks[k].clone();
        let attached = match (maybe.len(), maybe.first()) {
            // Still starting, it may not have begun its own yet: not said until its time is up.
            _ if starting(t) && !t.picks => Attached::None,
            (0, _) => Attached::Unsure { why: TOGETHER, maybe: fitting[k].iter().filter_map(|&i| written(i)).collect() },
            (1, Some(Some(i))) => Attached::On(pool[*i].id.clone()),
            (1, _) => Attached::None,
            _ => {
                let maybe: Vec<String> = maybe.iter().flatten().filter_map(|&i| written(i)).collect();
                // None of them has a conversation yet: nothing to continue either way.
                if maybe.is_empty() { Attached::None } else { Attached::Unsure { why: TOGETHER, maybe } }
            }
        };
        out.insert(t.pid, attached);
    }
    for t in tuis.iter().filter(|t| t.open.is_none()) {
        if let Some(id) = &t.told {
            out.insert(t.pid, Attached::On(id.clone()));
        }
    }

    // A Codex in a folder where a conversation has been loaded since it started that no Codex
    // began as it started may be on that one now.
    for t in tuis.iter().filter(|t| t.open.is_none()) {
        let since: Vec<String> = (0..pool.len())
            .filter(|i| !accounted.contains(i))
            .filter(|&i| pool[i].cwd.is_some() && pool[i].cwd == t.cwd && pool[i].at_ms > t.started_ms + WITHIN && !begun_by(t, pool[i].at_ms))
            .map(|i| pool[i].id.clone())
            .collect();
        if since.is_empty() {
            continue;
        }
        let Some(a) = out.get_mut(&t.pid) else { continue };
        let mut maybe = match a {
            Attached::On(id) => vec![id.clone()],
            Attached::Unsure { maybe, .. } => std::mem::take(maybe),
            Attached::None => vec![],
        };
        maybe.extend(since);
        *a = Attached::Unsure { why: MOVED, maybe };
    }
    out
}

/// Every way each Codex could have begun one of its `fitting` (at most one each, none begun by
/// two; every one did, but one that's `optional` may have begun none), of those where the most
/// did (one begun that none of them began is some Codex's that has quit, or a stranger's: the
/// rarer case): what each could have begun across them all (`None`: none). One whose set is empty
/// fits no way at all.
fn possible(fitting: &[Vec<usize>], optional: &[bool]) -> Vec<BTreeSet<Option<usize>>> {
    // Codexes that could have begun the same ones decide each other: worked out together, apart
    // from the rest. A group this big never starts at once by hand: unsure, not worked through.
    const MAX: usize = 10;
    let n = fitting.len();
    let mut group: Vec<usize> = (0..n).collect();
    fn root(g: &mut [usize], i: usize) -> usize {
        if g[i] != i {
            g[i] = root(g, g[i]);
        }
        g[i]
    }
    for a in 0..n {
        for b in a + 1..n {
            if fitting[a].iter().any(|x| fitting[b].contains(x)) {
                let (ra, rb) = (root(&mut group, a), root(&mut group, b));
                group[ra] = rb;
            }
        }
    }
    let mut out = vec![BTreeSet::new(); n];
    let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..n {
        let r = root(&mut group, i);
        groups.entry(r).or_default().push(i);
    }
    for members in groups.values() {
        if members.len() > MAX {
            continue;
        }
        let mut chosen = vec![None; members.len()];
        let mut used = HashSet::new();
        let mut ways: Vec<Vec<Option<usize>>> = vec![];
        each_way(members, 0, fitting, optional, &mut chosen, &mut used, &mut |c| ways.push(c.to_vec()));
        let most = ways.iter().map(|w| w.iter().flatten().count()).max().unwrap_or(0);
        for w in ways.iter().filter(|w| w.iter().flatten().count() == most) {
            for (k, &m) in members.iter().enumerate() {
                out[m].insert(w[k]);
            }
        }
    }
    out
}

fn each_way(
    members: &[usize],
    k: usize,
    fitting: &[Vec<usize>],
    optional: &[bool],
    chosen: &mut Vec<Option<usize>>,
    used: &mut HashSet<usize>,
    found: &mut dyn FnMut(&[Option<usize>]),
) {
    let Some(&m) = members.get(k) else {
        found(chosen);
        return;
    };
    if optional[m] || fitting[m].is_empty() {
        chosen[k] = None;
        each_way(members, k + 1, fitting, optional, chosen, used, found);
    }
    for &x in &fitting[m] {
        if used.insert(x) {
            chosen[k] = Some(x);
            each_way(members, k + 1, fitting, optional, chosen, used, found);
            used.remove(&x);
        }
    }
}

/// When conversation `id` began, in milliseconds since the epoch: Codex's ids are UUIDv7s, which
/// begin with it. `None` for an id of another kind.
pub fn begun_ms(id: &str) -> Option<u64> {
    let hex: String = id.split('-').take(2).collect();
    (hex.len() == 12 && id.as_bytes().get(14) == Some(&b'7')).then(|| u64::from_str_radix(&hex, 16).ok()).flatten()
}

/// What we know of a rollout once it's written: it never moves, and its first line says the rest.
#[derive(Clone)]
struct Written {
    path: PathBuf,
    cwd: Option<PathBuf>,
    /// A subagent's, or a headless run's (`codex exec`): no terminal's.
    hidden: bool,
}

static WRITTEN: Mutex<Option<HashMap<String, Written>>> = Mutex::new(None);

/// The conversations Codex's shared server (of Codex home `home`) has loaded now: its writer
/// locks. Those of a subagent or a headless run aren't a terminal's, and aren't listed.
pub fn loaded_in(home: &Path) -> Vec<Loaded> {
    let dir = home.join("thread-writer-locks");
    let mut out = vec![];
    let mut seen = HashSet::new();
    for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let name = e.file_name();
        let Some(id) = name.to_str().and_then(|n| n.strip_suffix(".lock")) else { continue };
        let Some(begun) = begun_ms(id) else { continue };
        // Made as it was loaded (again, for one resumed after it was let go); its id's time else.
        let at_ms = e.metadata().ok().and_then(|m| m.created().ok()).and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(begun, |d| d.as_millis() as u64);
        seen.insert(id.to_string());
        let written = written(home, id, begun);
        if written.as_ref().is_some_and(|w| w.hidden) {
            continue;
        }
        out.push(Loaded { id: id.to_string(), at_ms, written: written.is_some(), cwd: written.and_then(|w| w.cwd) });
    }
    // Forget the ones it let go.
    if let Some(w) = WRITTEN.lock().unwrap().as_mut() {
        w.retain(|id, w| seen.contains(id) || !w.path.starts_with(home));
    }
    out
}

/// Conversation `id`'s rollout, as its first line describes it, once written.
fn written(home: &Path, id: &str, begun_ms: u64) -> Option<Written> {
    if let Some(w) = WRITTEN.lock().unwrap().get_or_insert_default().get(id) {
        return Some(w.clone());
    }
    let path = super::rollout_in(&home.join("sessions"), id, begun_ms / 1000)?;
    let meta = history::codex_meta(&path);
    let w = Written { cwd: meta.cwd.map(|c| resolved(Path::new(&c))), hidden: meta.hidden, path };
    WRITTEN.lock().unwrap().get_or_insert_default().insert(id.to_string(), w.clone());
    Some(w)
}

/// When Codex's shared server (of Codex home `home`) came up, in milliseconds since the
/// epoch, while it runs: the process its record names (`app-server-daemon/daemon.pid`, or
/// `app-server.pid` where Codex runs it from its standalone package), still the one that started
/// when the record says (its pid may be another process's since).
pub fn up_in(home: &Path) -> Option<u64> {
    let dir = home.join("app-server-daemon");
    ["daemon.pid", "app-server.pid"].iter().find_map(|f| {
        let record: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join(f)).ok()?).ok()?;
        let started = &record["processIdentity"];
        let started_us = started["startSeconds"].as_u64()? * 1_000_000 + started["startMicroseconds"].as_u64()?;
        procinfo::alive(u32::try_from(record["pid"].as_u64()?).ok()?, started_us).then_some(started_us / 1000)
    })
}

/// Conversation `id` is loaded by a Codex server of Codex home `home` (its lock is there).
pub fn in_server(home: &Path, id: &str) -> bool {
    begun_ms(id).is_some() && home.join("thread-writer-locks").join(format!("{id}.lock")).exists()
}

/// Loaded conversation `id`'s rollout, once [`loaded_in`] has seen it written.
pub fn rollout(id: &str) -> Option<PathBuf> {
    WRITTEN.lock().unwrap().as_ref()?.get(id).map(|w| w.path.clone())
}

/// `p` with links resolved, as the kernel names a process's folder.
pub fn resolved(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tui(pid: u32, started_ms: u64, cwd: &str) -> Tui {
        Tui { pid, home: PathBuf::new(), started_ms, cwd: Some(PathBuf::from(cwd)), open: None, told: None, picks: false }
    }

    fn loaded(id: &str, at_ms: u64, cwd: Option<&str>) -> Loaded {
        Loaded { id: id.into(), at_ms, cwd: cwd.map(PathBuf::from), written: cwd.is_some() }
    }

    const LATER: u64 = 60_000;

    fn on(id: &str) -> Attached {
        Attached::On(id.into())
    }

    fn unsure(why: &'static str, maybe: &[&str]) -> Attached {
        Attached::Unsure { why, maybe: maybe.iter().map(|s| s.to_string()).collect() }
    }

    #[test]
    fn a_codex_is_on_the_one_it_began_as_it_started() {
        let a = attach(&[tui(1, 1000, "/w")], &[loaded("a", 1300, Some("/w"))], None, &[], LATER);
        assert_eq!(a[&1], on("a"));
        // Not one begun before it started, nor in another folder.
        for l in [loaded("a", 900, Some("/w")), loaded("a", 1300, Some("/x"))] {
            assert_eq!(attach(&[tui(1, 1000, "/w")], &[l], None, &[], LATER)[&1], Attached::None);
        }
        // One loaded there long after it started is no Codex's first: it may have moved to it.
        assert_eq!(attach(&[tui(1, 1000, "/w")], &[loaded("a", 1000 + WITHIN + 1, Some("/w"))], None, &[], LATER)[&1], unsure(MOVED, &["a"]));
        // One with no prompt yet (no folder on record) is its own too: nothing to continue yet.
        assert_eq!(attach(&[tui(1, 1000, "/w")], &[loaded("a", 1300, None)], None, &[], LATER)[&1], on("a"));
    }

    /// Two started a second apart in one folder: each began its own before the other started, so
    /// each is on its own. Started together, before either began one: either could be on either.
    #[test]
    fn two_started_together_in_one_folder() {
        let apart = attach(&[tui(1, 1000, "/w"), tui(2, 2000, "/w")], &[loaded("a", 1300, Some("/w")), loaded("b", 2300, Some("/w"))], None, &[], LATER);
        assert_eq!((apart[&1].clone(), apart[&2].clone()), (on("a"), on("b")));
        let together = attach(&[tui(1, 1000, "/w"), tui(2, 1100, "/w")], &[loaded("a", 1300, Some("/w")), loaded("b", 1400, Some("/w"))], None, &[], LATER);
        assert_eq!(together[&1], unsure(TOGETHER, &["a", "b"]));
        assert_eq!(together[&2], unsure(TOGETHER, &["a", "b"]));
        // In two folders they're told apart by their folders.
        let two = attach(&[tui(1, 1000, "/w"), tui(2, 1100, "/x")], &[loaded("a", 1300, Some("/w")), loaded("b", 1400, Some("/x"))], None, &[], LATER);
        assert_eq!((two[&1].clone(), two[&2].clone()), (on("a"), on("b")));
        // One with no prompt yet could be either's, but the other's folder says which is whose.
        let one_prompted = attach(&[tui(1, 1000, "/w"), tui(2, 1100, "/x")], &[loaded("a", 1300, None), loaded("b", 1400, Some("/x"))], None, &[], LATER);
        assert_eq!((one_prompted[&1].clone(), one_prompted[&2].clone()), (on("a"), on("b")));
        // Together in one folder, one prompted: either could be on it.
        let same = attach(&[tui(1, 1000, "/w"), tui(2, 1100, "/w")], &[loaded("a", 1300, None), loaded("b", 1400, Some("/w"))], None, &[], LATER);
        assert_eq!((same[&1].clone(), same[&2].clone()), (unsure(TOGETHER, &["b"]), unsure(TOGETHER, &["b"])));
        // Neither prompted: nothing to continue either way.
        let neither = attach(&[tui(1, 1000, "/w"), tui(2, 1100, "/w")], &[loaded("a", 1300, None), loaded("b", 1400, None)], None, &[], LATER);
        assert_eq!((neither[&1].clone(), neither[&2].clone()), (Attached::None, Attached::None));
    }

    /// One still starting may not have begun its own yet: what it's on isn't said until its time
    /// is up. Meanwhile an older one in its folder, on one begun before it started, stays on it.
    #[test]
    fn one_still_starting_may_not_have_begun_yet() {
        let tuis = [tui(1, 1000, "/w"), tui(2, 1100, "/w")];
        let early = attach(&tuis, &[loaded("a", 1300, Some("/w"))], None, &[], 1350);
        assert_eq!((early[&1].clone(), early[&2].clone()), (Attached::None, Attached::None));
        // Its time up, still only one begun: there's no way each began one.
        let late = attach(&tuis, &[loaded("a", 1300, Some("/w"))], None, &[], LATER);
        assert_eq!((late[&1].clone(), late[&2].clone()), (unsure(TOGETHER, &["a"]), unsure(TOGETHER, &["a"])));
        // One started late in the first's time: the first stays on its own as the second starts
        // (and may not have begun yet), and each is on its own once the second's time is up.
        let (first, second) = (tui(1, 1000, "/w"), tui(2, 5500, "/w"));
        let both = [loaded("a", 1200, Some("/w")), loaded("b", 5700, Some("/w"))];
        for (now, n) in [(6100, 1), (6100, 2), (9000, 2)] {
            let a = attach(&[first.clone(), second.clone()], &both[..n], None, &[], now);
            assert_eq!((a[&1].clone(), a[&2].clone()), (on("a"), Attached::None), "at {now} with {n}");
        }
        let a = attach(&[first, second], &both, None, &[], LATER);
        assert_eq!((a[&1].clone(), a[&2].clone()), (on("a"), on("b")));
    }

    #[test]
    fn what_it_has_open_or_was_told_wins_and_is_no_one_elses() {
        let mut old = tui(1, 1000, "/w");
        old.open = Some("a".into());
        let mut told = tui(2, 1100, "/w");
        told.told = Some("b".into());
        let a = attach(&[old, told, tui(3, 1050, "/w")], &[loaded("a", 1300, Some("/w")), loaded("b", 1200, Some("/w")), loaded("c", 1400, Some("/w"))], None, &[], LATER);
        assert!(!a.contains_key(&1), "one with its own open needs no working out");
        assert_eq!((a[&2].clone(), a[&3].clone()), (on("b"), on("c")));
        // dino's own sessions' are no one else's.
        assert_eq!(attach(&[tui(3, 1050, "/w")], &[loaded("c", 1400, Some("/w"))], None, &["c".into()], LATER)[&3], Attached::None);
    }

    /// `/new` in it (or `/resume`, `/fork`): a conversation loaded later in its folder that no
    /// Codex began as it started. Its own stays loaded, so it can't be told from the new one.
    #[test]
    fn one_that_may_have_moved_on() {
        let tuis = [tui(1, 1000, "/w")];
        let moved = attach(&tuis, &[loaded("a", 1300, Some("/w")), loaded("n", 30_000, Some("/w"))], None, &[], LATER);
        assert_eq!(moved[&1], unsure(MOVED, &["a", "n"]));
        // A later Codex's own, there, is that one's; one elsewhere, or not yet prompted, says nothing.
        let other = attach(&[tui(1, 1000, "/w"), tui(2, 29_800, "/w")], &[loaded("a", 1300, Some("/w")), loaded("n", 30_000, Some("/w"))], None, &[], LATER);
        assert_eq!((other[&1].clone(), other[&2].clone()), (on("a"), on("n")));
        for n in [loaded("n", 30_000, Some("/x")), loaded("n", 30_000, None)] {
            assert_eq!(attach(&tuis, &[loaded("a", 1300, Some("/w")), n], None, &[], LATER)[&1], on("a"));
        }
        // One resumed by name, then moved on.
        let mut told = tui(1, 1000, "/w");
        told.told = Some("r".into());
        assert_eq!(attach(&[told], &[loaded("r", 1200, Some("/w")), loaded("n", 30_000, Some("/w"))], None, &[], LATER)[&1], unsure(MOVED, &["r", "n"]));
    }

    /// One started to pick a conversation (`codex resume`) began none as it started, but is on one
    /// it loaded then that no one else could have.
    #[test]
    fn one_that_picks_may_have_begun_none() {
        let mut picker = tui(1, 1000, "/w");
        picker.picks = true;
        let a = attach(&[picker.clone(), tui(2, 1100, "/w")], &[loaded("b", 1400, Some("/w"))], None, &[], LATER);
        assert_eq!((a[&1].clone(), a[&2].clone()), (Attached::None, on("b")), "the other began that one, so it picked none yet");
        assert_eq!(attach(&[picker], &[loaded("r", 4000, Some("/w"))], None, &[], LATER)[&1], on("r"), "it picked that one");
    }

    /// The first Codex on a fresh Codex home installs and starts its server first (Codex 0.160.1:
    /// up 6 s after it started): it begins its conversation as that server comes up.
    #[test]
    fn one_that_waited_for_its_server_began_its_own_as_it_came_up() {
        let (cold, up) = ([tui(1, 1000, "/w")], Some(7000));
        let its = [loaded("a", 7900, Some("/w"))];
        assert_eq!(attach(&cold, &its, up, &[], LATER)[&1], on("a"));
        // Not said while it may not have begun its own yet: up to [`WITHIN`] after its server came up.
        assert_eq!(attach(&cold, &[], up, &[], 7500)[&1], Attached::None);
        assert_eq!(attach(&cold, &its, up, &[], 8000)[&1], Attached::None);
        // Not one loaded before it started; one loaded there later than that may be one it moved to.
        let before = [its[0].clone(), loaded("n", 900, Some("/w"))];
        assert_eq!(attach(&cold, &before, up, &[], LATER)[&1], on("a"));
        let after = [its[0].clone(), loaded("n", 7000 + WITHIN + 1, Some("/w"))];
        assert_eq!(attach(&cold, &after, up, &[], LATER)[&1], unsure(MOVED, &["a", "n"]));
        // With no server on record, it may have moved to it (as before).
        assert_eq!(attach(&cold, &its, None, &[], LATER)[&1], unsure(MOVED, &["a"]));
        // Another Codex's there, as it came up, could be either's: dino can't tell.
        let stranger = [its[0].clone(), loaded("s", 7400, Some("/w"))];
        assert_eq!(attach(&cold, &stranger, up, &[], LATER)[&1], unsure(TOGETHER, &["a", "s"]));
        // A later one, on the server up by then, is on its own; the first stays on its own.
        let later = attach(&[cold[0].clone(), tui(2, 9000, "/w")], &[its[0].clone(), loaded("b", 9400, Some("/w"))], up, &[], LATER);
        assert_eq!((later[&1].clone(), later[&2].clone()), (on("a"), on("b")));
    }

    /// Two started a second apart in one folder, both waiting for the server to come up, each began
    /// its own as it came up: either could be on either. With the server up, each is on its own.
    #[test]
    fn two_that_waited_for_their_server_together() {
        let tuis = [tui(1, 1000, "/w"), tui(2, 2000, "/w")];
        let a = attach(&tuis, &[loaded("a", 7900, Some("/w")), loaded("b", 8000, Some("/w"))], Some(7000), &[], LATER);
        assert_eq!((a[&1].clone(), a[&2].clone()), (unsure(TOGETHER, &["a", "b"]), unsure(TOGETHER, &["a", "b"])));
        let warm = attach(&tuis, &[loaded("a", 1300, Some("/w")), loaded("b", 2300, Some("/w"))], Some(500), &[], LATER);
        assert_eq!((warm[&1].clone(), warm[&2].clone()), (on("a"), on("b")));
    }

    /// A server that comes up long after a Codex started is one restarted under it, not one it
    /// waited for: one loaded there as it comes up may be its own again or another's (as before).
    #[test]
    fn a_server_restarted_long_after_it_started_is_none_it_waited_for() {
        let up = Some(1000 + WAIT + 1);
        let a = attach(&[tui(1, 1000, "/w")], &[loaded("s", 1000 + WAIT + 400, Some("/w"))], up, &[], LATER + WAIT);
        assert_eq!(a[&1], unsure(MOVED, &["s"]));
        // One it began as it started stays its own.
        let a = attach(&[tui(1, 1000, "/w")], &[loaded("a", 1300, Some("/w"))], up, &[], LATER + WAIT);
        assert_eq!(a[&1], on("a"));
    }

    /// When the server came up: the process its record names, while it's the one that started then.
    #[test]
    fn when_its_server_came_up() {
        let home = std::env::temp_dir().join(format!("dino-codex-up-{}", std::process::id()));
        let dir = home.join("app-server-daemon");
        std::fs::create_dir_all(&dir).unwrap();
        let me = procinfo::process(std::process::id()).unwrap();
        let record = |started_us: u64| format!(r#"{{"pid":{},"processStartTime":"Tue Oct  6 19:14:36 2026","processIdentity":{{"bootId":"C14C44A7","uniqueId":1,"startSeconds":{},"startMicroseconds":{}}}}}"#, me.pid, started_us / 1_000_000, started_us % 1_000_000);
        assert_eq!(up_in(&home), None, "no record");
        std::fs::write(dir.join("daemon.pid"), record(me.started_us)).unwrap();
        assert_eq!(up_in(&home), Some(me.started_us / 1000));
        std::fs::write(dir.join("daemon.pid"), record(me.started_us - 1)).unwrap();
        assert_eq!(up_in(&home), None, "its pid is another process's now");
        std::fs::write(dir.join("daemon.pid"), "").unwrap();
        assert_eq!(up_in(&home), None, "still starting");
        std::fs::write(dir.join("app-server.pid"), record(me.started_us)).unwrap();
        assert_eq!(up_in(&home), Some(me.started_us / 1000), "Codex run from its standalone package");
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn begun_from_its_id() {
        assert_eq!(begun_ms("01a11353-2b00-7bb1-bbdf-74c0582c3b78"), Some(0x01a113532b00));
        assert_eq!(begun_ms("8b0a3c52-29a1-4e37-9a1c-5d1a1f1b2c3d"), None, "not a UUIDv7");
    }
}
