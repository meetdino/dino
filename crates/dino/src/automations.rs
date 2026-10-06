//! `dino automations`: what dinod does by itself when something happens: on a schedule, when a
//! PR opens or CI fails, when files change, after another run. The same automations as the app's
//! sidebar.

use dino_core::ipc::{Request, Response};
use dino_core::providers::ProviderRoute;
use dino_core::schedule::{ActionKind, Frequency, ScheduledRun, ScheduledTask, TriggerKind};

use crate::out::{self, Cell, Column, Paint};
use crate::{client, created, hinted, printable, say, unexpected};

pub const HELP: &str = "Automations: run an agent or a command on a schedule, or when something happens.

  dino automations [--json]               every automation: when it runs, what it does, its last run
  dino automations show <name>            one, with its runs: what each said and changed
  dino automations add <name> [options] [--] <prompt>
  dino automations edit <name> [options] [--] [<prompt>]
  dino automations run <name>             run it now, whatever its trigger and conditions
  dino automations pause | resume | rm <name>

When (one; without any, it runs only when you run it):
  --daily HH:MM | --weekdays HH:MM | --weekly <day> HH:MM | --hourly <minute> | --manual
  --on-pr                       a pull request opens         ┐ in --repo owner/name, or the
  --on-merge                    a pull request merges        │
  --on-review                   your review is requested     │ GitHub repo the folder is a
  --on-ci-fail [--branch <b>]   a check fails (on a branch,  │ clone of (--on-review without
                [--mine]        or on open PRs: yours only)  │ --repo: any repo). Through your
  --on-label <label>            an issue gets the label      │ gh login, looked at every
  --on-comment <words>          a comment says the words     ┘ minute (--interval <minutes>)
  --on-commits [--branch <b>]   new commits on the default branch (or <b>) after a fetch
  --on-behind [--branch <b>]    the branch falls behind its upstream
                                git looks every 10 minutes (--interval <minutes>)
  --on-files [<patterns>]       files git doesn't ignore change in the folder (--path <sub>): `*.rs, docs/**`
  --after <name> [--outcome success|failure]
                                another automation's run (or a session's turn) finishes

What it does (default: start the agent with the prompt):
  --agent <agent>               which agent (default: the first dino offers)
  --continue <session>          send the prompt into a session that's already there
  --agents <agent,agent,…>      start several agents with the prompt, a worktree each
  --command <command>           run a command (sh -c, in the folder); with --then-agent failure
                                or always, the agent after it, its output in the prompt

Where and how:
  --cwd <folder>                where it runs (default: here)
  --worktree | --no-worktree    each run in a new worktree (default in a git repo)
  --args <args>                 the agent's options: --args '--model haiku'
  --on <provider> <model>       on a provider's model instead of the agent's own account

Only when (each run that doesn't is kept as skipped):
  --if-changed                  the repo changed since the last run
  --on-limit skip|fallback|run  the agent's account is at its limit (default: skip)
  --ac-power | --lid-open       the Mac is on power | its lid is open
  --parallel                    even while the run before is still going
  --retries <n> [--backoff <s>] try a failed run again, waiting longer each time
  --max-runs <n>                pause after n runs

Afterwards:
  --comment                     post the summary as a comment on the PR (or issue)
  --no-notify                   no notification when a run finishes

The prompt can use what happened: {pr.url} {pr.title} {pr.number} {pr.branch} {ci.check}
{ci.log} {issue.url} {issue.title} {label} {comment.body} {comment.author} {commits.log}
{commits.range} {branch} {behind} {files} {after.name} {after.summary} {after.outcome}
{cmd.output} {cmd.exit} {event} {repo}. A command can use them too: each goes in as one
quoted word. A prompt of `-` is read from stdin.";

fn list() -> anyhow::Result<Vec<ScheduledTask>> {
    match client::request(&Request::ScheduleList)? {
        Response::Schedule { tasks } => Ok(tasks),
        Response::Error { message } => Err(hinted(message)),
        _ => Err(unexpected()),
    }
}

fn find(key: &str) -> anyhow::Result<ScheduledTask> {
    let tasks = list()?;
    tasks
        .iter()
        .find(|t| t.id == key)
        .or_else(|| tasks.iter().find(|t| t.name.eq_ignore_ascii_case(key.trim())))
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("no automation called {}\n`dino automations` lists them.", printable(key)))
}

