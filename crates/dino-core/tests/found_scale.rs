//! "On this Mac" at scale: 150+ agent processes of every sort running, the scan stays within its
//! budget. dinod scans when the app asks, every 3 s while it's open, so a scan's CPU over 3 s is
//! what "On this Mac" costs dinod at idle.

mod lab;

use std::time::{Duration, Instant};

use lab::{Fake, Lab, strings};

/// A scan, wall time (median), in a release build.
const SCAN_MS: f64 = 20.0;
/// dinod's CPU at idle from scans, with the app asking every 3 s.
const IDLE_CPU_PERCENT: f64 = 0.2;
const POLL: Duration = Duration::from_secs(3);

fn cpu_secs() -> f64 {
    let mut total = 0.0;
    for who in [libc::RUSAGE_SELF, libc::RUSAGE_CHILDREN] {
        let mut u: libc::rusage = unsafe { std::mem::zeroed() };
        unsafe { libc::getrusage(who, &mut u) };
        let t = |v: libc::timeval| v.tv_sec as f64 + v.tv_usec as f64 / 1e6;
        total += t(u.ru_utime) + t(u.ru_stime);
    }
    total
}

#[test]
fn scans_150_agents_within_budget() {
    let mut lab = Lab::new("scale");
    let work = lab.dir("any");
    let mut listed = 0;
    for i in 0..40 {
        let p = lab.spawn(Fake { name: "claude", tty: true, ..Fake::default() });
        lab.claude_session(p, &format!("c-{i}"), &work, "interactive", lab::now_ms());
        listed += 1;
    }
    for i in 0..30 {
        let id = format!("01a0e93b-2fcf-7a20-8efb-{i:012}");
        lab.spawn(Fake { name: "codex", tty: true, open: Some(lab.codex_rollout(&id)), ..Fake::default() });
        listed += 1;
    }
    for i in 0..20 {
        let p = lab.spawn(Fake { name: "claude", tty: true, args: strings(&["-p", "x"]), ..Fake::default() });
        lab.claude_session(p, &format!("p-{i}"), &work, "interactive", lab::now_ms());
    }
    for i in 0..20 {
        let id = format!("01a0e93b-2fcf-7a20-8efc-{i:012}");
        lab.spawn(Fake { name: "codex", args: strings(&["app-server"]), open: Some(lab.codex_rollout(&id)), ..Fake::default() });
    }
    for i in 0..10 {
        let d = lab.spawn(Fake { name: "dino", args: strings(&["daemon"]), child: Some(Box::new(Fake { name: "claude", tty: true, ..Fake::default() })), ..Fake::default() });
        let p = lab.children(d, 1)[0];
        lab.claude_session(p, &format!("d-{i}"), &work, "interactive", lab::now_ms());
    }
    for _ in 0..20 {
        lab.spawn(Fake { name: "zsh", tty: true, ..Fake::default() });
    }
    // Agents with helpers under them.
    for i in 0..10 {
        let id = format!("01a0e93b-2fcf-7a20-8efd-{i:012}");
        lab.spawn(Fake { name: "claude", tty: true, child: Some(Box::new(Fake { name: "codex", open: Some(lab.codex_rollout(&id)), ..Fake::default() })), ..Fake::default() });
    }
    let started = lab::all_processes_of(&lab);
    assert!(started >= 150, "{started} processes");
    std::thread::sleep(found_min_age() + Duration::from_millis(500));

    // Two to be listed; then measured.
    lab.scan();
    assert_eq!(lab.scan().len(), listed);
    let (mut walls, cpu0) = (vec![], cpu_secs());
    let n = 30;
    for _ in 0..n {
        let t = Instant::now();
        let got = lab.scan_all();
        walls.push(t.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(got, listed);
    }
    let cpu_ms = (cpu_secs() - cpu0) * 1000.0 / n as f64;
    walls.sort_by(f64::total_cmp);
    let median = walls[n / 2];
    let p95 = walls[n * 95 / 100];
    let idle = cpu_ms / POLL.as_secs_f64() / 10.0;
    eprintln!(
        "found scan with {started} lab processes ({} on this Mac): median {median:.1} ms, p95 {p95:.1} ms, max {:.1} ms; CPU {cpu_ms:.1} ms a scan, {idle:.3} % of a core at a scan every {POLL:?}",
        dino_core::procinfo::processes().len(),
        walls[n - 1]
    );
    if cfg!(debug_assertions) {
        eprintln!("(a debug build: the budget is for release builds)");
        return;
    }
    assert!(median <= SCAN_MS, "a scan took {median:.1} ms (budget {SCAN_MS} ms)");
    assert!(idle <= IDLE_CPU_PERCENT, "scans cost {idle:.3} % at idle (budget {IDLE_CPU_PERCENT} %)");
}

fn found_min_age() -> Duration {
    dino_core::found::MIN_AGE
}
