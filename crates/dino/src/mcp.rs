//! `dino mcp`: an MCP server on stdio that lets an agent see and drive the other dino sessions.
//! dinod passes it to Claude sessions by itself (Settings → Policies); for other agents, add
//! `dino mcp` as a stdio MCP server in their config, e.g. Codex's `~/.codex/config.toml`:
//!
//! ```toml
//! [mcp_servers.dino]
//! command = "dino"
//! args = ["mcp"]
//! env_vars = ["DINO_SESSION", "DINO_HOME"]
//! ```
//!
//! dinod sets `DINO_SESSION` in every session, so the server knows which session is asking; an
//! agent that filters its MCP servers' environment (Codex does) has to be told to pass it on.

use std::io::{BufRead, Write};

use dino_core::ipc::{Request, Response, SessionInfo};
use serde_json::{Value, json};

use crate::client::Control;

const VERSIONS: &[&str] = &["2026-07-28", "2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "dino runs coding agents (Claude Code, Codex, shells, …) side by side on this computer. These tools list those sessions, read what one has been doing, message one that's idle, and start new ones.";

pub fn serve(read_only: bool) -> anyhow::Result<()> {
    let me = std::env::var("DINO_SESSION").ok().filter(|s| !s.is_empty());
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => handle(&msg, me.as_deref(), read_only),
            Err(e) => Some(error(Value::Null, -32700, &format!("parse error: {e}"))),
        };
        if let Some(reply) = reply {
            writeln!(stdout, "{reply}")?;
            stdout.flush()?;
        }
    }
    Ok(())
}

