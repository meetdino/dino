//! Cursor Agent (`cursor-agent`, also installed as `agent`). It signs in to the user's Cursor
//! account and talks to Cursor's own servers, where the models run; dino doesn't route it. It takes
//! its conversation id up front (`--new-session-id`), continues one with `--resume`, and keeps each
//! in two places: `~/.cursor/chats/<md5 of its folder>/<id>/` (`meta.json`: title, times, folder;
//! `store.db`), and a transcript it appends to as it goes,
//! `~/.cursor/projects/<folder slug>/agent-transcripts/<id>/<id>.jsonl`, which dino follows for
//! status and reads as the conversation. Its permission and trust dialogs are only on its screen.
//! It runs as Node from its own install (`…/cursor-agent/versions/<v>/index.js`), under the name
//! it was started by.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{Agent, ControlKind, LogEvent, StatusSource, Wiring, strings};
use crate::found::{self, FoundSession};
use crate::history::{self, Meta, Turn, one_line, turn};
use crate::models::{Catalog, ModelInfo};

pub(crate) struct Cursor;

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

/// Its settings and chats: `$CURSOR_CONFIG_DIR`, else `$XDG_CONFIG_HOME/cursor`, else `~/.cursor`.
fn config_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("CURSOR_CONFIG_DIR") {
        return d.into();
    }
    std::env::var_os("XDG_CONFIG_HOME").map(|x| PathBuf::from(x).join("cursor")).unwrap_or_else(|| home().join(".cursor"))
}

/// Its transcripts: `$CURSOR_DATA_DIR`, else `~/.cursor`.
fn data_dir() -> PathBuf {
    std::env::var_os("CURSOR_DATA_DIR").map(PathBuf::from).unwrap_or_else(|| home().join(".cursor"))
}

/// Only ids it could have made: no path separators.
fn valid(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Every chat's folder, with its id.
fn chats() -> Vec<(String, PathBuf)> {
    std::fs::read_dir(config_dir().join("chats"))
        .into_iter()
        .flatten()
        .flatten()
        .flat_map(|d| std::fs::read_dir(d.path()).into_iter().flatten().flatten())
        .filter_map(|e| Some((e.file_name().to_str()?.to_string(), e.path())))
        .collect()
}

fn chat_meta(dir: &Path) -> Value {
    std::fs::read_to_string(dir.join("meta.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null)
}

/// Conversation `id`'s transcript, wherever its folder's slug put it.
fn transcript_of(id: &str) -> Option<PathBuf> {
    if !valid(id) {
        return None;
    }
    let rel = Path::new("agent-transcripts").join(id).join(format!("{id}.jsonl"));
    std::fs::read_dir(data_dir().join("projects")).ok()?.flatten().map(|e| e.path().join(&rel)).find(|p| p.exists())
}

/// What a person typed: Cursor may keep it inside its `<user_query>` wrapper.
fn typed_text(text: &str) -> Option<&str> {
    let inner = text.split_once("<user_query>").and_then(|(_, r)| r.split_once("</user_query>")).map(|(q, _)| q);
    history::typed(inner.unwrap_or(text))
}

fn texts(content: &Value) -> String {
    let parts: Vec<&str> = content.as_array().into_iter().flatten().filter(|p| p["type"] == "text").filter_map(|p| p["text"].as_str()).collect();
    parts.join("\n")
}

fn turns_in(jsonl: &str) -> Vec<Turn> {
    let mut out = vec![];
    for v in jsonl.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
        let content = &v["message"]["content"];
        match (v["role"].as_str(), v["type"].as_str()) {
            (Some("user"), _) => out.extend(typed_text(&texts(content)).map(|t| turn("user", t))),
            (Some("assistant"), _) => {
                let text = texts(content);
                if !text.trim().is_empty() {
                    out.push(turn("assistant", text.trim()));
                }
                for call in content.as_array().into_iter().flatten().filter(|p| p["type"] == "tool_use") {
                    out.push(turn("tool", format!("{}{}", call["name"].as_str().unwrap_or("tool"), history::hint(&call["input"]))));
                }
            }
            (_, Some("turn_ended")) if v["status"] == "aborted" => out.push(turn("note", "Interrupted")),
            (_, Some("turn_ended")) if v["status"] == "error" => out.push(turn("note", format!("Error: {}", v["error"].as_str().map(history::short).unwrap_or_default()))),
            _ => {}
        }
    }
    out
}

/// The first prompt in a transcript.
fn first_prompt(jsonl: &str) -> Meta {
    let first = jsonl
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|v| v["role"] == "user")
        .and_then(|v| typed_text(&texts(&v["message"]["content"])).and_then(one_line));
    Meta { hidden: first.is_none(), title: first, cwd: None }
}

/// What one line of its transcript says about its turn: a prompt starts one, `turn_ended` ends it.
fn event_of(v: &Value) -> LogEvent {
    match (v["role"].as_str(), v["type"].as_str()) {
        (Some("user"), _) => LogEvent::TurnStarted,
        (_, Some("turn_ended")) => LogEvent::TurnEnded,
        (_, Some("metadata")) => LogEvent::Bookkeeping,
        _ => LogEvent::Other,
    }
}

/// The tools an answer in its transcript calls, as `Agent::tool_calls` says them. Its transcript
/// keeps no results, so each counts as made when it's written: started and ended at once. An MCP
/// server's tool is `CallMcpTool` (its `server` and `toolName` in its input), or
/// `CallDynamicTool` (`namespace` and `toolName`): named here `<server>-<tool>`.
fn tool_calls_in(v: &Value) -> Vec<(String, bool)> {
    if v["role"] != "assistant" {
        return vec![];
    }
    let calls = v["message"]["content"].as_array().into_iter().flatten().filter(|p| p["type"] == "tool_use");
    calls
        .filter_map(|c| {
            let (i, name) = (&c["input"], c["name"].as_str()?);
            let mcp = matches!(name, "CallMcpTool" | "CallDynamicTool");
            let name = match (mcp, i["server"].as_str().or(i["namespace"].as_str()), i["toolName"].as_str()) {
                (true, Some(server), Some(tool)) => format!("{server}-{tool}"),
                (true, None, Some(tool)) => tool.to_string(),
                _ => name.to_string(),
            };
            Some([(name.clone(), true), (name, false)])
        })
        .flatten()
        .collect()
}

fn busy_in(jsonl: &str) -> bool {
    let mut busy = false;
    for v in jsonl.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
        match event_of(&v) {
            LogEvent::TurnStarted => busy = true,
            LogEvent::TurnEnded => busy = false,
            _ => {}
        }
    }
    busy
}

