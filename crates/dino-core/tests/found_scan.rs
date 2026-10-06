//! "On this Mac": what a scan lists, against real processes of every kind an agent runs as. Each
//! agent's stand-in has the name, arguments, terminal (or none), parents and records the real one
//! would; the scan must list exactly the ones a person could take over, the same way every time.

mod lab;

use std::collections::HashSet;
use std::time::{Duration, Instant};

use dino_core::found::{self, LINGER, MIN_AGE};
use dino_core::procinfo;
use lab::{Fake, Lab, strings};

fn tty(name: &str) -> Fake<'_> {
    Fake { name, tty: true, ..Fake::default() }
}

#[test]
fn lists_exactly_the_agents_a_person_could_take_over() {
    let mut lab = Lab::new("matrix");
    // Codex's conversations.
    let [u1, u2, u3, u4, u5, u6] = [1, 2, 3, 4, 5, 6].map(|n| lab.uuid(n));
    let t0 = Instant::now();
    let work = lab.dir("any");
    // (pid, agent, conversation): what must be listed.
    let mut want: Vec<(u32, &str, String)> = vec![];
    // Processes that must never be listed.
    let mut never: Vec<u32> = vec![];

    // ---- Listed ----
    // Claude Code in a terminal.
    let p = lab.spawn(tty("claude"));
    lab.claude_session(p, "c-tty", &work, "interactive", lab::now_ms());
    want.push((p, "claude", "c-tty".into()));
    let claude_tty = p;
    // Codex in a terminal, its conversation open.
    let p = lab.spawn(Fake { open: Some(lab.codex_rollout(&u1)), ..tty("codex") });
    want.push((p, "codex", u1.clone()));
    let codex_tty = p;
    // Claude Code in a tmux pane: the server has no terminal; the pane is one.
    let server = lab.spawn(Fake { name: "tmux", args: strings(&["new-session", "-d"]), child: Some(Box::new(tty("claude"))), ..Fake::default() });
    let p = lab.children(server, 1)[0];
    lab.claude_session(p, "c-tmux", &work, "interactive", lab::now_ms());
    want.push((p, "claude", "c-tmux".into()));
    let in_tmux = p;
    // Kimi Code, Copilot, OpenCode, CodeWhale: each in a terminal, with a conversation begun since.
    let (dk, dop, dw) = (lab.dir("kimi"), lab.dir("opencode"), lab.dir("whale"));
    let p = lab.spawn(Fake { cwd: Some(dk.clone()), ..tty("kimi-code") });
    want.push((p, "kimi", "k-1".into()));
    let kimi = p;
    let p = lab.spawn(tty("copilot"));
    lab.copilot_session(p, "cp-1");
    want.push((p, "copilot", "cp-1".into()));
    let p = lab.spawn(Fake { cwd: Some(dop.clone()), ..tty("opencode") });
    want.push((p, "opencode", "ses_o1".into()));
    let p = lab.spawn(Fake { cwd: Some(dw.clone()), ..tty("codewhale") });
    want.push((p, "codewhale", "w-1".into()));
    // Cursor Agent and Amp at their first screen: no conversation yet, "starting".
    let p = lab.spawn(tty("cursor-agent"));
    want.push((p, "cursor", String::new()));
    let p = lab.spawn(tty("amp"));
    want.push((p, "amp", String::new()));
    // A launcher (Codex's Node wrapper, say) and the agent it runs: the agent, once.
    let wrapper = lab.spawn(Fake { child: Some(Box::new(Fake { name: "codex", open: Some(lab.codex_rollout(&u2)), ..Fake::default() })), ..tty("codex") });
    let p = lab.children(wrapper, 1)[0];
    want.push((p, "codex", u2.clone()));
    never.push(wrapper);
    // Two processes on one conversation: one row, the older process.
    let p = lab.spawn(tty("claude"));
    lab.claude_session(p, "c-dup", &work, "interactive", lab::now_ms());
    want.push((p, "claude", "c-dup".into()));
    std::thread::sleep(Duration::from_millis(20));
    let p = lab.spawn(tty("claude"));
    lab.claude_session(p, "c-dup", &work, "interactive", lab::now_ms());
    never.push(p);
    // An agent and an agent it started (a worker, a subagent): only the one in the terminal.
    let parent = lab.spawn(Fake { child: Some(Box::new(Fake { name: "codex", open: Some(lab.codex_rollout(&u3)), ..Fake::default() })), ..tty("claude") });
    lab.claude_session(parent, "c-parent", &work, "interactive", lab::now_ms());
    want.push((parent, "claude", "c-parent".into()));
    never.push(lab.children(parent, 1)[0]);
    // Node programs: Pi (titled "pi") and Qwen Code (by its record).
    let dp = lab.dir("pi");
    let (pi_node, qwen_node) = if let Some(node) = lab.node.clone() {
        let keep = "process.title='pi';setTimeout(()=>{},600000)".to_string();
        let p = lab.spawn(Fake { cwd: Some(dp.clone()), exec: Some((node.clone(), vec!["node".into(), "-e".into(), keep])), ..tty("zsh") });
        lab.wait_named(p, "node");
        want.push((p, "pi", "pi-1".into()));
        let keep = "setTimeout(()=>{},600000)".to_string();
        let q = lab.spawn(Fake { exec: Some((node.clone(), vec!["node".into(), "-e".into(), keep.clone()])), ..tty("zsh") });
        lab.wait_named(q, "node");
        lab.qwen_session(q, "q-1", &work);
        want.push((q, "qwen", "q-1".into()));
        // Qwen run once: `-p`.
        let q2 = lab.spawn(Fake { exec: Some((node, vec!["node".into(), "-e".into(), keep, "-p".into(), "hi".into()])), ..tty("zsh") });
        lab.wait_named(q2, "node");
        lab.qwen_session(q2, "q-2", &work);
        never.push(q2);
        (Some(p), Some(q))
    } else {
        eprintln!("no node: the Node agents (Pi, Qwen) aren't tried");
        (None, None)
    };

    // ---- Not listed ----
    // Run once: `claude -p`, `codex exec`, and each agent's own way.
    let p = lab.spawn(Fake { args: strings(&["-p", "hello"]), ..tty("claude") });
    lab.claude_session(p, "c-print", &work, "interactive", lab::now_ms());
    never.push(p);
    never.push(lab.spawn(Fake { args: strings(&["exec", "hello"]), open: Some(lab.codex_rollout(&u4)), ..tty("codex") }));
    let (dk2, dop2, dw2, dp2) = (lab.dir("kimi2"), lab.dir("opencode2"), lab.dir("whale2"), lab.dir("pi2"));
    never.push(lab.spawn(Fake { args: strings(&["--print", "-p", "hi"]), cwd: Some(dk2.clone()), ..tty("kimi-code") }));
    let p = lab.spawn(Fake { args: strings(&["-p", "hi"]), ..tty("copilot") });
    lab.copilot_session(p, "cp-2");
    never.push(p);
    never.push(lab.spawn(Fake { args: strings(&["run", "hi"]), cwd: Some(dop2.clone()), ..tty("opencode") }));
    never.push(lab.spawn(Fake { args: strings(&["exec", "hi"]), cwd: Some(dw2.clone()), ..tty("codewhale") }));
    never.push(lab.spawn(Fake { args: strings(&["-p", "hi"]), cwd: Some(dp2.clone()), ..tty("pi") }));
    never.push(lab.spawn(Fake { args: strings(&["-p", "hi"]), ..tty("cursor-agent") }));
    never.push(lab.spawn(Fake { args: strings(&["-x", "hi"]), ..tty("amp") }));
    // No terminal: a background or server process (Codex's app server, a headless Claude).
    let p = lab.spawn(Fake { name: "claude", ..Fake::default() });
    lab.claude_session(p, "c-notty", &work, "interactive", lab::now_ms());
    never.push(p);
    never.push(lab.spawn(Fake { name: "codex", args: strings(&["app-server"]), open: Some(lab.codex_rollout(&u5)), ..Fake::default() }));
    // A dinod's sessions: its own pane, and one in a shell in another pane.
    let dinod = lab.spawn(Fake {
        name: "dino",
        args: strings(&["daemon"]),
        child: Some(Box::new(Fake { child: Some(Box::new(Fake { name: "codex", open: Some(lab.codex_rollout(&u6)), ..Fake::default() })), ..tty("zsh") })),
        ..Fake::default()
    });
    let shell = lab.children(dinod, 1)[0];
    never.extend(lab.children(shell, 1));
    let dinod2 = lab.spawn(Fake { name: "dino", args: strings(&["daemon"]), child: Some(Box::new(tty("claude"))), ..Fake::default() });
    let p = lab.children(dinod2, 1)[0];
    lab.claude_session(p, "c-dinod", &work, "interactive", lab::now_ms());
    never.push(p);
    // A young agent under a dinod isn't "starting" either.
    let dinod3 = lab.spawn(Fake { name: "dino", args: strings(&["daemon"]), child: Some(Box::new(tty("amp"))), ..Fake::default() });
    never.extend(lab.children(dinod3, 1));
    // A record left by a Claude that crashed, its pid since given to a shell.
    let p = lab.spawn(tty("zsh"));
    lab.claude_session(p, "c-stale", &work, "interactive", lab::now_ms() - 60_000);
    never.push(p);
    // Gone within a second: never listed, not even for a moment.
    let p = lab.spawn(Fake { life_ms: Some(800), ..tty("claude") });
    lab.claude_session(p, "c-short", &work, "interactive", lab::now_ms());
    never.push(p);
    let p = lab.spawn(Fake { life_ms: Some(800), ..tty("codex") });
    never.push(p);

    // Conversations begun since their processes started.
    lab.kimi_session("k-1", &dk);
    lab.kimi_session("k-2", &dk2);
    lab.opencode_session("ses_o1", &dop);
    lab.opencode_session("ses_o2", &dop2);
    lab.codewhale_session("w-1", &dw);
    lab.codewhale_session("w-2", &dw2);
    lab.pi_session("pi-1", &dp);
    lab.pi_session("pi-2", &dp2);
    let spawned = t0.elapsed();
    eprintln!("{} processes started in {spawned:?}", lab.pids.len());

    // Younger than MIN_AGE: nothing yet.
    let first = lab.scan();
    // On a loaded Mac the first scan can end after MIN_AGE, having seen some old enough already:
    // the next one may then list them, as it should.
    let young = t0.elapsed() < MIN_AGE - Duration::from_millis(300);
    if young {
        assert_eq!(lab::ids(&first), vec![], "listed before {MIN_AGE:?}");
    }
    // Every process older than MIN_AGE (the last started as `spawned` ended).
    std::thread::sleep((spawned + MIN_AGE + Duration::from_millis(300)).saturating_sub(t0.elapsed()));
    // Old enough, but found by one scan only.
    let second = lab.scan();
    if young {
        assert_eq!(lab::ids(&second), vec![], "listed after one scan");
    }

    // From the second scan on: exactly these, newest first, the same every time.
    let procs = procinfo::processes();
    want.sort_by_key(|(pid, ..)| std::cmp::Reverse((procs[pid].started_us, *pid)));
    let expected: Vec<(String, String, u32)> = want.iter().map(|(p, a, s)| (a.to_string(), s.clone(), *p)).collect();
    for i in 0..8 {
        std::thread::sleep(Duration::from_millis(400));
        let got = lab.scan();
        assert_eq!(lab::ids(&got), expected, "scan {i}");
        assert!(never.iter().all(|p| !got.iter().any(|f| f.pid == Some(*p))));
        if i == 0 {
            let tmux = got.iter().find(|f| f.pid == Some(in_tmux)).unwrap();
            assert_eq!(tmux.terminal.as_deref(), Some("tmux"));
            let cursor = got.iter().find(|f| f.agent == "cursor").unwrap();
            assert_eq!(cursor.status.as_deref(), Some("starting"));
            assert_eq!(got.iter().find(|f| f.pid == Some(kimi)).unwrap().title, "kimi k-1");
        }
    }
    // Those not listed are still running conversations: never offered as finished ones.
    let live: HashSet<String> = found::live().iter().map(|f| f.session_id.clone()).collect();
    for id in ["c-print", "c-notty", "c-dinod", &u4, &u5, &u6, "cp-2"] {
        assert!(live.contains(id), "{id} isn't live");
    }
    let _ = pi_node;

    // A moment without its record (rewritten as it runs): still listed, for LINGER. Qwen's,
    // since Node alone isn't an agent "starting" (an agent's own program would be, after it).
    if let Some(qwen) = qwen_node {
        let record = lab.home.join(format!(".qwen/sessions/{qwen}.json"));
        let saved = std::fs::read_to_string(&record).unwrap();
        std::fs::remove_file(&record).unwrap();
        let gone = Instant::now();
        let mut lingered = 0;
        while gone.elapsed() < LINGER - Duration::from_millis(500) {
            assert!(lab.scan().iter().any(|f| f.pid == Some(qwen)), "blinked out after {:?}", gone.elapsed());
            lingered += 1;
            std::thread::sleep(Duration::from_millis(400));
        }
        assert!(lingered >= 3);
        std::thread::sleep(LINGER.saturating_sub(gone.elapsed()) + Duration::from_millis(200));
        assert!(!lab.scan().iter().any(|f| f.pid == Some(qwen)), "still listed after {LINGER:?} without its record");
        // Back: listed again from the second scan that finds it, where it was.
        std::fs::write(&record, saved).unwrap();
        assert!(!lab.scan().iter().any(|f| f.pid == Some(qwen)));
        assert_eq!(lab::ids(&lab.scan()), expected);
    }
    // A Claude Code whose record is gone (between conversations) is starting, at once: the same
    // row, not a gap.
    let record = lab.home.join(format!(".claude/sessions/{claude_tty}.json"));
    std::fs::remove_file(&record).unwrap();
    let got = lab.scan();
    assert_eq!(got.iter().find(|f| f.pid == Some(claude_tty)).map(|f| (f.session_id.as_str(), f.status.as_deref())), Some(("", Some("starting"))));

    // An agent that quits is gone from the next scan: nothing of a dead process lingers.
    lab.kill(codex_tty);
    std::thread::sleep(Duration::from_millis(50));
    let got = lab.scan();
    assert!(!got.iter().any(|f| f.pid == Some(codex_tty)));
    let rest: Vec<_> = expected.iter().filter(|(.., p)| *p != codex_tty).map(|(a, s, p)| if *p == claude_tty { (a.clone(), String::new(), *p) } else { (a.clone(), s.clone(), *p) }).collect();
    assert_eq!(lab::ids(&got), rest);
}
