//! Automations: when something happens (a time comes round, a PR opens, CI fails, files change,
//! another run finishes), dinod does something (starts an agent with a prompt, continues a
//! session, fans out, runs a command). They began as scheduled tasks, so the file is still
//! `schedule.json` and the types and requests keep their names: a task from before is an
//! automation with a schedule for its trigger and an agent for its action. Every field added since
//! has a default, so either side can be older.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::providers::ProviderRoute;
use crate::worktree::DiffStat;

/// When a task runs, in local time. Weekdays count from Sunday (0) to Saturday (6).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(tag = "every", rename_all = "snake_case")]
pub enum Frequency {
    /// Only when the user asks (Run now).
    Manual,
    Hourly { minute: u8 },
    Daily { hour: u8, minute: u8 },
    /// Monday to Friday.
    Weekdays { hour: u8, minute: u8 },
    Weekly { weekday: u8, hour: u8, minute: u8 },
}

impl Default for Frequency {
    fn default() -> Self {
        Frequency::Daily { hour: 9, minute: 0 }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default)]
pub struct ScheduledTask {
    /// Empty in a `SchedulePut` for a new task; dinod assigns one.
    pub id: String,
    /// Unique; also the name of the sessions it starts.
    pub name: String,
    pub prompt: String,
    /// Which agent, by launcher short name.
    pub launcher: String,
    /// The folder it runs in.
    pub cwd: String,
    /// Each run in a new worktree and branch of the repo at `cwd`.
    pub worktree: bool,
    /// Extra arguments for the agent (`--model haiku`, `--permission-mode plan`), split like a shell would.
    pub args: String,
    pub frequency: Frequency,
    /// Paused tasks only run when asked to.
    pub enabled: bool,
    pub created_at: u64,
    /// The last scheduled time dinod dealt with (ran, or chose not to); later ones are still owed.
    pub last_due: Option<u64>,
    /// Newest last, at most `MAX_HISTORY`.
    pub history: Vec<ScheduledRun>,
    /// When it runs next; filled in by dinod, ignored on the way in.
    pub next_run: Option<u64>,
    /// What starts it; a schedule (`frequency`) unless set.
    pub trigger: Trigger,
    /// What it does; starts `launcher` with `prompt` unless set.
    pub action: Action,
    pub conditions: Conditions,
    pub output: Output,
    /// On a provider's model instead of the agent's own account.
    pub route: Option<ProviderRoute>,
    /// dinod's own bookkeeping (what it has seen, what's owed); ignored on the way in.
    pub state: TriggerState,
    /// Why its trigger can't look right now (gh signed out, no such repo); filled in by dinod.
    pub problem: Option<String>,
}

/// What starts an automation. One struct for every kind, each reading the fields it needs, so
/// clients can keep what they don't show.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default)]
pub struct Trigger {
    pub on: TriggerKind,
    /// GitHub: `owner/name`; empty for the repo `cwd` is a clone of (review requests: any repo).
    pub repo: String,
    /// Failed CI: the branch, or empty for the branches of open PRs. New commits: empty for the
    /// default branch. Behind: empty for the branch `cwd` is on.
    pub branch: String,
    /// Failed CI on open PRs' branches: only PRs you opened.
    pub mine: bool,
    /// Issue labeled: the label.
    pub label: String,
    /// Comment: the words it has to contain (`@dino`), any case.
    pub phrase: String,
    /// Files changed: the folder (empty: `cwd`) and patterns, `*.rs, docs/**` (empty: any file).
    pub path: String,
    pub glob: String,
    /// Git: minutes between fetches (0: 10). GitHub: minutes between looks (0: 1).
    pub interval: u32,
    /// After: the automation (id or name) or session (id) whose finish starts this one.
    pub after: String,
    /// After: "any" (the default), "success" or "failure".
    pub when: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
#[serde(rename_all = "snake_case")]
pub enum TriggerKind {
    #[default]
    Schedule,
    /// A pull request opened in a repo.
    PrOpened,
    /// Your review requested on a pull request.
    ReviewRequested,
    /// A check or status failed on a branch.
    CiFailed,
    /// An issue (or PR) got a label.
    IssueLabeled,
    /// A comment on an issue or PR that says a phrase.
    Comment,
    /// New commits on a branch of `origin`, after a fetch.
    NewCommits,
    /// The branch `cwd` is on fell behind its upstream.
    Behind,
    /// Files changed in a folder.
    Files,
    /// Another automation's run, or a session's turn, finished.
    After,
}

impl TriggerKind {
    pub fn github(self) -> bool {
        matches!(self, Self::PrOpened | Self::ReviewRequested | Self::CiFailed | Self::IssueLabeled | Self::Comment)
    }

