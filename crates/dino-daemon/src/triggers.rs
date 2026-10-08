//! Automations' triggers that look outside dinod: GitHub and git, from one thread that wakes only
//! when the next look is due (a minute apart for GitHub, ten for a fetch, unless set otherwise),
//! and files, from FSEvents (see `fsevents`). What each finds goes to `schedule::deliver`, which
//! makes sure no event runs twice.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use dino_core::schedule::{Event, ScheduledTask, TriggerKind, glob_match};
use dino_core::triggers as parse;
use dino_core::worktree;
use serde_json::Value;

use crate::fsevents::Watch;
use crate::github::GitHub;
use crate::{Daemon, schedule};

/// Between looks at GitHub, unless the automation says otherwise.
const GITHUB_EVERY: Duration = Duration::from_secs(60);
/// The search API (review requests in any repo) allows 30 calls a minute for everything: slower.
const SEARCH_EVERY: Duration = Duration::from_secs(180);
/// Between fetches, unless the automation says otherwise.
const GIT_EVERY: Duration = Duration::from_secs(600);
/// A fetch that takes longer than this is given up on.
const FETCH_TIME: Duration = Duration::from_secs(120);
/// At most this many open PRs' branches are looked at for failed CI.
const MAX_BRANCHES: usize = 10;

#[derive(Default)]
pub(crate) struct Triggers {
    pub(crate) github: GitHub,
    wake: Arc<(Mutex<bool>, Condvar)>,
    /// Automation id → when to look next.
    next: Mutex<HashMap<String, Instant>>,
    /// Automation id → why its last look failed.
    problems: Mutex<HashMap<String, String>>,
    /// Automation id → the folder watched for it.
    watches: Mutex<HashMap<String, Watch>>,
}

impl Triggers {
    /// An automation changed: look at every trigger again now.
    pub(crate) fn changed(&self) {
        self.next.lock().unwrap().clear();
        self.problems.lock().unwrap().clear();
        let (m, c) = &*self.wake;
        *m.lock().unwrap() = true;
        c.notify_all();
    }

    pub(crate) fn problems(&self) -> HashMap<String, String> {
        self.problems.lock().unwrap().clone()
    }

    /// Files changed and haven't settled yet.
    pub(crate) fn files_pending(&self) -> bool {
        self.watches.lock().unwrap().values().any(|w| w.last().is_some())
    }
}

fn every(t: &ScheduledTask) -> Duration {
    let set = Duration::from_secs(u64::from(t.trigger.interval) * 60);
    let base = if t.trigger.on.git() { GIT_EVERY } else { GITHUB_EVERY };
    let at_least = if t.trigger.on == TriggerKind::ReviewRequested && t.trigger.repo.is_empty() { SEARCH_EVERY } else { Duration::from_secs(30) };
    (if set.is_zero() { base } else { set }).max(at_least)
}

/// The thread that looks at GitHub and git.
pub(crate) fn start(d: &Arc<Daemon>) {
    let d = d.clone();
    std::thread::Builder::new()
        .name("dinod-triggers".into())
        .spawn(move || {
            loop {
                let wait = look_all(&d);
                let (m, c) = &*d.schedule.triggers.wake;
                let mut woken = m.lock().unwrap();
                if !*woken {
                    woken = c.wait_timeout(woken, wait).unwrap().0;
                }
                *woken = false;
            }
        })
        .expect("dinod: no thread for triggers");
}

