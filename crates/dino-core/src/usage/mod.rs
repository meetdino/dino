//! Usage statistics across every agent: what each model call read and wrote, when, where, on
//! which route, and how fast. Two sources feed it. dino's proxy records every call it carries
//! (`Call`, written by dinod in batches); agents' own records (`Used`, read from their
//! transcripts by each `Agent::usage`) cover what didn't go through the proxy: agents dino doesn't
//! route, agents run outside dino, and everything from before dino. A conversation the proxy
//! carried isn't counted again from its transcript (see `report`).
//!
//! It lives in `stats.db` in dino's config folder, is never synced, and can be cleared.

pub mod report;
mod store;

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

pub use report::{Range, Report, RouteWindow, report};
pub use store::{Store, path};

/// How a model call ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    #[default]
    Ok,
    Error,
    /// The route's limit turned it down: a plan's window spent, a balance gone, a 429.
    Limit,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Error => "error",
            Status::Limit => "limit",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "error" => Status::Error,
            "limit" => Status::Limit,
            _ => Status::Ok,
        }
    }
}

/// One model call dino's proxy carried, as dinod writes it down.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Call {
    /// When it was asked, in ms since the epoch.
    pub at_ms: i64,
    /// The dino session it was made for.
    pub session: String,
    /// The agent's id ("claude", "codex"), or "shell".
    pub agent: String,
    /// The agent's own conversation id, when known.
    pub conversation: Option<String>,
    /// The folder the session runs in.
    pub cwd: Option<String>,
    /// The proxy route it went out on: "anthropic", "openai", "chatgpt" (the agent's own sign-in
    /// or key), "siwc" (dino's ChatGPT plan), "or" (OpenRouter), "free", "local/<runtime>",
    /// "plan/<id>".
    pub route: String,
    pub model: Option<String>,
    pub input: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub output: u64,
    /// Until the answer's first byte.
    pub ttft_ms: Option<u32>,
    /// Until its last byte.
    pub duration_ms: Option<u32>,
    pub status: Status,
    /// Answered by a fallback: the route it stood in for (`route` is the one that answered); `None`
    /// when the session's own route answered.
    pub fallback: Option<String>,
    /// What the route itself said the call cost (OpenRouter's `usage.cost`), in its currency (USD).
    pub cost: Option<f64>,
}

/// One model answer as an agent's own record has it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Used {
    /// Unique among the agent's answers (a message id, or file + line), so reading a record again
    /// counts nothing twice.
    pub id: String,
    pub at_ms: i64,
    /// Its conversation id, as dino knows it (`FoundSession::session_id`).
    pub conversation: String,
    pub cwd: Option<String>,
    pub model: Option<String>,
    pub input: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub output: u64,
    /// The record keeps no time per answer, so `at_ms` is when the conversation was last saved: a
    /// guess. It counts toward totals, never toward a day or an hour (Cursor, CodeWhale).
    pub undated: bool,
}

/// What has been read of agents' records already, so each look reads only what's new: byte
/// offsets into files that only grow, and marks (a row id, a time) for stores that aren't files.
#[derive(Default, Debug, Clone, Serialize, Deserialize)]
pub struct Seen {
    /// Path → (mtime, size, how far it's been read).
    files: HashMap<String, (u64, u64, u64)>,
    marks: HashMap<String, String>,
    /// A file had more than one look reads: `scan` looks again.
    #[serde(skip)]
    more: bool,
}

