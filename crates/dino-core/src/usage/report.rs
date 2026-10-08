//! What `dino stats` and the Stats window show, worked out from `stats.db` for a range of days.
//! Days and hours are the Mac's local time. The shape is `dino stats --json`'s, documented in
//! `dino stats --help`: a change there bumps `SCHEMA`.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::store::{Row, Store};
use super::{Status, days_from_civil};

/// Gaps in a session longer than this are time away, not time in it.
const AWAY_MS: i64 = 30 * 60 * 1000;
/// How far around the proxy's calls of a conversation its transcript is taken as already counted.
const SLACK_MS: i64 = 60 * 1000;
/// Models drawn by name in the daily chart; the rest are "Other".
const TOP_MODELS: usize = 5;
/// Days in the activity heatmap.
const HEATMAP_DAYS: i64 = 371;
/// Sessions listed one by one, most tokens first.
const TOP_SESSIONS: usize = 50;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Range {
    #[serde(rename = "7d")]
    Week,
    #[default]
    #[serde(rename = "30d")]
    Month,
    All,
}

impl Range {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "7d" | "week" => Some(Range::Week),
            "30d" | "month" => Some(Range::Month),
            "all" => Some(Range::All),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Range::Week => "7d",
            Range::Month => "30d",
            Range::All => "all",
        }
    }

    fn days(self) -> Option<i64> {
        match self {
            Range::Week => Some(7),
            Range::Month => Some(30),
            Range::All => None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Tokens {
    pub input: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub output: u64,
    /// All four.
    pub total: u64,
}

impl Tokens {
    fn add(&mut self, r: &Row) {
        self.input += r.input;
        self.cache_read += r.cache_read;
        self.cache_write += r.cache_write;
        self.output += r.output;
        self.total += r.input + r.cache_read + r.cache_write + r.output;
    }
}

/// Some of the tokens, and the calls that used them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Part {
    pub tokens: Tokens,
    pub requests: u64,
}

/// What in a conversation used its tokens. The three add up to the whole.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Parts {
    /// The conversation itself, and whatever can't be told apart from it (agents that don't say).
    pub main: Part,
    /// Its subagents (Claude's Agent tool): as each call said, or the agent's record of them.
    pub subagents: Part,
    /// Calls the agent made that its own record doesn't keep: Claude Code's titles, compaction,
    /// auto mode's checks of tool calls, prompt suggestions. Only dino's proxy sees them, so
    /// they're counted only for sessions it carried.
    pub side: Part,
}