/// Look at every trigger that's due; how long until the next one is.
fn look_all(d: &Daemon) -> Duration {
    let tr = &d.schedule.triggers;
    let tasks: Vec<ScheduledTask> = d.schedule.tasks().into_iter().filter(|t| t.enabled && (t.trigger.on.github() || t.trigger.on.git())).collect();
    {
        let ids: Vec<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
        tr.next.lock().unwrap().retain(|id, _| ids.contains(&id.as_str()));
        tr.problems.lock().unwrap().retain(|id, _| ids.contains(&id.as_str()));
    }
    for t in &tasks {
        let due = tr.next.lock().unwrap().get(&t.id).is_none_or(|at| *at <= Instant::now());
        if !due {
            continue;
        }
        tr.next.lock().unwrap().insert(t.id.clone(), Instant::now() + every(t));
        match look(d, t) {
            Ok(found) => {
                tr.problems.lock().unwrap().remove(&t.id);
                d.schedule.update(&t.id, |x| {
                    if found.cursor.is_some() {
                        x.state.cursor = found.cursor.clone();
                    }
                    // A review no longer waited on is forgotten: asked again, it runs again.
                    if let Some(current) = &found.waiting {
                        x.state.seen.retain(|k| !k.starts_with("review:") || current.contains(k));
                    }
                });
                for e in found.events {
                    schedule::deliver(d, &t.id, e);
                }
            }
            Err(e) => {
                tr.problems.lock().unwrap().insert(t.id.clone(), format!("{e:#}"));
            }
        }
    }
    let next = tr.next.lock().unwrap().values().min().copied();
    // Nothing to look at: sleep until an automation changes (and wake now and then regardless).
    next.map_or(Duration::from_secs(600), |at| at.saturating_duration_since(Instant::now()).max(Duration::from_secs(1)))
}

#[derive(Default)]
struct Found {
    events: Vec<Event>,
    cursor: Option<String>,
    /// Review requests: the keys of the PRs still waiting.
    waiting: Option<Vec<String>>,
}

fn look(d: &Daemon, t: &ScheduledTask) -> anyhow::Result<Found> {
    if t.trigger.on.git() {
        return look_git(t);
    }
    let gh = &d.schedule.triggers.github;
    let repo = t.trigger.repo.as_str();
    let since = t.state.since;
    let pulls = || gh.get(&format!("/repos/{repo}/pulls?state=open&sort=created&direction=desc&per_page=30"));
    let mut found = Found::default();
    match t.trigger.on {
        TriggerKind::PrOpened => found.events = parse::prs_opened(&pulls()?, repo, since),
        TriggerKind::PrMerged => found.events = parse::prs_merged(&gh.get(&format!("/repos/{repo}/pulls?state=closed&sort=updated&direction=desc&per_page=30"))?, repo, since),
        TriggerKind::ReviewRequested => {
            found.events = if repo.is_empty() {
                parse::reviews_requested_anywhere(&gh.get("/search/issues?q=is%3Apr+is%3Aopen+review-requested%3A%40me&per_page=50")?)
            } else {
                parse::reviews_requested(&pulls()?, repo, &gh.login()?)
            };
            found.waiting = Some(found.events.iter().map(|e| e.key.clone()).collect());
        }
        TriggerKind::CiFailed => {
            let pulls = pulls()?;
            let pr_of =
                |branch: &str| pulls.as_array().into_iter().flatten().find(|p| p["head"]["ref"] == branch).map(|p| (p["number"].to_string(), p["html_url"].as_str().unwrap_or_default().to_string()));
            let mut refs: Vec<(String, String)> = vec![];
            if !t.trigger.branch.trim().is_empty() {
                let branch = t.trigger.branch.trim();
                let r = gh.get(&format!("/repos/{repo}/git/ref/heads/{branch}"))?;
                let sha = r["object"]["sha"].as_str().ok_or_else(|| anyhow::anyhow!("no branch {branch} in {repo}"))?;
                refs.push((branch.to_string(), sha.to_string()));
            } else {
                let me = if t.trigger.mine { Some(gh.login()?) } else { None };
                for p in pulls.as_array().into_iter().flatten() {
                    if me.as_deref().is_none_or(|m| p["user"]["login"].as_str().is_some_and(|l| l.eq_ignore_ascii_case(m))) {
                        refs.push((p["head"]["ref"].as_str().unwrap_or_default().to_string(), p["head"]["sha"].as_str().unwrap_or_default().to_string()));
                    }
                }
                refs.truncate(MAX_BRANCHES);
            }
            for (branch, sha) in refs.into_iter().filter(|(_, sha)| !sha.is_empty()) {
                let status = gh.get(&format!("/repos/{repo}/commits/{sha}/status"))?;
                // Without Actions or a checks app there may be no check runs to read.
                let checks = gh.get(&format!("/repos/{repo}/commits/{sha}/check-runs?per_page=50")).unwrap_or(Value::Null);
                let pr = pr_of(&branch);
                if let Some(e) = parse::ci_failed(&status, &checks, repo, &branch, &sha, pr.as_ref().map(|(n, u)| (n.as_str(), u.as_str())), since) {
                    found.events.push(e);
                }
            }
        }
        TriggerKind::IssueLabeled => found.events = parse::labeled(&gh.get(&format!("/repos/{repo}/issues/events?per_page=50"))?, repo, &t.trigger.label, since),
        TriggerKind::Comment => {
            let from = dino_core::schedule::format_time(since);
            let q = format!("since={from}&sort=created&direction=desc&per_page=50");
            let mut events = parse::comments(&gh.get(&format!("/repos/{repo}/issues/comments?{q}"))?, repo, &t.trigger.phrase, since);
            events.extend(parse::comments(&gh.get(&format!("/repos/{repo}/pulls/comments?{q}"))?, repo, &t.trigger.phrase, since));
            // Only ones not run yet need their issue's title: one more call each.
            for e in events.iter_mut().filter(|e| !t.state.seen.contains(&e.key)) {
                let n = e.fields.get("issue.number").cloned().unwrap_or_default();
                if let Ok(issue) = gh.get(&format!("/repos/{repo}/issues/{n}")) {
                    e.fields.insert("issue.title".into(), issue["title"].as_str().unwrap_or_default().to_string());
                }
            }
            found.events = events;
        }
        _ => {}
    }
    Ok(found)
}

