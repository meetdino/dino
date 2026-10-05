//! How dino's commands print. On a terminal: tables that fit its width, with a dim header and the
//! app's status colours. Piped: no header, no colour, nothing cut short, one tab-separated line per
//! row, for `cut` and `awk`. No colour either with `NO_COLOR` set (no-color.org) or a dumb terminal.

use std::io::IsTerminal;
use std::sync::OnceLock;

use dino_core::status::Status;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Between columns.
const GAP: usize = 2;

pub fn tty() -> bool {
    static TTY: OnceLock<bool> = OnceLock::new();
    *TTY.get_or_init(|| std::io::stdout().is_terminal())
}

fn colour_on(terminal: bool) -> bool {
    terminal && std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty()) && std::env::var("TERM").is_ok_and(|t| t != "dumb")
}

fn colour() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| colour_on(tty()))
}

#[derive(Clone, Copy, PartialEq)]
pub enum Paint {
    Plain,
    Dim,
    Bold,
    Green,
    /// The app's orange, for what needs you: bold, so it stands out from everything else.
    Orange,
    Blue,
    Red,
}

impl Paint {
    fn sgr(self) -> &'static str {
        match self {
            Paint::Plain => "",
            Paint::Dim => "2",
            Paint::Bold => "1",
            Paint::Green => "32",
            Paint::Orange => "1;33",
            Paint::Blue => "34",
            Paint::Red => "31",
        }
    }

    /// The colour of the app's status dot.
    pub fn of(status: Status) -> Paint {
        match status {
            Status::NeedsYou => Paint::Orange,
            Status::Working => Paint::Green,
            Status::Done => Paint::Blue,
            Status::Idle | Status::Ended => Paint::Dim,
            Status::Exited => Paint::Red,
        }
    }
}

pub fn paint(text: &str, p: Paint) -> String {
    painted(text, p, colour())
}

fn painted(text: &str, p: Paint, on: bool) -> String {
    if !on || p == Paint::Plain || text.is_empty() { text.into() } else { format!("\x1b[{}m{text}\x1b[0m", p.sgr()) }
}

/// A status as the app shows it: its dot and word in its colour. Without colour the dot says
/// nothing the word doesn't, so it's left out.
pub fn status(s: Status) -> String {
    if colour() { paint(&format!("● {}", s.label()), Paint::of(s)) } else { s.label().into() }
}

/// `error: <what>`, the way cargo and git say it, red on a terminal; on lines of its own after it,
/// what to do about it.
pub fn error(e: &anyhow::Error) {
    let message = format!("{e:#}");
    // A command used wrong says how to use it, and that's all.
    if message.starts_with("usage: ") {
        eprintln!("{message}");
        return;
    }
    let on = colour_on(std::io::stderr().is_terminal());
    eprintln!("{} {message}", painted("error:", Paint::Red, on));
}

/// The terminal's width, for fitting tables; 80 when it can't be told.
pub fn width() -> usize {
    crossterm::terminal::size().ok().map(|(c, _)| c as usize).filter(|&c| c >= 20).unwrap_or(80)
}

pub enum Fit {
    /// Never shortened.
    Keep,
    /// Cut at the end: `Dinosaurs ess…`.
    End,
    /// A path, cut in the middle so where it starts and its last folders show: `~/src/…/api/v2`.
    Path,
}

pub struct Column {
    pub head: &'static str,
    pub fit: Fit,
    /// The narrowest it gets when the table has to give way.
    pub min: usize,
    pub right: bool,
}

impl Column {
    pub const fn keep(head: &'static str) -> Column {
        Column { head, fit: Fit::Keep, min: 0, right: false }
    }
    pub const fn end(head: &'static str, min: usize) -> Column {
        Column { head, fit: Fit::End, min, right: false }
    }
    pub const fn path(head: &'static str, min: usize) -> Column {
        Column { head, fit: Fit::Path, min, right: false }
    }
    pub const fn right(head: &'static str) -> Column {
        Column { head, fit: Fit::Keep, min: 0, right: true }
    }
}

/// What a cell shows on a terminal, and what it prints when piped (a full path, a timestamp).
pub struct Cell {
    text: String,
    raw: String,
    paint: Paint,
    /// Shown already coloured (a status), so not cut.
    styled: Option<String>,
}