impl Parts {
    fn add(&mut self, r: &Row) {
        // A call the proxy carried with an answer id the agent's record never kept.
        let side = r.proxied && r.has_answer && !r.matched;
        let p = if r.subagent {
            &mut self.subagents
        } else if side {
            &mut self.side
        } else {
            &mut self.main
        };
        p.tokens.add(r);
        p.requests += 1;
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Totals {
    pub tokens: Tokens,
    /// The same tokens, by what used them.
    #[serde(default)]
    pub parts: Parts,
    /// Model calls (answers, in agents' own records).
    pub requests: u64,
    pub errors: u64,
    pub limit_hits: u64,
    pub sessions: u64,
    /// Days with any use in the range, and days in it (since the first use, for `all`).
    pub active_days: u64,
    pub days: u64,
    pub favorite_model: Option<String>,
    /// The session with the most time in it (gaps over 30 minutes don't count).
    pub longest_session: Option<LongestSession>,
    /// Hour of the day (0–23, local) with the most calls.
    pub peak_hour: Option<u8>,
    /// The day with the most tokens.
    pub most_active_day: Option<Day>,
    /// What the routes that report a price said these calls cost (USD); `None` when none did.
    pub cost: Option<f64>,
    /// How many calls that price covers.
    pub priced_requests: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LongestSession {
    pub agent: String,
    pub conversation: Option<String>,
    pub cwd: Option<String>,
    pub ms: i64,
    pub started_ms: i64,
}

/// One conversation (or a dino session whose conversation isn't known) in the range.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionUse {
    pub agent: String,
    pub conversation: Option<String>,
    /// The dino session that carried it, when one did.
    pub session: Option<String>,
    pub cwd: Option<String>,
    pub first_ms: i64,
    pub last_ms: i64,
    pub tokens: Tokens,
    pub requests: u64,
    pub parts: Parts,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Streaks {
    /// Days in a row up to today, or up to yesterday while today has none yet.
    pub current: u64,
    pub longest: u64,
    /// First day of the longest one.
    pub longest_from: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Day {
    /// Local date, "2026-10-04".
    pub date: String,
    pub tokens: u64,
    pub requests: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Period {
    pub tokens: u64,
    pub requests: u64,
    pub sessions: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Periods {
    pub today: Period,
    pub week: Period,
    pub month: Period,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Hour {
    pub hour: u8,
    pub requests: u64,
    pub tokens: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Model {
    pub model: String,
    pub tokens: Tokens,
    pub requests: u64,
    /// Of all tokens in the range, 0–1.
    pub share: f64,
    pub agents: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DailyModel {
    pub date: String,
    /// A model's name, or "Other".
    pub model: String,
    pub tokens: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentUse {
    pub agent: String,
    pub tokens: Tokens,
    pub requests: u64,
    pub sessions: u64,
    pub active_days: u64,
    /// Calls seen by dino's proxy, and read from the agent's own records.
    pub proxied: u64,
    pub recorded: u64,
    pub last_ms: i64,
    pub top_model: Option<String>,
    /// Answers whose record keeps no time of their own (Cursor, CodeWhale): in its totals, but
    /// on no day or hour (heatmap, streaks, active days, by hour, tokens per day).
    #[serde(default)]
    pub undated: u64,
    #[serde(default)]
    pub parts: Parts,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Project {
    /// The repo's (or folder's) name, with where it is when another project has the same name:
    /// "proj · dino-tabs-test/work".
    pub name: String,
    /// Its repo's root, or the folder when it isn't in one: a worktree's counts for its repo.
    pub path: String,
    pub tokens: Tokens,
    pub requests: u64,
    pub sessions: u64,
    pub agents: Vec<String>,
    pub last_ms: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RouteUse {
    /// As the proxy names it: "anthropic", "plan/<id>", "local/ollama", "or", …
    pub route: String,
    /// For people; dinod names coding plans.
    pub label: String,
    /// "own" (the agent's own sign-in or key), "plan", "openrouter", "local", "free", "chatgpt_plan".
    pub kind: String,
    pub tokens: Tokens,
    pub requests: u64,
    pub errors: u64,
    pub limit_hits: u64,
    pub last_limit_ms: Option<i64>,
    /// What the route said it cost, where it says.
    pub cost: Option<f64>,
    pub models: Vec<String>,
    /// Its usage windows as it last reported them (dinod fills these in).
    #[serde(default)]
    pub windows: Vec<RouteWindow>,
    /// Calls it answered as another route's fallback.
    pub fallbacks: u64,
    /// What the route's account says was spent on its key, and the key's limit (OpenRouter's
    /// `/key`; dinod fills these in), in USD.
    #[serde(default)]
    pub account_spend: Option<f64>,
    #[serde(default)]
    pub account_limit: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RouteWindow {
    pub name: String,
    /// 0–1.
    pub used: f64,
    pub resets_at: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Speed {
    pub route: String,
    /// The route, for people (as in `RouteUse::label`).
    pub label: String,
    pub model: String,
    /// Streamed calls measured.
    pub requests: u64,
    pub ttft_p50_ms: Option<u32>,
    pub ttft_p90_ms: Option<u32>,
    /// Output tokens per second after the first byte.
    pub tps_p50: Option<f64>,
    pub tps_p90: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Sources {
    /// Calls dino's proxy carried.
    pub proxied: u64,
    /// Answers read from agents' own records and counted.
    pub recorded: u64,
    /// Answers in agents' records left out because the proxy counted them.
    pub deduplicated: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Report {
    /// Bumped when the shape changes in a way readers must know.
    pub schema: u32,
    pub range: Range,
    /// The range's first ms (local midnight), and now.
    pub from_ms: i64,
    pub to_ms: i64,
    /// The local time zone's offset from UTC now, in seconds.
    pub utc_offset: i64,
    pub totals: Totals,
    pub streaks: Streaks,
    pub periods: Periods,
    /// Every day of the last 53 weeks, oldest first, for the heatmap (whatever the range).
    pub heatmap: Vec<Day>,
    /// Every day of the range, oldest first.
    pub days: Vec<Day>,
    pub hours: Vec<Hour>,
    pub models: Vec<Model>,
    pub daily_models: Vec<DailyModel>,
    pub agents: Vec<AgentUse>,
    pub projects: Vec<Project>,
    pub routes: Vec<RouteUse>,
    pub speed: Vec<Speed>,
    pub sources: Sources,
    /// The sessions with the most tokens, most first, each by what used them.
    #[serde(default)]
    pub sessions: Vec<SessionUse>,
}

pub const SCHEMA: u32 = 1;

/// Local calendar day of `ms`, as days since 1970-01-01, and the local hour.
fn local_day(ms: i64) -> (i64, u8) {
    let t = (ms.div_euclid(1000)) as libc::time_t;
    // SAFETY: localtime_r only writes the tm it's given.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        tm
    };
    (days_from_civil(tm.tm_year as i64 + 1900, tm.tm_mon as i64 + 1, tm.tm_mday as i64), tm.tm_hour as u8)
}

fn utc_offset(ms: i64) -> i64 {
    let t = (ms.div_euclid(1000)) as libc::time_t;
    // SAFETY: as above.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        tm.tm_gmtoff
    }
}

/// Local midnight starting day `day` (days since 1970-01-01), in ms.
fn midnight(day: i64) -> i64 {
    let (y, m, d) = civil(day);
    // SAFETY: mktime reads and normalizes the tm it's given.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        tm.tm_year = (y - 1900) as i32;
        tm.tm_mon = (m - 1) as i32;
        tm.tm_mday = d as i32;
        tm.tm_isdst = -1;
        libc::mktime(&mut tm) as i64 * 1000
    }
}

/// Year, month, day of days since 1970-01-01.
pub(crate) fn civil(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn date(day: i64) -> String {
    let (y, m, d) = civil(day);
    format!("{y:04}-{m:02}-{d:02}")
}

/// A folder's project: the root of the repo it's in (a worktree's counts for the repo it was made
/// from), else the folder itself. Looked up once per folder.
fn project_of(cwd: &str, seen: &mut HashMap<String, String>) -> String {
    if let Some(p) = seen.get(cwd) {
        return p.clone();
    }
    let p = find_project(cwd);
    seen.insert(cwd.to_string(), p.clone());
    p
}

fn find_project(cwd: &str) -> String {
    let cwd = cwd.trim_end_matches('/');
    for marker in ["/.claude/worktrees/", "/.codex/worktrees/"] {
        if let Some(i) = cwd.find(marker) {
            return cwd[..i].to_string();
        }
    }
    let home = std::env::var("HOME").unwrap_or_default();
    // Up to the repo's root; never the home folder for a folder in it (a dotfiles repo there
    // isn't every folder's project).
    let mut dir = std::path::Path::new(cwd);
    loop {
        let git = dir.join(".git");
        if git.is_dir() {
            return dir.display().to_string();
        }
        if git.is_file() {
            // A worktree: `gitdir: <repo>/.git/worktrees/<name>`.
            let made_from = std::fs::read_to_string(&git).ok().and_then(|t| {
                let gitdir = t.trim().strip_prefix("gitdir:")?.trim().to_string();
                gitdir.find("/.git/worktrees/").map(|i| gitdir[..i].to_string())
            });
            return made_from.unwrap_or_else(|| dir.display().to_string());
        }
        match dir.parent() {
            Some(up) if !up.as_os_str().is_empty() && up.to_str() != Some(home.as_str()) && up != std::path::Path::new("/") => dir = up,
            _ => break,
        }
    }
    // Not on this Mac any more: dino's own worktree folder still says the repo's name.
    let dino = format!("{home}/.dino/worktrees/");
    if !home.is_empty()
        && let Some(rest) = cwd.strip_prefix(&dino)
    {
        let repo = rest.split('/').next().unwrap_or_default();
        return format!("{dino}{repo}");
    }
    if let Some(i) = cwd.find("/.dino/worktrees/") {
        return cwd[..i].to_string();
    }
    cwd.to_string()
}

/// Names for `roots`, by folder name, with the folders above it where two would read the same.
fn project_names(roots: &[String]) -> HashMap<String, String> {
    let parent_tail = |root: &str, n: usize| {
        let parts: Vec<&str> = root.trim_end_matches('/').split('/').filter(|p| !p.is_empty()).collect();
        let above = &parts[..parts.len().saturating_sub(1)];
        above[above.len().saturating_sub(n)..].join("/")
    };
    let mut names = HashMap::new();
    for root in roots {
        let name = name_of(root);
        let same: Vec<&String> = roots.iter().filter(|r| name_of(r) == name).collect();
        if same.len() == 1 {
            names.insert(root.clone(), name);
            continue;
        }
        // The two folders above it, else all of them, when that still doesn't tell them apart.
        let short = parent_tail(root, 2);
        let clash = same.iter().filter(|r| parent_tail(r, 2) == short).count() > 1;
        let tail = if clash || short.is_empty() { parent_tail(root, usize::MAX) } else { short };
        names.insert(root.clone(), if tail.is_empty() { name } else { format!("{name} · {tail}") });
    }
    names
}

fn name_of(path: &str) -> String {
    path.trim_end_matches('/').rsplit('/').next().filter(|n| !n.is_empty()).unwrap_or("/").to_string()
}

fn route_kind(route: &str) -> (&'static str, String) {
    match route {
        "anthropic" => ("own", "Claude sign-in or Anthropic key".into()),
        "openai" => ("own", "OpenAI key".into()),
        "chatgpt" => ("own", "ChatGPT sign-in (Codex)".into()),
        "siwc" => ("chatgpt_plan", "ChatGPT plan (dino)".into()),
        "or" => ("openrouter", "OpenRouter".into()),
        "free" => ("free", "Free models".into()),
        r if r.starts_with("local/") => ("local", format!("{} on this Mac", &r[6..])),
        r if r.starts_with("plan/") => ("plan", format!("Coding plan {}", &r[5..])),
        r => ("own", r.to_string()),
    }
}

fn percentile<T: Copy + PartialOrd>(v: &mut [T], p: f64) -> Option<T> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let i = ((v.len() - 1) as f64 * p).round() as usize;
    Some(v[i])
}

/// The report for `range` as of `now_ms`.
pub fn report(store: &Store, range: Range, now_ms: i64) -> anyhow::Result<Report> {
    let (today, _) = local_day(now_ms);
    let first_day = range.days().map(|n| today - n + 1);
    let heat_from = today - HEATMAP_DAYS + 1;
    let links = store.links()?;
    let spans = store.proxied_spans()?;
    let mut by_conv: HashMap<&str, Vec<(i64, i64)>> = HashMap::new();
    for (c, a, b) in &spans {
        by_conv.entry(c.as_str()).or_default().push((*a, *b));
    }
    let session_conv: HashMap<&str, &str> = links.iter().map(|(s, c)| (s.as_str(), c.as_str())).collect();

    let mut sources = Sources::default();
    // All-time days (as far as read) for streaks and the heatmap.
    let mut per_day: BTreeMap<i64, Day> = BTreeMap::new();
    let mut periods = Periods::default();
    let mut period_sessions: [HashSet<String>; 3] = Default::default();
    // The range's first day: for `all`, the first day with use.
    let mut start = first_day;

    let mut totals = Totals::default();
    let mut hours: Vec<Hour> = (0..24).map(|h| Hour { hour: h, ..Default::default() }).collect();
    let mut models: HashMap<String, Model> = HashMap::new();
    // With each: its sessions, its days, its tokens per model.
    type AgentAcc = (AgentUse, HashSet<String>, HashSet<i64>, HashMap<String, u64>);
    let mut agents: HashMap<String, AgentAcc> = HashMap::new();
    // By root, with its sessions.
    let mut projects: HashMap<String, (Project, HashSet<String>)> = HashMap::new();
    let mut folders: HashMap<String, String> = HashMap::new();
    let mut routes: HashMap<String, (RouteUse, HashSet<String>)> = HashMap::new();
    let mut speed: HashMap<(String, String), (Vec<u32>, Vec<f64>)> = HashMap::new();
    /// A session so far: agent, conversation, folder, first and last call, time in it.
    struct Sess {
        agent: String,
        conversation: Option<String>,
        session: Option<String>,
        cwd: Option<String>,
        first: i64,
        last: i64,
        busy: i64,
        tokens: Tokens,
        requests: u64,
        parts: Parts,
    }
    let mut sessions: HashMap<String, Sess> = HashMap::new();
    let mut day_model: BTreeMap<(i64, String), u64> = BTreeMap::new();

    // One pass over the rows as they come, oldest first: months of history are never all held.
    // Streaks and the heatmap look further back than a short range.
    let since = match range {
        Range::All => 0,
        _ => midnight(heat_from.min(first_day.unwrap_or(heat_from))).min(midnight(today - 29)),
    };
    store.each_row(since, |r| {
        // An answer the proxy carried too: the same answer id, or (for calls that have none) in
        // the time the proxy carried its conversation.
        let in_span = || r.conversation.as_deref().is_some_and(|c| by_conv.get(c).is_some_and(|v| v.iter().any(|(a, b)| r.ts >= a - SLACK_MS && r.ts <= b + SLACK_MS)));
        if !r.proxied && (r.matched || in_span()) {
            if first_day.is_none_or(|f| local_day(r.ts).0 >= f) {
                sources.deduplicated += 1;
            }
            return;
        }
        let (day, hour) = local_day(r.ts);
        // A session is its conversation wherever it's known, so the proxy's part of it and the
        // agent's record of the rest are one.
        let key = match (&r.conversation, r.session.as_deref().and_then(|s| session_conv.get(s))) {
            (Some(c), _) => format!("{}:{c}", r.agent),
            (None, Some(c)) => format!("{}:{c}", r.agent),
            (None, None) => format!("dino:{}", r.session.as_deref().unwrap_or("?")),
        };
        let tokens = r.input + r.cache_read + r.cache_write + r.output;
        // A guessed time puts nothing on a day or an hour.
        let dated = !r.undated;
        if dated {
            let d = per_day.entry(day).or_insert_with(|| Day { date: date(day), ..Default::default() });
            d.tokens += tokens;
            d.requests += 1;
        }
        for (i, (p, days)) in [(&mut periods.today, 1), (&mut periods.week, 7), (&mut periods.month, 30)].into_iter().enumerate() {
            if day > today - days {
                p.tokens += tokens;
                p.requests += 1;
                if !period_sessions[i].contains(&key) {
                    period_sessions[i].insert(key.clone());
                }
            }
        }
        if day < *start.get_or_insert(day) {
            return;
        }

        // In the range.
        if r.proxied {
            sources.proxied += 1;
        } else {
            sources.recorded += 1;
        }
        totals.tokens.add(&r);
        totals.parts.add(&r);
        totals.requests += 1;
        match r.status {
            Status::Error => totals.errors += 1,
            Status::Limit => totals.limit_hits += 1,
            Status::Ok => {}
        }
        if let Some(c) = r.cost {
            *totals.cost.get_or_insert(0.0) += c;
            totals.priced_requests += 1;
        }
        if dated {
            let h = &mut hours[hour as usize];
            h.requests += 1;
            h.tokens += tokens;
        }
        let model = r.model.clone().filter(|m| !m.is_empty()).unwrap_or_else(|| "unknown".into());
        if tokens > 0 {
            let m = models.entry(model.clone()).or_insert_with(|| Model { model: model.clone(), ..Default::default() });
            m.tokens.add(&r);
            m.requests += 1;
            if !m.agents.contains(&r.agent) {
                m.agents.push(r.agent.clone());
            }
            if dated {
                *day_model.entry((day, model.clone())).or_default() += tokens;
            }
        }
        let a = agents.entry(r.agent.clone()).or_insert_with(|| (AgentUse { agent: r.agent.clone(), ..Default::default() }, HashSet::new(), HashSet::new(), HashMap::new()));
        a.0.tokens.add(&r);
        a.0.parts.add(&r);
        a.0.requests += 1;
        if r.proxied {
            a.0.proxied += 1
        } else {
            a.0.recorded += 1
        }
        a.0.last_ms = a.0.last_ms.max(r.ts);
        if !a.1.contains(&key) {
            a.1.insert(key.clone());
        }
        if dated {
            a.2.insert(day);
        } else {
            a.0.undated += 1;
        }
        *a.3.entry(model.clone()).or_default() += tokens;
        if let Some(cwd) = r.cwd.as_deref().filter(|c| !c.is_empty()) {
            let root = project_of(cwd, &mut folders);
            let p = projects.entry(root.clone()).or_insert_with(|| (Project { path: root, ..Default::default() }, HashSet::new()));
            p.0.tokens.add(&r);
            p.0.requests += 1;
            p.0.last_ms = p.0.last_ms.max(r.ts);
            if !p.0.agents.contains(&r.agent) {
                p.0.agents.push(r.agent.clone());
            }
            if !p.1.contains(&key) {
                p.1.insert(key.clone());
            }
        }
        if let Some(route) = &r.route {
            let e = routes.entry(route.clone()).or_insert_with(|| {
                let (kind, label) = route_kind(route);
                (RouteUse { route: route.clone(), kind: kind.into(), label, ..Default::default() }, HashSet::new())
            });
            e.0.tokens.add(&r);
            e.0.requests += 1;
            match r.status {
                Status::Error => e.0.errors += 1,
                Status::Limit => {
                    e.0.limit_hits += 1;
                    e.0.last_limit_ms = Some(e.0.last_limit_ms.unwrap_or(0).max(r.ts));
                }
                Status::Ok => {}
            }
            if let Some(c) = r.cost {
                *e.0.cost.get_or_insert(0.0) += c;
            }
            if r.fallback.is_some() {
                e.0.fallbacks += 1;
            }
            if tokens > 0 {
                e.1.insert(model.clone());
            }
            if r.status == Status::Ok
                && let (Some(ttft), Some(dur)) = (r.ttft_ms, r.duration_ms)
            {
                let s = speed.entry((route.clone(), model.clone())).or_default();
                s.0.push(ttft);
                // Some tokens over some time after the first byte, or the rate is noise.
                let gen_ms = dur.saturating_sub(ttft);
                if r.output >= 16 && gen_ms >= 200 {
                    s.1.push(r.output as f64 * 1000.0 / gen_ms as f64);
                }
            }
        }
        let s = sessions.entry(key).or_insert_with(|| Sess {
            agent: r.agent.clone(),
            conversation: r.conversation.clone(),
            session: None,
            cwd: r.cwd.clone(),
            first: r.ts,
            last: r.ts,
            busy: 0,
            tokens: Tokens::default(),
            requests: 0,
            parts: Parts::default(),
        });
        s.tokens.add(&r);
        s.parts.add(&r);
        s.requests += 1;
        if s.session.is_none() {
            s.session.clone_from(&r.session);
        }
        // Time in a session is only measured between real times.
        if dated {
            let gap = r.ts - s.last;
            if gap <= AWAY_MS {
                s.busy += gap;
            }
            s.last = r.ts;
        }
        if s.cwd.is_none() {
            s.cwd = r.cwd.clone();
        }
    })?;
    let start = start.unwrap_or(today);
    totals.days = (today - start + 1).max(1) as u64;
    periods.today.sessions = period_sessions[0].len() as u64;
    periods.week.sessions = period_sessions[1].len() as u64;
    periods.month.sessions = period_sessions[2].len() as u64;

    let mut streaks = Streaks::default();
    {
        let (mut run, mut from, mut prev) = (0u64, 0i64, i64::MIN);
        for &day in per_day.keys() {
            if day == prev + 1 {
                run += 1;
            } else {
                run = 1;
                from = day;
            }
            if run > streaks.longest {
                streaks.longest = run;
                streaks.longest_from = Some(date(from));
            }
            prev = day;
        }
        let mut day = if per_day.contains_key(&today) { today } else { today - 1 };
        while per_day.contains_key(&day) {
            streaks.current += 1;
            day -= 1;
        }
    }
    let heatmap: Vec<Day> = (heat_from..=today).map(|d| per_day.get(&d).cloned().unwrap_or_else(|| Day { date: date(d), ..Default::default() })).collect();
    totals.sessions = sessions.len() as u64;
    let range_days: Vec<i64> = per_day.keys().copied().filter(|d| *d >= start).collect();
    totals.active_days = range_days.len() as u64;
    totals.most_active_day = range_days.iter().filter_map(|d| per_day.get(d)).max_by_key(|d| d.tokens).cloned();
    totals.peak_hour = hours.iter().filter(|h| h.requests > 0).max_by_key(|h| h.requests).map(|h| h.hour);
    totals.longest_session = sessions.values().filter(|s| s.busy > 0).max_by_key(|s| s.busy).map(|s| LongestSession {
        agent: s.agent.clone(),
        conversation: s.conversation.clone(),
        cwd: s.cwd.clone(),
        ms: s.busy,
        started_ms: s.first,
    });
    let mut top: Vec<Sess> = sessions.into_values().collect();
    top.sort_by(|a, b| b.tokens.total.cmp(&a.tokens.total).then(a.first.cmp(&b.first)));
    let sessions: Vec<SessionUse> = top
        .into_iter()
        .take(TOP_SESSIONS)
        .map(|s| SessionUse {
            agent: s.agent,
            conversation: s.conversation,
            session: s.session,
            cwd: s.cwd,
            first_ms: s.first,
            last_ms: s.last,
            tokens: s.tokens,
            requests: s.requests,
            parts: s.parts,
        })
        .collect();

    let total_tokens = totals.tokens.total.max(1) as f64;
    let mut models: Vec<Model> = models.into_values().collect();
    models.sort_by(|a, b| b.tokens.total.cmp(&a.tokens.total).then(a.model.cmp(&b.model)));
    for m in &mut models {
        m.share = m.tokens.total as f64 / total_tokens;
        m.agents.sort();
    }
    totals.favorite_model = models.first().map(|m| m.model.clone());
    let top: HashSet<&str> = models.iter().take(TOP_MODELS).map(|m| m.model.as_str()).collect();
    let mut daily: BTreeMap<(i64, String), u64> = BTreeMap::new();
    for ((day, model), n) in &day_model {
        let name = if top.contains(model.as_str()) { model.clone() } else { "Other".into() };
        *daily.entry((*day, name)).or_default() += n;
    }
    // Every day of the range for every model drawn, so lines drop to zero rather than skip.
    let mut drawn: Vec<String> = models.iter().take(TOP_MODELS).map(|m| m.model.clone()).collect();
    if models.len() > TOP_MODELS {
        drawn.push("Other".into());
    }
    let mut daily_models = vec![];
    for day in start..=today {
        for m in &drawn {
            daily_models.push(DailyModel { date: date(day), model: m.clone(), tokens: daily.get(&(day, m.clone())).copied().unwrap_or(0) });
        }
    }

    let mut agents: Vec<AgentUse> = agents
        .into_values()
        .map(|(mut a, s, d, m)| {
            a.sessions = s.len() as u64;
            a.active_days = d.len() as u64;
            a.top_model = m.into_iter().filter(|(_, n)| *n > 0).max_by_key(|(_, n)| *n).map(|(m, _)| m);
            a
        })
        .collect();
    agents.sort_by(|a, b| b.tokens.total.cmp(&a.tokens.total).then(a.agent.cmp(&b.agent)));
    let names = project_names(&projects.keys().cloned().collect::<Vec<_>>());
    let mut projects: Vec<Project> = projects
        .into_values()
        .map(|(mut p, s)| {
            p.sessions = s.len() as u64;
            p.name = names.get(&p.path).cloned().unwrap_or_else(|| name_of(&p.path));
            p.agents.sort();
            p
        })
        .collect();
    projects.sort_by(|a, b| b.tokens.total.cmp(&a.tokens.total).then(a.name.cmp(&b.name)));
    let mut routes: Vec<RouteUse> = routes
        .into_values()
        .map(|(mut r, m)| {
            r.models = m.into_iter().collect();
            r.models.sort();
            r
        })
        .collect();
    routes.sort_by(|a, b| b.tokens.total.cmp(&a.tokens.total).then(a.route.cmp(&b.route)));
    let mut speed: Vec<Speed> = speed
        .into_iter()
        .map(|((route, model), (mut ttft, mut tps))| Speed {
            requests: ttft.len() as u64,
            ttft_p50_ms: percentile(&mut ttft, 0.5),
            ttft_p90_ms: percentile(&mut ttft, 0.9),
            tps_p50: percentile(&mut tps, 0.5),
            tps_p90: percentile(&mut tps, 0.9),
            label: route_kind(&route).1,
            route,
            model,
        })
        .collect();
    speed.sort_by(|a, b| b.requests.cmp(&a.requests).then(a.model.cmp(&b.model)));

    Ok(Report {
        schema: SCHEMA,
        range,
        from_ms: midnight(start),
        to_ms: now_ms,
        utc_offset: utc_offset(now_ms),
        totals,
        streaks,
        periods,
        heatmap,
        days: (start..=today).map(|d| per_day.get(&d).cloned().unwrap_or_else(|| Day { date: date(d), ..Default::default() })).collect(),
        hours,
        models,
        daily_models,
        agents,
        projects,
        routes,
        speed,
        sources,
        sessions,
    })
}

#[cfg(test)]
mod tests {
    use super::super::{Call, Used};
    use super::*;

    fn store() -> (Store, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("dino-usage-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&dir);
        let p = dir.join("stats.db");
        (Store::open_at(&p).unwrap(), dir)
    }

    /// Local noon of the day `ago` days before `now`'s.
    fn noon(now: i64, ago: i64) -> i64 {
        midnight(local_day(now).0 - ago) + 12 * 3600 * 1000
    }

    fn call(at: i64, session: &str, conv: Option<&str>, model: &str, tokens: u64) -> Call {
        Call {
            at_ms: at,
            session: session.into(),
            agent: "claude".into(),
            conversation: conv.map(String::from),
            cwd: Some("/src/app".into()),
            route: "anthropic".into(),
            model: Some(model.into()),
            input: tokens,
            output: 10,
            ttft_ms: Some(400),
            duration_ms: Some(2400),
            ..Default::default()
        }
    }

    fn used(id: &str, at: i64, conv: &str, tokens: u64) -> Used {
        Used { id: id.into(), at_ms: at, conversation: conv.into(), cwd: Some("/src/app/.claude/worktrees/x".into()), model: Some("m".into()), input: tokens, output: 1, ..Default::default() }
    }

    #[test]
    fn a_sessions_usage_so_far() {
        let (mut st, dir) = store();
        let mut plan = call(5_000, "7", Some("conv-a"), "m", 100);
        plan.route = "plan/zai".into();
        st.add_calls(&[
            // An earlier session that had id 7, before this one started: not its usage.
            call(1_000, "7", Some("conv-old"), "m", 1),
            call(5_000, "7", Some("conv-a"), "m", 10),
            plan,
            // Its conversation, carried on in another session (resumed, unarchived).
            call(6_000, "3", Some("conv-a"), "m", 1000),
            // Someone else's.
            call(6_000, "4", Some("conv-b"), "m", 5),
        ])
        .unwrap();
        let mut routes = st.session_routes("7", None, 4_000, Some("conv-a")).unwrap();
        routes.sort();
        assert_eq!(routes, vec![("anthropic".to_string(), [1010, 0, 0, 20]), ("plan/zai".to_string(), [100, 0, 0, 10])]);
        // Unarchived as 9: its calls under 7 come along; with no conversation known, only those.
        let routes = st.session_routes("9", Some("7"), 0, None).unwrap();
        assert_eq!(routes.iter().map(|(_, t)| t[0]).sum::<u64>(), 111);
        // Agents' own records aren't the proxy's: never in the meter.
        st.add_used(&[("claude", used("u1", 5_500, "conv-a", 50_000))]).unwrap();
        assert_eq!(st.session_routes("7", None, 4_000, Some("conv-a")).unwrap().iter().map(|(_, t)| t[0]).sum::<u64>(), 1110);
        // Its subagents' part, the same way.
        assert_eq!(st.session_subagents("7", None, 4_000, Some("conv-a")).unwrap(), [0; 4]);
        st.add_calls(&[Call { subagent: true, ..call(7_000, "7", Some("conv-a"), "m", 40) }, Call { subagent: true, ..call(7_000, "4", Some("conv-b"), "m", 9) }]).unwrap();
        assert_eq!(st.session_subagents("7", None, 4_000, Some("conv-a")).unwrap(), [40, 0, 0, 10]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn calendar_round_trips() {
        for day in [-1000, 0, 19000, 20730, 30000] {
            let (y, m, d) = civil(day);
            assert_eq!(days_from_civil(y, m, d), day);
        }
        assert_eq!(date(0), "1970-01-01");
    }

    #[test]
    fn day_boundaries_are_local_midnight() {
        let now = 1_791_115_200_000;
        let (today, _) = local_day(now);
        let m = midnight(today);
        assert_eq!(local_day(m).0, today);
        assert_eq!(local_day(m - 1).0, today - 1);
        assert_eq!(local_day(m).1, 0);
    }

    #[test]
    fn a_conversation_the_proxy_carried_is_not_counted_again() {
        let (mut s, dir) = store();
        let now = noon(1_791_115_200_000, 0) + 3_600_000;
        let t = noon(now, 0);
        s.add_calls(&[call(t, "7", Some("c1"), "opus", 100), call(t + 60_000, "7", Some("c1"), "opus", 200)]).unwrap();
        // The same two answers in the transcript, and one from days later outside dino.
        let n = s.add_used(&[("claude", used("a", t + 2000, "c1", 100)), ("claude", used("b", t + 62_000, "c1", 200)), ("claude", used("c", noon(now, 3), "c1", 50))]).unwrap();
        assert_eq!(n, 3);
        // Read again: nothing new.
        assert_eq!(s.add_used(&[("claude", used("a", t + 2000, "c1", 100))]).unwrap(), 0);
        let r = report(&s, Range::Week, now).unwrap();
        assert_eq!(r.sources, Sources { proxied: 2, recorded: 1, deduplicated: 2 });
        assert_eq!(r.totals.requests, 3);
        assert_eq!(r.totals.tokens.input, 350);
        assert_eq!(r.totals.sessions, 1, "the proxy's part and the transcript's are one conversation");
        assert_eq!(r.totals.longest_session.as_ref().unwrap().ms, 60_000);
        assert_eq!(r.totals.favorite_model.as_deref(), Some("opus"));
        assert_eq!(r.projects.len(), 1, "a worktree counts for its repo");
        assert_eq!(r.projects[0].path, "/src/app");
        assert_eq!(r.speed[0].ttft_p50_ms, Some(400));
        assert_eq!(r.speed[0].tps_p50, None, "too few tokens to say");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Claude Code's calls say their conversation, their subagent and their answer's id: an answer
    /// in both the proxy's calls and the transcript is counted once, exactly, whichever dino
    /// session carried it (a background job in another session's terminal is its own
    /// conversation); one only the transcript has is counted even inside the time the proxy
    /// carried that conversation; one only the proxy carried is a side request (a title,
    /// compaction). Subagents' tokens are their own part, per session too.
    #[test]
    fn answers_are_matched_by_id_and_split_by_what_used_them() {
        let (mut s, dir) = store();
        let now = noon(1_791_115_200_000, 0) + 3_600_000;
        let t = noon(now, 0);
        let claude = |at: i64, conv: &str, answer: Option<&str>, subagent: bool, tokens: u64| Call { answer: answer.map(String::from), subagent, ..call(at, "7", Some(conv), "opus", tokens) };
        s.add_calls(&[
            claude(t, "c1", Some("m1:r1"), false, 100),
            claude(t + 1_000, "c1", Some("m2:r2"), true, 200),
            // A title: no transcript keeps it.
            claude(t + 2_000, "c1", Some("m3:r3"), false, 7),
            // A background job started in the same dino session: its own conversation.
            claude(t + 3_000, "job", Some("m4:r4"), false, 1_000),
            // A call that failed: no answer, no tokens.
            Call { status: Status::Error, ..claude(t + 4_000, "c1", None, false, 0) },
        ])
        .unwrap();
        s.link("7", "claude", "c1").unwrap();
        let rec = |id: &str, at: i64, conv: &str, tokens: u64, subagent: bool| ("claude", Used { answer: Some(id.into()), subagent, ..used(id, at, conv, tokens) });
        s.add_used(&[
            rec("m1:r1", t + 500, "c1", 100, false),
            rec("m2:r2", t + 1_500, "c1", 200, true),
            rec("m4:r4", t + 3_500, "job", 1_000, false),
            // Made while the proxy carried c1, but not through it (the agent went direct).
            rec("m5:r5", t + 2_500, "c1", 50, false),
        ])
        .unwrap();
        let r = report(&s, Range::Week, now).unwrap();
        assert_eq!(r.sources, Sources { proxied: 5, recorded: 1, deduplicated: 3 });
        assert_eq!(r.totals.tokens.input, 100 + 200 + 7 + 1_000 + 50, "every answer once");
        let p = &r.totals.parts;
        assert_eq!((p.main.tokens.input, p.main.requests), (100 + 1_000 + 50, 4));
        assert_eq!((p.subagents.tokens.input, p.subagents.requests), (200, 1));
        assert_eq!((p.side.tokens.input, p.side.requests), (7, 1));
        assert_eq!(p.main.tokens.total + p.subagents.tokens.total + p.side.tokens.total, r.totals.tokens.total, "the parts add up");
        assert_eq!(r.agents[0].parts, r.totals.parts);
        assert_eq!(r.totals.sessions, 2, "the job is a conversation of its own");
        let c1 = r.sessions.iter().find(|x| x.conversation.as_deref() == Some("c1")).unwrap();
        assert_eq!((c1.tokens.input, c1.requests, c1.session.as_deref()), (357, 5, Some("7")));
        assert_eq!((c1.parts.subagents.tokens.input, c1.parts.side.tokens.input), (200, 7));
        assert_eq!(r.sessions[0].conversation.as_deref(), Some("job"), "most tokens first");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A stats.db from before answer ids: the columns are added, Claude's answers read before get
    /// theirs from their key, and subagents' records are read again to say they were subagents'.
    /// A call the proxy carried then gets the id of the answer with the same counts near it, and
    /// that answer's conversation (here a `claude -p` run in the session's terminal): counted
    /// once, as the subagent's its record says it was.
    #[test]
    fn an_older_stats_db_learns_answer_ids() {
        let dir = std::env::temp_dir().join(format!("dino-usage-migrate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("stats.db");
        let t = noon(1_791_115_200_000, 0);
        {
            let db = rusqlite::Connection::open(&p).unwrap();
            db.execute_batch(&format!(
                "CREATE TABLE calls (id INTEGER PRIMARY KEY, source INTEGER NOT NULL, key TEXT UNIQUE, ts INTEGER NOT NULL, agent TEXT NOT NULL,
                   session TEXT, conversation TEXT, cwd TEXT, route TEXT, model TEXT, input INTEGER NOT NULL DEFAULT 0, cache_read INTEGER NOT NULL DEFAULT 0,
                   cache_write INTEGER NOT NULL DEFAULT 0, output INTEGER NOT NULL DEFAULT 0, ttft_ms INTEGER, duration_ms INTEGER,
                   status TEXT NOT NULL DEFAULT 'ok', fallback TEXT, cost REAL, undated INTEGER NOT NULL DEFAULT 0);
                 CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 INSERT INTO calls (source, key, ts, agent, conversation, input) VALUES (1, 'claude:msg_1:req_1', {t}, 'claude', 'c1', 5);
                 INSERT INTO calls (source, key, ts, agent, conversation, input) VALUES (1, 'codex:r1:9', {t}, 'codex', 'r1', 5);
                 INSERT INTO calls (source, ts, agent, session, conversation, input) VALUES (0, {t} - 1000, 'claude', '7', 'the-sessions', 5);
                 INSERT INTO calls (source, ts, agent, session, conversation, input) VALUES (0, {t} + 1000, 'claude', '7', 'the-sessions', 6);
                 INSERT INTO meta (key, value) VALUES ('seen', '{{\"files\":{{\"/p/c1.jsonl\":[1,2,2],\"/p/c1/subagents/agent-a.jsonl\":[1,2,2]}},\"marks\":{{}}}}');"
            ))
            .unwrap();
        }
        let mut s = Store::open_at(&p).unwrap();
        let rows: Vec<(i64, String, Option<String>, Option<String>)> = s
            .db()
            .prepare("SELECT source, agent, answer, conversation FROM calls ORDER BY source, agent, ts")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        let row = |source, agent: &str, answer: Option<&str>, conv: &str| (source, agent.to_string(), answer.map(String::from), Some(conv.to_string()));
        assert_eq!(rows, [row(0, "claude", Some("msg_1:req_1"), "c1"), row(0, "claude", None, "the-sessions"), row(1, "claude", Some("msg_1:req_1"), "c1"), row(1, "codex", None, "r1")]);
        let seen = s.seen();
        assert!(seen.files.contains_key("/p/c1.jsonl") && !seen.files.contains_key("/p/c1/subagents/agent-a.jsonl"), "{seen:?}");
        // Read again, the answer is a subagent's: marked, nothing added.
        let again = Used { answer: Some("msg_1:req_1".into()), subagent: true, ..used("msg_1:req_1", t, "c1", 5) };
        assert_eq!(s.add_used(&[("claude", again)]).unwrap(), 1);
        let marked: (i64, i64) = s.db().query_row("SELECT COUNT(*), SUM(subagent) FROM calls WHERE agent = 'claude' AND source = 1", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(marked, (1, 1));
        let r = report(&s, Range::Week, t + 3_600_000).unwrap();
        let claude = r.agents.iter().find(|a| a.agent == "claude").unwrap();
        assert_eq!((claude.requests, claude.tokens.input), (2, 11), "the matched answer once, the other call too");
        assert_eq!((claude.parts.subagents.requests, claude.parts.subagents.tokens.input), (1, 5));
        drop(s);
        // Opened again: nothing to do.
        Store::open_at(&p).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn projects_are_their_repos_named_apart_when_names_clash() {
        let dir = std::env::temp_dir().join(format!("dino-usage-projects-{}", std::process::id()));
        let repo = dir.join("work/app");
        std::fs::create_dir_all(repo.join(".git/worktrees/fix")).unwrap();
        std::fs::create_dir_all(repo.join("src")).unwrap();
        let wt = dir.join("elsewhere/app-fix");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), format!("gitdir: {}/.git/worktrees/fix\n", repo.display())).unwrap();
        let root = repo.display().to_string();
        assert_eq!(find_project(&format!("{root}/src")), root, "a folder in a repo is the repo");
        assert_eq!(find_project(&wt.display().to_string()), root, "a worktree is the repo it was made from");
        assert_eq!(find_project(&format!("{root}/.claude/worktrees/x")), root);
        assert_eq!(find_project("/gone/tmp/proj"), "/gone/tmp/proj", "not there: the folder itself");
        let names = project_names(&["/private/tmp/dino-tabs-test/work/proj".into(), "/private/tmp/dino-e2e4/work/proj".into(), "/src/app".into(), "/a/x/y/proj".into()]);
        assert_eq!(names["/private/tmp/dino-tabs-test/work/proj"], "proj · dino-tabs-test/work");
        assert_eq!(names["/private/tmp/dino-e2e4/work/proj"], "proj · dino-e2e4/work");
        assert_eq!(names["/src/app"], "app", "only one app");
        let names = project_names(&["/one/work/proj".into(), "/two/work/proj".into(), "/x/two/work/proj".into()]);
        assert_eq!(names["/two/work/proj"], "proj · two/work", "told apart by two folders");
        assert_eq!(names["/x/two/work/proj"], "proj · x/two/work", "the whole path where two aren't enough");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_link_learned_later_still_deduplicates() {
        let (mut s, dir) = store();
        let now = noon(1_791_115_200_000, 0);
        s.add_calls(&[call(now - 1000, "9", None, "gpt", 100)]).unwrap();
        s.add_used(&[("claude", used("a", now, "rollout-1", 100))]).unwrap();
        assert_eq!(report(&s, Range::Week, now).unwrap().totals.requests, 2);
        s.link("9", "codex", "rollout-1").unwrap();
        let r = report(&s, Range::Week, now).unwrap();
        assert_eq!((r.totals.requests, r.sources.deduplicated), (1, 1));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Cursor's and CodeWhale's answers carry the time their chat was saved, not their own: they
    /// count in totals and for their agent, on no day and no hour.
    #[test]
    fn answers_with_a_guessed_time_count_on_no_day() {
        let (mut s, dir) = store();
        let now = noon(1_791_115_200_000, 0);
        s.add_calls(&[call(noon(now, 0), "1", None, "m", 10)]).unwrap();
        let guess = Used { undated: true, ..used("u", noon(now, 2) + 3 * 3600 * 1000, "chat", 40) };
        s.add_used(&[("cursor", guess)]).unwrap();
        let r = report(&s, Range::Week, now).unwrap();
        assert_eq!((r.totals.requests, r.totals.tokens.input), (2, 50), "in the totals");
        let cursor = r.agents.iter().find(|a| a.agent == "cursor").unwrap();
        assert_eq!((cursor.requests, cursor.undated, cursor.active_days), (1, 1, 0));
        assert_eq!((r.totals.active_days, r.streaks.current, r.streaks.longest), (1, 1, 1));
        assert_eq!(r.days.iter().map(|d| d.requests).sum::<u64>(), 1);
        assert_eq!(r.heatmap.iter().map(|d| d.requests).sum::<u64>(), 1);
        assert_eq!(r.hours.iter().map(|h| h.requests).sum::<u64>(), 1);
        assert_eq!(r.totals.peak_hour, Some(12));
        assert_eq!(r.daily_models.iter().map(|d| d.tokens).sum::<u64>(), 20, "only the dated call's tokens");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn streaks_and_ranges_count_local_days() {
        let (mut s, dir) = store();
        let now = noon(1_791_115_200_000, 0);
        // Today, yesterday, the day before; a gap; then four days in a row two weeks back.
        let mut calls = vec![];
        for ago in [0, 1, 2, 14, 15, 16, 17] {
            calls.push(call(noon(now, ago), &format!("s{ago}"), None, "m", 10));
        }
        // Just after midnight counts for that day, just before for the one before.
        let (today, _) = local_day(now);
        calls.push(call(midnight(today - 20) + 1, "late", None, "m", 10));
        calls.push(call(midnight(today - 20) - 1, "late", None, "m", 10));
        s.add_calls(&calls).unwrap();
        let r = report(&s, Range::Week, now).unwrap();
        assert_eq!(r.streaks.current, 3);
        assert_eq!(r.streaks.longest, 4);
        assert_eq!(r.totals.active_days, 3);
        assert_eq!(r.totals.days, 7);
        assert_eq!(r.days.len(), 7);
        assert_eq!(r.heatmap.len() as i64, HEATMAP_DAYS);
        assert_eq!(r.heatmap.last().unwrap().requests, 1);
        let all = report(&s, Range::All, now).unwrap();
        assert_eq!(all.totals.requests, 9);
        assert_eq!(all.totals.active_days, 9);
        assert_eq!(all.totals.days, 22);
        assert_eq!(all.days.iter().filter(|d| d.requests > 0).count(), 9);
        let r30 = report(&s, Range::Month, now).unwrap();
        assert_eq!(r30.periods.today.requests, 1);
        assert_eq!(r30.periods.week.requests, 3);
        assert_eq!(r30.periods.month.requests, 9);
        // Today not used yet: the streak up to yesterday still holds.
        let tomorrow = noon(now, -1) - 6 * 3600 * 1000;
        assert_eq!(report(&s, Range::Week, tomorrow).unwrap().streaks.current, 3);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
