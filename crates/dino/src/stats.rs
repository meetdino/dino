//! `dino stats`: usage across every agent, from dino's proxy and agents' own records.

use dino_core::ipc::{Request, Response};
use dino_core::usage::{Range, Report};

use crate::out::{self, Cell, Column, Paint};
use crate::{client, printable, unexpected};

pub const HELP: &str = "usage: dino stats [<section>] [--range 7d|30d|all] [--json] [--clear [--yes]]

Usage across every agent: what dino's proxy carried for the sessions it routes, and what agents'
own records say (agents dino doesn't route, agents run outside dino, history from before dino). A
conversation the proxy carried isn't counted again from its record. Days and hours are local time.
Stats stay on this Mac and are never synced.

Sections: overview (the default: totals, streaks, activity), models, agents, projects, routes,
speed, all.

  --range 7d|30d|all  the last 7 days, the last 30 (the default), or everything
  --json              the whole report as one JSON object (below), whatever the section
  --clear             forget every statistic; agents' records are read again next time, what
                      only the proxy saw is gone. Asks first on a terminal; --yes doesn't.

When piped, it prints no header and no colour: one tab-separated line per row, its section first
(`totals`, `streaks`, `period`, `day`, `hour`, `model`, `agent`, `project`, `route`, `speed`),
then the row's raw values in the order the terminal shows them. Token counts are whole numbers,
times ISO dates or ms.

JSON (schema 1). Tokens are objects {input, cache_read, cache_write, output, total}. Times in ms
since the epoch, durations in ms, dates local (YYYY-MM-DD). Absent or null: not known.
  schema, range (\"7d\"|\"30d\"|\"all\"), from_ms, to_ms, utc_offset (s)
  totals    {tokens, requests, errors, limit_hits, sessions, active_days, days, favorite_model,
             longest_session {agent, conversation, cwd, ms, started_ms}, peak_hour (0-23),
             most_active_day {date, tokens, requests}, cost, priced_requests}
  streaks   {current, longest, longest_from}
  periods   {today, week, month}: {tokens, requests, sessions}
  heatmap   the last 53 weeks, [{date, tokens, requests}]; days: the range's, the same shape
  hours     [{hour, requests, tokens}] x 24
  models    [{model, tokens, requests, share (0-1), agents}]
  daily_models [{date, model, tokens}]: the top 5 models and \"Other\", every day of the range
  agents    [{agent, tokens, requests, sessions, active_days, proxied, recorded, last_ms, top_model,
             undated}]: undated answers (Cursor, CodeWhale keep no time per answer) are in the
             totals but on no day or hour: not in heatmap, days, streaks, hours, daily_models
  projects  [{name, path, tokens, requests, sessions, agents, last_ms}]
  routes    [{route, label, kind, tokens, requests, errors, limit_hits, last_limit_ms, cost,
             models, windows [{name, used (0-1), resets_at (s)}], fallbacks, account_spend,
             account_limit}]
  speed     [{route, label, model, requests, ttft_p50_ms, ttft_p90_ms, tps_p50, tps_p90}]
  sources   {proxied, recorded, deduplicated}
Cost is only what a route itself reported (OpenRouter's usage.cost and key spend); dino keeps no
price list, so elsewhere it's null.";

const SECTIONS: &[&str] = &["overview", "models", "agents", "projects", "routes", "speed", "all"];

pub fn run(args: &[String]) -> anyhow::Result<()> {
    let mut range = Range::Month;
    let (mut json, mut clear, mut yes, mut section) = (false, false, false, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-h" | "--help" => {
                println!("{HELP}");
                return Ok(());
            }
            "--json" => json = true,
            "--clear" => clear = true,
            "--yes" | "-y" => yes = true,
            "--range" | "-r" => {
                let v = it.next().ok_or_else(|| anyhow::anyhow!("--range takes 7d, 30d or all"))?;
                range = Range::parse(v).ok_or_else(|| anyhow::anyhow!("--range takes 7d, 30d or all, not `{}`", printable(v)))?;
            }
            a if a.starts_with("--range=") => {
                let v = &a["--range=".len()..];
                range = Range::parse(v).ok_or_else(|| anyhow::anyhow!("--range takes 7d, 30d or all, not `{}`", printable(v)))?;
            }
            a if section.is_none() && SECTIONS.contains(&a) => section = Some(a.to_string()),
            a => anyhow::bail!("dino stats doesn't take `{}`\n`dino stats --help` lists its options.", printable(a)),
        }
    }
    if clear {
        if !yes {
            anyhow::ensure!(out::tty() && std::io::IsTerminal::is_terminal(&std::io::stdin()), "dino stats --clear deletes all statistics: add --yes to confirm");
            eprint!("Delete all usage statistics? Usage that only dino recorded can't be recovered. [y/N] ");
            let mut answer = String::new();
            std::io::stdin().read_line(&mut answer)?;
            if !matches!(answer.trim(), "y" | "Y" | "yes") {
                eprintln!("Nothing deleted.");
                return Ok(());
            }
        }
        match client::request(&Request::StatsClear)? {
            Response::Ok => eprintln!("Usage statistics cleared."),
            _ => return Err(unexpected()),
        }
        return Ok(());
    }
    let Response::Stats { report } = client::request(&Request::Stats { range })? else { return Err(unexpected()) };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    let section = section.unwrap_or_else(|| "overview".into());
    let show = |s: &str| section == "all" || section == s;
    let mut p = Printer { first: true };
    if show("overview") {
        overview(&mut p, &report);
    }
    if show("models") {
        models(&mut p, &report);
    }
    if show("agents") {
        agents(&mut p, &report);
    }
    if show("projects") {
        projects(&mut p, &report);
    }
    if show("routes") {
        routes(&mut p, &report);
    }
    if show("speed") {
        speed(&mut p, &report);
    }
    Ok(())
}

/// Sections one after another: on a terminal under a bold heading, piped as lines that start
/// with the section's name.
struct Printer {
    first: bool,
}

impl Printer {
    fn heading(&mut self, title: &str) {
        if !out::tty() {
            return;
        }
        if !self.first {
            println!();
        }
        self.first = false;
        println!("{}", out::paint(title, Paint::Bold));
    }

    fn table(&mut self, kind: &str, columns: &[Column], rows: Vec<Vec<Cell>>) {
        if out::tty() {
            print!("{}", out::table(columns, &rows, true));
            return;
        }
        let rows: Vec<Vec<Cell>> = rows.into_iter().map(|r| std::iter::once(Cell::new(kind)).chain(r).collect()).collect();
        print!("{}", out::table(columns, &rows, false));
    }

    /// `key  value` lines; piped, `kind<TAB>key<TAB>value`.
    fn fields(&mut self, kind: &str, rows: &[(&str, String, String)]) {
        if out::tty() {
            let shown: Vec<(&str, String)> = rows.iter().map(|(k, v, _)| (*k, v.clone())).collect();
            print!("{}", out::fields(&shown));
        } else {
            for (k, _, raw) in rows {
                println!("{kind}\t{}\t{}", k.to_lowercase().replace(' ', "_"), printable(raw));
            }
        }
    }
}

fn range_words(r: Range) -> &'static str {
    match r {
        Range::Week => "the last 7 days",
        Range::Month => "the last 30 days",
        Range::All => "all time",
    }
}