    pub fn git(self) -> bool {
        matches!(self, Self::NewCommits | Self::Behind)
    }

    /// The placeholders its events fill, for editors to offer.
    pub fn placeholders(self) -> &'static [&'static str] {
        match self {
            Self::Schedule => &[],
            Self::PrOpened | Self::ReviewRequested => &["pr.number", "pr.title", "pr.url", "pr.author", "pr.branch", "repo"],
            Self::CiFailed => &["ci.check", "ci.log", "ci.branch", "ci.sha", "pr.number", "pr.url", "repo"],
            Self::IssueLabeled => &["issue.number", "issue.title", "issue.url", "label", "repo"],
            Self::Comment => &["comment.body", "comment.url", "comment.author", "issue.number", "issue.title", "issue.url", "repo"],
            Self::NewCommits => &["commits.range", "commits.log", "branch"],
            Self::Behind => &["branch", "behind", "commits.log"],
            Self::Files => &["files", "path"],
            Self::After => &["after.name", "after.outcome", "after.summary", "after.session"],
        }
    }
}

/// What an automation does when it fires.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default)]
pub struct Action {
    #[serde(rename = "do")]
    pub kind: ActionKind,
    /// Continue: the session (id or name) the prompt is sent into.
    pub session: String,
    /// Fan-out: the agents, by launcher short name.
    pub agents: Vec<String>,
    /// Command: what to run, with `sh -c` in `cwd`.
    pub command: String,
    /// Command: when to start the agent with its output: "never" (the default), "failure" or "always".
    pub then_agent: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    /// A new session of `launcher`, with the prompt.
    #[default]
    Agent,
    /// The prompt sent into a session that's already there.
    Continue,
    /// The prompt to several agents, a worktree each.
    Fanout,
    /// A shell command, and maybe an agent after it.
    Command,
}

/// When an automation that fired doesn't run, and what happens after one fails.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct Conditions {
    /// Only when the repo at `cwd` (its commit and its uncommitted changes) changed since the last run.
    pub if_changed: bool,
    /// When the agent's account is at its limit: "skip" (the default), "fallback" (the agent's
    /// fallback, or skip if it has none) or "run" (start it anyway).
    pub on_limit: String,
    /// Only while the Mac is on power.
    pub ac_power: bool,
    /// Only while the lid is open.
    pub lid_open: bool,
    /// Run a failed run again, this many times, waiting `backoff` seconds and twice as long each
    /// time after.
    pub retries: u32,
    pub backoff: u32,
    /// Pause after this many runs; 0 for no end.
    pub max_runs: u32,
    /// Start a run while the one before is still going. Off, a scheduled time is skipped and an
    /// event waits its turn.
    pub parallel: bool,
}

impl Default for Conditions {
    fn default() -> Self {
        Self { if_changed: false, on_limit: "skip".into(), ac_power: false, lid_open: false, retries: 0, backoff: 60, max_runs: 0, parallel: false }
    }
}

/// What happens with a finished run's result, besides showing it.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct Output {
    /// Post the summary as a comment on the PR: the one that started the run, or the run's own.
    pub pr_comment: bool,
    /// A notification when it's done.
    pub notify: bool,
}

impl Default for Output {
    fn default() -> Self {
        Self { pr_comment: false, notify: true }
    }
}

/// What happened that fired an automation, and the placeholders it fills.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default)]
pub struct Event {
    pub on: TriggerKind,
    /// Tells this event from every other of its trigger, so none runs twice.
    pub key: String,
    /// In a line: "PR #12 opened: Fix the login".
    pub title: String,
    pub url: Option<String>,
    pub fields: BTreeMap<String, String>,
}

/// dinod's memory for a trigger: kept across restarts so no event runs twice, and reset when the
/// trigger changes.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default)]
pub struct TriggerState {
    /// Events from before this don't count (when the trigger was set up).
    pub since: u64,
    /// The newest events' keys, at most `MAX_SEEN`.
    pub seen: Vec<String>,
    /// Where the trigger got to: a commit, an upstream, the newest event id.
    pub cursor: Option<String>,
    /// Events waiting for the run before to finish.
    pub queue: Vec<Event>,
    /// A failed run to try again.
    pub retry: Option<Retry>,
    /// The repo as it was at the last run (see `Conditions::if_changed`).
    pub fingerprint: Option<String>,
    /// Runs started since it was last resumed (see `Conditions::max_runs`).
    pub runs: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default)]