/// `cursor-agent models`: "Available models", then `<id> - <name> (current, default)` a line.
fn catalog_in(out: &str) -> Option<Catalog> {
    let mut lines = out.lines().map(str::trim).skip_while(|l| *l != "Available models").skip(1);
    let mut models = vec![];
    let mut default_model = None;
    for l in lines.by_ref().skip_while(|l| l.is_empty()).take_while(|l| !l.is_empty()) {
        let (rest, tags) = match l.rsplit_once(" (") {
            Some((r, t)) if t.ends_with(')') && t.trim_end_matches(')').split(", ").all(|x| x == "current" || x == "default") => (r, t),
            _ => (l, ""),
        };
        let (id, name) = rest.split_once(" - ").unwrap_or((rest, rest));
        if id.is_empty() || id.contains(' ') {
            continue;
        }
        if tags.contains("default") {
            default_model = Some(id.to_string());
        }
        models.push(ModelInfo { id: id.into(), label: name.trim().into(), ..ModelInfo::default() });
    }
    (!models.is_empty()).then_some(Catalog { models, default_model })
}

/// Its arguments after its `index.js`, if `args` (a process's, without the program) are Cursor Agent's.
fn own_args(args: &[String]) -> Option<&[String]> {
    let i = args.iter().position(|a| a.ends_with("/index.js") && a.contains("/cursor-agent/versions/"))?;
    Some(&args[i + 1..])
}

/// The conversation a live process (`args` its own) is on: the one it was told to resume or start
/// with, else the newest one begun in its folder since it started.
fn conversation_in(pid: u32, args: &[String]) -> Option<String> {
    let named = args.iter().position(|a| a == "--resume" || a == "--new-session-id").and_then(|i| args.get(i + 1)).filter(|v| valid(v));
    if let Some(id) = named {
        return Some(id.clone());
    }
    let cwd = crate::procinfo::cwd_of(pid)?;
    let since = crate::procinfo::started(pid).unwrap_or(0) * 1000;
    chats()
        .into_iter()
        .map(|(id, dir)| (id, chat_meta(&dir)))
        .filter(|(_, m)| m["isSubagent"] != true && m["createdAtMs"].as_u64().is_some_and(|c| c + 1000 >= since) && m["cwd"].as_str().is_some_and(|c| same_dir(c, &cwd)))
        .max_by_key(|(_, m)| m["createdAtMs"].as_u64())
        .map(|(id, _)| id)
}