impl Cell {
    pub fn new(text: impl Into<String>) -> Cell {
        let text = text.into();
        Cell { raw: text.clone(), text, paint: Paint::Plain, styled: None }
    }
    pub fn raw(mut self, raw: impl Into<String>) -> Cell {
        self.raw = raw.into();
        self
    }
    pub fn paint(mut self, p: Paint) -> Cell {
        self.paint = p;
        self
    }
    pub fn status(s: Status) -> Cell {
        let text = if colour() { format!("● {}", s.label()) } else { s.label().into() };
        Cell { text, raw: s.key().into(), paint: Paint::Plain, styled: Some(status(s)) }
    }
}

/// Rows under a dim header, fitted to the terminal; piped, tab-separated raw values and no header.
pub fn table(columns: &[Column], rows: &[Vec<Cell>], header: bool) -> String {
    if !tty() {
        return rows.iter().map(|r| r.iter().map(|c| c.raw.as_str()).collect::<Vec<_>>().join("\t") + "\n").collect();
    }
    render(columns, rows, header, width(), colour())
}

/// Rows under a dim header, fitted to the terminal, piped too: for people, not for `cut`.
pub fn grid(columns: &[Column], rows: &[Vec<Cell>]) -> String {
    render(columns, rows, true, width(), colour())
}

fn render(columns: &[Column], rows: &[Vec<Cell>], header: bool, width: usize, on: bool) -> String {
    let mut widths: Vec<usize> = columns
        .iter()
        .enumerate()
        .map(|(i, c)| rows.iter().map(|r| r[i].text.width()).chain(header.then(|| c.head.width())).max().unwrap_or(0))
        .collect();
    // Too wide: take a character at a time from the widest column that can give one.
    let total = |w: &[usize]| w.iter().sum::<usize>() + GAP * w.len().saturating_sub(1);
    while total(&widths) > width {
        let Some(i) = (0..columns.len())
            .filter(|&i| !matches!(columns[i].fit, Fit::Keep) && widths[i] > columns[i].min.max(if header { columns[i].head.width() } else { 1 }))
            .max_by_key(|&i| widths[i])
        else {
            break;
        };
        widths[i] -= 1;
    }
    let last = columns.len() - 1;
    let line = |cells: Vec<(String, Option<String>, Paint)>| {
        let mut out = String::new();
        for (i, (text, styled, p)) in cells.into_iter().enumerate() {
            let pad = " ".repeat(widths[i].saturating_sub(text.width()));
            let shown = styled.unwrap_or_else(|| painted(&text, p, on));
            if columns[i].right {
                out += &pad;
                out += &shown;
            } else {
                out += &shown;
                if i < last {
                    out += &pad;
                }
            }
            if i < last {
                out += &" ".repeat(GAP);
            }
        }
        out.trim_end().to_string() + "\n"
    };
    let mut out = String::new();
    if header {
        out += &line(columns.iter().map(|c| (c.head.to_string(), None, Paint::Dim)).collect());
    }
    for r in rows {
        out += &line(
            r.iter()
                .enumerate()
                .map(|(i, c)| match (&columns[i].fit, &c.styled) {
                    (_, Some(s)) => (c.text.clone(), Some(s.clone()), c.paint),
                    (Fit::Path, None) => (fit_path(&c.text, widths[i]), None, c.paint),
                    _ => (fit_end(&c.text, widths[i]), None, c.paint),
                })
                .collect(),
        );
    }
    out
}

/// `s` in `w` columns, cut at the end with `…`.
pub fn fit_end(s: &str, w: usize) -> String {
    if s.width() <= w {
        return s.into();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if used + cw + 1 > w {
            break;
        }
        used += cw;
        out.push(c);
    }
    out + "…"
}

/// `s` in `w` columns, cut at the start with `…`.
fn fit_start(s: &str, w: usize) -> String {
    if s.width() <= w {
        return s.into();
    }
    let mut kept = vec![];
    let mut used = 0;
    for c in s.chars().rev() {
        let cw = c.width().unwrap_or(0);
        if used + cw + 1 > w {
            break;
        }
        used += cw;
        kept.push(c);
    }
    std::iter::once('…').chain(kept.into_iter().rev()).collect()
}

