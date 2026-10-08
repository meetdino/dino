//! OpenCode (`opencode`, anomalyco/opencode, formerly sst/opencode). It keeps everything in one
//! SQLite store, `opencode.db` in its data folder (`$XDG_DATA_HOME/opencode`, `OPENCODE_DB`
//! overrides): sessions, their messages, and each message's parts, as JSON. dino only reads it,
//! for its conversations on this Mac, their titles and turns.
//!
//! Its status comes from OpenCode's own server: given `--port`, its terminal UI serves its API
//! there, locked with `OPENCODE_SERVER_PASSWORD`, and dinod follows its event stream (`/event`):
//! a session busy or idle, a permission or a question it waits on (see `ServerEvent`). Nothing is
//! added to it. It can't be given a conversation id up front; which one is busy says which it's
//! on, so `/new` and switching sessions in it are followed too. One run by hand has no server:
//! its store says whether it's on a turn.
//!
//! Its modes are its own agents, Build and Plan (`--agent`), and `--auto`, which approves
//! whatever its rules don't deny. Models are `provider/model`, as `opencode models` lists them
//! for the providers it can use. On a provider's model, dino gives it a provider of its own for
//! that process, in `OPENCODE_CONFIG_CONTENT`; its config and logins are left alone.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, params};
use serde_json::{Value, json};

use super::{Agent, ControlKind, ServerEvent, StatusSource, Wiring, strings};
use crate::found::{self, FoundSession, Source};
use crate::history::{self, Page, Turn, one_line, turn};
use crate::models::{Catalog, ModelInfo};
use crate::providers::Format;

pub(crate) struct OpenCode;

/// The provider dino gives it for a provider's model, and its models' prefix (`dino/<model>`).
const PROVIDER: &str = "dino";

/// The user its server takes, set rather than left to an `OPENCODE_SERVER_USERNAME` of the user's.
const SERVER_USER: &str = "opencode";

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

/// `$XDG_<kind>_HOME/opencode`, else its default under the home folder, as OpenCode finds them.
fn xdg(var: &str, default: &str) -> PathBuf {
    std::env::var_os(var).filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| home().join(default)).join("opencode")
}

fn data_dir() -> PathBuf {
    xdg("XDG_DATA_HOME", ".local/share")
}

fn config_dir() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config")
}

fn state_dir() -> PathBuf {
    xdg("XDG_STATE_HOME", ".local/state")
}

/// Its store: `OPENCODE_DB` (a path, or a name in its data folder), else `opencode.db`.
fn db_path() -> PathBuf {
    match std::env::var("OPENCODE_DB") {
        Ok(p) if !p.is_empty() && p != ":memory:" => {
            let p = PathBuf::from(p);
            if p.is_absolute() { p } else { data_dir().join(p) }
        }
        _ => data_dir().join("opencode.db"),
    }
}

/// Its store, opened only to read, waiting briefly while OpenCode writes.
fn store() -> Option<Connection> {
    let db = db_path();
    if !db.exists() {
        return None;
    }
    let c = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX).ok()?;
    c.busy_timeout(std::time::Duration::from_millis(500)).ok()?;
    Some(c)
}

/// A conversation as its store has it (its sessions; subagents' are sessions with a parent).
struct Row {
    id: String,
    title: String,
    dir: String,
    /// Milliseconds.
    created: u64,
    updated: u64,
}

/// Its own conversations, not its subagents' nor archived ones, oldest first.
fn sessions(c: &Connection) -> Vec<Row> {
    rows(c, "", &[])
}

/// Those in folder `dir`. It keeps a folder by its real path; `dir` may lead there by a link.
fn sessions_in(c: &Connection, dir: &Path) -> Vec<Row> {
    let real = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let (a, b) = (dir.to_string_lossy(), real.to_string_lossy());
    rows(c, "and directory in (?1, ?2)", &[&a, &b])
}

fn rows(c: &Connection, filter: &str, values: &[&dyn rusqlite::ToSql]) -> Vec<Row> {
    let q = format!(
        "select id, title, directory, time_created, time_updated from session
         where parent_id is null and time_archived is null {filter} order by time_created"
    );
    let Ok(mut stmt) = c.prepare(&q) else { return vec![] };
    stmt.query_map(values, |r| Ok(Row { id: r.get(0)?, title: r.get(1)?, dir: r.get(2)?, created: r.get(3)?, updated: r.get(4)? }))
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
}

/// The title it gives a conversation until it has made one up ("New session - <time>").
fn placeholder(title: &str) -> bool {
    title.starts_with("New session - ") || title.starts_with("Child session - ")
}

/// What the person first typed in `session`.
fn first_prompt(c: &Connection, session: &str) -> Option<String> {
    let mut stmt = c.prepare("select id, data from message where session_id = ?1 order by rowid").ok()?;
    let messages: Vec<(String, String)> = stmt.query_map(params![session], |r| Ok((r.get(0)?, r.get(1)?))).ok()?.flatten().collect();
    let (id, _) = messages.into_iter().find(|(_, d)| serde_json::from_str::<Value>(d).is_ok_and(|v| v["role"] == "user"))?;
    typed_text(&parts(c, &id)).and_then(|t| one_line(&t))
}

/// A message's parts, in order.
fn parts(c: &Connection, message: &str) -> Vec<Value> {
    let Ok(mut stmt) = c.prepare("select data from part where message_id = ?1 order by id") else { return vec![] };
    stmt.query_map(params![message], |r| r.get::<_, String>(0)).map(|rows| rows.flatten().filter_map(|d| serde_json::from_str(&d).ok()).collect()).unwrap_or_default()
}

/// What the person typed in a message: its text parts, but those OpenCode added itself.
fn typed_text(parts: &[Value]) -> Option<String> {
    let text: Vec<&str> = parts.iter().filter(|p| p["type"] == "text" && p["synthetic"] != true && p["ignored"] != true).filter_map(|p| p["text"].as_str()).collect();
    let text = text.join("\n");
    history::typed(&text).map(String::from)
}

impl Row {
    fn title(&self, c: &Connection) -> String {
        Some(self.title.as_str()).filter(|t| !placeholder(t)).and_then(one_line).or_else(|| first_prompt(c, &self.id)).unwrap_or_else(|| "OpenCode".into())
    }
}