fn same_dir(a: &str, b: &str) -> bool {
    let canon = |p: &str| std::fs::canonicalize(p).unwrap_or_else(|_| PathBuf::from(p));
    canon(a) == canon(b)
}

/// A chat's title as Cursor shows it, else its first prompt.
fn title_of(id: &str, meta: &Value) -> Option<String> {
    let named = meta["title"].as_str().filter(|t| *t != "New Agent").and_then(one_line);
    named.or_else(|| transcript_of(id).and_then(|p| history::cached(&p, |p| first_prompt(&history::peek(p))).title))
}

impl Agent for Cursor {
    fn id(&self) -> &'static str {
        "cursor"
    }

    // It asks before commands; `--auto-review` has Cursor's classifier run the safe ones, `--plan`
    // only plans, `--yolo` runs everything.
    fn modes(&self) -> &'static [&'static str] {
        &["ask", "plan", "auto", "bypass"]
    }

    fn mode_label(&self, mode: &str) -> Option<&'static str> {
        Some(match mode {
            "ask" => "Default",
            "plan" => "Plan",
            "auto" => "Auto-review",
            "bypass" => "Run Everything",
            _ => return None,
        })
    }

    fn mode_args(&self, mode: &str) -> Vec<String> {
        match mode {
            "plan" => strings(&["--plan"]),
            "auto" => strings(&["--auto-review"]),
            "bypass" => strings(&["--yolo"]),
            _ => vec![],
        }
    }

    fn model_args(&self, model: &str) -> Vec<String> {
        strings(&["--model", model])
    }

    // Effort is part of a model's id (`model[effort=high]`), as its model list gives them.
    fn effort_args(&self, _effort: &str) -> Vec<String> {
        vec![]
    }

    fn value_flags(&self) -> &'static [&'static str] {
        &[
            "--api-key", "-H", "--header", "-e", "--endpoint", "--output-format", "--mode", "--resume", "--new-session-id", "--model", "--sandbox",
            "--workspace", "--add-dir", "--plugin-dir", "--worktree-base",
        ]
    }

    fn control_of(&self, name: &str, value: Option<&str>) -> Option<ControlKind> {
        match name {
            "--plan" | "--auto-review" | "--yolo" | "-f" | "--force" => Some(ControlKind::Mode),
            "--mode" if value == Some("plan") => Some(ControlKind::Mode),
            "--model" => Some(ControlKind::Model),
            _ => None,
        }
    }

    fn read_mode(&self, flags: &[(&str, Option<&str>)]) -> Option<String> {
        let mode = match flags.last()?.0 {
            "--plan" | "--mode" => "plan",
            "--auto-review" => "auto",
            _ => "bypass",
        };
        Some(mode.into())
    }

    // The models its account has, as it lists them (asking Cursor's servers; nothing when signed out).
    fn catalog(&self, program: &str) -> Option<Catalog> {
        let program = program.to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let out = std::process::Command::new(&program).arg("models").env("NO_COLOR", "1").stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).output();
            let _ = tx.send(out);
        });
        let out = rx.recv_timeout(std::time::Duration::from_secs(15)).ok()?.ok()?;
        out.status.success().then(|| catalog_in(&String::from_utf8_lossy(&out.stdout))).flatten()
    }

    // Its model calls happen on Cursor's servers, signed in to the user's account; dino leaves them be.
    fn wiring(&self, _route: bool, _base: &dyn Fn(&str) -> String, _status_line: Option<String>) -> Wiring {
        (vec![], vec![])
    }

    // Its first prompt goes last on its command line, and its terminal stays open.
    fn prompt_args(&self, prompt: String) -> Vec<String> {
        vec![prompt]
    }

    fn session_args(&self, session: &mut Option<String>, restoring: bool) -> (Vec<String>, Vec<String>) {
        match session {
            // Its chat is made as it starts (`--new-session-id`): continue it if it was.
            Some(id) if restoring && chats().iter().any(|(c, _)| c == id) => (vec!["--resume".into(), id.clone()], vec![]),
            _ => {
                let id = session.get_or_insert_with(crate::new_uuid).clone();
                (vec!["--new-session-id".into(), id], vec![])
            }
        }
    }

    fn status_source(&self) -> StatusSource {
        StatusSource::Log
    }

    fn log_path(&self, session: &str) -> Option<PathBuf> {
        transcript_of(session)
    }

    fn log_event(&self, line: &Value) -> LogEvent {
        event_of(line)
    }

    fn tool_calls(&self, line: &Value) -> Vec<(String, bool)> {
        tool_calls_in(line)
    }

    fn busy(&self, pid: u32) -> Option<bool> {
        let args = found::args_of(pid);
        let id = conversation_in(pid, own_args(&args)?)?;
        Some(busy_in(&std::fs::read_to_string(transcript_of(&id)?).ok()?))
    }

    fn asks_on_screen(&self) -> bool {
        true
    }

    fn asking(&self, screen: &str) -> Option<String> {
        const APPROVALS: &[&str] = &["Run this command?", "Run this command outside the sandbox?", "Run this MCP tool?", "Write to this file?", "Delete this file?", "Allow this web fetch?"];
        if screen.contains("Workspace Trust Required") || screen.contains("Do you trust the contents of this directory?") {
            Some("Trust this folder?".into())
        } else if screen.contains("Press any key to log in") {
            Some("Sign in to Cursor".into())
        } else if screen.contains("Skip & tell the agent what to do instead") || APPROVALS.iter().any(|a| screen.contains(a)) {
            Some("Cursor asks for permission".into())
        } else {
            None
        }
    }

    fn portable_flags(&self, args: &[String]) -> Vec<String> {
        found::drop_flags(args, &["--resume", "--new-session-id", "--output-format", "--api-key", "-w", "--worktree", "--worktree-base"], &["--continue", "-p", "--print", "--stream-partial-output", "--list-models"])
    }

    fn headless(&self, args: &[String]) -> bool {
        super::runs_with(
            args,
            &["-p", "--print", "--output-format"],
            &["acp", "worker", "login", "logout", "status", "whoami", "about", "models", "update", "ls", "create-chat", "mcp", "generate-rule"],
        )
    }

    // Started as `cursor-agent` or `agent`, which name its Node process takes.
    fn may_be(&self, comm: &str) -> bool {
        matches!(comm.rsplit('/').next(), Some("cursor-agent" | "agent"))
    }

    // Found only in dino's shells: it runs as `node`, among every other Node program.
    fn running(&self, _procs: &crate::procinfo::Procs) -> Vec<FoundSession> {
        vec![]
    }

    fn inside(&self, pid: u32, comm: &str, args: &dyn Fn() -> Vec<String>) -> Option<FoundSession> {
        if !self.may_be(comm) {
            return None;
        }
        let all = args();
        let own = own_args(&all)?;
        let mut s = found::by_hand("cursor", pid);
        s.cwd = crate::procinfo::cwd_of(pid);
        s.title = "Cursor Agent".into();
        if let Some(id) = conversation_in(pid, own) {
            let dir = chats().into_iter().find(|(c, _)| *c == id).map(|(_, d)| d);
            let meta = dir.as_deref().map(chat_meta).unwrap_or(Value::Null);
            s.title = title_of(&id, &meta).unwrap_or_else(|| "Cursor Agent".into());
            s.updated_at = meta["updatedAtMs"].as_u64().unwrap_or(0) / 1000;
            s.status = transcript_of(&id).and_then(|p| std::fs::read_to_string(p).ok()).map(|t| if busy_in(&t) { "busy" } else { "idle" }.into());
            s.session_id = id;
        }
        s.args = self.portable_flags(own);
        Some(s)
    }

    fn recent(&self, running: &dyn Fn(&str) -> bool) -> Vec<FoundSession> {
        let mut out = vec![];
        for (id, dir) in chats() {
            let meta = chat_meta(&dir);
            if meta["hasConversation"] != true || meta["isSubagent"] == true || running(&id) {
                continue;
            }
            let Some(title) = title_of(&id, &meta) else { continue };
            let updated = meta["updatedAtMs"].as_u64().or(meta["createdAtMs"].as_u64()).unwrap_or(0) / 1000;
            out.push(history::recent("cursor", id, title, meta["cwd"].as_str().map(String::from), updated));
        }
        out
    }

    // Its cloud agents are Cursor's, not listed here.
    fn cloud_args(&self, _session_id: &str) -> Vec<String> {
        vec![]
    }

    fn transcript(&self, session_id: &str) -> Option<PathBuf> {
        transcript_of(session_id)
    }

    // Its transcript keeps no time, model or tokens: each new answer counts as a call, at the time
    // its chat was last updated, on the model its chat says when it does.
    fn usage(&self, seen: &mut crate::usage::Seen) -> Vec<crate::usage::Used> {
        let mut out = vec![];
        for (id, dir) in chats() {
            let Some(p) = transcript_of(&id) else { continue };
            let Some((text, from)) = seen.new_lines(&p, b"") else { continue };
            let meta = chat_meta(&dir);
            let at = meta["updatedAtMs"].as_i64().or_else(|| history::modified(&p).checked_mul(1000).map(|t| t as i64)).unwrap_or(0);
            let model = ["model", "lastUsedModel", "modelName"].iter().find_map(|k| meta[*k].as_str()).map(String::from);
            out.extend(usage_in(&text, from, &id, at, meta["cwd"].as_str(), model));
        }
        out
    }

    fn turns(&self, text: &str, _path: &Path, _start: u64) -> Vec<Turn> {
        turns_in(text)
    }
}