/// 1234567 as "1.2M", as the app shows token counts.
pub fn tokens(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => format!("{:.1}k", n as f64 / 1e3),
        1_000_000..1_000_000_000 => format!("{:.1}M", n as f64 / 1e6),
        _ => format!("{:.2}B", n as f64 / 1e9),
    }
}

fn duration(ms: i64) -> String {
    let m = ms / 60_000;
    match m {
        0 => format!("{}s", ms / 1000),
        1..60 => format!("{m}m"),
        _ => format!("{}h {:02}m", m / 60, m % 60),
    }
}

fn date_of(ms: i64) -> String {
    if ms <= 0 { String::new() } else { out::iso((ms / 1000) as u64) }
}

fn opt<T: ToString>(v: Option<T>) -> String {
    v.map(|v| v.to_string()).unwrap_or_default()
}

fn overview(p: &mut Printer, r: &Report) {
    let t = &r.totals;
    p.heading(&format!("Usage, {}", range_words(r.range)));
    if t.requests == 0 {
        if out::tty() {
            println!("{}", out::paint("No usage yet. It shows here as your agents work, in dino or on their own.", Paint::Dim));
        }
        return;
    }
    let k = &t.tokens;
    let mut rows = vec![
        ("Tokens", format!("{}  {}", tokens(k.total), out::paint(&format!("{} in, {} out, {} cache read, {} cache write", tokens(k.input), tokens(k.output), tokens(k.cache_read), tokens(k.cache_write)), Paint::Dim)), k.total.to_string()),
        (
            "Requests",
            {
                let mut said = vec![];
                if t.errors > 0 {
                    said.push(format!("{} failed", t.errors));
                }
                if t.limit_hits > 0 {
                    said.push(format!("{} hit a limit", t.limit_hits));
                }
                if said.is_empty() { t.requests.to_string() } else { format!("{}  {}", t.requests, out::paint(&said.join(", "), Paint::Dim)) }
            },
            t.requests.to_string(),
        ),
        ("Sessions", t.sessions.to_string(), t.sessions.to_string()),
        ("Active days", format!("{} of {}", t.active_days, t.days), t.active_days.to_string()),
        ("Streak", format!("{} days now, {} at most", r.streaks.current, r.streaks.longest), r.streaks.current.to_string()),
    ];
    if let Some(m) = &t.favorite_model {
        rows.push(("Favorite model", m.clone(), m.clone()));
    }
    if let Some(s) = &t.longest_session {
        let place = s.cwd.as_deref().map(|c| format!(", {}", out::fit_path(&out::short_path(c), 40))).unwrap_or_default();
        rows.push(("Longest session", format!("{}  {}", duration(s.ms), out::paint(&format!("{}{place}", s.agent), Paint::Dim)), s.ms.to_string()));
    }
    if let Some(h) = t.peak_hour {
        rows.push(("Peak hour", format!("{h:02}:00"), h.to_string()));
    }
    if let Some(d) = &t.most_active_day {
        rows.push(("Most active day", format!("{}  {}", d.date, out::paint(&format!("{} tokens", tokens(d.tokens)), Paint::Dim)), d.date.clone()));
    }
    if let Some(c) = t.cost {
        rows.push(("Reported cost", format!("${c:.2}  {}", out::paint(&format!("for {} of {} requests, as their routes said", t.priced_requests, t.requests), Paint::Dim)), format!("{c}")));
    }
    p.fields("totals", &rows);
    if !out::tty() {
        println!("streaks\tlongest\t{}", r.streaks.longest);
        for (name, x) in [("today", &r.periods.today), ("7d", &r.periods.week), ("30d", &r.periods.month)] {
            println!("period\t{name}\t{}\t{}\t{}", x.tokens, x.requests, x.sessions);
        }
        for d in &r.days {
            println!("day\t{}\t{}\t{}", d.date, d.tokens, d.requests);
        }
        for h in &r.hours {
            println!("hour\t{}\t{}\t{}", h.hour, h.requests, h.tokens);
        }
        return;
    }
    let period = |x: &dino_core::usage::report::Period| format!("{} tokens, {} requests", tokens(x.tokens), x.requests);
    println!(
        "{}",
        out::paint(&format!("Today {}  ·  7 days {}  ·  30 days {}", period(&r.periods.today), period(&r.periods.week), period(&r.periods.month)), Paint::Dim)
    );
    println!();
    print!("{}", heatmap(r));
}