/// Whether `session`'s latest message leaves a turn going: a prompt not yet answered, an answer
/// still being written, or one that called tools (the next step follows). `None` for no session.
fn turn_in(c: &Connection, session: &str) -> Option<bool> {
    c.query_row("select 1 from session where id = ?1", params![session], |_| Ok(())).ok()?;
    let last: Option<String> = c.query_row("select data from message where session_id = ?1 order by rowid desc limit 1", params![session], |r| r.get(0)).ok();
    let Some(v) = last.and_then(|d| serde_json::from_str::<Value>(&d).ok()) else { return Some(false) };
    Some(match v["role"].as_str() {
        Some("user") => true,
        Some("assistant") if !v["error"].is_null() => false,
        Some("assistant") => v["time"]["completed"].is_null() || v["finish"] == "tool-calls",
        _ => false,
    })
}

/// What its process is called: its own installer's binary is `opencode`; npm's runs as `opencode.exe`.
const PROCESS_NAMES: &[&str] = &["opencode", "opencode.exe"];

/// Its commands other than its terminal UI: they aren't a conversation someone is in.
const COMMANDS: &[&str] = &[
    "completion",
    "acp",
    "mcp",
    "attach",
    "run",
    "debug",
    "providers",
    "auth",
    "agent",
    "upgrade",
    "uninstall",
    "serve",
    "web",
    "models",
    "stats",
    "export",
    "import",
    "github",
    "pr",
    "session",
    "plugin",
    "plug",
    "db",
];

/// Arguments (after the program) that run its terminal UI.
fn is_tui(args: &[String]) -> bool {
    args.first().is_none_or(|a| !COMMANDS.contains(&a.as_str()))
}

/// The value of flag `names` in `args`, as the next argument or after `=`.
fn flag_value<'a>(args: &'a [String], names: &[&str]) -> Option<&'a str> {
    args.iter().enumerate().find_map(|(i, a)| {
        if let Some((_, v)) = a.split_once('=').filter(|(n, _)| names.contains(n)) {
            return Some(v);
        }
        names.contains(&a.as_str()).then(|| args.get(i + 1).map(String::as_str)).flatten()
    })
}

/// The conversation a live process (`args` after the program) is on: the one it began in its
/// folder since it started, before any it continued, so another OpenCode in the same folder isn't
/// taken for it; else the one it was told to continue (`-s`); else the latest it took up there
/// since it started (`-c`, or one picked in it).
fn conversation_in(c: &Connection, pid: u32, args: &[String]) -> Option<Row> {
    let cwd = crate::procinfo::cwd_of(pid)?;
    let since = crate::procinfo::started(pid).unwrap_or(0) * 1000;
    let here = sessions_in(c, Path::new(&cwd));
    let begun = here.iter().filter(|r| r.created + 1000 >= since).min_by_key(|r| r.created).map(|r| r.id.clone());
    let told = flag_value(args, &["-s", "--session"]).map(String::from);
    let latest = here.iter().filter(|r| r.updated + 1000 >= since).max_by_key(|r| r.updated).map(|r| r.id.clone());
    let id = begun.or(told).or(latest)?;
    // One it was told to continue may be in another folder.
    rows(c, "and id = ?1", &[&id]).into_iter().next()
}

/// Up to this many messages a page.
const PAGE: i64 = 100;

/// A page of a conversation's turns, positions being its messages' places in the store.
fn turns_page(c: &Connection, session: &str, before: Option<u64>) -> Option<Page> {
    let before = before.map_or(i64::MAX, |b| b as i64);
    let mut stmt = c.prepare("select rowid, id, data from message where session_id = ?1 and rowid < ?2 order by rowid desc limit ?3").ok()?;
    let mut rows: Vec<(i64, String, String)> = stmt.query_map(params![session, before, PAGE], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).ok()?.flatten().collect();
    rows.reverse();
    let first: Option<i64> = c.query_row("select min(rowid) from message where session_id = ?1", params![session], |r| r.get(0)).ok()?;
    let start = match rows.first() {
        Some((rowid, ..)) if Some(*rowid) != first => *rowid as u64,
        _ => 0,
    };
    let mut turns = vec![];
    for (_, id, data) in rows {
        let role = serde_json::from_str::<Value>(&data).ok().and_then(|v| v["role"].as_str().map(String::from)).unwrap_or_default();
        turns.extend(turns_of(&role, &parts(c, &id)));
    }
    Some(Page { turns, start, path: None })
}

/// A message's parts as turns: what the person typed, what it answered, the tools it ran.
fn turns_of(role: &str, parts: &[Value]) -> Vec<Turn> {
    match role {
        "user" => typed_text(parts).map(|t| turn("user", t)).into_iter().collect(),
        "assistant" => parts
            .iter()
            .filter_map(|p| match p["type"].as_str()? {
                "text" if p["synthetic"] != true => p["text"].as_str().map(str::trim).filter(|t| !t.is_empty()).map(|t| turn("assistant", t)),
                "tool" => Some(turn("tool", format!("{}{}", p["tool"].as_str().unwrap_or("tool"), history::hint(&p["state"]["input"])))),
                _ => None,
            })
            .collect(),
        _ => vec![],
    }
}

/// What `opencode models --verbose` prints: each model's `provider/model` line, then its JSON.
/// `config` is its config's model; `recent` the ones last used in it, latest first.
fn catalog_in(listing: &str, config: Option<String>, recent: &[String]) -> Option<Catalog> {
    let mut models: Vec<ModelInfo> = vec![];
    let mut id: Option<&str> = None;
    let mut json = String::new();
    let mut flush = |id: Option<&str>, json: &str| {
        let Some(id) = id else { return };
        let v: Value = serde_json::from_str(json).unwrap_or(Value::Null);
        // Ones its catalog says are gone aren't offered.
        if v["status"] == "deprecated" {
            return;
        }
        let label = v["name"].as_str().filter(|n| !n.is_empty()).unwrap_or(id).to_string();
        models.push(ModelInfo { id: id.to_string(), label, ..ModelInfo::default() });
    };
    for line in listing.lines() {
        let header = !line.is_empty() && !line.starts_with(['{', '}', ' ', '\t', ']', '"']) && line.contains('/') && !line.contains(' ');
        if header {
            flush(id, &json);
            id = Some(line.trim());
            json.clear();
        } else {
            json.push_str(line);
            json.push('\n');
        }
    }
    flush(id, &json);
    // As it picks: its config's, else the latest one used that it can still use.
    let default_model = config.or_else(|| recent.iter().find(|r| models.iter().any(|m| &m.id == *r)).cloned());
    (!models.is_empty()).then(|| Catalog { models, default_model, ..Catalog::default() })
}

