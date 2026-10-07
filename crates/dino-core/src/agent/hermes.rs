//! Hermes Agent (Nous Research's `hermes`), and Hermes on dino's free tier (`free`), which it
//! reaches with its own `custom` provider (an OpenAI-compatible endpoint, Chat Completions)
//! pointed at dino by its environment (`CUSTOM_BASE_URL`); its config is left alone. It keeps its
//! sessions in a SQLite store, `~/.hermes/state.db` (tables `sessions` and `messages`), which dino
//! only reads: which conversation a process started, where its turn is, and its turns for the
//! preview. It runs as a Python script, found by its arguments.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, params};
use serde_json::Value;

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

/// The tools its last message asks for, while their results aren't in yet: the calls out now.
fn tools_in(c: &Connection, session: &str) -> Vec<String> {
    let last = c.query_row(
        "select role, tool_calls from messages where session_id = ?1 order by id desc limit 1",
        params![session],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
    );
    let Ok((role, Some(calls))) = last else { return vec![] };
    if role != "assistant" {
        return vec![];
    }
    let calls: Value = serde_json::from_str(&calls).unwrap_or_default();
    calls.as_array().into_iter().flatten().filter_map(|c| c["function"]["name"].as_str().map(String::from)).collect()
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
        .is_some_and(|e| e.split_whitespace().any(|w| w.starts_with("CUSTOM_BASE_URL=http://127.0.0.1:") && w.contains("/free")))
}