fn put(t: ScheduledTask) -> anyhow::Result<ScheduledTask> {
    let name = t.name.clone();
    match client::request(&Request::SchedulePut { task: t })? {
        Response::Schedule { tasks } => tasks.into_iter().find(|t| t.name.eq_ignore_ascii_case(&name)).ok_or_else(unexpected),
        Response::Error { message } => Err(hinted(message)),
        _ => Err(unexpected()),
    }
}

pub fn run(args: &[String]) -> anyhow::Result<()> {
    let name = |i: usize| args.get(i).cloned().ok_or_else(|| anyhow::anyhow!("usage: dino automations {} <name>", args[0]));
    match args.first().map(String::as_str) {
        None | Some("ls" | "list") => cmd_list(false),
        Some("--json") => cmd_list(true),
        Some("-h" | "--help" | "help") => {
            println!("{HELP}");
            Ok(())
        }
        Some("show") => cmd_show(&name(1)?),
        Some("add" | "new") => cmd_add(&args[1..]),
        Some("edit") => cmd_edit(&args[1..]),
        Some("run") => {
            let t = find(&name(1)?)?;
            let session = created(client::request(&Request::ScheduleRun { id: t.id.clone() })?)?;
            if session.is_empty() {
                say(&format!("Running {}. `dino automations show {}` shows how it goes.", printable(&t.name), quoted(&t.name)));
            } else if out::tty() {
                println!("Running {} as session {session}. `dino attach {session}` opens it here.", printable(&t.name));
            } else {
                println!("{session}");
            }
            Ok(())
        }
        Some(verb @ ("pause" | "resume")) => {
            let mut t = find(&name(1)?)?;
            t.enabled = verb == "resume";
            let t = put(t)?;
            say(&format!("{} {}.{}", if t.enabled { "Resumed" } else { "Paused" }, printable(&t.name), t.next_run.map(|n| format!(" It runs next {}.", when(n))).unwrap_or_default()));
            Ok(())
        }
        Some("rm" | "delete") => {
            let t = find(&name(1)?)?;
            match client::request(&Request::ScheduleDelete { id: t.id.clone() })? {
                Response::Schedule { .. } => {}
                Response::Error { message } => return Err(hinted(message)),
                _ => return Err(unexpected()),
            }
            say(&format!("Deleted {}. Sessions it started keep running.", printable(&t.name)));
            Ok(())
        }
        Some(other) => anyhow::bail!("dino automations has no `{}` command\n`dino automations --help` lists them.", printable(other)),
    }
}

/// A name as the command line needs it.
fn quoted(name: &str) -> String {
    if name.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)) { name.to_string() } else { format!("'{}'", name.replace('\'', "'\\''")) }
}

/// "today 09:00", "Tue 09:00": local time, as `date` would say it.
pub(crate) fn when(secs: u64) -> String {
    let t = secs as libc::time_t;
    // SAFETY: localtime_r only writes the tm it's given.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        tm
    };
    let today = {
        let n = out::now() as libc::time_t;
        unsafe {
            let mut now: libc::tm = std::mem::zeroed();
            libc::localtime_r(&n, &mut now);
            (now.tm_year, now.tm_yday)
        }
    };
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    let day = if (tm.tm_year, tm.tm_yday) == today {
        "today".to_string()
    } else if (tm.tm_year, tm.tm_yday) == (today.0, today.1 + 1) {
        "tomorrow".to_string()
    } else {
        DAYS[tm.tm_wday.clamp(0, 6) as usize].to_string()
    };
    format!("{day} {:02}:{:02}", tm.tm_hour, tm.tm_min)
}