impl Seen {
    /// The whole lines of `p` added since it was last read, with where they start; `None` when
    /// nothing changed. A file that shrank or was replaced is read again from its start (ids keep
    /// that from counting twice). `filter` skips lines that can't matter before they're kept.
    /// At most `PIECE` bytes of the file are read at once; the rest comes on the next look, which
    /// `scan` makes right away.
    pub fn new_lines(&mut self, p: &Path, filter: &[u8]) -> Option<(String, u64)> {
        let m = p.metadata().ok()?;
        let mtime = m.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_millis() as u64;
        let size = m.len();
        let key = p.to_string_lossy().into_owned();
        let from = match self.files.get(&key) {
            Some(&(t, s, _)) if t == mtime && s == size => return None,
            Some(&(_, s, off)) if size >= s && off <= size => off,
            _ => 0,
        };
        let mut f = std::fs::File::open(p).ok()?;
        f.seek(SeekFrom::Start(from)).ok()?;
        // Read in pieces: a transcript can be hundreds of megabytes, and with a filter only the
        // lines that matter are kept.
        let finder = (!filter.is_empty()).then(|| memchr::memmem::Finder::new(filter));
        let mut kept = Vec::new();
        let mut carry: Vec<u8> = Vec::new();
        let mut left = (size - from).min(PIECE);
        let mut read_to = from;
        let mut chunk = vec![0u8; CHUNK.min(left as usize).max(1)];
        while left > 0 {
            let want = chunk.len().min(left as usize);
            let n = f.read(&mut chunk[..want]).ok()?;
            if n == 0 {
                break;
            }
            left -= n as u64;
            carry.extend_from_slice(&chunk[..n]);
            // Only whole lines: a line being written is read next time.
            let Some(end) = memchr_last(&carry) else { continue };
            for line in carry[..end].split(|b| *b == b'\n') {
                if finder.as_ref().is_none_or(|f| f.find(line).is_some()) {
                    kept.extend_from_slice(line);
                    kept.push(b'\n');
                }
            }
            read_to += end as u64 + 1;
            carry.drain(..=end);
        }
        // Not all read: no mtime, so the next look goes on from here.
        let partial = read_to < size && size - from > PIECE;
        self.more |= partial;
        self.files.insert(key, (if partial { u64::MAX } else { mtime }, size, read_to));
        let buf = kept;
        Some((String::from_utf8(buf).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned()), from))
    }

    /// Whether `p` changed since `remember(p)`; a store that isn't appended to is read whole then.
    pub fn changed(&self, p: &Path) -> bool {
        let Some((mtime, size)) = stamp(p) else { return false };
        self.files.get(&p.to_string_lossy().into_owned()).is_none_or(|&(t, s, _)| t != mtime || s != size)
    }

    pub fn remember(&mut self, p: &Path) {
        if let Some((mtime, size)) = stamp(p) {
            self.files.insert(p.to_string_lossy().into_owned(), (mtime, size, size));
        }
    }

    pub fn mark(&self, key: &str) -> Option<&str> {
        self.marks.get(key).map(String::as_str)
    }

    pub fn set_mark(&mut self, key: &str, value: String) {
        self.marks.insert(key.to_string(), value);
    }
}

/// How much of a file is read at once.
const CHUNK: usize = 1 << 20;
/// How much of a file one look reads: a first look at months of history is many looks, each
/// written down before the next, so dinod never holds it all.
const PIECE: u64 = 4 << 20;

fn stamp(p: &Path) -> Option<(u64, u64)> {
    let m = p.metadata().ok()?;
    Some((m.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_millis() as u64, m.len()))
}

fn memchr_last(buf: &[u8]) -> Option<usize> {
    memchr::memrchr(b'\n', buf)
}