/// The model its config starts it on.
fn config_model() -> Option<String> {
    ["opencode.json", "opencode.jsonc", "config.json"]
        .iter()
        .filter_map(|f| std::fs::read_to_string(config_dir().join(f)).ok())
        .find_map(|t| serde_json::from_str::<Value>(&without_comments(&t)).ok()?["model"].as_str().map(String::from))
}

/// The models last used in it, latest first (`model.json`).
fn recent_models() -> Vec<String> {
    let v: Value = std::fs::read_to_string(state_dir().join("model.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null);
    v["recent"].as_array().into_iter().flatten().filter_map(|m| Some(format!("{}/{}", m["providerID"].as_str()?, m["modelID"].as_str()?))).collect()
}

/// JSON with comments (its `.jsonc`) as JSON: `//` and `/* */` outside strings dropped.
fn without_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if c == '\\' {
                out.extend(chars.next());
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match (c, chars.peek()) {
            ('"', _) => {
                in_string = true;
                out.push(c);
            }
            ('/', Some('/')) => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            ('/', Some('*')) => {
                chars.next();
                let mut last = ' ';
                for c in chars.by_ref() {
                    if last == '*' && c == '/' {
                        break;
                    }
                    last = c;
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// Config for one process, in `OPENCODE_CONFIG_CONTENT`: `add` over what the user's environment
/// already gives it there.
fn config_content(add: Value) -> String {
    let mut config: Value = std::env::var("OPENCODE_CONFIG_CONTENT").ok().and_then(|t| serde_json::from_str(&t).ok()).filter(Value::is_object).unwrap_or_else(|| json!({}));
    merge(&mut config, add);
    config.to_string()
}

fn merge(into: &mut Value, add: Value) {
    match (into, add) {
        (Value::Object(a), Value::Object(b)) => {
            for (k, v) in b {
                merge(a.entry(k).or_insert(Value::Null), v);
            }
        }
        (a, b) => *a = b,
    }
}

/// What a permission it asks for is about, said briefly: "read .env", "bash rm -rf build". Files
/// by their name: it gives their paths without the leading `/`.
fn asked_for(p: &Value) -> String {
    let what = p["permission"].as_str().unwrap_or("permission");
    let files = matches!(what, "read" | "edit" | "write" | "list" | "external_directory");
    let patterns: Vec<String> = p["patterns"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|s| if files { s.trim_end_matches(['/', '*']).rsplit('/').next().unwrap_or(s).to_string() } else { s.to_string() })
        .collect();
    if patterns.is_empty() { what.to_string() } else { history::short(&format!("{what} {}", patterns.join(" "))) }
}

impl OpenCode {
    fn found(&self, c: Option<&Connection>, pid: u32, args: &[String]) -> FoundSession {
        let mut s = found::by_hand("opencode", pid);
        s.source = Source::Running;
        s.cwd = crate::procinfo::cwd_of(pid);
        s.title = "OpenCode".into();
        if let Some((c, r)) = c.and_then(|c| Some((c, conversation_in(c, pid, args)?))) {
            s.title = r.title(c);
            s.updated_at = r.updated / 1000;
            s.status = turn_in(c, &r.id).map(|b| if b { "busy" } else { "idle" }.into());
            s.session_id = r.id;
        }
        s.args = self.portable_flags(args);
        s
    }
}

impl Agent for OpenCode {
    fn id(&self) -> &'static str {
        "opencode"
    }

    // Its Build agent asks only where its rules say (by default, outside the project or for
    // `.env` files); Plan changes nothing; `--auto` approves whatever they don't deny.
    fn modes(&self) -> &'static [&'static str] {
        &["ask", "plan", "bypass"]
    }

    fn mode_label(&self, mode: &str) -> Option<&'static str> {
        Some(match mode {
            "ask" => "Build",
            "plan" => "Plan",
            "bypass" => "Auto-approve",
            _ => return None,
        })
    }

    fn mode_args(&self, mode: &str) -> Vec<String> {
        match mode {
            "plan" => strings(&["--agent", "plan"]),
            "bypass" => strings(&["--auto"]),
            _ => vec![],
        }
    }

    fn model_args(&self, model: &str) -> Vec<String> {
        strings(&["-m", model])
    }

    // A model's variant (its effort) is only on `opencode run`'s command line, not its terminal UI's.
    fn effort_args(&self, _effort: &str) -> Vec<String> {
        vec![]
    }

    fn value_flags(&self) -> &'static [&'static str] {
        &["-m", "--model", "-s", "--session", "--prompt", "--agent", "--port", "--hostname", "--mdns-domain", "--cors", "--log-level", "--replay-limit"]
    }

    // `--agent` is a mode only for its own two; others are agents the user made.
    fn control_of(&self, name: &str, value: Option<&str>) -> Option<ControlKind> {
        match name {
            "--agent" if matches!(value, Some("plan" | "build")) => Some(ControlKind::Mode),
            "--auto" => Some(ControlKind::Mode),
            "-m" | "--model" => Some(ControlKind::Model),
            _ => None,
        }
    }

    fn read_mode(&self, flags: &[(&str, Option<&str>)]) -> Option<String> {
        let plan = flags.iter().rev().find(|(n, _)| *n == "--agent").is_some_and(|(_, v)| *v == Some("plan"));
        let mode = if plan {
            "plan"
        } else if flags.iter().any(|(n, _)| *n == "--auto") {
            "bypass"
        } else {
            "ask"
        };
        Some(mode.into())
    }

    fn catalog_sources(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = ["opencode.json", "opencode.jsonc", "config.json"].iter().map(|f| config_dir().join(f)).collect();
        out.push(data_dir().join("auth.json"));
        out
    }

    // What it says it can use, from its own catalog and the providers it's signed in to.
    fn catalog(&self, program: &str) -> Option<Catalog> {
        // Asking it makes its folders: don't, for someone who has never run it.
        if !data_dir().exists() {
            return None;
        }
        let out = std::process::Command::new(program)
            .args(["models", "--verbose"])
            .env("OPENCODE_DISABLE_AUTOUPDATE", "1")
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        catalog_in(&String::from_utf8_lossy(&out.stdout), config_model(), &recent_models())
    }

    // Asking it takes a second or two, and its models take no effort on its command line.
    fn catalog_first(&self) -> bool {
        false
    }

    // Its providers are its own setting, and dino follows its server for status (`serve`).
    fn wiring(&self, _route: bool, _base: &dyn Fn(&str) -> String, _status_line: Option<String>) -> Wiring {
        (vec![], vec![])
    }

    fn provider_formats(&self) -> &'static [Format] {
        &[Format::Chat, Format::Anthropic, Format::Responses]
    }

    // A provider of its own for this process, with the one model; the key is a placeholder, the
    // proxy holds the real one. The URL stays in its environment, off its command line.
    fn provider_wiring(&self, url: &str, format: Format, model: &str) -> Option<Wiring> {
        let npm = match format {
            Format::Chat => "@ai-sdk/openai-compatible",
            Format::Anthropic => "@ai-sdk/anthropic",
            Format::Responses => "@ai-sdk/openai",
        };
        let provider = json!({ "provider": { PROVIDER: {
            "npm": npm,
            "name": "dino",
            "options": { "baseURL": format!("{url}/v1"), "apiKey": "dino" },
            "models": { model: { "name": model } },
        }}});
        Some((vec![("OPENCODE_CONFIG_CONTENT".into(), config_content(provider))], strings(&["-m", &format!("{PROVIDER}/{model}")])))
    }

    fn prompt_args(&self, prompt: String) -> Vec<String> {
        vec!["--prompt".into(), prompt]
    }

    // The option `prompt_args` gives it with.
    fn launch_prompt(&self, args: &[String]) -> Option<(Vec<String>, String)> {
        super::option_prompt(args, &["--prompt"])
    }

    fn session_args(&self, session: &mut Option<String>, restoring: bool) -> (Vec<String>, Vec<String>) {
        match session {
            Some(id) if restoring => (vec![], vec!["-s".into(), id.clone()]),
            _ => (vec![], vec![]),
        }
    }

    fn status_source(&self) -> StatusSource {
        StatusSource::Server
    }

    fn serve(&self, port: u16, password: &str) -> Wiring {
        let env = vec![("OPENCODE_SERVER_USERNAME".to_string(), SERVER_USER.to_string()), ("OPENCODE_SERVER_PASSWORD".to_string(), password.to_string())];
        (env, vec!["--port".into(), port.to_string()])
    }

    fn server_user(&self) -> &'static str {
        SERVER_USER
    }

    fn server_event(&self, v: &Value) -> ServerEvent {
        let p = &v["properties"];
        let session = || p["sessionID"].as_str().unwrap_or_default().to_string();
        match v["type"].as_str().unwrap_or_default() {
            "session.status" => match p["status"]["type"].as_str() {
                Some("idle") => ServerEvent::Idle(session()),
                Some(_) => ServerEvent::Busy(session()),
                None => ServerEvent::Other,
            },
            "session.idle" => ServerEvent::Idle(session()),
            "permission.asked" => ServerEvent::Asked { id: p["id"].as_str().unwrap_or_default().into(), session: session(), what: asked_for(p) },
            "question.asked" => {
                let q = &p["questions"][0];
                let what = q["header"].as_str().or(q["question"].as_str()).map(history::short).unwrap_or_else(|| "an answer".into());
                ServerEvent::Asked { id: p["id"].as_str().unwrap_or_default().into(), session: session(), what }
            }
            "permission.replied" | "question.replied" | "question.rejected" => ServerEvent::Answered(p["requestID"].as_str().unwrap_or_default().into()),
            // Each answer as it's saved: what it read, so how full its context is.
            "message.updated" if p["info"]["role"] == "assistant" => {
                let i = &p["info"];
                let t = &i["tokens"];
                let n = |v: &Value| v.as_u64().unwrap_or(0);
                let used = n(&t["input"]) + n(&t["output"]) + n(&t["reasoning"]) + n(&t["cache"]["read"]) + n(&t["cache"]["write"]);
                match (i["providerID"].as_str(), i["modelID"].as_str()) {
                    (Some(provider), Some(model)) if used > 0 => ServerEvent::Context { session: session_of(i), model: format!("{provider}/{model}"), used },
                    _ => ServerEvent::Other,
                }
            }
            // A tool call as it goes: pending, running (again as it says more), then completed
            // or error. An MCP server's tool is `<server>_<tool>`, its name made safe.
            "message.part.updated" if p["part"]["type"] == "tool" => {
                let part = &p["part"];
                match (part["callID"].as_str().or(part["id"].as_str()), part["tool"].as_str()) {
                    (Some(call), Some(name)) => ServerEvent::Tool { call: call.into(), name: name.into(), done: matches!(part["state"]["status"].as_str(), Some("completed" | "error")) },
                    _ => ServerEvent::Other,
                }
            }
            _ => ServerEvent::Other,
        }
    }

    // Which conversations are busy, and the permissions and questions waiting on an answer.
    fn server_snapshot(&self) -> &'static [&'static str] {
        &["/session/status", "/permission", "/question"]
    }

    fn server_snapshot_events(&self, path: &str, answer: &Value) -> Vec<ServerEvent> {
        let as_event = |kind: &str, properties: &Value| self.server_event(&json!({ "type": kind, "properties": properties }));
        match path {
            "/session/status" => answer.as_object().into_iter().flatten().map(|(session, status)| as_event("session.status", &json!({ "sessionID": session, "status": status }))).collect(),
            "/permission" => answer.as_array().into_iter().flatten().map(|p| as_event("permission.asked", p)).collect(),
            "/question" => answer.as_array().into_iter().flatten().map(|q| as_event("question.asked", q)).collect(),
            _ => vec![],
        }
    }

    fn server_providers(&self) -> Option<&'static str> {
        Some("/config/providers")
    }

    fn server_context_window(&self, providers: &Value, model: &str) -> Option<u64> {
        let (provider, model) = model.split_once('/')?;
        let p = providers["providers"].as_array()?.iter().find(|p| p["id"] == provider)?;
        p["models"][model]["limit"]["context"].as_u64().filter(|w| *w > 0)
    }

    fn is_conversation(&self, session: &str) -> bool {
        // A subagent's session has a parent; one not in the store yet is the one being started.
        store().is_none_or(|c| c.query_row("select parent_id from session where id = ?1", params![session], |r| r.get::<_, Option<String>>(0)).map_or(true, |p| p.is_none()))
    }

    fn turn_now(&self, session: &str, _since: u64) -> Option<bool> {
        turn_in(&store()?, session)
    }

    fn new_conversation(&self, cwd: &Path, since: u64, claimed: &[String]) -> Option<String> {
        sessions_in(&store()?, cwd).into_iter().filter(|r| r.created / 1000 + 1 >= since && !claimed.contains(&r.id)).min_by_key(|r| r.created).map(|r| r.id)
    }

    fn busy(&self, pid: u32) -> Option<bool> {
        let c = store()?;
        let args = found::args_of(pid);
        turn_in(&c, &conversation_in(&c, pid, &args)?.id)
    }

    fn shown_title(&self, title: &str) -> Option<String> {
        let t = title.strip_prefix("OC | ").unwrap_or(title).trim();
        (!t.is_empty()).then(|| t.to_string())
    }

    fn portable_flags(&self, args: &[String]) -> Vec<String> {
        found::drop_flags(
            args,
            &["-s", "--session", "--prompt", "--port", "--hostname", "--mdns-domain", "--cors", "--log-level", "--replay-limit"],
            &["-c", "--continue", "--fork", "--mdns", "--print-logs", "--no-replay"],
        )
    }

    // `run`, `serve`, `acp`… anything but its TUI.
    fn headless(&self, args: &[String]) -> bool {
        !is_tui(args)
    }

    fn may_be(&self, comm: &str) -> bool {
        comm.rsplit('/').next().is_some_and(|n| PROCESS_NAMES.contains(&n))
    }

    fn running(&self, procs: &crate::procinfo::Procs) -> Vec<FoundSession> {
        let c = store();
        let mut out = vec![];
        for pid in PROCESS_NAMES.iter().flat_map(|n| crate::procinfo::named_in(procs, n)) {
            let args = found::args_of(pid);
            if !is_tui(&args) {
                continue;
            }
            let mut s = self.found(c.as_ref(), pid, &args);
            if s.session_id.is_empty() {
                continue;
            }
            let (terminal, flags) = found::terminal_and_flags(self, pid);
            s.terminal = terminal;
            s.args = flags;
            out.push(s);
        }
        out
    }

    fn inside(&self, pid: u32, comm: &str, args: &dyn Fn() -> Vec<String>) -> Option<FoundSession> {
        if !self.may_be(comm) {
            return None;
        }
        let args = args();
        if !is_tui(&args) {
            return None;
        }
        Some(self.found(store().as_ref(), pid, &args))
    }

    fn recent(&self, leave_out: &dyn Fn(&str, u64) -> bool) -> Vec<FoundSession> {
        let Some(c) = store() else { return vec![] };
        sessions(&c)
            .into_iter()
            .filter(|r| !leave_out(&r.id, r.updated / 1000))
            .filter_map(|r| {
                // One with nothing said in it yet isn't worth continuing.
                let said = c.query_row("select 1 from message where session_id = ?1 limit 1", params![r.id], |_| Ok(())).is_ok();
                said.then(|| {
                    let title = r.title(&c);
                    history::recent("opencode", r.id, title, Some(r.dir), r.updated / 1000)
                })
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

    // Read again when its store changes, from a little before the newest answer last read: an
    // answer is written as it starts and filled in as it ends.
    fn usage(&self, seen: &mut crate::usage::Seen) -> Vec<crate::usage::Used> {
        let db = db_path();
        let wal = PathBuf::from(format!("{}-wal", db.display()));
        if !seen.changed(&db) && !seen.changed(&wal) {
            return vec![];
        }
        let Some(c) = store() else { return vec![] };
        let since: i64 = seen.mark("opencode:since").and_then(|m| m.parse().ok()).unwrap_or(0);
        let (out, latest) = usage_from(&c, since - REREAD_MS);
        seen.remember(&db);
        seen.remember(&wal);
        seen.set_mark("opencode:since", latest.max(since).to_string());
        out
    }

    fn turns(&self, _text: &str, _path: &Path, _start: u64) -> Vec<Turn> {
        vec![]
    }

    fn page(&self, session_id: &str, before: Option<u64>) -> Option<Page> {
        turns_page(&store()?, session_id, before)
    }

    fn tail(&self, session_id: &str, budget: usize) -> Option<String> {
        let page = self.page(session_id, None)?;
        let mut out: Vec<String> = vec![];
        let mut used = 0;
        for t in page.turns.iter().rev() {
            let line = format!("{}: {}", t.role, t.text);
            used += line.len() + 1;
            if used > budget {
                break;
            }
            out.push(line);
        }
        out.reverse();
        (!out.is_empty()).then(|| out.join("\n"))
    }
}

/// The session a message belongs to.
fn session_of(info: &Value) -> String {
    info["sessionID"].as_str().unwrap_or_default().to_string()
}

/// How far before the newest answer read each look starts.
const REREAD_MS: i64 = 6 * 3600 * 1000;

/// Its finished answers started after `since` (ms), and when the newest message read started. A
/// subagent's count for the conversation that started it.
fn usage_from(c: &Connection, since: i64) -> (Vec<crate::usage::Used>, i64) {
    let q = "select m.id, coalesce(s.parent_id, s.id), s.directory, m.time_created, m.data
             from message m join session s on s.id = m.session_id where m.time_created > ?1 order by m.time_created";
    let Ok(mut stmt) = c.prepare(q) else { return (vec![], since) };
    let rows: Vec<(String, String, String, i64, String)> =
        stmt.query_map(params![since], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).map(|r| r.flatten().collect()).unwrap_or_default();
    let mut latest = since;
    let mut out = vec![];
    for (id, conversation, dir, created, data) in rows {
        latest = latest.max(created);
        let Ok(v) = serde_json::from_str::<Value>(&data) else { continue };
        let t = &v["tokens"];
        // Still answering: counted once it's done.
        if v["role"] != "assistant" || !v["time"]["completed"].is_number() || !t.is_object() {
            continue;
        }
        let used = crate::usage::Used {
            undated: false,
            id,
            at_ms: v["time"]["created"].as_i64().unwrap_or(created),
            conversation,
            cwd: Some(dir),
            model: v["modelID"].as_str().map(String::from),
            input: history::count(&t["input"]),
            cache_read: history::count(&t["cache"]["read"]),
            cache_write: history::count(&t["cache"]["write"]),
            // It counts reasoning apart; the API counts it as output.
            output: history::count(&t["output"]) + history::count(&t["reasoning"]),
        };
        if used.input + used.cache_read + used.cache_write + used.output > 0 {
            out.push(used);
        }
    }
    (out, latest)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The parts of OpenCode 1.18's store dino reads: a conversation that read a file, a
    /// subagent's, and one still at its placeholder title.
    fn store_like_opencode() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            r#"create table session (id text primary key, parent_id text, directory text not null, title text not null,
                 time_created integer not null, time_updated integer not null, time_archived integer);
             create table message (id text primary key, session_id text not null, time_created integer not null, data text not null);
             create table part (id text primary key, message_id text not null, session_id text not null, data text not null);
             insert into session values ('ses_a', null, '/r', 'Secret word in notes', 100000, 104000, null);
             insert into session values ('ses_child', 'ses_a', '/r', 'Child session - x', 101000, 102000, null);
             insert into session values ('ses_new', null, '/r', 'New session - 2026-10-04T04:12:28.275Z', 200000, 201000, null);
             insert into message values ('msg_1', 'ses_a', 100100, '{"role":"user","agent":"build"}');
             insert into part values ('prt_1', 'msg_1', 'ses_a', '{"type":"text","text":"Read notes.txt and tell me the secret word."}');
             insert into part values ('prt_2', 'msg_1', 'ses_a', '{"type":"text","text":"Called the Read tool","synthetic":true}');
             insert into message values ('msg_2', 'ses_a', 100200, '{"role":"assistant","finish":"tool-calls","time":{"created":100200,"completed":100900},"tokens":{"input":9000}}');
             insert into part values ('prt_3', 'msg_2', 'ses_a', '{"type":"step-start"}');
             insert into part values ('prt_4', 'msg_2', 'ses_a', '{"type":"reasoning","text":"hmm"}');
             insert into part values ('prt_5', 'msg_2', 'ses_a', '{"type":"tool","tool":"read","state":{"status":"completed","input":{"filePath":"/r/notes.txt"}}}');
             insert into message values ('msg_3', 'ses_new', 200100, '{"role":"user"}');
             insert into part values ('prt_6', 'msg_3', 'ses_new', '{"type":"text","text":"Fix the flaky test\nin ci"}');"#,
        )
        .unwrap();
        c
    }

    #[test]
    fn its_store_says_where_the_turn_is() {
        let c = store_like_opencode();
        assert_eq!(turn_in(&c, "ses_a"), Some(true), "it called a tool: the next step follows");
        c.execute(r#"insert into message values ('msg_4', 'ses_a', 101000, '{"role":"assistant","time":{"created":101000}}')"#, []).unwrap();
        assert_eq!(turn_in(&c, "ses_a"), Some(true), "answering");
        c.execute(r#"update message set data = '{"role":"assistant","finish":"stop","time":{"created":101000,"completed":101500}}' where id = 'msg_4'"#, []).unwrap();
        assert_eq!(turn_in(&c, "ses_a"), Some(false), "answered");
        c.execute(r#"insert into message values ('msg_5', 'ses_a', 102000, '{"role":"user"}')"#, []).unwrap();
        assert_eq!(turn_in(&c, "ses_a"), Some(true), "asked again");
        c.execute(r#"insert into message values ('msg_6', 'ses_a', 102100, '{"role":"assistant","error":{"name":"MessageAbortedError"},"time":{"created":102100}}')"#, [])
            .unwrap();
        assert_eq!(turn_in(&c, "ses_a"), Some(false), "interrupted");
        assert_eq!(turn_in(&c, "nope"), None);
    }

    #[test]
    fn its_conversations_and_their_turns() {
        let c = store_like_opencode();
        let all = sessions(&c);
        assert_eq!(all.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["ses_a", "ses_new"], "its own, not its subagents'");
        assert_eq!(all[0].title(&c), "Secret word in notes");
        assert_eq!(all[1].title(&c), "Fix the flaky test", "no title of its own yet: the first prompt");
        assert_eq!(sessions_in(&c, Path::new("/r")).len(), 2);
        assert!(sessions_in(&c, Path::new("/elsewhere")).is_empty());
        assert_eq!(rows(&c, "and id = ?1", &[&"ses_new"]).len(), 1);
        let page = turns_page(&c, "ses_a", None).unwrap();
        let turns: Vec<(String, String)> = page.turns.into_iter().map(|t| (t.role, t.text)).collect();
        let want = [("user", "Read notes.txt and tell me the secret word."), ("tool", "read /r/notes.txt")];
        assert_eq!(turns, want.map(|(r, t)| (r.to_string(), t.to_string())));
        assert_eq!(page.start, 0, "from its beginning");
        let before = turns_page(&c, "ses_a", Some(2)).unwrap();
        assert_eq!(before.turns.len(), 1, "the page before the second message");
    }

    #[test]
    fn its_server_events() {
        let o = OpenCode;
        let ev = |s: &str| o.server_event(&serde_json::from_str(s).unwrap());
        assert_eq!(ev(r#"{"type":"session.status","properties":{"sessionID":"ses_a","status":{"type":"busy"}}}"#), ServerEvent::Busy("ses_a".into()));
        assert_eq!(ev(r#"{"type":"session.status","properties":{"sessionID":"ses_a","status":{"type":"retry","attempt":1,"message":"rate limited","next":5}}}"#), ServerEvent::Busy("ses_a".into()));
        assert_eq!(ev(r#"{"type":"session.status","properties":{"sessionID":"ses_a","status":{"type":"idle"}}}"#), ServerEvent::Idle("ses_a".into()));
        // As OpenCode 1.18.34 sent it.
        let asked = ev(
            r#"{"type":"permission.asked","properties":{"id":"per_1","sessionID":"ses_a","permission":"read","patterns":["Users/x/p/.env"],"metadata":{},"always":["*"],"tool":{"messageID":"msg_1","callID":"call_1"}}}"#,
        );
        assert_eq!(asked, ServerEvent::Asked { id: "per_1".into(), session: "ses_a".into(), what: "read .env".into() });
        let bash = ev(r#"{"type":"permission.asked","properties":{"id":"per_2","sessionID":"ses_a","permission":"bash","patterns":["rm -rf build/"]}}"#);
        assert_eq!(bash, ServerEvent::Asked { id: "per_2".into(), session: "ses_a".into(), what: "bash rm -rf build/".into() });
        assert_eq!(ev(r#"{"type":"permission.replied","properties":{"sessionID":"ses_a","requestID":"per_1","reply":"once"}}"#), ServerEvent::Answered("per_1".into()));
        let q = ev(r#"{"type":"question.asked","properties":{"id":"que_1","sessionID":"ses_a","questions":[{"question":"Which database?","header":"Database","options":[]}]}}"#);
        assert_eq!(q, ServerEvent::Asked { id: "que_1".into(), session: "ses_a".into(), what: "Database".into() });
        assert_eq!(ev(r#"{"type":"question.rejected","properties":{"sessionID":"ses_a","requestID":"que_1"}}"#), ServerEvent::Answered("que_1".into()));
        let ctx = ev(
            r#"{"type":"message.updated","properties":{"sessionID":"ses_a","info":{"role":"assistant","sessionID":"ses_a","providerID":"anthropic","modelID":"claude-x","tokens":{"input":10,"output":5,"reasoning":0,"cache":{"read":1000,"write":20}}}}}"#,
        );
        assert_eq!(ctx, ServerEvent::Context { session: "ses_a".into(), model: "anthropic/claude-x".into(), used: 1035 });
        assert_eq!(ev(r#"{"type":"message.part.delta","properties":{}}"#), ServerEvent::Other);
        let busy = o.server_snapshot_events("/session/status", &json!({"ses_a": {"type": "busy"}}));
        assert_eq!(busy, [ServerEvent::Busy("ses_a".into())], "what it was doing before dino followed it");
        let waiting = o.server_snapshot_events("/permission", &json!([{"id":"per_3","sessionID":"ses_a","permission":"external_directory","patterns":["tmp/x/*"]}]));
        assert_eq!(waiting, [ServerEvent::Asked { id: "per_3".into(), session: "ses_a".into(), what: "external_directory x".into() }]);
        let providers = json!({"providers":[{"id":"anthropic","models":{"claude-x":{"limit":{"context":200000}}}},{"id":"dino","models":{"m":{"limit":{"context":0}}}}]});
        assert_eq!(o.server_context_window(&providers, "anthropic/claude-x"), Some(200_000));
        assert_eq!(o.server_context_window(&providers, "dino/m"), None, "unknown is 0: no window rather than a wrong one");
    }

    #[test]
    fn its_server_says_which_tools_it_calls() {
        let o = OpenCode;
        let ev = |s: &str| o.server_event(&serde_json::from_str(s).unwrap());
        // OpenCode 1.18.34 on qwen3:4b, calling open-computer-use's list_apps (its output cut).
        let pending = r#"{"id":"evt_1","type":"message.part.updated","properties":{"sessionID":"ses_b","part":{"id":"prt_1","messageID":"msg_1","sessionID":"ses_b","type":"tool","tool":"open-computer-use_list_apps","callID":"call_z1f95el9","state":{"status":"pending","input":{},"raw":""}},"time":1791129168471}}"#;
        let running = r#"{"id":"evt_2","type":"message.part.updated","properties":{"sessionID":"ses_b","part":{"type":"tool","tool":"open-computer-use_list_apps","callID":"call_z1f95el9","state":{"status":"running","input":{},"time":{"start":1791129168843}},"id":"prt_1","sessionID":"ses_b","messageID":"msg_1"},"time":1791129168843}}"#;
        let completed = r#"{"id":"evt_3","type":"message.part.updated","properties":{"sessionID":"ses_b","part":{"type":"tool","tool":"open-computer-use_list_apps","callID":"call_z1f95el9","state":{"status":"completed","input":{},"output":"Finder — com.apple.finder [running]"},"id":"prt_1","sessionID":"ses_b","messageID":"msg_1"},"time":1791129174440}}"#;
        let call = |done| ServerEvent::Tool { call: "call_z1f95el9".into(), name: "open-computer-use_list_apps".into(), done };
        assert_eq!(ev(pending), call(false));
        assert_eq!(ev(running), call(false));
        assert_eq!(ev(completed), call(true));
        let text = r#"{"type":"message.part.updated","properties":{"sessionID":"ses_b","part":{"type":"text","text":"0","id":"prt_2"}}}"#;
        assert_eq!(ev(text), ServerEvent::Other);
    }

    #[test]
    fn its_server_is_locked_and_on_dinos_port() {
        let (env, args) = OpenCode.serve(41234, "s3cret");
        assert_eq!(args, ["--port", "41234"]);
        assert!(env.contains(&("OPENCODE_SERVER_PASSWORD".into(), "s3cret".into())));
        assert!(env.contains(&("OPENCODE_SERVER_USERNAME".into(), "opencode".into())));
    }

    #[test]
    fn modes_both_ways() {
        let o = OpenCode;
        assert!(o.mode_args("ask").is_empty(), "Build is its default");
        assert_eq!(o.read_mode(&[("--agent", Some("plan"))]).as_deref(), Some("plan"));
        assert_eq!(o.read_mode(&[("--auto", None)]).as_deref(), Some("bypass"));
        assert_eq!(o.read_mode(&[("--agent", Some("build"))]).as_deref(), Some("ask"));
        assert_eq!(o.control_of("--agent", Some("plan")), Some(ControlKind::Mode));
        assert_eq!(o.control_of("--agent", Some("reviewer")), None, "an agent of the user's isn't a mode");
        assert_eq!(o.mode_label("ask"), Some("Build"));
    }

    #[test]
    fn models_from_its_own_listing() {
        let listing = "opencode/big-pickle\n{\n  \"id\": \"big-pickle\",\n  \"name\": \"Big Pickle\",\n  \"variants\": {}\n}\nanthropic/claude-x\n{\n  \"id\": \"claude-x\",\n  \"name\": \"Claude X\",\n  \"limit\": {\"context\": 200000}\n}\nanthropic/old\n{\n  \"name\": \"Old\",\n  \"status\": \"deprecated\"\n}\n";
        let c = catalog_in(listing, Some("anthropic/claude-x".into()), &[]).unwrap();
        let ids: Vec<(&str, &str)> = c.models.iter().map(|m| (m.id.as_str(), m.label.as_str())).collect();
        assert_eq!(ids, [("opencode/big-pickle", "Big Pickle"), ("anthropic/claude-x", "Claude X")]);
        assert_eq!(c.default_model.as_deref(), Some("anthropic/claude-x"), "its config's");
        let recent = ["gone/model".to_string(), "opencode/big-pickle".to_string()];
        assert_eq!(catalog_in(listing, None, &recent).unwrap().default_model.as_deref(), Some("opencode/big-pickle"), "the latest it can still use");
        assert!(catalog_in("", None, &[]).is_none(), "none listed: typed by hand");
    }

    #[test]
    fn jsonc_reads_as_json() {
        let t = "{\n  // the model\n  \"model\": \"a/b\", /* inline */ \"url\": \"http://x//y\"\n}";
        let v: Value = serde_json::from_str(&without_comments(t)).unwrap();
        assert_eq!(v["model"], "a/b");
        assert_eq!(v["url"], "http://x//y", "slashes inside strings stay");
    }

    #[test]
    fn a_provider_of_its_own_for_the_process() {
        let url = "http://127.0.0.1:5000/s/7/abc/local/ollama";
        let (env, args) = OpenCode.provider_wiring(url, Format::Chat, "qwen3:4b").unwrap();
        assert_eq!(args, ["-m", "dino/qwen3:4b"]);
        assert!(!args.iter().any(|a| a.contains(url)), "the URL, and the proxy's secret in it, stays off its command line");
        let config: Value = serde_json::from_str(&env.iter().find(|(k, _)| k == "OPENCODE_CONFIG_CONTENT").unwrap().1).unwrap();
        let p = &config["provider"]["dino"];
        assert_eq!(p["npm"], "@ai-sdk/openai-compatible");
        assert_eq!(p["options"]["baseURL"], format!("{url}/v1"));
        assert!(p["models"]["qwen3:4b"].is_object());
        let (env, _) = OpenCode.provider_wiring(url, Format::Anthropic, "m").unwrap();
        assert!(env[0].1.contains("@ai-sdk/anthropic"));
        let mut base = json!({"provider": {"mine": {"npm": "x"}}, "model": "mine/a"});
        merge(&mut base, json!({"provider": {"dino": {"npm": "y"}}}));
        assert_eq!(base["provider"]["mine"]["npm"], "x", "what the user's environment gives it stays");
        assert_eq!(base["provider"]["dino"]["npm"], "y");
    }

    #[test]
    fn found_by_its_terminal_ui() {
        assert!(is_tui(&[]));
        assert!(is_tui(&["-m".into(), "a/b".into()]));
        assert!(is_tui(&["/some/project".into()]));
        assert!(!is_tui(&["serve".into(), "--port".into(), "4096".into()]));
        assert!(!is_tui(&["run".into(), "hi".into()]));
        let args: Vec<String> = ["-s", "ses_x", "-m", "anthropic/claude-x", "--agent", "plan", "--port", "41234", "-c"].iter().map(|s| s.to_string()).collect();
        assert_eq!(OpenCode.portable_flags(&args), ["-m", "anthropic/claude-x", "--agent", "plan"]);
        assert_eq!(flag_value(&args, &["-s", "--session"]), Some("ses_x"));
        assert_eq!(flag_value(&["--session=ses_y".to_string()], &["-s", "--session"]), Some("ses_y"));
        assert_eq!(OpenCode.shown_title("OC | Secret word in notes").as_deref(), Some("Secret word in notes"));
        assert_eq!(OpenCode.shown_title("OpenCode").as_deref(), Some("OpenCode"));
        assert!(OpenCode.may_be("/opt/homebrew/bin/opencode") && OpenCode.may_be("opencode.exe"), "npm's runs as opencode.exe");
        assert!(!OpenCode.may_be("opencode-helper"));
    }

    #[test]
    fn its_finished_answers_are_its_usage() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            r#"create table session (id text primary key, parent_id text, directory text not null, title text not null,
                 time_created integer not null, time_updated integer not null, time_archived integer);
             create table message (id text primary key, session_id text not null, time_created integer not null, data text not null);
             insert into session values ('ses_a', null, '/r', 't', 1, 1, null);
             insert into session values ('ses_sub', 'ses_a', '/r', 'sub', 1, 1, null);
             insert into message values ('m1', 'ses_a', 1000, '{"role":"user"}');
             insert into message values ('m2', 'ses_a', 2000, '{"role":"assistant","modelID":"qwen3:4b","time":{"created":2000,"completed":2500},"tokens":{"input":10,"output":5,"reasoning":2,"cache":{"read":100,"write":3}}}');
             insert into message values ('m3', 'ses_sub', 3000, '{"role":"assistant","modelID":"qwen3:4b","time":{"created":3000,"completed":3100},"tokens":{"input":1,"output":1,"reasoning":0,"cache":{"read":0,"write":0}}}');
             insert into message values ('m4', 'ses_a', 4000, '{"role":"assistant","modelID":"qwen3:4b","time":{"created":4000},"tokens":{"input":9,"output":0,"reasoning":0,"cache":{"read":0,"write":0}}}');"#,
        )
        .unwrap();
        let (used, latest) = usage_from(&c, 0);
        assert_eq!(latest, 4000);
        assert_eq!(used.len(), 2, "not the user's message, nor an answer still going");
        assert_eq!((used[0].id.as_str(), used[0].conversation.as_str(), used[0].at_ms), ("m2", "ses_a", 2000));
        assert_eq!((used[0].input, used[0].cache_read, used[0].cache_write, used[0].output), (10, 100, 3, 7), "reasoning is output");
        assert_eq!(used[1].conversation, "ses_a", "a subagent's counts for its conversation");
        assert_eq!(usage_from(&c, 2000).0.len(), 1, "only what started since");
    }
}
