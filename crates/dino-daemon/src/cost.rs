//! What a session's processes cost the Mac: memory and CPU of its program and everything under
//! it, and of what it runs apart from its terminal (see `procs`), for the sidebar's hover card. Measured only when a client asks (it asks while the card is
//! open), never in the background: with nothing hovered, this costs nothing.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dino_core::ipc::{ProcessCost, SessionCost};
use dino_core::procinfo;

/// Asked again sooner than this, the last measurement stands: two clients don't halve the
/// interval CPU is measured over, nor double the work.
const MIN_INTERVAL: Duration = Duration::from_millis(500);
/// A measurement older than this isn't "now": a first look measures over `FIRST_LOOK` instead.
const FRESH: Duration = Duration::from_secs(3);
const FIRST_LOOK: Duration = Duration::from_millis(250);
/// The recent average covers at least this much, when it's been watched that long.
const AVERAGE: Duration = Duration::from_secs(30);
/// Measurements kept for a session nobody has asked about for this long are dropped.
const FORGET: Duration = Duration::from_secs(5 * 60);

/// Sessions being looked at, by id.
#[derive(Default)]
pub(crate) struct Costs {
    watched: Mutex<HashMap<String, Arc<Mutex<Watch>>>>,
}

#[derive(Default)]
struct Watch {
    /// Oldest first, all of one root process.
    samples: Vec<Sample>,
}

struct Sample {
    /// Its program; 0 once it has ended.
    root: u32,
    at_ns: u64,
    procs: HashMap<u32, Proc>,
}

#[derive(Clone)]
struct Proc {
    parent: u32,
    usage: procinfo::Rusage,
    name: String,
    /// Not under its program: in the background, apart from its terminal.
    apart: bool,
}

impl Sample {
    /// Session `id`'s processes now: its program `root` and everything under it, and what it
    /// runs apart from them.
    fn take(id: &str, root: u32) -> Sample {
        let mut found: Vec<(u32, u32, bool)> = if root > 0 { procinfo::tree(root).into_iter().map(|(p, parent)| (p, parent, false)).collect() } else { vec![] };
        for pid in crate::procs::of_session(id) {
            if !found.iter().any(|f| f.0 == pid) {
                found.push((pid, procinfo::parent_of(pid).unwrap_or(0), true));
            }
        }
        let procs = found
            .into_iter()
            .filter_map(|(pid, parent, apart)| Some((pid, Proc { parent, usage: procinfo::rusage(pid)?, name: procinfo::name(pid).unwrap_or_default(), apart })))
            .collect();
        Sample { root, at_ns: procinfo::now_ns(), procs }
    }

    /// `top` and every process under it, by the parents this sample saw: of a process seen
    /// since, only if it's the same one.
    fn under(&self, top: u32, started_ns: u64) -> HashMap<u32, Proc> {
        let mut out = HashMap::new();
        if !self.procs.get(&top).is_some_and(|p| p.usage.started_ns == started_ns) {
            return out;
        }
        for (&pid, p) in &self.procs {
            let mut at = pid;
            for _ in 0..64 {
                if at == top {
                    out.insert(pid, p.clone());
                    break;
                }
                match self.procs.get(&at) {
                    Some(q) => at = q.parent,
                    None => break,
                }
            }
        }
        out
    }

    fn same(&self, pid: u32, p: &Proc) -> Option<&Proc> {
        self.procs.get(&pid).filter(|q| q.usage.started_ns == p.usage.started_ns)
    }
}

/// CPU the tree used between two samples, in nanoseconds. Each process counts its own time and
/// its ended children's (once it waited for them), so a compiler that came and went between the
/// two still counts, in the build tool that ran it. One seen last time that has ended since is
/// in its parent's children's time now, all of it: what it had then was counted then.
fn used_between(prev: &Sample, now: &Sample) -> u64 {
    let total = |p: &Proc| p.usage.cpu_ns + p.usage.children_ns;
    let mut used: u64 = 0;
    // Parent → its ended children's time, gained since.
    let mut reaped: HashMap<u32, u64> = HashMap::new();
    for (pid, p) in &now.procs {
        match prev.same(*pid, p) {
            Some(q) => {
                let gained = p.usage.children_ns.saturating_sub(q.usage.children_ns);
                used += p.usage.cpu_ns.saturating_sub(q.usage.cpu_ns) + gained;
                reaped.insert(*pid, gained);
            }
            // Started since: all of it is new. Already running and only now under the tree (a
            // pid list read mid-change): nothing to compare with.
            None if p.usage.started_ns >= prev.at_ns => used += total(p),
            None => {}
        }
    }
    // What ended went up to the closest of its ancestors still here: a build tool that ended
    // with its compilers takes theirs up to the shell that waited for it.
    let mut counted: HashMap<u32, u64> = HashMap::new();
    for (pid, q) in &prev.procs {
        if now.same(*pid, q).is_some() {
            continue;
        }
        let mut up = q.parent;
        while let Some(a) = prev.procs.get(&up).filter(|a| now.same(up, a).is_none()) {
            up = a.parent;
        }
        *counted.entry(up).or_default() += total(q);
    }
    // A parent that doesn't wait for its children (they're reaped for it) gains nothing.
    for (parent, before) in counted {
        used -= reaped.get(&parent).map_or(0, |&gained| gained.min(before));
    }
    used
}