/// RFC 3339 time ("2026-10-04T12:34:56.789Z", or with an offset) as ms since the epoch.
pub fn parse_time(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || (b[10] != b'T' && b[10] != b' ') {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, sec) = (num(0..4)?, num(5..7)?, num(8..10)?, num(11..13)?, num(14..16)?, num(17..19)?);
    let mut rest = &s[19..];
    let mut ms = 0;
    if let Some(frac) = rest.strip_prefix('.') {
        let digits = frac.bytes().take_while(u8::is_ascii_digit).count();
        let f = &frac[..digits];
        ms = format!("{:0<3}", &f[..f.len().min(3)]).parse().unwrap_or(0);
        rest = &frac[digits..];
    }
    let offset = match rest.as_bytes().first() {
        None | Some(b'Z') | Some(b'z') => 0,
        Some(sign @ (b'+' | b'-')) => {
            let hh: i64 = rest.get(1..3)?.parse().ok()?;
            let mm: i64 = rest.get(4..6).or(rest.get(3..5)).and_then(|m| m.parse().ok()).unwrap_or(0);
            let o = (hh * 60 + mm) * 60;
            if *sign == b'+' { o } else { -o }
        }
        _ => return None,
    };
    Some((days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + sec - offset) * 1000 + ms)
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's algorithm).
pub(crate) fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Everything the agents' own records say was used that hasn't been read yet, as of `seen`,
/// handed to `keep` a piece at a time.
pub fn scan(seen: &mut Seen, mut keep: impl FnMut(&[(&'static str, Used)])) {
    loop {
        seen.more = false;
        for a in crate::agent::all() {
            let id = a.id();
            let batch: Vec<(&'static str, Used)> = a.usage(seen).into_iter().map(|u| (id, u)).collect();
            if !batch.is_empty() {
                keep(&batch);
            }
        }
        if !seen.more {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_parse_with_any_offset() {
        assert_eq!(parse_time("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_time("2026-10-04T12:00:00.5Z"), Some(1_791_115_200_500));
        assert_eq!(parse_time("2026-10-04T14:00:00.500+02:00"), parse_time("2026-10-04T12:00:00.5Z"));
        assert_eq!(parse_time("2026-10-04T07:00:00-0500"), parse_time("2026-10-04T12:00:00Z"));
        assert_eq!(parse_time("2026-10-04 12:00:00"), parse_time("2026-10-04T12:00:00Z"));
        assert_eq!(parse_time("yesterday"), None);
    }

    #[test]
    fn only_new_whole_lines_are_read() {
        let dir = std::env::temp_dir().join(format!("dino-usage-seen-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.jsonl");
        std::fs::write(&p, "{\"usage\":1}\n{\"x\":2}\n{\"usage\":3").unwrap();
        let mut seen = Seen::default();
        let (text, from) = seen.new_lines(&p, b"\"usage\"").unwrap();
        assert_eq!((text.as_str(), from), ("{\"usage\":1}\n", 0));
        assert_eq!(seen.new_lines(&p, b""), None, "nothing changed");
        std::fs::write(&p, "{\"usage\":1}\n{\"x\":2}\n{\"usage\":3}\n").unwrap();
        let (text, from) = seen.new_lines(&p, b"").unwrap();
        assert_eq!((text.as_str(), from), ("{\"usage\":3}\n", 20));
        // Replaced by something shorter: read again from the start.
        std::fs::write(&p, "{\"usage\":9}\n").unwrap();
        let (text, from) = seen.new_lines(&p, b"").unwrap();
        assert_eq!((text.as_str(), from), ("{\"usage\":9}\n", 0));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_long_record_is_read_a_piece_at_a_time() {
        let dir = std::env::temp_dir().join(format!("dino-usage-piece-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("big.jsonl");
        let line = format!("{{\"usage\":1,\"pad\":\"{}\"}}\n", "x".repeat(1000));
        let lines = (PIECE as usize / line.len()) * 2 + 10;
        std::fs::write(&p, line.repeat(lines)).unwrap();
        let mut seen = Seen::default();
        let (mut read, mut looks) = (0, 0);
        loop {
            seen.more = false;
            let Some((text, _)) = seen.new_lines(&p, b"\"usage\"") else { break };
            read += text.lines().count();
            looks += 1;
            if !seen.more {
                break;
            }
        }
        assert_eq!((read, looks), (lines, 3), "every line once, over three looks");
        assert_eq!(seen.new_lines(&p, b""), None, "all read");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