/// git in `dir`, never asking for a password (nobody is there to type it), given up on after `max`.
fn git(dir: &Path, args: &[&str], max: Duration) -> anyhow::Result<String> {
    let mut c = Command::new("git");
    c.arg("-C").arg(dir).args(args).env("GIT_TERMINAL_PROMPT", "0").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    if std::env::var_os("GIT_SSH_COMMAND").is_none() {
        c.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
    }
    let mut child = c.spawn()?;
    let start = Instant::now();
    while child.try_wait()?.is_none() {
        if start.elapsed() > max {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("git {} took over {} s", args.first().unwrap_or(&""), max.as_secs());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let out = child.wait_with_output()?;
    anyhow::ensure!(out.status.success(), "git {}: {}", args.first().unwrap_or(&""), String::from_utf8_lossy(&out.stderr).trim());
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn look_git(t: &ScheduledTask) -> anyhow::Result<Found> {
    let root = worktree::repo_root(&crate::work_dir(Some(&t.cwd)))?;
    let quick = Duration::from_secs(20);
    let log = |range: &str| git(&root, &["log", "--oneline", "--no-decorate", "-n", "50", range], quick).unwrap_or_default();
    let mut found = Found::default();
    match t.trigger.on {
        TriggerKind::NewCommits => {
            let branch = match t.trigger.branch.trim() {
                "" => dino_core::pr::default_branch(&root),
                b => b.to_string(),
            };
            git(&root, &["fetch", "--quiet", "--no-tags", "origin", &branch], FETCH_TIME)?;
            let sha = git(&root, &["rev-parse", &format!("refs/remotes/origin/{branch}")], quick)?;
            found.cursor = Some(sha.clone());
            match t.state.cursor.as_deref() {
                // The first look only says where it starts from.
                None => {}
                Some(c) if c == sha => {}
                Some(c) => {
                    let range = format!("{}..{}", &c[..c.len().min(9)], &sha[..sha.len().min(9)]);
                    let commits = log(&format!("{c}..{sha}"));
                    let n = commits.lines().count();
                    found.events.push(Event {
                        on: TriggerKind::NewCommits,
                        key: format!("commits:{sha}"),
                        title: format!("{n} new commit{} on {branch}", if n == 1 { "" } else { "s" }),
                        url: None,
                        fields: [("commits.range".to_string(), range), ("commits.log".into(), commits), ("branch".into(), branch.clone())].into(),
                    });
                }
            }
        }
        TriggerKind::Behind => {
            let branch = match t.trigger.branch.trim() {
                "" => git(&root, &["rev-parse", "--abbrev-ref", "HEAD"], quick)?,
                b => b.to_string(),
            };
            anyhow::ensure!(branch != "HEAD", "{} isn't on a branch", root.display());
            let up = format!("{branch}@{{u}}");
            let upstream = git(&root, &["rev-parse", "--abbrev-ref", "--symbolic-full-name", &up], quick).map_err(|_| anyhow::anyhow!("{branch} has no upstream branch"))?;
            let remote = upstream.split('/').next().unwrap_or("origin").to_string();
            git(&root, &["fetch", "--quiet", "--no-tags", &remote], FETCH_TIME)?;
            let up_sha = git(&root, &["rev-parse", &up], quick)?;
            let behind: u32 = git(&root, &["rev-list", "--count", &format!("{branch}..{up}")], quick)?.parse().unwrap_or(0);
            found.cursor = Some(up_sha.clone());
            if behind > 0 {
                found.events.push(Event {
                    on: TriggerKind::Behind,
                    key: format!("behind:{branch}:{up_sha}"),
                    title: format!("{branch} is {behind} commit{} behind {upstream}", if behind == 1 { "" } else { "s" }),
                    url: None,
                    fields: [("branch".to_string(), branch.clone()), ("behind".into(), behind.to_string()), ("commits.log".into(), log(&format!("{branch}..{up}")))].into(),
                });
            }
        }
        _ => {}
    }
    Ok(found)
}

// ---- Files ----

/// The folder a files trigger watches.
fn folder(t: &ScheduledTask) -> PathBuf {
    let dir = crate::work_dir(Some(&t.cwd));
    match t.trigger.path.trim() {
        "" => dir,
        p => std::fs::canonicalize(dir.join(p)).unwrap_or_else(|_| dir.join(p)),
    }
}

/// Watch the folders files triggers name, and fire the ones whose changes have settled. Called
/// from the deciding thread.
pub(crate) fn files(d: &Daemon) {
    let tasks: Vec<ScheduledTask> = d.schedule.tasks().into_iter().filter(|t| t.enabled && t.trigger.on == TriggerKind::Files).collect();
    let tr = &d.schedule.triggers;
    {
        let mut watches = tr.watches.lock().unwrap();
        watches.retain(|id, w| tasks.iter().any(|t| &t.id == id && folder(t) == w.root));
        for t in &tasks {
            if watches.contains_key(&t.id) {
                continue;
            }
            let wake = d.schedule.waker();
            let poke = move || {
                let (m, c) = &*wake;
                *m.lock().unwrap() = true;
                c.notify_all();
            };
            match Watch::new(&folder(t), poke) {
                Ok(w) => {
                    tr.problems.lock().unwrap().remove(&t.id);
                    watches.insert(t.id.clone(), w);
                }
                Err(e) => {
                    tr.problems.lock().unwrap().insert(t.id.clone(), e.to_string());
                }
            }
        }
    }
    let settled: Vec<(String, PathBuf, crate::fsevents::Changes)> = tr
        .watches
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, w)| w.last().is_some_and(|at| at.elapsed() >= schedule::SETTLE))
        .map(|(id, w)| (id.clone(), w.root.clone(), w.take()))
        .collect();
    let dino_worktrees: Vec<PathBuf> = d.worktrees.lock().unwrap().iter().map(|w| w.path.clone()).collect();
    for (id, root, changes) in settled {
        let Some(t) = tasks.iter().find(|t| t.id == id) else { continue };
        // Its own run's edits, or a run's while it waits its turn: not a reason to run again.
        if !t.conditions.parallel && d.schedule.busy(&id) {
            continue;
        }
        let files = not_ignored(&root, changed_files(&root, &changes.paths, &t.trigger.glob, &dino_worktrees));
        if files.is_empty() {
            continue;
        }
        let shown = files.len().min(50);
        let mut list = files[..shown].join("\n");
        if files.len() > shown || changes.more {
            list.push_str("\n…and more");
        }
        let title = match files.as_slice() {
            [one] => format!("{one} changed"),
            [first, rest @ ..] => format!("{first} and {} more changed", rest.len()),
            [] => continue,
        };
        let event = Event {
            on: TriggerKind::Files,
            key: format!("files:{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |t| t.as_millis())),
            title,
            url: None,
            fields: [("files".to_string(), list), ("path".into(), root.display().to_string())].into(),
        };
        schedule::deliver(d, &id, event);
    }
}

/// The changed files under `root` that count: relative, matching `glob`, not in `.git` or a
/// worktree dino made, and still files (or gone).
fn changed_files(root: &Path, paths: &[PathBuf], glob: &str, skip: &[PathBuf]) -> Vec<String> {
    let mut out: Vec<String> = paths
        .iter()
        .filter(|p| !p.is_dir() && !skip.iter().any(|s| p.starts_with(s)))
        .filter_map(|p| p.strip_prefix(root).ok())
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .filter(|rel| !rel.is_empty() && !rel.split('/').any(|c| c == ".git" || c == ".DS_Store"))
        .filter(|rel| glob_match(glob, rel))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// `files` (relative to `root`) without the ones git ignores: build output, caches, what a test
/// run writes. Without them a run that builds would start the next. Outside a repo, all of them.
fn not_ignored(root: &Path, files: Vec<String>) -> Vec<String> {
    use std::io::Write;
    if files.is_empty() {
        return files;
    }
    let child = Command::new("git").arg("-C").arg(root).args(["check-ignore", "--stdin", "-z"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn();
    let Ok(mut child) = child else { return files };
    let input: Vec<u8> = files.iter().flat_map(|f| f.bytes().chain([0])).collect();
    // Written from a thread: a long list could fill both pipes at once.
    let mut stdin = child.stdin.take();
    let writer = std::thread::spawn(move || stdin.as_mut().map(|i| i.write_all(&input)));
    let out = child.wait_with_output();
    let _ = writer.join();
    // 0: some are ignored; 1: none are; anything else: not a repo, or git failed.
    match out {
        Ok(o) if o.status.code() == Some(0) => {
            let ignored: Vec<&[u8]> = o.stdout.split(|b| *b == 0).filter(|p| !p.is_empty()).collect();
            files.into_iter().filter(|f| !ignored.contains(&f.as_bytes())).collect()
        }
        _ => files,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_files_that_count() {
        let root = Path::new("/r");
        let paths: Vec<PathBuf> = ["/r/src/a.rs", "/r/.git/index", "/r/wt/x.rs", "/r/docs/b.md", "/elsewhere/c.rs", "/r/src/a.rs"].iter().map(PathBuf::from).collect();
        assert_eq!(changed_files(root, &paths, "", &[PathBuf::from("/r/wt")]), ["docs/b.md", "src/a.rs"]);
        assert_eq!(changed_files(root, &paths, "*.rs", &[]), ["src/a.rs", "wt/x.rs"]);
    }

    #[test]
    fn ignored_files_dont_count() {
        let dir = std::env::temp_dir().join(format!("dino-ignored-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("target")).unwrap();
        let ok = Command::new("git").arg("-C").arg(&dir).args(["init", "-q"]).status().unwrap().success();
        assert!(ok);
        std::fs::write(dir.join(".gitignore"), "target/\n*.log\n").unwrap();
        let files: Vec<String> = ["src/a.rs", "target/debug/x", "run.log", "notes.md"].map(String::from).into();
        assert_eq!(not_ignored(&dir, files.clone()), ["src/a.rs", "notes.md"]);
        // Outside a repo nothing is left out.
        let plain = std::env::temp_dir().join(format!("dino-plain-{}", std::process::id()));
        std::fs::create_dir_all(&plain).unwrap();
        assert_eq!(not_ignored(&plain, files.clone()).len(), 4);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&plain);
    }
}