/// A run in a few words, and its colour.
fn outcome(r: &ScheduledRun) -> (String, Paint) {
    match (r.outcome.as_str(), r.result.as_deref()) {
        ("skipped", _) => ("skipped".into(), Paint::Orange),
        ("failed", _) => ("didn't start".into(), Paint::Red),
        (_, Some("success")) => ("done".into(), Paint::Blue),
        (_, Some(_)) => ("failed".into(), Paint::Red),
        _ if r.finished_at.is_some() => ("ended".into(), Paint::Dim),
        _ => ("running".into(), Paint::Green),
    }
}

fn trigger(t: &ScheduledTask, all: &[ScheduledTask]) -> String {
    t.trigger_text(|id| all.iter().find(|o| o.id == id).map(|o| o.name.clone()))
}

fn cmd_list(json: bool) -> anyhow::Result<()> {
    let tasks = list()?;
    if json {
        println!("{}", serde_json::to_string_pretty(&tasks)?);
        return Ok(());
    }
    if tasks.is_empty() {
        eprintln!("No automations. `dino automations add <name> --daily 09:00 -- <prompt>` makes one;\n`dino automations --help` says what else can start one.");
        return Ok(());
    }
    let cols = [Column::end("NAME", 10), Column::end("WHEN", 16), Column::end("DOES", 12), Column::keep("NEXT"), Column::end("LAST RUN", 14)];
    let rows: Vec<_> = tasks
        .iter()
        .map(|t| {
            let next = match (t.enabled, t.next_run, &t.problem) {
                (false, ..) => Cell::new("paused").paint(Paint::Dim),
                (true, _, Some(p)) => Cell::new("can't look").raw(printable(p)).paint(Paint::Red),
                (true, Some(n), _) => Cell::new(when(n)).raw(out::iso(n)),
                (true, None, _) if !t.state.queue.is_empty() => Cell::new(format!("{} waiting", t.state.queue.len())).paint(Paint::Orange),
                (true, None, _) => Cell::new(if t.scheduled() { "when run" } else { "on its trigger" }).paint(Paint::Dim),
            };
            let last = t.history.last().map_or_else(
                || Cell::new("never").paint(Paint::Dim),
                |r| {
                    let (word, paint) = outcome(r);
                    let ago = out::ago(out::now().saturating_sub(r.at));
                    Cell::new(format!("{word}, {ago}")).raw(format!("{word}\t{}", out::iso(r.at))).paint(paint)
                },
            );
            vec![Cell::new(printable(&t.name)), Cell::new(printable(&trigger(t, &tasks))), Cell::new(printable(&t.action_text())), next, last]
        })
        .collect();
    print!("{}", out::table(&cols, &rows, true));
    if out::tty() {
        println!("\n{}", out::paint("`dino automations show <name>` shows its runs; `dino automations --help` the rest.", Paint::Dim));
    }
    Ok(())
}