fn pct(cpu_ns: u64, wall_ns: u64) -> f64 {
    if wall_ns == 0 { 0.0 } else { cpu_ns as f64 * 100.0 / wall_ns as f64 }
}

/// Own CPU of one process between two samples, as Activity Monitor's row for it shows.
fn own_pct(prev: &Sample, now: &Sample, pid: u32, p: &Proc) -> f64 {
    let (since_ns, before) = match prev.same(pid, p) {
        Some(q) => (prev.at_ns, q.usage.cpu_ns),
        None => (prev.at_ns.max(p.usage.started_ns), 0),
    };
    pct(p.usage.cpu_ns.saturating_sub(before), now.at_ns.saturating_sub(since_ns))
}

fn report(samples: &[Sample]) -> SessionCost {
    let now = samples.last().expect("measured");
    let mut cost = SessionCost {
        mem_bytes: now.procs.values().map(|p| p.usage.footprint).sum(),
        processes: now.procs.len() as u32,
        ..Default::default()
    };
    let Some(prev) = samples.len().checked_sub(2).map(|i| &samples[i]) else { return cost };
    cost.cpu_pct = pct(used_between(prev, now), now.at_ns - prev.at_ns);
    // Back from the newest until it covers `AVERAGE`.
    let (mut used, mut from) = (0, now.at_ns);
    for pair in samples.windows(2).rev() {
        used += used_between(&pair[0], &pair[1]);
        from = pair[0].at_ns;
        if now.at_ns - from >= AVERAGE.as_nanos() as u64 {
            break;
        }
    }
    cost.cpu_avg_pct = pct(used, now.at_ns - from);
    cost.avg_secs = ((now.at_ns - from) as f64 / 1e9).round() as u32;
    cost.top_child = now
        .procs
        .iter()
        .filter(|(pid, _)| **pid != now.root)
        .max_by_key(|(_, p)| p.usage.footprint)
        .map(|(&pid, p)| ProcessCost {
            name: p.name.clone(),
            pid,
            mem_bytes: p.usage.footprint,
            cpu_pct: own_pct(prev, now, pid, p),
            ..Default::default()
        });
    cost.builds = builds(prev, now);
    cost
}

/// The builds in `now`, each the topmost build program of its tree with everything under it, the
/// busiest first.
fn builds(prev: &Sample, now: &Sample) -> Vec<ProcessCost> {
    let is_build = |p: &Proc| crate::procs::is_build(&p.name);
    let mut out: Vec<ProcessCost> = now
        .procs
        .iter()
        .filter(|(_, p)| is_build(p) && !now.procs.get(&p.parent).is_some_and(is_build))
        .map(|(&top, p)| {
            let (a, b) = (prev.under(top, p.usage.started_ns), now.under(top, p.usage.started_ns));
            let cpu = if a.is_empty() {
                pct(b.values().map(|q| q.usage.cpu_ns + q.usage.children_ns).sum(), now.at_ns.saturating_sub(p.usage.started_ns))
            } else {
                pct(used_between(&Sample { root: top, at_ns: prev.at_ns, procs: a }, &Sample { root: top, at_ns: now.at_ns, procs: b.clone() }), now.at_ns - prev.at_ns)
            };
            ProcessCost {
                name: crate::procs::label(top, &p.name),
                pid: top,
                mem_bytes: b.values().map(|q| q.usage.footprint).sum(),
                cpu_pct: cpu,
                background: p.apart,
                ..Default::default()
            }
        })
        .collect();
    out.sort_by(|x, y| y.cpu_pct.total_cmp(&x.cpu_pct).then(x.pid.cmp(&y.pid)));
    out
}

