//! Hermes Agent (Nous Research's `hermes`), and Hermes on dino's free tier (`free`), which it
//! reaches with its own `openai-api` provider pointed at dino by its environment
//! (`OPENAI_BASE_URL`); its config is left alone. It keeps its sessions in a SQLite store,
//! `~/.hermes/state.db` (tables `sessions` and `messages`), which dino only reads: which
//! conversation a process started, where its turn is, and its turns for the preview. It runs as a
//! Python script, found by its arguments.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, params};

use super::{Agent, ControlKind, StatusSource, Wiring, strings};
use crate::found::{self, FoundSession};
use crate::history::{self, Page, Turn, one_line, turn};
use crate::providers::Format;

pub(crate) struct Hermes {
    pub(crate) free: bool,
}

/// Where Hermes keeps its own files: `$HERMES_HOME`, else `~/.hermes`.
fn hermes_home() -> PathBuf {
    std::env::var_os("HERMES_HOME").map(PathBuf::from).unwrap_or_else(|| std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(".hermes"))
}

/// Its store, opened only to read, waiting briefly while Hermes writes.
fn store() -> Option<Connection> {
    let db = hermes_home().join("state.db");
    if !db.exists() {
        return None;
    }
    let c = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX).ok()?;
    c.busy_timeout(std::time::Duration::from_millis(500)).ok()?;
    Some(c)
}

/// A session as the store has it.
struct Row {
    id: String,
    title: Option<String>,
    cwd: Option<String>,
    started: f64,
    base_url: Option<String>,
    first_prompt: Option<String>,
    updated: f64,
}

/// Its sessions from its terminal (not its messaging gateways), newest last.
fn sessions(c: &Connection) -> Vec<Row> {
    let q = "select s.id, s.title, s.cwd, s.started_at, s.billing_base_url,
                (select content from messages m where m.session_id = s.id and m.role = 'user' order by m.id limit 1),
                coalesce((select max(timestamp) from messages m where m.session_id = s.id), s.started_at)
             from sessions s where s.source = 'cli' and coalesce(s.archived, 0) = 0 order by s.started_at";
    let Ok(mut stmt) = c.prepare(q) else { return vec![] };
    stmt.query_map([], |r| {
        Ok(Row {
            id: r.get(0)?,
            title: r.get(1)?,
            cwd: r.get(2)?,
            started: r.get(3)?,
            base_url: r.get(4)?,
            first_prompt: r.get(5)?,
            updated: r.get(6)?,
        })
    })
    .map(|rows| rows.flatten().collect())
    .unwrap_or_default()
}

impl Row {
    fn title(&self) -> String {
        self.title.as_deref().and_then(one_line).or_else(|| self.first_prompt.as_deref().and_then(one_line)).unwrap_or_else(|| "Hermes".into())
    }

    /// It talked to dino's free tier.
    fn free(&self) -> bool {
        self.base_url.as_deref().is_some_and(|u| u.starts_with("http://127.0.0.1:") && u.contains("/free"))
    }
}

/// Whether its latest message leaves a turn going: a prompt, a tool's result, or an answer that
/// asks for tools.
fn turn_in(c: &Connection, session: &str) -> Option<bool> {
    let last = c
        .query_row(
            "select role, finish_reason from messages where session_id = ?1 order by id desc limit 1",
            params![session],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
        )
        .ok();
    let ended: bool = c.query_row("select ended_at is not null from sessions where id = ?1", params![session], |r| r.get(0)).ok()?;
    Some(match last {
        _ if ended => false,
        Some((role, finish)) => match role.as_str() {
            "user" | "tool" => true,
            "assistant" => finish.as_deref() == Some("tool_calls"),
            _ => false,
        },
        None => false,
    })
}

/// Folders compared as the same folder, whatever links lead to them.
fn same_dir(a: &str, b: &Path) -> bool {
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    canon(Path::new(a)) == canon(b)
}

/// Process `pid` was pointed at dino's free tier by its environment, which (unlike a Node agent's)
/// its process still shows before its first request is done.
fn free_env(pid: u32) -> bool {
    found::run("ps", &["eww", "-o", "command=", "-p", &pid.to_string()])
        .is_some_and(|e| e.split_whitespace().any(|w| w.starts_with("OPENAI_BASE_URL=http://127.0.0.1:") && w.contains("/free")))
}