fn cmd_show(key: &str) -> anyhow::Result<()> {
    let all = list()?;
    let t = find(key)?;
    let mut rows: Vec<(&str, String)> = vec![
        ("when", trigger(&t, &all)),
        ("does", t.action_text()),
        ("in", out::short_path(&t.cwd) + if t.worktree && matches!(t.action.kind, ActionKind::Agent | ActionKind::Command) { ", a worktree each run" } else { "" }),
    ];
    if !t.prompt.trim().is_empty() {
        rows.push(("prompt", out::fit_end(&t.prompt.split_whitespace().collect::<Vec<_>>().join(" "), out::width().saturating_sub(10).max(20))));
    }
    let c = &t.conditions;
    let mut only = vec![];
    if c.if_changed {
        only.push("the repo changed".to_string());
    }
    if c.ac_power {
        only.push("on power".into());
    }
    if c.lid_open {
        only.push("lid open".into());
    }
    if c.parallel {
        only.push("runs overlap".into());
    }
    if c.retries > 0 {
        only.push(format!("{} retr{}", c.retries, if c.retries == 1 { "y" } else { "ies" }));
    }
    if c.max_runs > 0 {
        only.push(format!("{} of {} runs", t.state.runs, c.max_runs));
    }
    if c.on_limit != "skip" {
        only.push(format!("at a limit: {}", c.on_limit));
    }
    if !only.is_empty() {
        rows.push(("only", only.join(", ")));
    }
    if t.output.pr_comment {
        rows.push(("after", "comments on the PR".into()));
    }
    rows.push(("status", match (&t.problem, t.enabled, t.next_run) {
        (Some(p), ..) => format!("can't look: {p}"),
        (None, false, _) => "paused".into(),
        (None, true, Some(n)) => format!("runs next {}", when(n)),
        (None, true, None) => "waiting for its trigger".into(),
    }));
    if !t.state.queue.is_empty() {
        rows.push(("waiting", t.state.queue.iter().map(|e| e.title.clone()).collect::<Vec<_>>().join("; ")));
    }
    println!("{}", out::paint(&printable(&t.name), Paint::Bold));
    print!("{}", out::fields(&rows.into_iter().map(|(k, v)| (k, printable(&v))).collect::<Vec<_>>()));
    if t.history.is_empty() {
        println!("\n{}", out::paint("No runs yet.", Paint::Dim));
        return Ok(());
    }
    println!("\n{}", out::paint("Runs, newest first", Paint::Dim));
    for r in t.history.iter().rev() {
        let (word, paint) = outcome(r);
        let mut head = format!("{}  {}", when(r.at), out::paint(&word, paint));
        if let Some(s) = &r.session {
            head.push_str(&format!("  session {}", printable(s)));
        }
        if r.attempt > 0 {
            head.push_str(&format!("  retry {}", r.attempt));
        }
        if r.catch_up {
            head.push_str("  caught up");
        }
        println!("{head}");
        let indent = |s: &str| println!("    {}", printable(s));
        if let Some(e) = &r.event {
            indent(&format!("{}{}", e.title, e.url.as_ref().map(|u| format!("  {u}")).unwrap_or_default()));
        }
        if let Some(why) = &r.reason {
            println!("    {}", out::paint(&printable(why), paint));
        }
        if let Some(s) = &r.summary {
            for line in s.lines().filter(|l| !l.trim().is_empty()).take(4) {
                println!("    {}", out::paint(&printable(&out::fit_end(line, out::width().saturating_sub(6).max(20))), Paint::Dim));
            }
        }
        let mut facts = vec![];
        if let Some(c) = r.changes.filter(|c| c.files > 0) {
            facts.push(format!("{} file{} +{} -{}", c.files, if c.files == 1 { "" } else { "s" }, c.added, c.removed));
        }
        if let Some(code) = r.exit {
            facts.push(format!("exit {code}"));
        }
        if let Some(pr) = &r.pr {
            facts.push(pr.clone());
        }
        if let Some(c) = &r.commented {
            facts.push(format!("commented: {c}"));
        }
        if !facts.is_empty() {
            indent(&facts.join("  ·  "));
        }
    }
    if let Some(s) = t.history.iter().rev().find_map(|r| r.session.clone()) {
        println!("\n{}", out::paint(&format!("`dino attach {s}` opens the last run."), Paint::Dim));
    }
    Ok(())
}