pub struct Retry {
    pub at: u64,
    /// The attempt it will be: 1 for the first retry.
    pub attempt: u32,
    pub event: Option<Event>,
}

pub const MAX_SEEN: usize = 300;
pub const MAX_QUEUE: usize = 20;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default)]
pub struct ScheduledRun {
    /// When dinod acted.
    pub at: u64,
    /// The scheduled time it was for; none for Run now.
    pub due: Option<u64>,
    /// The session it started.
    pub session: Option<String>,
    /// "started", "skipped" or "failed".
    pub outcome: String,
    pub reason: Option<String>,
    /// Started late, for a time missed while the Mac slept or dinod wasn't running.
    pub catch_up: bool,
    /// Names the run, for the ones after it to refer to.
    pub id: String,
    /// What fired it; none for a schedule or Run now.
    pub event: Option<Event>,
    /// 0 for the first try, 1 for the first retry, …
    pub attempt: u32,
    /// Every session it started, `session` first (a fan-out's).
    pub sessions: Vec<String>,
    /// The fan-out it started.
    pub group: Option<String>,
    /// When it finished, and how: "success" or "failure".
    pub finished_at: Option<u64>,
    pub result: Option<String>,
    /// The agent's last message, or the command's last lines, in short.
    pub summary: Option<String>,
    /// What the run changed in its checkout.
    pub changes: Option<DiffStat>,
    /// The PR of the run's branch, or the one that started it.
    pub pr: Option<String>,
    /// A command's exit status and the end of what it printed.
    pub exit: Option<i32>,
    pub output: Option<String>,
    /// Where the summary was posted as a comment.
    pub commented: Option<String>,
}

pub const MAX_HISTORY: usize = 20;

impl ScheduledTask {
    /// Whether it starts on a schedule (the only trigger before automations).
    pub fn scheduled(&self) -> bool {
        self.trigger.on == TriggerKind::Schedule
    }

    /// What starts it, in words: "Weekdays at 09:00", "PR opened in o/r".
    pub fn trigger_text(&self, after_name: impl Fn(&str) -> Option<String>) -> String {
        let t = &self.trigger;
        let repo = if t.repo.is_empty() { "this repo".to_string() } else { t.repo.clone() };
        match t.on {
            TriggerKind::Schedule => self.frequency.text(),
            TriggerKind::PrOpened => format!("PR opened in {repo}"),
            TriggerKind::ReviewRequested if t.repo.is_empty() => "Your review requested, any repo".into(),
            TriggerKind::ReviewRequested => format!("Your review requested in {repo}"),
            TriggerKind::CiFailed => match (t.branch.trim(), t.mine) {
                ("", true) => format!("CI failed on your PRs in {repo}"),
                ("", false) => format!("CI failed on a PR in {repo}"),
                (b, _) => format!("CI failed on {b} in {repo}"),
            },
            TriggerKind::IssueLabeled => format!("Labeled {} in {repo}", t.label.trim()),
            TriggerKind::Comment => format!("Comment says “{}” in {repo}", t.phrase.trim()),
            TriggerKind::NewCommits => format!("New commits on {}", if t.branch.trim().is_empty() { "the default branch" } else { t.branch.trim() }),
            TriggerKind::Behind => format!("{} behind its upstream", if t.branch.trim().is_empty() { "The branch" } else { t.branch.trim() }),
            TriggerKind::Files => {
                let what = if t.glob.trim().is_empty() { "Files".to_string() } else { t.glob.trim().to_string() };
                let place = if t.path.trim().is_empty() { String::new() } else { format!(" in {}", t.path.trim()) };
                format!("{what} changed{place}")
            }
            TriggerKind::After => {
                let name = after_name(&t.after).unwrap_or_else(|| t.after.clone());
                match t.when.as_str() {
                    "success" => format!("After {name} succeeds"),
                    "failure" => format!("After {name} fails"),
                    _ => format!("After {name} finishes"),
                }
            }
        }
    }