/// A path in `w` columns: its start (`~`, `/tmp`) and as many of its last folders as fit, the
/// middle given way to `…`. When keeping the start would leave only the last folder, its last
/// folders say more: `…/fanrepo/claude`.
pub fn fit_path(p: &str, w: usize) -> String {
    if p.width() <= w {
        return p.into();
    }
    let parts: Vec<&str> = p.split('/').collect();
    // `~/a/b` starts with `~`; `/tmp/a/b` with `/tmp`.
    let (head, rest) = match parts.as_slice() {
        ["", first, rest @ ..] => (format!("/{first}"), rest),
        [first, rest @ ..] => (first.to_string(), rest),
        [] => return fit_start(p, w),
    };
    let fits = |head: &str| (0..rest.len()).rev().map_while(|i| Some(format!("{head}…/{}", rest[i..].join("/"))).filter(|s| s.width() <= w)).enumerate().last();
    match (fits(&format!("{head}/")), fits("")) {
        (Some((with, s)), Some((without, _))) if with > 0 || without == 0 => s,
        (_, Some((_, s))) => s,
        _ => fit_start(p, w),
    }
}

/// A path as people write it: `~` for home, and `/tmp` rather than where macOS keeps it.
pub fn short_path(p: &str) -> String {
    let p = ["/private/tmp", "/private/var", "/private/etc"].iter().find_map(|d| under(p, d).map(|_| &p["/private".len()..])).unwrap_or(p);
    match std::env::var("HOME") {
        Ok(home) if home.len() > 1 => under(p, &home).map_or_else(|| p.into(), |rest| format!("~{rest}")),
        _ => p.into(),
    }
}

/// What's left of `p` inside `dir`, when it's in it.
fn under<'a>(p: &'a str, dir: &str) -> Option<&'a str> {
    p.strip_prefix(dir).filter(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// How long ago, in a word or two: `now`, `5m ago`, `3h ago`, `2d ago`.
pub fn ago(secs: u64) -> String {
    match secs {
        0..60 => "now".into(),
        60..3600 => format!("{}m ago", secs / 60),
        3600..86_400 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86_400),
    }
}

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Seconds since the epoch as an ISO 8601 time in UTC: `2026-10-03T21:40:12Z`.
pub fn iso(secs: u64) -> String {
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Howard Hinnant's days-to-civil.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// Aligned `key  value` lines, keys dim: `dino power`, `dino claude-token`.
pub fn fields(rows: &[(&str, String)]) -> String {
    let w = rows.iter().map(|(k, _)| k.width()).max().unwrap_or(0);
    rows.iter().map(|(k, v)| format!("{}{}  {v}\n", paint(k, Paint::Dim), " ".repeat(w - k.width()))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_give_way_in_the_middle() {
        let p = "~/src/dino-terminal/.claude/worktrees/cli";
        assert_eq!(fit_path(p, 60), p);
        assert_eq!(fit_path(p, 24), "~/…/worktrees/cli");
        assert_eq!(fit_path("/tmp/a-long-folder/with/levels/payments-service", 30), "/tmp/…/levels/payments-service");
        // The start would leave only the last folder: more of the end instead.
        assert_eq!(fit_path("/tmp/dino/home/worktrees/fanrepo/reply-with-the-9c90/claude", 30), "…/reply-with-the-9c90/claude");
        // Not even the last folder fits after the start: the end of it.
        assert_eq!(fit_path("~/a/payments-service-and-more", 12), "…ce-and-more");
        assert_eq!(fit_end("Dinosaurs essay", 10), "Dinosaurs…");
        assert_eq!(fit_end("日本語のタイトル", 7), "日本語…");
    }

    #[test]
    fn times_read_as_people_say_them() {
        assert_eq!(iso(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso(1_791_074_159), "2026-10-04T00:35:59Z");
        assert_eq!(iso(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!((ago(5), ago(125), ago(7200), ago(200_000)), ("now".into(), "2m ago".into(), "2h ago".into(), "2d ago".into()));
    }

    #[test]
    fn tables_fit_and_stay_aligned() {
        let cols = [Column::keep("ID"), Column::end("NAME", 6), Column::path("FOLDER", 8)];
        let rows = vec![
            vec![Cell::new("1"), Cell::new("Dinosaurs essay"), Cell::new("~/src/dino-terminal/work")],
            vec![Cell::new("12"), Cell::new("shell"), Cell::new("/tmp")],
        ];
        let wide = render(&cols, &rows, true, 80, false);
        assert_eq!(wide, "ID  NAME             FOLDER\n1   Dinosaurs essay  ~/src/dino-terminal/work\n12  shell            /tmp\n");
        let narrow = render(&cols, &rows, true, 30, false);
        assert!(narrow.lines().all(|l| l.width() <= 30), "{narrow}");
        assert!(narrow.contains("~/…/work"), "{narrow}");
    }
}