fn cmd_add(args: &[String]) -> anyhow::Result<()> {
    let name = args.first().filter(|a| !a.starts_with('-')).ok_or_else(|| anyhow::anyhow!("usage: dino automations add <name> [options] [--] <prompt>\n`dino automations --help` lists the options."))?;
    let cwd = std::env::current_dir()?.display().to_string();
    let mut t = ScheduledTask { name: name.clone(), cwd, enabled: true, frequency: Frequency::Manual, ..Default::default() };
    t.worktree = dino_core::worktree::repo_root(std::path::Path::new(&t.cwd)).is_ok();
    let prompt = apply(&mut t, &args[1..])?;
    if let Some(p) = prompt {
        t.prompt = p;
    }
    if t.launcher.is_empty() && needs_agent(&t) {
        let Response::Launchers { launchers } = client::request(&Request::Launchers)? else { return Err(unexpected()) };
        t.launcher = launchers.iter().find(|l| l.agent_id != "shell").or(launchers.first()).map(|l| l.short.clone()).unwrap_or_default();
    }
    let t = put(t)?;
    if out::tty() {
        let all = list().unwrap_or_default();
        let next = t.next_run.map(|n| format!(", next {}", when(n))).unwrap_or_default();
        println!("Added {}: {}{next}. {}.", printable(&t.name), trigger(&t, &all).to_lowercase_first(), t.action_text());
    } else {
        println!("{}", t.id);
    }
    Ok(())
}

fn cmd_edit(args: &[String]) -> anyhow::Result<()> {
    let name = args.first().ok_or_else(|| anyhow::anyhow!("usage: dino automations edit <name> [options] [--] [<prompt>]"))?;
    let mut t = find(name)?;
    if let Some(p) = apply(&mut t, &args[1..])? {
        t.prompt = p;
    }
    let t = put(t)?;
    say(&format!("Saved {}.", printable(&t.name)));
    Ok(())
}

fn needs_agent(t: &ScheduledTask) -> bool {
    match t.action.kind {
        ActionKind::Agent => true,
        ActionKind::Command => matches!(t.action.then_agent.as_str(), "failure" | "always"),
        _ => false,
    }
}

trait LowerFirst {
    fn to_lowercase_first(&self) -> String;
}

impl LowerFirst for String {
    fn to_lowercase_first(&self) -> String {
        // "PR opened…" and "CI failed…" keep their capitals.
        let mut c = self.chars();
        match (c.next(), self.chars().nth(1)) {
            (Some(f), Some(s)) if f.is_uppercase() && !s.is_uppercase() => f.to_lowercase().chain(c).collect(),
            _ => self.clone(),
        }
    }
}

fn hhmm(s: &str) -> anyhow::Result<(u8, u8)> {
    let (h, m) = s.split_once(':').unwrap_or((s, "0"));
    let (h, m): (u8, u8) = (h.trim().parse()?, m.trim().parse()?);
    anyhow::ensure!(h < 24 && m < 60, "no such time: {s}");
    Ok((h, m))
}

fn weekday(s: &str) -> anyhow::Result<u8> {
    let s = s.to_lowercase();
    ["sun", "mon", "tue", "wed", "thu", "fri", "sat"].iter().position(|d| s.starts_with(d)).map(|i| i as u8).ok_or_else(|| anyhow::anyhow!("no such day: {s} (mon, tue, …)"))
}