    /// What it does, in words: "Start claude", "Run `make test`, then codex if it fails".
    pub fn action_text(&self) -> String {
        let a = &self.action;
        match a.kind {
            ActionKind::Agent => format!("Start {}", self.launcher),
            ActionKind::Continue => format!("Continue {}", a.session),
            ActionKind::Fanout => format!("Fan out to {}", a.agents.join(", ")),
            ActionKind::Command => {
                let cmd = shorten(a.command.lines().next().unwrap_or_default(), 40);
                match a.then_agent.as_str() {
                    "failure" => format!("Run `{cmd}`, then {} if it fails", self.launcher),
                    "always" => format!("Run `{cmd}`, then {}", self.launcher),
                    _ => format!("Run `{cmd}`"),
                }
            }
        }
    }
}

impl Frequency {
    /// "Weekdays at 09:00", in 24-hour time (clients show their own).
    pub fn text(&self) -> String {
        const DAYS: [&str; 7] = ["Sundays", "Mondays", "Tuesdays", "Wednesdays", "Thursdays", "Fridays", "Saturdays"];
        match *self {
            Frequency::Manual => "Manually".into(),
            Frequency::Hourly { minute } => format!("Every hour at :{minute:02}"),
            Frequency::Daily { hour, minute } => format!("Every day at {hour:02}:{minute:02}"),
            Frequency::Weekdays { hour, minute } => format!("Weekdays at {hour:02}:{minute:02}"),
            Frequency::Weekly { weekday, hour, minute } => format!("{} at {hour:02}:{minute:02}", DAYS.get(weekday as usize).unwrap_or(&"Weekly")),
        }
    }
}