/// Hermes on an OpenAI-compatible endpoint at `url` (ending `/v1`): its bare `custom` provider,
/// which takes the endpoint from `CUSTOM_BASE_URL` and speaks Chat Completions to a host that isn't
/// OpenAI's. Its `openai-api` provider speaks the Responses API, which neither dino's free tier
/// nor a chat route answers. `OPENAI_BASE_URL` and a placeholder `OPENAI_API_KEY` only let a
/// Hermes nothing is set up in yet start (its first run asks for a provider otherwise); the key,
/// bound to that URL, goes only there, and dino's proxy holds the real ones.
fn on_endpoint(url: String, model: &str) -> Wiring {
    let env = [("CUSTOM_BASE_URL", url.clone()), ("OPENAI_BASE_URL", url), ("OPENAI_API_KEY", "dino".to_string())];
    (env.map(|(k, v)| (k.to_string(), v)).into(), strings(&["--provider", "custom", "-m", model]))
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
        Some(on_endpoint(format!("{url}/v1"), model))
    }

    fn model_args(&self, model: &str) -> Vec<String> {
        strings(&["-m", model])
    }

    fn effort_args(&self, _effort: &str) -> Vec<String> {
        vec![]
    }

    fn value_flags(&self) -> &'static [&'static str] {
        &["-m", "--model", "--provider", "-t", "--toolsets", "--resume", "-r", "--skills", "-s", "-z", "--oneshot", "-q", "--query", "--usage-file"]
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

    // On its own, nothing: its provider is its own setting. On the free tier its `custom`
    // provider, pointed at dino's OpenAI chat front by its environment, on the free tier's one
    // model, which picks the real one for each turn.
    fn wiring(&self, _route: bool, base: &dyn Fn(&str) -> String, _status_line: Option<String>) -> Wiring {
        if !self.free {
            return (vec![], vec![]);
        }
        on_endpoint(format!("{}/v1", base("free")), "auto")
    }

    // On a terminal, `chat -q` starts its session on the prompt, as its first turn, and stays
    // open (Hermes 2026.9.7 on; before, it answered and left, as `-z` does).
    fn prompt_args(&self, prompt: String) -> Vec<String> {
        vec!["chat".into(), "-q".into(), prompt]
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

    fn turn_now(&self, session: &str, _since: u64) -> Option<bool> {
        turn_in(&store()?, session)
    }

    /// Ctrl+C interrupts its turn; at its prompt, it quits (see `Agent::interrupt_keys`).
    fn interrupt_keys(&self) -> &'static [u8] {
        b"\x03"
    }

    fn tools_now(&self, session: &str) -> Vec<String> {
        store().map(|c| tools_in(&c, session)).unwrap_or_default()
    }

    fn new_conversation(&self, cwd: &Path, since: u64, claimed: &[String]) -> Option<String> {
        sessions(&store()?)
            .into_iter()
            .find(|r| r.started + 1.0 >= since as f64 && !claimed.contains(&r.id) && r.cwd.as_deref().is_some_and(|c| same_dir(c, cwd)))
            .map(|r| r.id)
    }

    fn busy(&self, pid: u32) -> Option<bool> {
        let args = found::args_of(pid);
        self.turn_now(&conversation_in(pid, args.get(1..).unwrap_or_default())?.id, 0)
    }

    fn portable_flags(&self, args: &[String]) -> Vec<String> {
        found::drop_flags(args, &["--resume", "-r", "-z", "--oneshot", "-q", "--query", "--usage-file"], &["-c", "--continue", "--worktree", "-w"])
    }

    // `-z` and `--oneshot` (`-q` alone keeps a session open on a terminal), its servers.
    fn headless(&self, args: &[String]) -> bool {
        super::runs_with(args, &["-z", "--oneshot", "-Q", "--quiet", "--query-file", "--format"], &["acp", "serve", "gateway", "mcp", "dashboard"])
    }

    // Its own command only: any Python may run it, and a Python is no agent until its arguments
    // say it runs the `hermes` script (`inside`).
    fn may_be(&self, comm: &str) -> bool {
        comm.rsplit('/').next() == Some("hermes")
    }

    // Found only in dino's shells: it runs under whatever Python installed it.
    fn running(&self, _procs: &crate::procinfo::Procs) -> Vec<FoundSession> {
        vec![]
    }

    fn inside(&self, pid: u32, comm: &str, args: &dyn Fn() -> Vec<String>) -> Option<FoundSession> {
        if self.free || !(self.may_be(comm) || comm.contains("python")) {
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
            s.status = self.turn_now(&r.id, 0).map(|b| if b { "busy" } else { "idle" }.into());
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

    fn usage(&self, seen: &mut crate::usage::Seen) -> Vec<crate::usage::Used> {
        if self.free {
            return vec![];
        }
        let db = hermes_home().join("state.db");
        let wal = PathBuf::from(format!("{}-wal", db.display()));
        if !seen.changed(&db) && !seen.changed(&wal) {
            return vec![];
        }
        let Some(c) = store() else { return vec![] };
        let out = usage_from(&c, seen);
        seen.remember(&db);
        seen.remember(&wal);
        out
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

/// Hermes keeps each session's token totals, not each answer's. Each new assistant message is an
/// answer, and what the totals grew by since the last look goes with the newest of them. What
/// was read is kept in `seen`: the last message id and the totals, per session.
fn usage_from(c: &Connection, seen: &mut crate::usage::Seen) -> Vec<crate::usage::Used> {
    let has = |col: &str| c.query_row("select count(*) from pragma_table_info('sessions') where name = ?1", params![col], |r| r.get::<_, i64>(0)).unwrap_or(0) > 0;
    let col = |name: &str| if has(name) { format!("coalesce({name}, 0)") } else { "0".into() };
    let model = if has("model") { "model" } else { "null" };
    let q = format!(
        "select id, cwd, {model}, {}, {}, {}, {}, {}, started_at from sessions",
        col("input_tokens"),
        col("cache_read_tokens"),
        col("cache_write_tokens"),
        col("output_tokens"),
        col("reasoning_tokens")
    );
    let Ok(mut stmt) = c.prepare(&q) else { return vec![] };
    type Totals = (String, Option<String>, Option<String>, [i64; 5], f64);
    let sessions: Vec<Totals> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, [r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?], r.get(8)?)))
        .map(|r| r.flatten().collect())
        .unwrap_or_default();
    let mut out = vec![];
    for (id, cwd, model, totals, started) in sessions {
        let key = format!("hermes:{id}");
        let last: Vec<i64> = seen.mark(&key).map(|m| m.split(',').filter_map(|n| n.parse().ok()).collect()).unwrap_or_default();
        let (after, before) = (last.first().copied().unwrap_or(0), last.get(1..6).map(|t| t.to_vec()).unwrap_or(vec![0; 5]));
        let Ok(mut q) = c.prepare("select id, timestamp from messages where session_id = ?1 and role = 'assistant' and id > ?2 order by id") else { continue };
        let answers: Vec<(i64, f64)> = q.query_map(params![id, after], |r| Ok((r.get(0)?, r.get(1)?))).map(|r| r.flatten().collect()).unwrap_or_default();
        let grew: Vec<u64> = totals.iter().zip(&before).map(|(now, then)| (now - then).max(0) as u64).collect();
        if answers.is_empty() && grew.iter().all(|g| *g == 0) {
            continue;
        }
        let used = |id: String, at: f64, tokens: bool| crate::usage::Used { undated: false,
            id,
            at_ms: (at * 1000.0) as i64,
            conversation: key[7..].to_string(),
            cwd: cwd.clone(),
            model: model.clone(),
            input: if tokens { grew[0] } else { 0 },
            cache_read: if tokens { grew[1] } else { 0 },
            cache_write: if tokens { grew[2] } else { 0 },
            output: if tokens { grew[3] + grew[4] } else { 0 },
        };
        match answers.split_last() {
            Some((newest, rest)) => {
                out.extend(rest.iter().map(|(m, at)| used(format!("{id}:{m}"), *at, false)));
                out.push(used(format!("{id}:{}", newest.0), newest.1, true));
            }
            // Its totals grew with no answer since: a call its messages don't show.
            None => out.push(used(format!("{id}:t{}", totals.iter().sum::<i64>()), started, true)),
        }
        let newest = answers.last().map_or(after, |a| a.0);
        seen.set_mark(&key, format!("{newest},{}", totals.map(|t| t.to_string()).join(",")));
    }
    out
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
        assert_eq!(tools_in(&c, id), ["terminal"], "the call out");
        c.execute("insert into messages (session_id, role, content, timestamp) values (?1, 'tool', 'a b', 103.0)", params![id]).unwrap();
        assert_eq!(turn_in(&c, id), Some(true), "the tool's result is back");
        assert!(tools_in(&c, id).is_empty());
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

    /// Its `openai-api` provider speaks the Responses API (Hermes 0.21), which the free tier and
    /// a chat route don't answer: its `custom` one speaks Chat Completions to them.
    #[test]
    fn it_reaches_dino_as_a_chat_endpoint() {
        let var = |env: &[(String, String)], k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        let (env, args) = Hermes { free: true }.wiring(true, &|p| format!("http://127.0.0.1:9/s/4/{p}"), None);
        assert_eq!(var(&env, "CUSTOM_BASE_URL").as_deref(), Some("http://127.0.0.1:9/s/4/free/v1"));
        assert_eq!(var(&env, "OPENAI_BASE_URL"), var(&env, "CUSTOM_BASE_URL"), "its placeholder key bound to dino");
        assert_eq!(args, ["--provider", "custom", "-m", "auto"]);
        let (env, args) = Hermes { free: false }.provider_wiring("http://127.0.0.1:9/s/4/or", Format::Chat, "qwen/qwen3").unwrap();
        assert_eq!(var(&env, "CUSTOM_BASE_URL").as_deref(), Some("http://127.0.0.1:9/s/4/or/v1"));
        assert_eq!(args, ["--provider", "custom", "-m", "qwen/qwen3"]);
    }

    #[test]
    fn found_by_its_script() {
        assert!(is_hermes(&["/Users/x/.local/bin/hermes".into(), "--yolo".into()]));
        assert!(!is_hermes(&["/usr/bin/python3".into(), "-m".into(), "http.server".into()]));
        let h = Hermes { free: false };
        assert!(!h.may_be("/opt/homebrew/bin/python3"), "a Python is it only by its arguments");
        let args: Vec<String> = ["--resume", "20260930_x", "-m", "auto", "--yolo", "-c"].iter().map(|s| s.to_string()).collect();
        assert_eq!(h.portable_flags(&args), ["-m", "auto", "--yolo"]);
        // Its first prompt, given once: not again where it's continued.
        assert_eq!(h.prompt_args("fix it".into()), ["chat", "-q", "fix it"]);
        assert!(!h.headless(&h.prompt_args("fix it".into())), "it stays open on its terminal");
        let started: Vec<String> = ["-m", "auto", "chat", "-q", "fix it"].iter().map(|s| s.to_string()).collect();
        assert_eq!(h.portable_flags(&started), ["-m", "auto"]);
        assert_eq!(h.read_mode(&[("--yolo", None)]).as_deref(), Some("bypass"));
    }

    #[test]
    fn its_session_totals_go_with_its_newest_answer() {
        let c = store_like_hermes();
        let mut seen = crate::usage::Seen::default();
        // No token columns in this store: answers count, with nothing used.
        let used = usage_from(&c, &mut seen);
        assert_eq!(used.len(), 1);
        assert_eq!((used[0].conversation.as_str(), used[0].at_ms, used[0].input), ("20260930_001903_cdc088", 102_000, 0));
        assert!(usage_from(&c, &mut seen).is_empty(), "nothing new");
        c.execute_batch("alter table sessions add column model text; alter table sessions add column input_tokens integer; alter table sessions add column output_tokens integer;
             update sessions set model = 'h-1', input_tokens = 500, output_tokens = 20 where id = '20260930_001903_cdc088';
             insert into messages (session_id, role, content, timestamp) values ('20260930_001903_cdc088', 'assistant', 'a', 103.0);
             insert into messages (session_id, role, content, timestamp) values ('20260930_001903_cdc088', 'assistant', 'b', 104.0);").unwrap();
        let used = usage_from(&c, &mut seen);
        assert_eq!(used.len(), 2);
        assert_eq!((used[0].input, used[1].input, used[1].output, used[1].model.as_deref()), (0, 500, 20, Some("h-1")));
        c.execute("update sessions set input_tokens = 800 where id = '20260930_001903_cdc088'", []).unwrap();
        let used = usage_from(&c, &mut seen);
        assert_eq!((used.len(), used[0].input), (1, 300), "what the totals grew by");
    }
}