/// The answer to one JSON-RPC message; none for a notification.
fn handle(msg: &Value, me: Option<&str>, read_only: bool) -> Option<Value> {
    let id = msg.get("id")?.clone();
    let method = msg["method"].as_str().unwrap_or("");
    let params = &msg["params"];
    let result = match method {
        "initialize" | "server/discover" => {
            let asked = params["protocolVersion"].as_str().or_else(|| params["_meta"]["protocolVersion"].as_str());
            let version = asked.filter(|v| VERSIONS.contains(v)).unwrap_or(VERSIONS[1]);
            json!({
                "protocolVersion": version,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "dino", "version": env!("CARGO_PKG_VERSION") },
                "instructions": INSTRUCTIONS,
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tools(read_only) }),
        "tools/call" => {
            let name = params["name"].as_str().unwrap_or("");
            if !tools(read_only).iter().any(|t| t["name"] == name) {
                return Some(error(id, -32602, &format!("unknown tool: {name}")));
            }
            match call(name, &params["arguments"], me) {
                Ok(text) => json!({ "content": [{ "type": "text", "text": text }], "isError": false }),
                Err(e) => json!({ "content": [{ "type": "text", "text": e.to_string() }], "isError": true }),
            }
        }
        _ => return Some(error(id, -32601, &format!("method not found: {method}"))),
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn error(id: Value, code: i32, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn tools(read_only: bool) -> Vec<Value> {
    let session = json!({ "type": "string", "description": "Session id or name, from list_sessions" });
    let mut out = vec![
        json!({
            "name": "list_sessions",
            "description": "List the agent sessions dino is running: id, name, agent, status (working, idle, waiting on the user, exited), folder, git branch and PR.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true },
        }),
        json!({
            "name": "read_session",
            "description": "Read what a session has been doing: its recent conversation (when dino can read it) and its terminal screen now.",
            "inputSchema": {
                "type": "object",
                "properties": { "session": session, "lines": { "type": "integer", "description": "Screen lines to include (default 60, at most 400)" } },
                "required": ["session"],
            },
            "annotations": { "readOnlyHint": true },
        }),
    ];
    if !read_only {
        out.push(json!({
            "name": "send_message",
            "description": "Type a message into another session's agent and submit it, as if the user had. Only works while that session is idle; if it's working or waiting on the user, try later.",
            "inputSchema": {
                "type": "object",
                "properties": { "session": session, "text": { "type": "string", "description": "The message" } },
                "required": ["session", "text"],
            },
        }));
        out.push(json!({
            "name": "create_session",
            "description": "Start a new agent session in dino, shown to the user alongside the others. Returns its id; use read_session to follow it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "agent": { "type": "string", "description": "Which agent: claude (default), codex, shell, or another launcher dino offers" },
                    "prompt": { "type": "string", "description": "Its first message (for a shell, a command)" },
                    "cwd": { "type": "string", "description": "Folder to work in (default: this session's)" },
                    "worktree": { "type": "boolean", "description": "Work in a new git worktree and branch of that folder's repo, so its changes stay apart (default false)" },
                },
                "required": ["prompt"],
            },
        }));
    }
    out
}

fn call(name: &str, args: &Value, me: Option<&str>) -> anyhow::Result<String> {
    let mut dinod = Control::open_existing().map_err(|_| anyhow::anyhow!("dinod isn't running"))?;
    let mut ask = |req: Request| match dinod.request(&req)? {
        Response::Error { message } => Err(anyhow::anyhow!(message)),
        other => Ok(other),
    };
    let arg = |k: &str| args[k].as_str().map(str::to_string);
    match name {
        "list_sessions" => {
            let Response::State { sessions, .. } = ask(Request::State)? else { anyhow::bail!("unexpected reply from dinod") };
            if sessions.is_empty() {
                return Ok("No sessions.".into());
            }
            Ok(sessions.iter().map(|s| describe(s, &sessions, me)).collect::<Vec<_>>().join("\n"))
        }
        "read_session" => {
            let id = resolve(&mut ask, &arg("session").unwrap_or_default())?;
            let lines = args["lines"].as_u64().map(|n| n.min(u32::MAX as u64) as u32);
            let Response::Text { text } = ask(Request::ReadSession { id, lines })? else { anyhow::bail!("unexpected reply from dinod") };
            Ok(text)
        }
        "send_message" => {
            let id = resolve(&mut ask, &arg("session").unwrap_or_default())?;
            ask(Request::Message { id: id.clone(), text: arg("text").unwrap_or_default(), by: me.map(Into::into) })?;
            Ok(format!("Sent to session {id}. Use read_session to see its answer once it's done."))
        }
        "create_session" => {
            let launcher = arg("agent").filter(|a| !a.is_empty()).unwrap_or_else(|| "claude".into());
            let req = Request::Start { launcher, cwd: arg("cwd"), prompt: arg("prompt"), worktree: args["worktree"].as_bool().unwrap_or(false), by: me.map(Into::into) };
            let Response::Created { id } = ask(req)? else { anyhow::bail!("unexpected reply from dinod") };
            let Response::State { sessions, .. } = ask(Request::State)? else { return Ok(format!("Started session {id}.")) };
            Ok(match sessions.iter().find(|s| s.id == id) {
                Some(s) => format!("Started {}", describe(s, &sessions, me)),
                None => format!("Started session {id}."),
            })
        }
        _ => anyhow::bail!("unknown tool {name}"),
    }
}

/// A session id from an id or a name.
fn resolve(ask: &mut impl FnMut(Request) -> anyhow::Result<Response>, key: &str) -> anyhow::Result<String> {
    let key = key.trim();
    anyhow::ensure!(!key.is_empty(), "say which session (its id or name, from list_sessions)");
    let Response::State { sessions, .. } = ask(Request::State)? else { anyhow::bail!("unexpected reply from dinod") };
    sessions
        .iter()
        .find(|s| s.id == key)
        .or_else(|| sessions.iter().find(|s| s.name == key))
        .or_else(|| sessions.iter().find(|s| s.name.eq_ignore_ascii_case(key)))
        .map(|s| s.id.clone())
        .ok_or_else(|| anyhow::anyhow!("no session {key:?}; list_sessions shows them"))
}

/// One line about session `s`, for the agent.
fn describe(s: &SessionInfo, all: &[SessionInfo], me: Option<&str>) -> String {
    let mut parts = vec![format!("id {}", s.id), format!("{} ({})", s.name, s.agent_id), status(s), s.cwd.clone()];
    if let Some(b) = branch(&s.cwd) {
        parts.push(format!("branch {b}"));
    }
    if let Some(pr) = &s.pr {
        parts.push(format!("PR #{} {}", pr.number, pr.state.to_lowercase()));
    }
    if let Some(t) = s.title.as_deref().filter(|t| !t.is_empty()) {
        parts.push(format!("title {t:?}"));
    }
    let name_of = |id: &str| all.iter().find(|o| o.id == id).map_or(id.to_string(), |o| o.name.clone());
    if let Some(by) = &s.started_by {
        parts.push(format!("started by {}", name_of(by)));
    }
    let mut line = parts.join(" · ");
    if me == Some(s.id.as_str()) {
        line.push_str("  ← you");
    }
    line
}

/// As dinod words it when refusing a message (`peers::status`), so the two agree.
fn status(s: &SessionInfo) -> String {
    let quiet = s.output_ms_ago.is_none_or(|ms| ms > 2500);
    match s.activity.as_deref() {
        _ if s.exited => "exited".into(),
        Some(a) if a.starts_with("needs:") => format!("waiting on the user ({})", &a[6..]),
        _ if s.in_flight == 0 && s.activity.as_deref() != Some("working") && quiet => "idle".into(),
        Some("working") => "working".into(),
        _ => "busy".into(),
    }
}

fn branch(cwd: &str) -> Option<String> {
    let out = std::process::Command::new("git").args(["-C", cwd, "branch", "--show-current"]).stderr(std::process::Stdio::null()).output().ok()?;
    let b = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!b.is_empty()).then_some(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_and_tool_list() {
        let init = handle(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{}}}), None, false).unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
        assert!(init["result"]["capabilities"]["tools"].is_object());
        let unknown = handle(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"1999-01-01"}}), None, false).unwrap();
        assert_eq!(unknown["result"]["protocolVersion"], VERSIONS[1]);
        assert!(handle(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}), None, false).is_none());
        let names = |ro| {
            let list = handle(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}), None, ro).unwrap();
            list["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect::<Vec<_>>()
        };
        assert_eq!(names(false), ["list_sessions", "read_session", "send_message", "create_session"]);
        assert_eq!(names(true), ["list_sessions", "read_session"]);
        // A tool the read-only server doesn't offer is refused before reaching dinod.
        let refused = handle(&json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"send_message","arguments":{}}}), None, true).unwrap();
        assert_eq!(refused["error"]["code"], -32602);
        assert_eq!(handle(&json!({"jsonrpc":"2.0","id":4,"method":"nope"}), None, false).unwrap()["error"]["code"], -32601);
    }

    #[test]
    fn status_words() {
        let s = |activity: Option<&str>, ms: Option<u64>| SessionInfo { activity: activity.map(Into::into), output_ms_ago: ms, ..Default::default() };
        assert_eq!(status(&s(Some("needs:Bash"), None)), "waiting on the user (Bash)");
        assert_eq!(status(&s(Some("done"), Some(10_000))), "idle");
        assert_eq!(status(&s(None, Some(100))), "busy");
        assert_eq!(status(&s(Some("working"), Some(10_000))), "working");
        assert_eq!(status(&SessionInfo { exited: true, ..s(Some("working"), None) }), "exited");
    }
}