impl Costs {
    /// What session `id`, whose program is `root` (none once it has ended), costs now; none
    /// with nothing of it running.
    pub(crate) fn measure(&self, id: &str, root: Option<u32>) -> Option<SessionCost> {
        let root = root.unwrap_or(0);
        let watch = {
            let mut watched = self.watched.lock().unwrap();
            let now = procinfo::now_ns();
            watched.retain(|_, w| w.lock().unwrap().samples.last().is_some_and(|s| now.saturating_sub(s.at_ns) < FORGET.as_nanos() as u64));
            watched.entry(id.to_string()).or_default().clone()
        };
        let mut w = watch.lock().unwrap();
        // Its program changed (resumed, restarted): what was measured is someone else's.
        if w.samples.last().is_some_and(|s| s.root != root) {
            w.samples.clear();
        }
        let age = w.samples.last().map(|s| Duration::from_nanos(procinfo::now_ns().saturating_sub(s.at_ns)));
        if age.is_none_or(|a| a >= MIN_INTERVAL) {
            if age.is_none_or(|a| a >= FRESH) {
                // CPU "now" needs two looks close together.
                w.samples.push(Sample::take(id, root));
                if w.samples.last().is_some_and(|s| s.procs.is_empty()) {
                    w.samples.clear();
                    return None;
                }
                std::thread::sleep(FIRST_LOOK);
            }
            w.samples.push(Sample::take(id, root));
        }
        // Keep what the average needs: the newest, and back to the first older than `AVERAGE`.
        let newest = w.samples.last().map_or(0, |s| s.at_ns);
        if let Some(cut) = w.samples.iter().rposition(|s| newest - s.at_ns >= AVERAGE.as_nanos() as u64) {
            w.samples.drain(..cut);
        }
        Some(report(&w.samples))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(parent: u32, started_ns: u64, cpu_ns: u64, children_ns: u64, footprint: u64) -> Proc {
        Proc { parent, usage: procinfo::Rusage { footprint, cpu_ns, children_ns, started_ns }, name: String::new(), apart: false }
    }

    fn named(name: &str, apart: bool, p: Proc) -> Proc {
        Proc { name: name.into(), apart, ..p }
    }

    fn sample(at_ns: u64, procs: &[(u32, Proc)]) -> Sample {
        Sample { root: 1, at_ns, procs: procs.iter().cloned().collect() }
    }

    const S: u64 = 1_000_000_000;

    #[test]
    fn counts_children_that_came_and_went() {
        // The agent (1) runs cargo (2), which ran rustc (3): seen once, then ended and waited for.
        let a = sample(10 * S, &[(1, proc(0, 0, S, 0, 100)), (2, proc(1, 9 * S, 0, 0, 10)), (3, proc(2, 9 * S, S / 2, 0, 500))]);
        // rustc used 1.5 s more before it ended; cargo collected all 2 s of it. A new rustc (4)
        // started and used 0.5 s; a pid reused by a stranger isn't the same process.
        let b = sample(12 * S, &[(1, proc(0, 0, S, 0, 100)), (2, proc(1, 9 * S, 0, 2 * S, 10)), (4, proc(2, 11 * S, S / 2, 0, 300))]);
        assert_eq!(used_between(&a, &b), 2 * S);
        let cost = report(&[a, b]);
        assert_eq!(cost.cpu_pct, 100.0);
        assert_eq!((cost.mem_bytes, cost.processes), (410, 3));
        let top = cost.top_child.unwrap();
        // Its own 0.5 s over the 1 s it has run.
        assert_eq!((top.pid, top.mem_bytes, top.cpu_pct), (4, 300, 50.0));
    }

    #[test]
    fn a_build_that_ended_counts_once() {
        // The shell (1) ran cargo (2), running rustc (3) with 4 s used; cargo itself had
        // collected 6 s from rustcs before. Both ended: the shell has all 12 s of the build now
        // (cargo 1 + 6, the last rustc 5).
        let a = sample(10 * S, &[(1, proc(0, 0, S, 0, 1)), (2, proc(1, 2 * S, S, 6 * S, 1)), (3, proc(2, 6 * S, 4 * S, 0, 1))]);
        let b = sample(12 * S, &[(1, proc(0, 0, S, 12 * S, 1))]);
        // Counted before: 7 + 4; since: the last rustc's last 1 s.
        assert_eq!(used_between(&a, &b), S);
    }

    #[test]
    fn builds_show_with_what_runs_under_them() {
        // The agent (1) runs a shell running cargo with a rustc; another cargo runs apart from it
        // (its shell gone), running a test. Pids no process has, so their arguments aren't read.
        let (sh, cargo, rustc, apart, test) = (999_002, 999_003, 999_004, 999_005, 999_006);
        let a = sample(
            10 * S,
            &[
                (1, proc(0, 0, S, 0, 100)),
                (sh, named("zsh", false, proc(1, S, 0, 0, 10))),
                (cargo, named("cargo", false, proc(sh, 2 * S, S, 0, 50))),
                (rustc, named("rustc", false, proc(cargo, 9 * S, S, 0, 500))),
                (apart, named("cargo", true, proc(0, 3 * S, 0, 0, 20))),
                (test, named("dino_daemon-1f2e", true, proc(apart, 8 * S, 2 * S, 0, 30))),
            ],
        );
        // A second later: rustc used half of it, the test all of it.
        let b = sample(
            11 * S,
            &[
                (1, proc(0, 0, S, 0, 100)),
                (sh, named("zsh", false, proc(1, S, 0, 0, 10))),
                (cargo, named("cargo", false, proc(sh, 2 * S, S, 0, 50))),
                (rustc, named("rustc", false, proc(cargo, 9 * S, S + S / 2, 0, 500))),
                (apart, named("cargo", true, proc(0, 3 * S, 0, 0, 20))),
                (test, named("dino_daemon-1f2e", true, proc(apart, 8 * S, 3 * S, 0, 30))),
            ],
        );
        let got: Vec<(u32, u64, f64, bool)> = builds(&a, &b).into_iter().map(|c| (c.pid, c.mem_bytes, c.cpu_pct, c.background)).collect();
        assert_eq!(got, vec![(apart, 50, 100.0, true), (cargo, 550, 50.0, false)]);
        assert_eq!(builds(&a, &b)[0].name, "cargo");
        // One that started since the last look: what it has used since it started.
        let c = sample(11 * S, &[(1, proc(0, 0, S, 0, 100)), (7, named("make", false, proc(1, 10 * S + S / 2, S / 4, 0, 5)))]);
        assert_eq!(builds(&a, &c).into_iter().map(|c| (c.name, c.cpu_pct)).collect::<Vec<_>>(), vec![("make".to_string(), 50.0)]);
    }

    #[test]
    fn a_reused_pid_and_a_leaver_cost_nothing() {
        let a = sample(10 * S, &[(1, proc(0, 0, S, 0, 1)), (5, proc(1, 2 * S, 3 * S, 0, 1))]);
        // 5 is now a different process that started before the last look (moved under the tree
        // mid-read): not counted. The old 5 ended without its parent collecting its time.
        let b = sample(11 * S, &[(1, proc(0, 0, S + S / 10, 0, 1)), (5, proc(1, 4 * S, 7 * S, 0, 1))]);
        assert_eq!(used_between(&a, &b), S / 10);
    }

    #[test]
    fn averages_over_the_last_half_minute() {
        let at = |t: u64, cpu: u64| sample(t * S, &[(1, proc(0, 0, cpu * S, 0, 1))]);
        // Busy on one core for 40 s, then idle for 2.
        let samples = [at(0, 0), at(20, 20), at(40, 40), at(42, 40)];
        let cost = report(&samples);
        assert_eq!(cost.cpu_pct, 0.0);
        assert_eq!(cost.avg_secs, 42 - 20 + 20);
        assert!((cost.cpu_avg_pct - 40.0 / 42.0 * 100.0).abs() < 0.01, "{}", cost.cpu_avg_pct);
    }

    #[test]
    fn measures_a_real_tree() {
        // A shell running a busy job in its own process group (`set -m`), and a sleeper.
        let mut sh = std::process::Command::new("/bin/sh")
            .args(["-c", "set -m; (while :; do :; done) & sleep 30 & wait"])
            .spawn()
            .unwrap();
        let root = sh.id();
        std::thread::sleep(Duration::from_millis(300));
        let costs = Costs::default();
        let first = costs.measure("t", Some(root)).unwrap();
        std::thread::sleep(Duration::from_millis(1000));
        let cost = costs.measure("t", Some(root)).unwrap();
        for (pid, _) in procinfo::tree(root) {
            unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        }
        let _ = sh.kill();
        let _ = sh.wait();
        assert!(first.processes >= 3 && cost.processes >= 3, "{first:?} {cost:?}");
        assert!(cost.mem_bytes > 0);
        // One core busy at most; on a loaded Mac the busy loop may get only a sliver of one, so the
        // lower bound only says it's measured at all.
        assert!(cost.cpu_pct > 1.0 && cost.cpu_pct < 130.0, "{cost:?}");
        // macOS's sh is bash.
        assert!(cost.top_child.as_ref().is_some_and(|c| ["bash", "sh", "sleep"].contains(&c.name.as_str()) && c.mem_bytes > 0), "{cost:?}");
    }
}