/// Apply the options in `args` to `t`; the prompt, if the rest of the line (or stdin) gave one.
fn apply(t: &mut ScheduledTask, args: &[String]) -> anyhow::Result<Option<String>> {
    let mut i = 0;
    let mut words: Vec<String> = vec![];
    let val = |i: &mut usize, flag: &str| -> anyhow::Result<String> {
        *i += 1;
        args.get(*i).cloned().ok_or_else(|| anyhow::anyhow!("{flag} needs a value\n`dino automations --help` lists the options."))
    };
    let num = |s: String, flag: &str| -> anyhow::Result<u32> { s.parse().map_err(|_| anyhow::anyhow!("{flag} takes a number, not {s}")) };
    fn trigger(t: &mut ScheduledTask, on: TriggerKind) {
        if t.trigger.on != on {
            t.trigger = dino_core::schedule::Trigger { on, repo: std::mem::take(&mut t.trigger.repo), ..Default::default() };
        }
    }
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--" => {
                words.extend(args[i + 1..].iter().cloned());
                break;
            }
            "--manual" => {
                trigger(t, TriggerKind::Schedule);
                t.frequency = Frequency::Manual;
            }
            "--daily" | "--weekdays" => {
                let (hour, minute) = hhmm(&val(&mut i, a)?)?;
                trigger(t, TriggerKind::Schedule);
                t.frequency = if a == "--daily" { Frequency::Daily { hour, minute } } else { Frequency::Weekdays { hour, minute } };
            }
            "--weekly" => {
                let weekday = weekday(&val(&mut i, a)?)?;
                let (hour, minute) = hhmm(&val(&mut i, a)?)?;
                trigger(t, TriggerKind::Schedule);
                t.frequency = Frequency::Weekly { weekday, hour, minute };
            }
            "--hourly" => {
                let minute = num(val(&mut i, a)?.trim_start_matches(':').to_string(), a)?;
                anyhow::ensure!(minute < 60, "no such minute: {minute}");
                trigger(t, TriggerKind::Schedule);
                t.frequency = Frequency::Hourly { minute: minute as u8 };
            }
            "--on-pr" => trigger(t, TriggerKind::PrOpened),
            "--on-merge" => trigger(t, TriggerKind::PrMerged),
            "--on-review" => trigger(t, TriggerKind::ReviewRequested),
            "--on-ci-fail" => trigger(t, TriggerKind::CiFailed),
            "--on-label" => {
                let label = val(&mut i, a)?;
                trigger(t, TriggerKind::IssueLabeled);
                t.trigger.label = label;
            }
            "--on-comment" => {
                let phrase = val(&mut i, a)?;
                trigger(t, TriggerKind::Comment);
                t.trigger.phrase = phrase;
            }
            "--on-commits" => trigger(t, TriggerKind::NewCommits),
            "--on-behind" => trigger(t, TriggerKind::Behind),
            "--on-files" => {
                trigger(t, TriggerKind::Files);
                // Patterns are optional: the next word is one unless it's an option.
                if let Some(g) = args.get(i + 1).filter(|g| !g.starts_with("--")) {
                    t.trigger.glob = g.clone();
                    i += 1;
                }
            }
            "--after" => {
                let after = val(&mut i, a)?;
                trigger(t, TriggerKind::After);
                t.trigger.after = after;
            }
            "--outcome" => {
                let w = val(&mut i, a)?;
                anyhow::ensure!(matches!(w.as_str(), "success" | "failure" | "any"), "--outcome is success, failure or any");
                t.trigger.when = w;
            }
            "--repo" => t.trigger.repo = val(&mut i, a)?,
            "--branch" => t.trigger.branch = val(&mut i, a)?,
            "--mine" => t.trigger.mine = true,
            "--path" => t.trigger.path = val(&mut i, a)?,
            "--interval" => t.trigger.interval = num(val(&mut i, a)?, a)?,
            "--agent" => {
                t.launcher = val(&mut i, a)?;
                if t.action.kind != ActionKind::Command {
                    t.action.kind = ActionKind::Agent;
                }
            }
            "--continue" => {
                t.action.kind = ActionKind::Continue;
                t.action.session = val(&mut i, a)?;
            }
            // `--fan`: its name from before.
            "--agents" | "--fan" => {
                t.action.kind = ActionKind::Agents;
                t.action.agents = val(&mut i, a)?.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            }
            "--command" => {
                t.action.kind = ActionKind::Command;
                t.action.command = val(&mut i, a)?;
            }
            "--then-agent" => {
                let w = val(&mut i, a)?;
                anyhow::ensure!(matches!(w.as_str(), "never" | "failure" | "always"), "--then-agent is never, failure or always");
                t.action.then_agent = w;
            }
            "--cwd" => {
                let dir = val(&mut i, a)?;
                t.cwd = std::fs::canonicalize(&dir).map_err(|_| anyhow::anyhow!("{} isn't a folder", printable(&dir)))?.display().to_string();
            }
            "--worktree" => t.worktree = true,
            "--no-worktree" => t.worktree = false,
            "--args" => t.args = val(&mut i, a)?,
            "--on" => {
                let provider = val(&mut i, a)?;
                let model = val(&mut i, a)?;
                t.route = Some(ProviderRoute { provider, model, format: None, name: String::new() });
            }
            "--if-changed" => t.conditions.if_changed = true,
            "--on-limit" => {
                let w = val(&mut i, a)?;
                anyhow::ensure!(matches!(w.as_str(), "skip" | "fallback" | "run"), "--on-limit is skip, fallback or run");
                t.conditions.on_limit = w;
            }
            "--ac-power" => t.conditions.ac_power = true,
            "--lid-open" => t.conditions.lid_open = true,
            "--parallel" => t.conditions.parallel = true,
            "--retries" => t.conditions.retries = num(val(&mut i, a)?, a)?,
            "--backoff" => t.conditions.backoff = num(val(&mut i, a)?, a)?,
            "--max-runs" => t.conditions.max_runs = num(val(&mut i, a)?, a)?,
            "--comment" => t.output.pr_comment = true,
            "--no-comment" => t.output.pr_comment = false,
            "--no-notify" => t.output.notify = false,
            "--notify" => t.output.notify = true,
            "--prompt" => words = vec![val(&mut i, a)?],
            "-h" | "--help" => {
                println!("{HELP}");
                std::process::exit(0);
            }
            other if other.starts_with("--") => anyhow::bail!("dino automations doesn't take {}\n`dino automations --help` lists the options.", printable(other)),
            word => words.push(word.to_string()),
        }
        i += 1;
    }
    if words.len() == 1 && words[0] == "-" {
        let mut p = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut p)?;
        return Ok(Some(p.trim().to_string()));
    }
    Ok((!words.is_empty()).then(|| words.join(" ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str) -> (ScheduledTask, Option<String>) {
        let mut t = ScheduledTask::default();
        let args: Vec<String> = dino_core::schedule::split_args(line);
        let p = apply(&mut t, &args).unwrap();
        (t, p)
    }

    #[test]
    fn options_set_the_automation() {
        let (t, p) = parse("--weekly mon 9:30 --agent codex --if-changed --retries 2 -- fix {pr.url} now");
        assert_eq!(t.frequency, Frequency::Weekly { weekday: 1, hour: 9, minute: 30 });
        assert_eq!(t.launcher, "codex");
        assert!(t.conditions.if_changed);
        assert_eq!(t.conditions.retries, 2);
        assert_eq!(p.as_deref(), Some("fix {pr.url} now"));

        let (t, _) = parse("--on-ci-fail --repo o/r --mine --command 'make test' --then-agent failure");
        assert_eq!(t.trigger.on, TriggerKind::CiFailed);
        assert_eq!(t.trigger.repo, "o/r");
        assert!(t.trigger.mine);
        assert_eq!(t.action.kind, ActionKind::Command);
        assert_eq!(t.action.then_agent, "failure");

        let (t, p) = parse("--on-files '*.rs, docs/**' --path src review the change");
        assert_eq!(t.trigger.glob, "*.rs, docs/**");
        assert_eq!(t.trigger.path, "src");
        assert_eq!(p.as_deref(), Some("review the change"));

        let (t, _) = parse("--after nightly --outcome failure --continue s1");
        assert_eq!((t.trigger.on, t.trigger.after.as_str(), t.trigger.when.as_str()), (TriggerKind::After, "nightly", "failure"));
        assert_eq!(t.action.session, "s1");
    }
}
