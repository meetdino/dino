//! `stats.db`: the proxy's calls, agents' own records of theirs, which conversation each dino
//! session was on, and how far agents' records have been read.

use std::path::PathBuf;

use rusqlite::{Connection, OptionalExtension, params};

use super::{Call, Seen, Status, Used};

pub fn path() -> PathBuf {
    crate::config_dir().join("stats.db")
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS calls (
    id INTEGER PRIMARY KEY,
    -- 0: dino's proxy carried it; 1: an agent's own record says so.
    source INTEGER NOT NULL,
    -- Agents' records: agent + the answer's id, so reading again adds nothing.
    key TEXT UNIQUE,
    ts INTEGER NOT NULL,
    agent TEXT NOT NULL,
    session TEXT,
    conversation TEXT,
    cwd TEXT,
    route TEXT,
    model TEXT,
    input INTEGER NOT NULL DEFAULT 0,
    cache_read INTEGER NOT NULL DEFAULT 0,
    cache_write INTEGER NOT NULL DEFAULT 0,
    output INTEGER NOT NULL DEFAULT 0,
    ttft_ms INTEGER,
    duration_ms INTEGER,
    status TEXT NOT NULL DEFAULT 'ok',
    fallback TEXT,
    cost REAL,
    -- 1: its time is a guess (see `Used::undated`).
    undated INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS calls_ts ON calls(ts);
-- Which conversation a dino session's agent was on: learned late for some agents (Codex).
CREATE TABLE IF NOT EXISTS links (
    session TEXT NOT NULL,
    conversation TEXT NOT NULL,
    agent TEXT NOT NULL,
    PRIMARY KEY (session, conversation)
);
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
";

/// One row, as the report reads it.
#[derive(Clone, Debug, Default)]
pub(crate) struct Row {
    pub proxied: bool,
    pub ts: i64,
    pub agent: String,
    pub session: Option<String>,
    pub conversation: Option<String>,
    pub cwd: Option<String>,
    pub route: Option<String>,
    pub model: Option<String>,
    pub input: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub output: u64,
    pub ttft_ms: Option<u32>,
    pub duration_ms: Option<u32>,
    pub status: Status,
    pub fallback: Option<String>,
    pub cost: Option<f64>,
    pub undated: bool,
}

/// (dino session, conversation) pairs.
pub(crate) type Links = Vec<(String, String)>;

pub struct Store {
    db: Connection,
}

impl Store {
    pub fn open() -> anyhow::Result<Self> {
        Self::open_at(&path())
    }

    pub fn open_at(p: &std::path::Path) -> anyhow::Result<Self> {
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let db = Connection::open(p)?;
        // Never synced, and private like the rest of dino's folder.
        let _ = std::fs::set_permissions(p, std::os::unix::fs::PermissionsExt::from_mode(0o600));
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        db.pragma_update(None, "journal_mode", "WAL")?;
        db.pragma_update(None, "synchronous", "NORMAL")?;
        db.execute_batch(SCHEMA)?;
        // A stats.db from before `undated`.
        let has = db.prepare("SELECT 1 FROM pragma_table_info('calls') WHERE name = 'undated'")?.exists([])?;
        if !has {
            db.execute_batch("ALTER TABLE calls ADD COLUMN undated INTEGER NOT NULL DEFAULT 0")?;
        }
        Ok(Self { db })
    }

    /// The proxy's calls, all in one transaction.
    pub fn add_calls(&mut self, calls: &[Call]) -> anyhow::Result<()> {
        let tx = self.db.transaction()?;
        {
            let mut ins = tx.prepare_cached(
                "INSERT INTO calls (source, ts, agent, session, conversation, cwd, route, model, input, cache_read, cache_write, output, ttft_ms, duration_ms, status, fallback, cost)
                 VALUES (0, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            )?;
            let mut link = tx.prepare_cached("INSERT OR IGNORE INTO links (session, conversation, agent) VALUES (?1, ?2, ?3)")?;
            for c in calls {
                ins.execute(params![
                    c.at_ms,
                    c.agent,
                    c.session,
                    c.conversation,
                    c.cwd,
                    c.route,
                    c.model,
                    c.input as i64,
                    c.cache_read as i64,
                    c.cache_write as i64,
                    c.output as i64,
                    c.ttft_ms,
                    c.duration_ms,
                    c.status.as_str(),
                    c.fallback,
                    c.cost
                ])?;
                if let Some(conv) = &c.conversation {
                    link.execute(params![c.session, conv, c.agent])?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// What dino's proxy carried for one dino session, by route: its calls since it started (an
    /// id can be used again by a later session), any under `before` (the id an unarchived session
    /// had), and any on its conversation `conversation` whichever session made them (one resumed
    /// in another session). Tokens: input, cache read, cache write, output.
    pub fn session_routes(&self, session: &str, before: Option<&str>, since_ms: i64, conversation: Option<&str>) -> anyhow::Result<Vec<(String, [u64; 4])>> {
        let mut q = self.db.prepare_cached(
            "SELECT COALESCE(route, ''), SUM(input), SUM(cache_read), SUM(cache_write), SUM(output) FROM calls
             WHERE source = 0 AND ((session = ?1 AND ts >= ?2) OR (?3 IS NOT NULL AND session = ?3) OR (?4 IS NOT NULL AND conversation = ?4))
             GROUP BY COALESCE(route, '')",
        )?;
        let rows = q.query_map(params![session, since_ms, before, conversation], |r| {
            let n = |i: usize| r.get::<_, i64>(i).map(|v| v.max(0) as u64);
            Ok((r.get::<_, String>(0)?, [n(1)?, n(2)?, n(3)?, n(4)?]))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// dino session `session`'s agent is (or was) on `conversation`.
    pub fn link(&mut self, session: &str, agent: &str, conversation: &str) -> anyhow::Result<()> {
        self.db.execute("INSERT OR IGNORE INTO links (session, conversation, agent) VALUES (?1, ?2, ?3)", params![session, conversation, agent])?;
        Ok(())
    }

    /// Answers from agents' own records; ones already here only grow (see below). Returns how
    /// many rows were added or grew.
    pub fn add_used(&mut self, used: &[(&str, Used)]) -> anyhow::Result<usize> {
        let tx = self.db.transaction()?;
        let mut added = 0;
        {
            let mut ins = tx.prepare_cached(
                // An answer read again keeps the most each count reached: one written as several
                // lines can be read across two looks.
                "INSERT INTO calls (source, key, ts, agent, conversation, cwd, model, input, cache_read, cache_write, output, undated)
                 VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                 ON CONFLICT(key) DO UPDATE SET input = max(input, excluded.input), cache_read = max(cache_read, excluded.cache_read),
                   cache_write = max(cache_write, excluded.cache_write), output = max(output, excluded.output)
                 WHERE excluded.input > input OR excluded.cache_read > cache_read OR excluded.cache_write > cache_write OR excluded.output > output",
            )?;
            for (agent, u) in used {
                added += ins.execute(params![
                    format!("{agent}:{}", u.id),
                    u.at_ms,
                    agent,
                    u.conversation,
                    u.cwd,
                    u.model,
                    u.input as i64,
                    u.cache_read as i64,
                    u.cache_write as i64,
                    u.output as i64,
                    u.undated
                ])?;
            }
        }
        tx.commit()?;
        Ok(added)
    }

    pub fn seen(&self) -> Seen {
        let text: Option<String> = self.db.query_row("SELECT value FROM meta WHERE key = 'seen'", [], |r| r.get(0)).optional().ok().flatten();
        text.and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save_seen(&mut self, seen: &Seen) -> anyhow::Result<()> {
        self.db.execute("INSERT OR REPLACE INTO meta (key, value) VALUES ('seen', ?1)", params![serde_json::to_string(seen)?])?;
        Ok(())
    }

    /// Forget everything, including how far agents' records were read: the next look reads them
    /// all again (history before dino comes back; what only the proxy saw doesn't).
    pub fn clear(&mut self) -> anyhow::Result<()> {
        self.db.execute_batch("DELETE FROM calls; DELETE FROM links; DELETE FROM meta;")?;
        let _ = self.db.execute_batch("VACUUM;");
        Ok(())
    }

    /// Every row from `since` (ms) on, oldest first, one at a time.
    pub(crate) fn each_row(&self, since: i64, mut f: impl FnMut(Row)) -> anyhow::Result<()> {
        let mut q = self.db.prepare_cached(
            "SELECT source, ts, agent, session, conversation, cwd, route, model, input, cache_read, cache_write, output, ttft_ms, duration_ms, status, fallback, cost, undated
             FROM calls WHERE ts >= ?1 ORDER BY ts",
        )?;
        let mut rows = q.query(params![since])?;
        while let Some(r) = rows.next()? {
            f(Row {
                proxied: r.get::<_, i64>(0)? == 0,
                ts: r.get(1)?,
                agent: r.get(2)?,
                session: r.get(3)?,
                conversation: r.get(4)?,
                cwd: r.get(5)?,
                route: r.get(6)?,
                model: r.get(7)?,
                input: r.get::<_, i64>(8)? as u64,
                cache_read: r.get::<_, i64>(9)? as u64,
                cache_write: r.get::<_, i64>(10)? as u64,
                output: r.get::<_, i64>(11)? as u64,
                ttft_ms: r.get(12)?,
                duration_ms: r.get(13)?,
                status: Status::parse(&r.get::<_, String>(14)?),
                fallback: r.get(15)?,
                cost: r.get(16)?,
                undated: r.get(17)?,
            });
        }
        Ok(())
    }

    /// The links between dino sessions and conversations.
    pub(crate) fn links(&self) -> anyhow::Result<Links> {
        let mut q = self.db.prepare_cached("SELECT session, conversation FROM links")?;
        Ok(q.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?)
    }

    /// Proxy calls of each linked conversation: (conversation, first ms, last ms), across all time,
    /// so a transcript's answers inside that span are known to be counted already.
    pub(crate) fn proxied_spans(&self) -> anyhow::Result<Vec<(String, i64, i64)>> {
        let mut q = self.db.prepare_cached(
            "SELECT l.conversation, MIN(c.ts), MAX(c.ts + COALESCE(c.duration_ms, 0)) FROM calls c JOIN links l ON l.session = c.session
             WHERE c.source = 0 GROUP BY l.session, l.conversation",
        )?;
        Ok(q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<Vec<_>, _>>()?)
    }
}
