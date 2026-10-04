//! What a session's processes cost the Mac: memory and CPU of its program and everything under
//! it, for the sidebar's hover card. Measured only when a client asks (it asks while the card is
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
    root: u32,
    at_ns: u64,
    procs: HashMap<u32, Proc>,
}

#[derive(Clone, Copy)]
struct Proc {
    parent: u32,
    usage: procinfo::Rusage,
}

impl Sample {
    fn take(root: u32) -> Sample {
        let procs = procinfo::tree(root)
            .into_iter()
            .filter_map(|(pid, parent)| Some((pid, Proc { parent, usage: procinfo::rusage(pid)? })))
            .collect();
        Sample { root, at_ns: procinfo::now_ns(), procs }
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
            name: procinfo::name(pid).unwrap_or_default(),
            pid,
            mem_bytes: p.usage.footprint,
            cpu_pct: own_pct(prev, now, pid, p),
            ..Default::default()
        });
    cost
}

impl Costs {
    /// What session `id`, whose program is `root`, costs now.
    pub(crate) fn measure(&self, id: &str, root: u32) -> SessionCost {
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
                w.samples.push(Sample::take(root));
                std::thread::sleep(FIRST_LOOK);
            }
            w.samples.push(Sample::take(root));
        }
        // Keep what the average needs: the newest, and back to the first older than `AVERAGE`.
        let newest = w.samples.last().map_or(0, |s| s.at_ns);
        if let Some(cut) = w.samples.iter().rposition(|s| newest - s.at_ns >= AVERAGE.as_nanos() as u64) {
            w.samples.drain(..cut);
        }
        report(&w.samples)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(parent: u32, started_ns: u64, cpu_ns: u64, children_ns: u64, footprint: u64) -> Proc {
        Proc { parent, usage: procinfo::Rusage { footprint, cpu_ns, children_ns, started_ns } }
    }

    fn sample(at_ns: u64, procs: &[(u32, Proc)]) -> Sample {
        Sample { root: 1, at_ns, procs: procs.iter().copied().collect() }
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
        let first = costs.measure("t", root);
        std::thread::sleep(Duration::from_millis(1000));
        let cost = costs.measure("t", root);
        for (pid, _) in procinfo::tree(root) {
            unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        }
        let _ = sh.kill();
        let _ = sh.wait();
        assert!(first.processes >= 3 && cost.processes >= 3, "{first:?} {cost:?}");
        assert!(cost.mem_bytes > 0);
        // One core busy, give or take a loaded Mac.
        assert!(cost.cpu_pct > 20.0 && cost.cpu_pct < 130.0, "{cost:?}");
        // macOS's sh is bash.
        assert!(cost.top_child.as_ref().is_some_and(|c| ["bash", "sh", "sleep"].contains(&c.name.as_str()) && c.mem_bytes > 0), "{cost:?}");
    }
}