/// The last weeks as a grid of shades, a column a week, Monday on top; as wide as fits.
fn heatmap(r: &Report) -> String {
    let weeks_fit = out::width().saturating_sub(6) / 2;
    let days = &r.heatmap;
    if days.is_empty() || weeks_fit < 4 {
        return String::new();
    }
    // Weekday of the last day (today), Monday = 0.
    let last = days.last().map(|d| weekday(&d.date)).unwrap_or(0);
    let weeks = weeks_fit.min(53);
    let cells = (weeks - 1) * 7 + last + 1;
    let shown = &days[days.len().saturating_sub(cells)..];
    let mut active: Vec<u64> = shown.iter().map(|d| d.tokens).filter(|t| *t > 0).collect();
    active.sort_unstable();
    let q = |p: f64| active.get(((active.len().max(1) - 1) as f64 * p) as usize).copied().unwrap_or(0);
    let cuts = [q(0.25), q(0.5), q(0.75)];
    let shade = |t: u64| match t {
        0 => out::paint("·", Paint::Dim),
        t if t <= cuts[0] => out::paint("░", Paint::Green),
        t if t <= cuts[1] => out::paint("▒", Paint::Green),
        t if t <= cuts[2] => out::paint("▓", Paint::Green),
        _ => out::paint("█", Paint::Green),
    };
    let mut out_s = String::new();
    let names = ["Mon", "", "Wed", "", "Fri", "", "Sun"];
    for (row, name) in names.iter().enumerate() {
        out_s.push_str(&out::paint(&format!("{name:<4}"), Paint::Dim));
        for w in 0..weeks {
            let i = w * 7 + row;
            if i < shown.len() {
                out_s.push_str(&shade(shown[i].tokens));
                out_s.push(' ');
            }
        }
        out_s.push('\n');
    }
    out_s.push_str(&out::paint(&format!("    {} to {}, by tokens a day\n", shown.first().map_or("", |d| &d.date), shown.last().map_or("", |d| &d.date)), Paint::Dim));
    out_s
}