/// It, if process `pid` runs the `hermes` script.
fn is_hermes(args: &[String]) -> bool {
    args.first().is_some_and(|a| a.rsplit('/').next().is_some_and(|n| n == "hermes" || n == "hermes-agent"))
}

/// The conversation a live process (`args` after the script) is on: the one it was told to resume,
/// else the one it began in its folder since it started. Nothing until then: another Hermes in the
/// same folder has conversations open there too.
fn conversation_in(pid: u32, args: &[String]) -> Option<Row> {
    let rows = sessions(&store()?);
    let resumed = args.iter().position(|a| a == "--resume" || a == "-r").and_then(|i| args.get(i + 1));
    if let Some(id) = resumed {
        return rows.into_iter().find(|r| &r.id == id);
    }
    let cwd = crate::procinfo::cwd_of(pid)?;
    let since = crate::procinfo::started(pid).unwrap_or(0) as f64;
    rows.into_iter().find(|r| r.started + 1.0 >= since && r.cwd.as_deref().is_some_and(|c| same_dir(c, Path::new(&cwd))))
}

impl Agent for Hermes {
    fn id(&self) -> &'static str {
        if self.free { "hermes-free" } else { "hermes" }
    }

    // It asks before dangerous commands, unless told not to (`--yolo`).
    fn modes(&self) -> &'static [&'static str] {
        &["ask", "bypass"]
    }

    fn mode_label(&self, mode: &str) -> Option<&'static str> {
        Some(match mode {
            "ask" => "Default",
            "bypass" => "YOLO",
            _ => return None,
        })
    }

    fn mode_args(&self, mode: &str) -> Vec<String> {
        if mode == "bypass" { strings(&["--yolo"]) } else { vec![] }
    }

    // The free tier picks the model for each turn.
    fn picks_model(&self) -> bool {
        !self.free
    }

    fn free(&self) -> bool {
        self.free
    }

    fn provider_formats(&self) -> &'static [Format] {
        if self.free { &[] } else { &[Format::Chat] }
    }

    // Its OpenAI-compatible provider pointed at dino for the session, as on the free tier.
    fn provider_wiring(&self, url: &str, format: Format, model: &str) -> Option<Wiring> {
        if self.free || format != Format::Chat {
            return None;
        }
        let env = vec![("OPENAI_API_KEY".to_string(), "dino".to_string()), ("OPENAI_BASE_URL".to_string(), format!("{url}/v1"))];
        Some((env, strings(&["--provider", "openai-api", "-m", model])))
    }

    fn model_args(&self, model: &str) -> Vec<String> {
        strings(&["-m", model])
    }

    fn effort_args(&self, _effort: &str) -> Vec<String> {
        vec![]
    }

    fn value_flags(&self) -> &'static [&'static str] {
        &["-m", "--model", "--provider", "-t", "--toolsets", "--resume", "-r", "--skills", "-s", "-z", "--oneshot", "--usage-file"]
    }

    fn control_of(&self, name: &str, _value: Option<&str>) -> Option<ControlKind> {
        match name {
            "--yolo" => Some(ControlKind::Mode),
            "-m" | "--model" => Some(ControlKind::Model),
            _ => None,
        }
    }

    fn read_mode(&self, flags: &[(&str, Option<&str>)]) -> Option<String> {
        flags.last().map(|_| "bypass".into())
    }

    // On its own, nothing: its provider is its own setting. On the free tier its `openai-api`
    // provider, pointed at dino's OpenAI front by its environment; the key is a placeholder.
    fn wiring(&self, _route: bool, base: &dyn Fn(&str) -> String, _status_line: Option<String>) -> Wiring {
        if !self.free {
            return (vec![], vec![]);
        }
        let env = vec![("OPENAI_API_KEY".to_string(), "dino-free".to_string()), ("OPENAI_BASE_URL".to_string(), format!("{}/v1", base("free")))];
        (env, strings(&["--provider", "openai-api", "-m", "auto"]))
    }

    // Its terminal takes no prompt to start on (`-z` runs once and exits).
    fn prompt_args(&self, _prompt: String) -> Vec<String> {
        vec![]
    }

    fn session_args(&self, session: &mut Option<String>, restoring: bool) -> (Vec<String>, Vec<String>) {
        match session {
            Some(id) if restoring => (vec!["--resume".into(), id.clone()], vec![]),
            _ => (vec![], vec![]),
        }
    }

    fn status_source(&self) -> StatusSource {
        StatusSource::Polled
    }

    fn turn_now(&self, session: &str) -> Option<bool> {
        turn_in(&store()?, session)
    }

    fn new_conversation(&self, cwd: &Path, since: u64, claimed: &[String]) -> Option<String> {
        sessions(&store()?)
            .into_iter()
            .find(|r| r.started + 1.0 >= since as f64 && !claimed.contains(&r.id) && r.cwd.as_deref().is_some_and(|c| same_dir(c, cwd)))
            .map(|r| r.id)
    }

    fn busy(&self, pid: u32) -> Option<bool> {
        let args = found::args_of(pid);
        self.turn_now(&conversation_in(pid, args.get(1..).unwrap_or_default())?.id)
    }

    fn portable_flags(&self, args: &[String]) -> Vec<String> {
        found::drop_flags(args, &["--resume", "-r", "-z", "--oneshot", "--usage-file"], &["-c", "--continue", "--worktree", "-w"])
    }

    fn may_be(&self, comm: &str) -> bool {
        comm.contains("python") || comm.rsplit('/').next() == Some("hermes")
    }

    // Found only in dino's shells: it runs under whatever Python installed it.
    fn running(&self) -> Vec<FoundSession> {
        vec![]
    }

    fn inside(&self, pid: u32, comm: &str, args: &dyn Fn() -> Vec<String>) -> Option<FoundSession> {
        if self.free || !self.may_be(comm) {
            return None;
        }
        let args = args();
        if !is_hermes(&args) {
            return None;
        }
        let mut s = found::by_hand(if free_env(pid) { "hermes-free" } else { "hermes" }, pid);
        s.cwd = crate::procinfo::cwd_of(pid);
        s.title = "Hermes".into();
        if let Some(r) = conversation_in(pid, &args[1..]) {
            if r.free() {
                s.agent = "hermes-free".into();
            }
            s.title = r.title();
            s.updated_at = r.updated as u64;
            s.status = self.turn_now(&r.id).map(|b| if b { "busy" } else { "idle" }.into());
            s.session_id = r.id;
        }
        s.args = self.portable_flags(&args[1..]);
        Some(s)
    }

    fn recent(&self, running: &dyn Fn(&str) -> bool) -> Vec<FoundSession> {
        if self.free {
            return vec![];
        }
        let Some(c) = store() else { return vec![] };
        sessions(&c)
            .into_iter()
            .filter(|r| r.first_prompt.is_some() && !running(&r.id))
            .map(|r| {
                let title = r.title();
                history::recent("hermes", r.id, title, r.cwd, r.updated as u64)
            })
            .collect()
    }

    // No cloud work of its own.
    fn cloud_args(&self, _session_id: &str) -> Vec<String> {
        vec![]
    }

    // Its conversations are rows, not files.
    fn transcript(&self, _session_id: &str) -> Option<PathBuf> {
        None
    }

    fn turns(&self, _text: &str, _path: &Path, _start: u64) -> Vec<Turn> {
        vec![]
    }

    fn page(&self, session_id: &str, before: Option<u64>) -> Option<Page> {
        let c = store()?;
        turns_page(&c, session_id, before)
    }
}