/// Its answers in transcript lines from byte `from` of conversation `id`'s file, at `at`.
fn usage_in(jsonl: &str, from: u64, id: &str, at: i64, cwd: Option<&str>, model: Option<String>) -> Vec<crate::usage::Used> {
    history::lines_at(jsonl, from)
        .filter(|(_, l)| l.contains("\"assistant\""))
        .filter(|(_, l)| serde_json::from_str::<Value>(l).is_ok_and(|v| v["role"] == "assistant"))
        .map(|(offset, _)| crate::usage::Used {
            id: format!("{id}:{offset}"),
            at_ms: at,
            conversation: id.into(),
            cwd: cwd.map(String::from),
            model: model.clone(),
            undated: true,
            ..Default::default()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lines as Cursor Agent 2026.10.01 writes its transcript (its bundle's transcript writer: one
    /// `{role, message: {content}}` per message, tool calls as `tool_use`, tool results left out,
    /// `turn_ended` at each turn's end): a prompt that ran a command, then one stopped.
    const TRANSCRIPT: &str = r#"{"role":"user","message":{"content":[{"type":"text","text":"<user_query>\nlist the files here\n</user_query>"}]}}
{"role":"assistant","message":{"content":[{"type":"text","text":"I'll run ls."},{"type":"tool_use","name":"Shell","input":{"command":"ls","working_directory":"/Users/me/demo"}}]}}
{"role":"assistant","message":{"content":[{"type":"text","text":"There are two entries: README.md and src/."}]}}
{"type":"turn_ended","status":"success"}
{"role":"user","message":{"content":[{"type":"text","text":"now delete them"}]}}
{"type":"turn_ended","status":"aborted"}"#;

    #[test]
    fn its_transcript_reads_as_turns() {
        let turns: Vec<(String, String)> = turns_in(TRANSCRIPT).into_iter().map(|t| (t.role, t.text)).collect();
        let want = [
            ("user", "list the files here"),
            ("assistant", "I'll run ls."),
            ("tool", "Shell ls"),
            ("assistant", "There are two entries: README.md and src/."),
            ("user", "now delete them"),
            ("note", "Interrupted"),
        ];
        assert_eq!(turns, want.map(|(r, t)| (r.to_string(), t.to_string())));
        assert_eq!(first_prompt(TRANSCRIPT).title.as_deref(), Some("list the files here"));
    }

    #[test]
    fn its_transcript_says_where_the_turn_is() {
        assert!(!busy_in(TRANSCRIPT));
        let (mid, _) = TRANSCRIPT.split_once(r#"{"type":"turn_ended","status":"success"}"#).unwrap();
        assert!(busy_in(mid));
    }

    #[test]
    fn the_tools_it_calls_mcp_servers_by_their_own_names() {
        let calls = |line: &str| tool_calls_in(&serde_json::from_str(line).unwrap());
        let shell = TRANSCRIPT.lines().nth(1).unwrap();
        assert_eq!(calls(shell), [("Shell".to_string(), true), ("Shell".to_string(), false)]);
        // As Cursor writes an MCP call (2.6+): the server and tool in the call's input.
        let mcp = r#"{"role":"assistant","message":{"content":[{"type":"tool_use","name":"CallMcpTool","input":{"server":"open-computer-use","toolName":"list_apps","arguments":{}}},{"type":"tool_use","name":"CallDynamicTool","input":{"namespace":"playwright","toolName":"browser_navigate","arguments":{"url":"https://example.com"}}}]}}"#;
        let names: Vec<String> = calls(mcp).into_iter().filter(|(_, started)| *started).map(|(n, _)| n).collect();
        assert_eq!(names, ["open-computer-use-list_apps", "playwright-browser_navigate"]);
        assert!(calls(TRANSCRIPT.lines().next().unwrap()).is_empty(), "the user's line calls nothing");
    }

    #[test]
    fn models_as_it_lists_them() {
        let out = "Available models\n\nauto - Auto\nclaude-opus-4-8 - Claude Opus 4.8 (default)\ngpt-5.5 - GPT-5.5 (current)\n\nTip: use --model <id> (or /model <id> in interactive mode) to switch.\n";
        let c = catalog_in(out).unwrap();
        let ids: Vec<(&str, &str)> = c.models.iter().map(|m| (m.id.as_str(), m.label.as_str())).collect();
        assert_eq!(ids, [("auto", "Auto"), ("claude-opus-4-8", "Claude Opus 4.8"), ("gpt-5.5", "GPT-5.5")]);
        assert_eq!(c.default_model.as_deref(), Some("claude-opus-4-8"));
        assert!(catalog_in("No models available for this account.").is_none());
    }

    #[test]
    fn found_by_its_install_and_continued_by_id() {
        let args: Vec<String> = ["--use-system-ca", "/Users/me/.local/share/cursor-agent/versions/2026.10.01-e373342/index.js", "--resume", "3f6c1a52-9a0e", "--model", "gpt-5.5", "--yolo"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let own = own_args(&args).unwrap();
        assert_eq!(Cursor.portable_flags(own), ["--model", "gpt-5.5", "--yolo"]);
        assert!(own_args(&["/opt/homebrew/lib/node_modules/x/index.js".to_string()]).is_none());
        assert!(Cursor.may_be("/Users/me/.local/bin/cursor-agent") && Cursor.may_be("agent") && !Cursor.may_be("node"));
        let mut none = None;
        let (before, _) = Cursor.session_args(&mut none, false);
        assert_eq!(before, ["--new-session-id".to_string(), none.clone().unwrap()]);
        let mut gone = Some("3f6c1a52".to_string());
        assert_eq!(Cursor.session_args(&mut gone, true).0, ["--new-session-id", "3f6c1a52"], "its chat was never made: start it with the same id");
        for m in ["plan", "auto", "bypass"] {
            assert_eq!(Cursor.read_mode(&[(Cursor.mode_args(m)[0].as_str(), None)]).as_deref(), Some(m));
        }
    }

    #[test]
    fn its_dialogs_on_screen() {
        let run = "  Run this command?\n  $ rm -rf build\n  Run (once) (y)\n  Add Shell(rm) to allowlist? (tab)\n  Skip & tell the agent what to do instead (esc or n)";
        assert_eq!(Cursor.asking(run).as_deref(), Some("Cursor asks for permission"));
        assert_eq!(Cursor.asking("Workspace Trust Required\nDo you trust the contents of this directory?").as_deref(), Some("Trust this folder?"));
        assert_eq!(Cursor.asking("Cursor Agent\nv2026.10.01\nPress any key to log in...").as_deref(), Some("Sign in to Cursor"));
        assert_eq!(Cursor.asking("→ list the files").as_deref(), None);
    }

    #[test]
    fn its_answers_count_as_calls() {
        let used = usage_in(TRANSCRIPT, 0, "c1", 5000, Some("/me"), None);
        assert_eq!(used.len(), 2);
        let second = TRANSCRIPT.find(r#"{"role":"assistant","message":{"content":[{"type":"text","text":"There"#).unwrap();
        assert_eq!((used[1].id.clone(), used[1].at_ms, used[1].cwd.as_deref()), (format!("c1:{second}"), 5000, Some("/me")));
    }
}