/// Monday = 0, for a "YYYY-MM-DD" date.
fn weekday(date: &str) -> usize {
    let n = |r: std::ops::Range<usize>| date.get(r).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
    let (y, m, d) = (n(0..4), n(5..7), n(8..10));
    let (y, m) = if m <= 2 { (y - 1, m + 12) } else { (y, m) };
    // Zeller's congruence: 0 = Saturday.
    let h = (d + 13 * (m + 1) / 5 + y + y / 4 - y / 100 + y / 400).rem_euclid(7);
    ((h + 5) % 7) as usize
}

fn models(p: &mut Printer, r: &Report) {
    p.heading("Models");
    let cols = [Column::end("MODEL", 12), Column::right("TOKENS"), Column::right("SHARE"), Column::right("IN"), Column::right("OUT"), Column::right("CACHED"), Column::right("REQUESTS"), Column::end("AGENTS", 8)];
    let rows = r
        .models
        .iter()
        .map(|m| {
            vec![
                Cell::new(printable(&m.model)),
                Cell::new(tokens(m.tokens.total)).raw(m.tokens.total.to_string()),
                Cell::new(format!("{:.0}%", m.share * 100.0)).raw(format!("{:.4}", m.share)),
                Cell::new(tokens(m.tokens.input)).raw(m.tokens.input.to_string()),
                Cell::new(tokens(m.tokens.output)).raw(m.tokens.output.to_string()),
                Cell::new(tokens(m.tokens.cache_read + m.tokens.cache_write)).raw((m.tokens.cache_read + m.tokens.cache_write).to_string()),
                Cell::new(m.requests.to_string()),
                Cell::new(m.agents.join(", ")),
            ]
        })
        .collect();
    p.table("model", &cols, rows);
}

fn agents(p: &mut Printer, r: &Report) {
    p.heading("Agents");
    let cols = [Column::keep("AGENT"), Column::right("TOKENS"), Column::right("REQUESTS"), Column::right("SESSIONS"), Column::right("DAYS"), Column::right("VIA DINO"), Column::right("FROM RECORDS"), Column::end("TOP MODEL", 8), Column::keep("LAST")];
    let rows = r
        .agents
        .iter()
        .map(|a| {
            vec![
                Cell::new(printable(&a.agent)),
                Cell::new(tokens(a.tokens.total)).raw(a.tokens.total.to_string()),
                Cell::new(a.requests.to_string()),
                Cell::new(a.sessions.to_string()),
                Cell::new(a.active_days.to_string()),
                Cell::new(a.proxied.to_string()),
                Cell::new(a.recorded.to_string()),
                Cell::new(printable(&opt(a.top_model.clone()))),
                Cell::new(out::ago(out::now().saturating_sub((a.last_ms / 1000) as u64))).raw(date_of(a.last_ms)),
            ]
        })
        .collect();
    p.table("agent", &cols, rows);
    let guessed: Vec<String> = r.agents.iter().filter(|a| a.undated > 0).map(|a| a.agent.clone()).collect();
    if out::tty() && !guessed.is_empty() {
        let verb = if guessed.len() == 1 { "keeps" } else { "keep" };
        println!("{}", out::paint(&format!("{} {verb} no time per answer: counted here, but on no day or hour.", guessed.join(" and ")), Paint::Dim));
    }
}