/// Up to this many messages a page.
const PAGE: i64 = 200;

/// A page of a session's turns, positions being its message ids.
fn turns_page(c: &Connection, session: &str, before: Option<u64>) -> Option<Page> {
    let before = before.map_or(i64::MAX, |b| b as i64);
    let mut stmt = c
        .prepare("select id, role, content, tool_calls from messages where session_id = ?1 and id < ?2 order by id desc limit ?3")
        .ok()?;
    let mut rows: Vec<(i64, String, Option<String>, Option<String>)> =
        stmt.query_map(params![session, before, PAGE], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).ok()?.flatten().collect();
    rows.reverse();
    let first: Option<i64> = c.query_row("select min(id) from messages where session_id = ?1", params![session], |r| r.get(0)).ok()?;
    let start = match rows.first() {
        Some((id, ..)) if Some(*id) != first => *id as u64,
        _ => 0,
    };
    let mut turns = vec![];
    for (_, role, content, tool_calls) in rows {
        let text = content.unwrap_or_default();
        match role.as_str() {
            "user" => turns.extend(history::typed(&text).map(|t| turn("user", t))),
            "assistant" => {
                if !text.trim().is_empty() {
                    turns.push(turn("assistant", text.trim()));
                }
                let calls: Vec<serde_json::Value> = tool_calls.and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
                for call in calls {
                    let f = &call["function"];
                    let args = f["arguments"].as_str().and_then(|a| serde_json::from_str(a).ok()).unwrap_or(serde_json::Value::Null);
                    turns.push(turn("tool", format!("{}{}", f["name"].as_str().unwrap_or("tool"), history::hint(&args))));
                }
            }
            _ => {}
        }
    }
    Some(Page { turns, start, path: None })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The parts of Hermes 0.19's store dino reads, with a session that ran a tool.
    fn store_like_hermes() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "create table sessions (id text primary key, source text not null, title text, cwd text, started_at real not null,
                 ended_at real, billing_base_url text, archived integer default 0);
             create table messages (id integer primary key autoincrement, session_id text not null, role text not null,
                 content text, tool_calls text, timestamp real not null, finish_reason text);
             insert into sessions values ('20260930_001903_cdc088', 'cli', 'Memory Exercise PELICAN', '/r', 100.0, null, 'http://127.0.0.1:9/s/2/free/v1', 0);
             insert into sessions values ('20260930_gateway', 'telegram', null, null, 90.0, null, null, 0);
             insert into messages (session_id, role, content, timestamp) values ('20260930_001903_cdc088', 'user', 'List the files here', 101.0);
             insert into messages (session_id, role, content, tool_calls, timestamp, finish_reason) values ('20260930_001903_cdc088', 'assistant', '',
                 '[{\"id\":\"c1\",\"type\":\"function\",\"function\":{\"name\":\"terminal\",\"arguments\":\"{\\\"command\\\":\\\"ls\\\"}\"}}]', 102.0, 'tool_calls');",
        )
        .unwrap();
        c
    }

    #[test]
    fn its_store_says_where_the_turn_is() {
        let c = store_like_hermes();
        let id = "20260930_001903_cdc088";
        assert_eq!(turn_in(&c, id), Some(true), "asked for a tool");
        c.execute("insert into messages (session_id, role, content, timestamp) values (?1, 'tool', 'a b', 103.0)", params![id]).unwrap();
        assert_eq!(turn_in(&c, id), Some(true), "the tool's result is back");
        c.execute("insert into messages (session_id, role, content, timestamp, finish_reason) values (?1, 'assistant', 'Two files.', 104.0, 'stop')", params![id]).unwrap();
        assert_eq!(turn_in(&c, id), Some(false), "answered");
        assert_eq!(turn_in(&c, "nope"), None);
    }

    #[test]
    fn its_sessions_and_turns() {
        let c = store_like_hermes();
        let rows = sessions(&c);
        assert_eq!(rows.len(), 1, "its terminal's, not its gateways'");
        assert_eq!(rows[0].title(), "Memory Exercise PELICAN");
        assert!(rows[0].free(), "it talked to dino's free tier");
        let page = turns_page(&c, "20260930_001903_cdc088", None).unwrap();
        let turns: Vec<(String, String)> = page.turns.into_iter().map(|t| (t.role, t.text)).collect();
        assert_eq!(turns, [("user".to_string(), "List the files here".to_string()), ("tool".into(), "terminal ls".into())]);
        assert_eq!(page.start, 0, "from its beginning");
    }

    #[test]
    fn found_by_its_script() {
        assert!(is_hermes(&["/Users/x/.local/bin/hermes".into(), "--yolo".into()]));
        assert!(!is_hermes(&["/usr/bin/python3".into(), "-m".into(), "http.server".into()]));
        let h = Hermes { free: false };
        let args: Vec<String> = ["--resume", "20260930_x", "-m", "auto", "--yolo", "-c"].iter().map(|s| s.to_string()).collect();
        assert_eq!(h.portable_flags(&args), ["-m", "auto", "--yolo"]);
        assert_eq!(h.read_mode(&[("--yolo", None)]).as_deref(), Some("bypass"));
    }
}
