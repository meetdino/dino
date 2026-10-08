//! `stats.db`: the proxy's calls, agents' own records of theirs, which conversation each dino
//! session was on, and how far agents' records have been read.

use std::collections::HashMap;
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
    undated INTEGER NOT NULL DEFAULT 0,
    -- The answer's id as both the proxy and the agent's record know it (`Call::answer`), so an
    -- answer in both is counted once, exactly.
    answer TEXT,
    -- 1: one of the conversation's subagents made it.
    subagent INTEGER NOT NULL DEFAULT 0
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

/// How far apart in time a call the proxy carried and its answer in an agent's record can be, for
/// `match_carried`.
const MATCH_MS: i64 = 10 * 60 * 1000;

/// The calls dino's proxy carried before it kept answer ids: each is given the id of the answer
/// in Claude's record with exactly the same four counts, closest in time (and within `MATCH_MS`),
/// each answer once, and that answer's conversation (the session's may have been another: a
/// background job or a `claude -p` run in its terminal). From then on the two are one answer, as
/// with calls since; one with no such answer stays as it was.
fn match_carried(db: &Connection) -> anyhow::Result<usize> {
    type Counts = (i64, i64, i64, i64);
    let mut answers: HashMap<Counts, Vec<(i64, String, Option<String>)>> = HashMap::new();
    {
        let mut q = db.prepare("SELECT ts, input, cache_read, cache_write, output, answer, conversation FROM calls WHERE source = 1 AND answer IS NOT NULL")?;
        let mut rows = q.query([])?;
        while let Some(r) = rows.next()? {
            answers.entry((r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)).or_default().push((r.get(0)?, r.get(5)?, r.get(6)?));
        }
    }
    let mut calls: Vec<(i64, i64, Counts)> = vec![];
    {
        let mut q = db.prepare(
            "SELECT id, ts, input, cache_read, cache_write, output FROM calls
             WHERE source = 0 AND answer IS NULL AND status = 'ok' AND input + cache_read + cache_write + output > 0 ORDER BY ts",
        )?;
        let mut rows = q.query([])?;
        while let Some(r) = rows.next()? {
            calls.push((r.get(0)?, r.get(1)?, (r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)));
        }
    }
    let mut set = db.prepare("UPDATE calls SET answer = ?2, conversation = coalesce(?3, conversation) WHERE id = ?1")?;
    let mut matched = 0;
    for (id, ts, counts) in calls {
        let Some(same) = answers.get_mut(&counts) else { continue };
        let Some(i) = (0..same.len()).filter(|&i| (same[i].0 - ts).abs() <= MATCH_MS).min_by_key(|&i| (same[i].0 - ts).abs()) else { continue };
        let (_, answer, conversation) = same.swap_remove(i);
        set.execute(params![id, answer, conversation])?;
        matched += 1;
    }
    Ok(matched)
}

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
    /// One of the conversation's subagents made it.
    pub subagent: bool,
    /// It has an answer id (`Call::answer`), and the other source has the same answer: the
    /// agent's record has what the proxy carried, or the proxy carried what the record says.
    pub has_answer: bool,
    pub matched: bool,
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
        let db = Self::connect(p)?;
        let mut store = Self { db };
        store.migrate()?;
        Ok(store)
    }

    fn connect(p: &std::path::Path) -> anyhow::Result<Connection> {
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
        Ok(db)
    }

    /// A stats.db from before a column was added.
    fn migrate(&mut self) -> anyhow::Result<()> {
        let has = |db: &Connection, column: &str| -> rusqlite::Result<bool> { db.prepare("SELECT 1 FROM pragma_table_info('calls') WHERE name = ?1")?.exists([column]) };
        if !has(&self.db, "undated")? {
            self.db.execute_batch("ALTER TABLE calls ADD COLUMN undated INTEGER NOT NULL DEFAULT 0")?;
        }
        if !has(&self.db, "answer")? {
            let tx = self.db.transaction()?;
            tx.execute_batch(
                "ALTER TABLE calls ADD COLUMN answer TEXT;
                 ALTER TABLE calls ADD COLUMN subagent INTEGER NOT NULL DEFAULT 0;
                 -- Claude's answers read before: their key is the answer's id.
                 UPDATE calls SET answer = substr(key, 8) WHERE source = 1 AND key LIKE 'claude:%';",
            )?;
            // Which of them were subagents' isn't known: their records are read again, which
            // marks them (and adds nothing twice).
            let text: Option<String> = tx.query_row("SELECT value FROM meta WHERE key = 'seen'", [], |r| r.get(0)).optional()?;
            if let Some(mut seen) = text.and_then(|t| serde_json::from_str::<Seen>(&t).ok()) {
                seen.files.retain(|path, _| !crate::history::is_subagent(std::path::Path::new(path)));
                tx.execute("INSERT OR REPLACE INTO meta (key, value) VALUES ('seen', ?1)", params![serde_json::to_string(&seen)?])?;
            }
            match_carried(&tx)?;
            tx.commit()?;
        }
        self.db.execute_batch("CREATE INDEX IF NOT EXISTS calls_answer ON calls(answer) WHERE answer IS NOT NULL")?;
        Ok(())
    }

    /// The proxy's calls, all in one transaction.
    pub fn add_calls(&mut self, calls: &[Call]) -> anyhow::Result<()> {
        let tx = self.db.transaction()?;
        {
            let mut ins = tx.prepare_cached(
                "INSERT INTO calls (source, ts, agent, session, conversation, cwd, route, model, input, cache_read, cache_write, output, ttft_ms, duration_ms, status, fallback, cost, subagent, answer)
                 VALUES (0, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
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
                    c.cost,
                    c.subagent,
                    c.answer
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

    /// Of `session_routes`, what its subagents used.
    pub fn session_subagents(&self, session: &str, before: Option<&str>, since_ms: i64, conversation: Option<&str>) -> anyhow::Result<[u64; 4]> {
        let mut q = self.db.prepare_cached(
            "SELECT COALESCE(SUM(input), 0), COALESCE(SUM(cache_read), 0), COALESCE(SUM(cache_write), 0), COALESCE(SUM(output), 0) FROM calls
             WHERE source = 0 AND subagent = 1 AND ((session = ?1 AND ts >= ?2) OR (?3 IS NOT NULL AND session = ?3) OR (?4 IS NOT NULL AND conversation = ?4))",
        )?;
        Ok(q.query_row(params![session, since_ms, before, conversation], |r| {
            let n = |i: usize| r.get::<_, i64>(i).map(|v| v.max(0) as u64);
            Ok([n(0)?, n(1)?, n(2)?, n(3)?])
        })?)
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
                // Read again, it also learns what it didn't know then: its answer's id, that a
                // subagent gave it.
                "INSERT INTO calls (source, key, ts, agent, conversation, cwd, model, input, cache_read, cache_write, output, undated, answer, subagent)
                 VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                 ON CONFLICT(key) DO UPDATE SET input = max(input, excluded.input), cache_read = max(cache_read, excluded.cache_read),
                   cache_write = max(cache_write, excluded.cache_write), output = max(output, excluded.output),
                   answer = coalesce(answer, excluded.answer), subagent = max(subagent, excluded.subagent)
                 WHERE excluded.input > input OR excluded.cache_read > cache_read OR excluded.cache_write > cache_write OR excluded.output > output
                   OR (answer IS NULL AND excluded.answer IS NOT NULL) OR excluded.subagent > subagent",
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
                    u.undated,
                    u.answer,
                    u.subagent
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
            "SELECT source, ts, agent, session, conversation, cwd, route, model, input, cache_read, cache_write, output, ttft_ms, duration_ms, status, fallback, cost, undated,
               subagent, answer IS NOT NULL, (SELECT MAX(o.subagent) FROM calls o WHERE o.answer = c.answer AND o.source != c.source)
             FROM calls c WHERE ts >= ?1 ORDER BY ts",
        )?;
        let mut rows = q.query(params![since])?;
        while let Some(r) = rows.next()? {
            // The same answer in the other source, and whether a subagent gave it as that says
            // (a call carried before calls said so).
            let other: Option<bool> = r.get(20)?;
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
                subagent: r.get::<_, bool>(18)? || other == Some(true),
                has_answer: r.get(19)?,
                matched: other.is_some(),
            });
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn db(&self) -> &Connection {
        &self.db
    }

    /// The links between dino sessions and conversations.
    pub(crate) fn links(&self) -> anyhow::Result<Links> {
        let mut q = self.db.prepare_cached("SELECT session, conversation FROM links")?;
        Ok(q.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?)
    }

    /// Proxy calls of each linked conversation: (conversation, first ms, last ms), across all time,
    /// so a transcript's answers inside that span are taken as counted already. Only answered
    /// calls with no answer id (other agents', Claude's before dino kept it): one with an id is
    /// matched to its record exactly, and a span would also hide answers the proxy never carried.
    pub(crate) fn proxied_spans(&self) -> anyhow::Result<Vec<(String, i64, i64)>> {
        let mut q = self.db.prepare_cached(
            "SELECT l.conversation, MIN(c.ts), MAX(c.ts + COALESCE(c.duration_ms, 0)) FROM calls c JOIN links l ON l.session = c.session
             WHERE c.source = 0 AND c.answer IS NULL AND c.status = 'ok' GROUP BY l.session, l.conversation",
        )?;
        Ok(q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<Vec<_>, _>>()?)
    }
}