fn projects(p: &mut Printer, r: &Report) {
    p.heading("Projects");
    let cols = [Column::end("PROJECT", 10), Column::right("TOKENS"), Column::right("REQUESTS"), Column::right("SESSIONS"), Column::end("AGENTS", 8), Column::path("FOLDER", 12), Column::keep("LAST")];
    let rows = r
        .projects
        .iter()
        .map(|x| {
            vec![
                Cell::new(printable(&x.name)),
                Cell::new(tokens(x.tokens.total)).raw(x.tokens.total.to_string()),
                Cell::new(x.requests.to_string()),
                Cell::new(x.sessions.to_string()),
                Cell::new(x.agents.join(", ")),
                Cell::new(printable(&out::short_path(&x.path))).raw(printable(&x.path)),
                Cell::new(out::ago(out::now().saturating_sub((x.last_ms / 1000) as u64))).raw(date_of(x.last_ms)),
            ]
        })
        .collect();
    p.table("project", &cols, rows);
}

fn routes(p: &mut Printer, r: &Report) {
    p.heading("Routes");
    if r.routes.is_empty() {
        if out::tty() {
            println!("{}", out::paint("No calls through dino's proxy in this range.", Paint::Dim));
        }
        return;
    }
    let cols = [Column::end("ROUTE", 12), Column::right("TOKENS"), Column::right("REQUESTS"), Column::right("FAILED"), Column::right("LIMIT HITS"), Column::right("COST"), Column::end("WINDOWS", 10)];
    let rows = r
        .routes
        .iter()
        .map(|x| {
            let windows: Vec<String> = x.windows.iter().map(|w| format!("{} {:.0}%", w.name, w.used * 100.0)).collect();
            let cost = x.cost.map(|c| format!("${c:.4}"));
            vec![
                Cell::new(printable(&x.label)).raw(printable(&x.route)),
                Cell::new(tokens(x.tokens.total)).raw(x.tokens.total.to_string()),
                Cell::new(x.requests.to_string()),
                Cell::new(x.errors.to_string()),
                Cell::new(x.limit_hits.to_string()),
                Cell::new(cost.clone().unwrap_or_else(|| "–".into())).raw(opt(x.cost)),
                Cell::new(windows.join(", ")),
            ]
        })
        .collect();
    p.table("route", &cols, rows);
}

fn speed(p: &mut Printer, r: &Report) {
    p.heading("Speed");
    if r.speed.is_empty() {
        if out::tty() {
            println!("{}", out::paint("Measured on calls through dino's proxy; none in this range.", Paint::Dim));
        }
        return;
    }
    let cols = [Column::end("MODEL", 12), Column::end("ROUTE", 10), Column::right("CALLS"), Column::right("FIRST TOKEN"), Column::right("P90"), Column::right("TOKENS/S"), Column::right("P90")];
    let secs = |ms: Option<u32>| ms.map_or("–".into(), |m| format!("{:.2}s", m as f64 / 1000.0));
    let rate = |t: Option<f64>| t.map_or("–".into(), |t| format!("{t:.0}"));
    let rows = r
        .speed
        .iter()
        .map(|s| {
            vec![
                Cell::new(printable(&s.model)),
                Cell::new(printable(&s.label)).raw(printable(&s.route)),
                Cell::new(s.requests.to_string()),
                Cell::new(secs(s.ttft_p50_ms)).raw(opt(s.ttft_p50_ms)),
                Cell::new(secs(s.ttft_p90_ms)).raw(opt(s.ttft_p90_ms)),
                Cell::new(rate(s.tps_p50)).raw(s.tps_p50.map(|t| format!("{t:.2}")).unwrap_or_default()),
                Cell::new(rate(s.tps_p90)).raw(s.tps_p90.map(|t| format!("{t:.2}")).unwrap_or_default()),
            ]
        })
        .collect();
    p.table("speed", &cols, rows);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weekdays_and_counts_read_right() {
        assert_eq!(weekday("2026-10-05"), 0, "a Monday");
        assert_eq!(weekday("2026-10-04"), 6, "a Sunday");
        assert_eq!(weekday("2024-02-29"), 3);
        assert_eq!((tokens(999), tokens(1500), tokens(2_500_000), tokens(3_210_000_000)), ("999".into(), "1.5k".into(), "2.5M".into(), "3.21B".into()));
        assert_eq!((duration(42_000), duration(14 * 60_000), duration(134 * 60_000)), ("42s".into(), "14m".into(), "2h 14m".into()));
    }
}