/// `text` with every `{name}` that `fields` has filled in. Others stay as they are, braces
/// included, so a prompt that means braces keeps them.
pub fn fill(text: &str, fields: &BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('}').map(|close| (close, &after[..close])) {
            Some((close, name)) if fields.contains_key(name.trim()) => {
                out.push_str(&fields[name.trim()]);
                rest = &after[close + 1..];
            }
            _ => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// `fill` for a shell command: each value goes in as one word, single-quoted, so what an event
/// carries (a comment's text, a branch name) is never run as a command.
pub fn fill_command(command: &str, fields: &BTreeMap<String, String>) -> String {
    let quoted: BTreeMap<String, String> = fields.iter().map(|(k, v)| (k.clone(), shell_quote(v))).collect();
    fill(command, &quoted)
}

/// `s` as one shell word: `'it'\''s'`.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Whether `path` (relative, with `/`) matches any of `patterns`, separated by commas or spaces:
/// `*` within a folder, `**` across folders, `?` one character. A pattern without a `/` matches
/// the file's name anywhere, as `.gitignore` does. No patterns: everything matches.
pub fn glob_match(patterns: &str, path: &str) -> bool {
    let mut any = false;
    for p in patterns.split([',', ' ']).map(str::trim).filter(|p| !p.is_empty()) {
        any = true;
        let p = p.trim_start_matches("./");
        let hit = if p.contains('/') { glob(p.as_bytes(), path.as_bytes()) } else { glob(p.as_bytes(), path.rsplit('/').next().unwrap_or(path).as_bytes()) };
        if hit {
            return true;
        }
    }
    !any
}

fn glob(p: &[u8], s: &[u8]) -> bool {
    match p {
        [] => s.is_empty(),
        [b'*', b'*', b'/', rest @ ..] => glob(rest, s) || s.iter().enumerate().any(|(i, &c)| c == b'/' && glob(rest, &s[i + 1..])),
        [b'*', b'*', rest @ ..] => (0..=s.len()).any(|i| glob(rest, &s[i..])),
        [b'*', rest @ ..] => (0..=s.len()).take_while(|&i| i == 0 || s[i - 1] != b'/').any(|i| glob(rest, &s[i..])),
        [b'?', rest @ ..] => s.first().is_some_and(|&c| c != b'/') && glob(rest, &s[1..]),
        [c, rest @ ..] => s.first() == Some(c) && glob(rest, &s[1..]),
    }
}

/// Seconds since the epoch of a GitHub time, `2026-10-04T12:30:00Z`.
pub fn parse_time(t: &str) -> Option<u64> {
    let b = t.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' {
        return None;
    }
    let n = |r: std::ops::Range<usize>| t.get(r)?.parse::<i64>().ok();
    let (y, m, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let (hh, mm, ss) = (n(11..13)?, n(14..16)?, n(17..19)?);
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    u64::try_from(days * 86400 + hh * 3600 + mm * 60 + ss).ok()
}

/// A GitHub time for `since=` queries.
pub fn format_time(secs: u64) -> String {
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// `text` cut to `max` characters at a word, with an ellipsis when cut.
pub fn shorten(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max).collect();
    let cut = cut.rfind(char::is_whitespace).filter(|&i| i > max / 2).map_or(cut.as_str(), |i| &cut[..i]);
    format!("{}…", cut.trim_end())
}

/// Words of `s`, split like a shell: spaces separate, quotes group, backslash escapes.
pub fn split_args(s: &str) -> Vec<String> {
    let mut out = vec![];
    let mut word: Option<String> = None;
    let mut quote: Option<char> = None;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') | (None, '\\') => word.get_or_insert_default().extend(chars.next()),
            (Some(_), c) => word.get_or_insert_default().push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                word.get_or_insert_default();
            }
            (None, c) if c.is_whitespace() => out.extend(word.take()),
            (None, c) => word.get_or_insert_default().push(c),
        }
    }
    out.extend(word);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn older_tasks_load_as_scheduled_automations() {
        let old = r#"{"id":"a1","name":"Triage","prompt":"look","launcher":"claude","cwd":"/tmp","worktree":true,"args":"","frequency":{"every":"daily","hour":9,"minute":0},"enabled":true,"created_at":1,"last_due":null,"history":[{"at":5,"due":4,"session":"s1","outcome":"started","reason":null,"catch_up":false}],"next_run":null}"#;
        let t: ScheduledTask = serde_json::from_str(old).unwrap();
        assert!(t.scheduled());
        assert_eq!(t.action.kind, ActionKind::Agent);
        assert_eq!(t.conditions.on_limit, "skip");
        assert!(t.output.notify && !t.conditions.parallel);
        assert_eq!(t.history[0].session.as_deref(), Some("s1"));
        // And back: what an older client sends still reads.
        let again: ScheduledTask = serde_json::from_str(&serde_json::to_string(&t).unwrap()).unwrap();
        assert_eq!(again, t);
    }

    #[test]
    fn placeholders_fill_what_they_know() {
        let f: BTreeMap<String, String> = [("pr.url".to_string(), "https://x/1".to_string()), ("pr.number".into(), "1".into())].into();
        assert_eq!(fill("Review {pr.url} (#{ pr.number })", &f), "Review https://x/1 (#1)");
        assert_eq!(fill("keep {this} and {", &f), "keep {this} and {");
        assert_eq!(fill("{{pr.number}}", &f), "{1}");
    }

    #[test]
    fn commands_get_values_as_words() {
        let f: BTreeMap<String, String> = [("comment.body".to_string(), "hi'; rm -rf ~ #\n$(x)".to_string())].into();
        let cmd = fill_command("echo {comment.body}", &f);
        assert_eq!(cmd, r#"echo 'hi'\''; rm -rf ~ #
$(x)'"#);
        let out = std::process::Command::new("/bin/sh").arg("-c").arg(&cmd).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "hi'; rm -rf ~ #\n$(x)\n");
    }

    #[test]
    fn globs() {
        assert!(glob_match("", "a/b.rs"));
        assert!(glob_match("*.rs", "src/deep/main.rs"));
        assert!(!glob_match("*.rs", "src/main.swift"));
        assert!(glob_match("docs/**", "docs/a/b.md"));
        assert!(glob_match("src/*.rs, *.md", "README.md"));
        assert!(!glob_match("src/*.rs", "src/a/b.rs"));
        assert!(glob_match("src/**/*.rs", "src/b.rs"));
        assert!(glob_match("src/**/*.rs", "src/a/b.rs"));
        assert!(glob_match("file?.txt", "x/file1.txt"));
    }

    #[test]
    fn github_times() {
        assert_eq!(parse_time("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_time("2026-03-10T13:00:00Z"), Some(1773147600));
        assert_eq!(format_time(1773147600), "2026-03-10T13:00:00Z");
        assert_eq!(parse_time("2024-02-29T23:59:59Z").map(format_time).as_deref(), Some("2024-02-29T23:59:59Z"));
        assert_eq!(parse_time("nope"), None);
    }

    #[test]
    fn shortens_at_a_word() {
        assert_eq!(shorten("  short  ", 10), "short");
        assert_eq!(shorten("the quick brown fox jumps", 12), "the quick…");
    }

    #[test]
    fn splits_like_a_shell() {
        assert_eq!(split_args(""), Vec::<String>::new());
        assert_eq!(split_args("  --model  haiku "), ["--model", "haiku"]);
        assert_eq!(split_args(r#"--append-system-prompt "be brief" 'a b' c\ d"#), ["--append-system-prompt", "be brief", "a b", "c d"]);
        assert_eq!(split_args(r#"--x """#), ["--x", ""]);
    }
}
