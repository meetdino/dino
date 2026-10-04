//! GitHub Copilot CLI (`copilot`, `@github/copilot`). It signs in with GitHub (its own `/login`,
//! a token in its environment, or the GitHub CLI's), and its model calls go to the Copilot API on
//! the user's own plan; dino doesn't route them. It takes its conversation id up front
//! (`--session-id`, created if missing) and keeps each conversation in
//! `~/.copilot/session-state/<id>/` (`$COPILOT_HOME`): `workspace.yaml` (its folder, name and
//! times), `events.jsonl` (everything that happens in it, written as it goes, which dino follows
//! for status) and `inuse.<pid>.lock` while a process has it open. Its process is a native
//! `copilot`, under `node` when npm installed it.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{Agent, ControlKind, LogEvent, StatusSource, Wiring, strings};
use crate::found::{self, FoundSession};
use crate::history::{self, Meta, Turn, one_line, turn};

pub(crate) struct Copilot;

/// Where Copilot keeps its own files: `$COPILOT_HOME`, else `~/.copilot`.
fn copilot_home() -> PathBuf {
    std::env::var_os("COPILOT_HOME").map(PathBuf::from).unwrap_or_else(|| std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(".copilot"))
}

fn sessions_dir() -> PathBuf {
    copilot_home().join("session-state")
}

/// A conversation's folder; only ids it could have made (no path separators).
fn session_dir(id: &str) -> Option<PathBuf> {
    (!id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')).then(|| sessions_dir().join(id))
}

fn events_of(id: &str) -> Option<PathBuf> {
    session_dir(id).map(|d| d.join("events.jsonl"))
}

/// The flat `key: value` lines of its `workspace.yaml`, unquoted.
fn yaml_field(yaml: &str, key: &str) -> Option<String> {
    yaml.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        if k != key {
            return None;
        }
        let v = v.trim();
        let v = v.strip_prefix('"').and_then(|v| v.strip_suffix('"')).or_else(|| v.strip_prefix('\'').and_then(|v| v.strip_suffix('\''))).unwrap_or(v);
        (!v.is_empty()).then(|| v.to_string())
    })
}

/// A line written for a subagent, not the conversation itself.
fn subagent(v: &Value) -> bool {
    !v["agentId"].is_null() || !v["data"]["parentToolCallId"].is_null()
}

/// A prompt the person sent: not one injected (a skill's, another agent's) or autopilot's own.
fn typed_prompt(d: &Value) -> Option<&str> {
    let injected = d["source"].as_str().is_some_and(|s| s != "user") || d["isAutopilotContinuation"] == true;
    if injected { None } else { d["content"].as_str().and_then(history::typed) }
}

/// Title as Copilot shows it: the name given (`--name`, `/rename`) or its summary, else the first
/// prompt. Folder from `workspace.yaml`.
fn meta_of(dir: &Path) -> Meta {
    let yaml = std::fs::read_to_string(dir.join("workspace.yaml")).unwrap_or_default();
    let events = dir.join("events.jsonl");
    let first = history::cached(&events, |p| meta_in(&history::peek(p))).title;
    let named = yaml_field(&yaml, "name").or_else(|| yaml_field(&yaml, "summary")).and_then(|t| one_line(&t));
    Meta { hidden: first.is_none(), title: named.or(first), cwd: yaml_field(&yaml, "cwd") }
}

/// The first prompt in a record, and its folder.
fn meta_in(jsonl: &str) -> Meta {
    let (mut first, mut cwd) = (None, None);
    for v in jsonl.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
        match v["type"].as_str() {
            Some("session.start") => cwd = v["data"]["context"]["cwd"].as_str().map(String::from),
            Some("user.message") if first.is_none() && !subagent(&v) => first = typed_prompt(&v["data"]).and_then(one_line),
            _ => {}
        }
    }
    Meta { hidden: first.is_none(), title: first, cwd }
}

fn turns_in(jsonl: &str) -> Vec<Turn> {
    let mut out = vec![];
    for v in jsonl.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
        if subagent(&v) {
            continue;
        }
        let d = &v["data"];
        match v["type"].as_str() {
            Some("user.message") => out.extend(typed_prompt(d).map(|t| turn("user", t))),
            Some("assistant.message") => {
                let text = d["content"].as_str().unwrap_or_default().trim();
                if !text.is_empty() {
                    out.push(turn("assistant", text));
                }
                for call in d["toolRequests"].as_array().into_iter().flatten() {
                    out.push(turn("tool", format!("{}{}", call["name"].as_str().unwrap_or("tool"), history::hint(&call["arguments"]))));
                }
            }
            Some("abort") => out.push(turn("note", "Interrupted")),
            _ => {}
        }
    }
    out
}

/// What a permission it waits on is for: the command, the file, else what it means to do.
fn asked(d: &Value) -> String {
    let r = &d["permissionRequest"];
    let file = r["fileName"].as_str().map(|f| f.rsplit('/').next().unwrap_or(f));
    match (r["kind"].as_str(), r["fullCommandText"].as_str(), file) {
        (Some("shell"), Some(cmd), _) => format!("Run: {}?", history::short(cmd)),
        (Some("write"), _, Some(f)) => format!("Edit {f}?"),
        _ => r["intention"].as_str().map(|i| format!("{}?", history::short(i))).unwrap_or_else(|| "Allow Copilot?".into()),
    }
}

/// What one line of its record says about its turn. Its own end-of-turn notice (`session.idle`)
/// isn't written down: a turn ends on an answer that asks for no tool, or when it's stopped.
fn event_of(v: &Value) -> LogEvent {
    if subagent(v) {
        return LogEvent::Other;
    }
    let d = &v["data"];
    match v["type"].as_str().unwrap_or_default() {
        "user.message" | "assistant.turn_start" => LogEvent::TurnStarted,
        "assistant.message" if d["toolRequests"].as_array().is_none_or(|t| t.is_empty()) => LogEvent::TurnEnded,
        "abort" | "session.error" | "session.shutdown" | "session.task_complete" => LogEvent::TurnEnded,
        "permission.requested" if d["resolvedByHook"] != true => LogEvent::Needs(asked(d)),
        "assistant.turn_end" | "session.start" | "session.resume" | "session.usage_checkpoint" | "session.info" | "session.model_change" | "session.mode_changed" | "hook.start" | "hook.end" => LogEvent::Bookkeeping,
        _ => LogEvent::Other,
    }
}

/// The tool a line of its record starts or ends running, as `Agent::tool_calls` says it; its
/// subagents' calls too, which reach the same Mac. An MCP server's tool is named
/// `<server>-<tool>`, with the server and tool apart in `mcpServerName` and `mcpToolName` (empty
/// for its own tools); a server whose tools it doesn't prefix still gets its name in front. Its
/// end repeats the name in newer versions only (1.0.91 gives just the call's id).
fn tool_calls_in(v: &Value) -> Vec<(String, bool)> {
    let d = &v["data"];
    let name = || {
        let tool = d["toolName"].as_str().unwrap_or_default();
        match (d["mcpServerName"].as_str().filter(|s| !s.is_empty()), d["mcpToolName"].as_str().filter(|t| !t.is_empty())) {
            (Some(server), Some(own)) if !tool.starts_with(server) => format!("{server}-{own}"),
            _ => tool.to_string(),
        }
    };
    match v["type"].as_str() {
        Some("tool.execution_start") => vec![(name(), true)].into_iter().filter(|(n, _)| !n.is_empty()).collect(),
        Some("tool.execution_complete") => vec![(name(), false)],
        _ => vec![],
    }
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

/// The conversation process `pid` has open: it keeps `inuse.<pid>.lock` in its folder meanwhile.
fn conversation_in(pid: u32) -> Option<String> {
    let lock = format!("inuse.{pid}.lock");
    std::fs::read_dir(sessions_dir()).ok()?.flatten().find(|e| e.path().join(&lock).exists()).and_then(|e| e.file_name().to_str().map(String::from))
}

impl Copilot {
    fn found(&self, pid: u32) -> FoundSession {
        let mut s = found::by_hand("copilot", pid);
        s.cwd = crate::procinfo::cwd_of(pid);
        s.title = "Copilot".into();
        if let Some(id) = conversation_in(pid) {
            let dir = sessions_dir().join(&id);
            let events = dir.join("events.jsonl");
            s.title = meta_of(&dir).title.unwrap_or_else(|| "Copilot".into());
            s.updated_at = history::modified(&events);
            s.status = std::fs::read_to_string(&events).ok().map(|t| if busy_in(&t) { "busy" } else { "idle" }.into());
            s.session_id = id;
        }
        s
    }
}

impl Agent for Copilot {
    fn id(&self) -> &'static str {
        "copilot"
    }

    // It asks before edits and commands; `--allow-tool=write` stops asking before edits, `--plan`
    // plans first, `--yolo` allows everything.
    fn modes(&self) -> &'static [&'static str] {
        &["ask", "edits", "plan", "bypass"]
    }

    fn mode_label(&self, mode: &str) -> Option<&'static str> {
        Some(match mode {
            "ask" => "Default",
            "edits" => "Allow edits",
            "plan" => "Plan",
            "bypass" => "Allow all",
            _ => return None,
        })
    }

    fn mode_args(&self, mode: &str) -> Vec<String> {
        match mode {
            "edits" => strings(&["--allow-tool=write"]),
            "plan" => strings(&["--plan"]),
            "bypass" => strings(&["--yolo"]),
            _ => vec![],
        }
    }

    fn model_args(&self, model: &str) -> Vec<String> {
        strings(&["--model", model])
    }

    fn effort_args(&self, effort: &str) -> Vec<String> {
        strings(&["--reasoning-effort", effort])
    }

    fn value_flags(&self) -> &'static [&'static str] {
        &[
            "-i", "--interactive", "-p", "--prompt", "--model", "--reasoning-effort", "--context", "--auto-tier", "--agent", "-n", "--name", "--session-id",
            "-C", "--log-dir", "--extension-sdk-path", "--log-level", "--stream", "--output-format", "--add-dir", "--attachment", "--disable-mcp-server",
            "--add-github-mcp-toolset", "--add-github-mcp-tool", "--plugin-dir", "--additional-mcp-config", "--mcp-github-auth", "--allow-tool", "--deny-tool",
            "--available-tools", "--excluded-tools", "--secret-env-vars", "--allow-url", "--deny-url", "--max-autopilot-continues", "--mode",
            "--dynamic-retrieval", "--enable-mcp-server", "--max-ai-credits", "--usage-output-file",
        ]
    }

    fn control_of(&self, name: &str, value: Option<&str>) -> Option<ControlKind> {
        match name {
            "--plan" | "--yolo" | "--allow-all" => Some(ControlKind::Mode),
            "--mode" if value == Some("plan") => Some(ControlKind::Mode),
            "--allow-tool" if value == Some("write") => Some(ControlKind::Mode),
            "--model" => Some(ControlKind::Model),
            "--reasoning-effort" => Some(ControlKind::Effort),
            _ => None,
        }
    }

    fn read_mode(&self, flags: &[(&str, Option<&str>)]) -> Option<String> {
        let mode = match flags.last()? {
            ("--plan", _) | ("--mode", _) => "plan",
            ("--allow-tool", _) => "edits",
            _ => "bypass",
        };
        Some(mode.into())
    }

    // Its model calls go to the Copilot API with the user's GitHub sign-in; dino leaves them be.
    fn wiring(&self, _route: bool, _base: &dyn Fn(&str) -> String, _status_line: Option<String>) -> Wiring {
        (vec![], vec![])
    }

    // `-i` starts its terminal on a prompt (`-p` would run once and exit).
    fn prompt_args(&self, prompt: String) -> Vec<String> {
        vec!["-i".into(), prompt]
    }

    // Its exact id, created if missing: the same flag starts it and continues it.
    fn session_args(&self, session: &mut Option<String>, _restoring: bool) -> (Vec<String>, Vec<String>) {
        let id = session.get_or_insert_with(crate::new_uuid).clone();
        (vec![format!("--session-id={id}")], vec![])
    }

    fn status_source(&self) -> StatusSource {
        StatusSource::Log
    }

    fn log_path(&self, session: &str) -> Option<PathBuf> {
        events_of(session)
    }

    fn log_event(&self, line: &Value) -> LogEvent {
        event_of(line)
    }

    fn tool_calls(&self, line: &Value) -> Vec<(String, bool)> {
        tool_calls_in(line)
    }

    fn conversation_of(&self, pid: u32) -> Option<String> {
        conversation_in(pid)
    }

    fn busy(&self, pid: u32) -> Option<bool> {
        Some(busy_in(&std::fs::read_to_string(events_of(&conversation_in(pid)?)?).ok()?))
    }

    fn asks_on_screen(&self) -> bool {
        true
    }

    fn asking(&self, screen: &str) -> Option<String> {
        if screen.contains("Do you trust the files in this folder?") {
            Some("Trust this folder?".into())
        } else if screen.contains("No, and tell Copilot what to do differently") {
            Some("Copilot asks for permission".into())
        } else {
            None
        }
    }

    fn portable_flags(&self, args: &[String]) -> Vec<String> {
        found::drop_flags(
            args,
            &["-r", "--resume", "--session-id", "-n", "--name", "-i", "--interactive", "-p", "--prompt", "--connect", "--share", "--output-format", "--attachment", "--usage-output-file"],
            &["--continue", "-s", "--silent", "--share-gist", "--acp", "--fleet"],
        )
    }

    fn may_be(&self, comm: &str) -> bool {
        comm.rsplit('/').next() == Some("copilot")
    }

    fn running(&self) -> Vec<FoundSession> {
        let mut out = vec![];
        for pid in crate::procinfo::pids_named("copilot") {
            let mut s = self.found(pid);
            if s.session_id.is_empty() {
                continue;
            }
            let (terminal, args) = found::terminal_and_flags(self, pid);
            s.terminal = terminal;
            s.args = args;
            out.push(s);
        }
        out
    }

    fn inside(&self, pid: u32, comm: &str, args: &dyn Fn() -> Vec<String>) -> Option<FoundSession> {
        if !self.may_be(comm) {
            return None;
        }
        let mut s = self.found(pid);
        s.args = self.portable_flags(&args());
        Some(s)
    }

    fn recent(&self, running: &dyn Fn(&str) -> bool) -> Vec<FoundSession> {
        let mut out = vec![];
        for e in std::fs::read_dir(sessions_dir()).into_iter().flatten().flatten() {
            let dir = e.path();
            let Some(id) = e.file_name().to_str().map(String::from) else { continue };
            let events = dir.join("events.jsonl");
            // A launch that was never sent a prompt leaves a folder and no record.
            if !events.exists() || running(&id) {
                continue;
            }
            let meta = meta_of(&dir);
            if meta.hidden {
                continue;
            }
            let title = meta.title.unwrap_or_else(|| "Copilot session".into());
            out.push(history::recent("copilot", id, title, meta.cwd, history::modified(&events)));
        }
        out
    }

    // Its cloud agent's tasks are GitHub's, not listed here.
    fn cloud_args(&self, _session_id: &str) -> Vec<String> {
        vec![]
    }

    fn transcript(&self, session_id: &str) -> Option<PathBuf> {
        events_of(session_id).filter(|p| p.exists())
    }

    fn usage(&self, seen: &mut crate::usage::Seen) -> Vec<crate::usage::Used> {
        let mut out = vec![];
        for e in std::fs::read_dir(sessions_dir()).into_iter().flatten().flatten() {
            let Some(id) = e.file_name().to_str().map(String::from) else { continue };
            let p = e.path().join("events.jsonl");
            let Some((text, _)) = seen.new_lines(&p, b"") else { continue };
            let (mk, uk) = (format!("copilot:model:{id}"), format!("copilot:usage:{id}"));
            let mut state = Tally { model: seen.mark(&mk).map(String::from), metered: seen.mark(&uk).is_some() };
            let cwd = yaml_field(&std::fs::read_to_string(e.path().join("workspace.yaml")).unwrap_or_default(), "cwd");
            out.extend(usage_in(&text, &id, cwd.as_deref(), &mut state));
            if let Some(m) = state.model {
                seen.set_mark(&mk, m);
            }
            if state.metered {
                seen.set_mark(&uk, "1".into());
            }
        }
        out
    }

    fn turns(&self, text: &str, _path: &Path, _start: u64) -> Vec<Turn> {
        turns_in(text)
    }
}

/// What one read of its events carries to the next.
#[derive(Default)]
struct Tally {
    /// The model its last start or switch named.
    model: Option<String>,
    /// It writes `assistant.usage` events: those are its calls, with their tokens.
    metered: bool,
}

/// Its calls in conversation `id`'s events: its `assistant.usage` events where it writes them
/// (each call's model and tokens), else each `assistant.message`, on the model last chosen, with
/// no tokens.
fn usage_in(jsonl: &str, id: &str, cwd: Option<&str>, state: &mut Tally) -> Vec<crate::usage::Used> {
    const KINDS: [&str; 5] = ["\"assistant.usage\"", "\"assistant.message\"", "\"session.start\"", "\"session.resume\"", "\"session.model_change\""];
    let events: Vec<Value> = jsonl.lines().filter(|l| KINDS.iter().any(|k| l.contains(k))).filter_map(|l| serde_json::from_str(l).ok()).collect();
    state.metered |= events.iter().any(|v| v["type"] == "assistant.usage");
    let mut out = vec![];
    for v in &events {
        let d = &v["data"];
        let used = |model: Option<String>| crate::usage::Used {
            id: v["id"].as_str().map_or_else(String::new, |e| format!("{id}:{e}")),
            at_ms: history::ms_of(&v["timestamp"]).unwrap_or(0),
            conversation: id.into(),
            cwd: cwd.map(String::from),
            model,
            ..Default::default()
        };
        match v["type"].as_str() {
            Some("session.start" | "session.resume") => state.model = d["selectedModel"].as_str().map(String::from).or(state.model.take()),
            Some("session.model_change") => state.model = d["newModel"].as_str().map(String::from).or(state.model.take()),
            Some("assistant.usage") => out.push(crate::usage::Used {
                input: history::count(&d["inputTokens"]),
                cache_read: history::count(&d["cacheReadTokens"]),
                cache_write: history::count(&d["cacheWriteTokens"]),
                output: history::count(&d["outputTokens"]),
                ..used(d["model"].as_str().map(String::from).or(state.model.clone()))
            }),
            Some("assistant.message") if !state.metered => out.push(used(state.model.clone())),
            _ => {}
        }
    }
    out.retain(|u| !u.id.is_empty() && u.at_ms > 0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed from Copilot CLI 1.0.91's own record of a session (run offline against a scripted
    /// model): a prompt, one that ran `ls` (read-only, so not asked), then, resumed by the same
    /// `--session-id`, one that asked before running a command.
    const EVENTS: &str = r#"{"type":"session.start","data":{"sessionId":"b2fcbbeb-4980-4063-8e3d-4f1a5585ed36","copilotVersion":"1.0.91","selectedModel":"stub-model","context":{"cwd":"/private/tmp/proj"}},"id":"fb6cb0b2","timestamp":"2026-10-04T04:21:17.924Z"}
{"type":"session.model_change","data":{"newModel":"stub-model","source":"startup"},"id":"3ffa6930","timestamp":"2026-10-04T04:21:18.137Z"}
{"type":"user.message","data":{"content":"say hi","transformedContent":"<current_datetime>2026-10-03T21:21:17-07:00</current_datetime>\n\nsay hi","turnId":"0"},"id":"4487d859","timestamp":"2026-10-04T04:21:20.806Z"}
{"type":"assistant.turn_start","data":{"turnId":"0"},"id":"6effdbb4","timestamp":"2026-10-04T04:21:20.834Z"}
{"type":"assistant.message","data":{"content":"hi","toolRequests":[],"turnId":"0"},"id":"a67dff70","timestamp":"2026-10-04T04:21:21.396Z"}
{"type":"assistant.turn_end","data":{"turnId":"0"},"id":"66a061ad","timestamp":"2026-10-04T04:21:21.404Z"}
{"type":"user.message","data":{"content":"please run ls","transformedContent":"<current_datetime>2026-10-03T21:21:17-07:00</current_datetime>\n\nplease run ls","turnId":"0"},"id":"10ba4b80","timestamp":"2026-10-04T04:21:54.295Z"}
{"type":"assistant.turn_start","data":{"turnId":"0"},"id":"dbc1b9b1","timestamp":"2026-10-04T04:21:54.301Z"}
{"type":"assistant.message","data":{"content":"","toolRequests":[{"toolCallId":"call_a059c558","name":"bash","arguments":{"command":"ls","description":"List files"},"type":"function","intentionSummary":"List files"}],"turnId":"0"},"id":"f4c96774","timestamp":"2026-10-04T04:21:54.879Z"}
{"type":"tool.execution_start","data":{"toolCallId":"call_a059c558","toolName":"bash","turnId":"0"},"id":"1ca672a0","timestamp":"2026-10-04T04:21:54.881Z"}
{"type":"tool.execution_complete","data":{"toolCallId":"call_a059c558","success":true,"result":{"content":"\n<shellId: 0 completed with exit code 0>"},"turnId":"0"},"id":"9b209bef","timestamp":"2026-10-04T04:21:54.985Z"}
{"type":"assistant.turn_end","data":{"turnId":"0"},"id":"fdc09212","timestamp":"2026-10-04T04:21:54.993Z"}
{"type":"assistant.turn_start","data":{"turnId":"1"},"id":"11e76276","timestamp":"2026-10-04T04:21:54.993Z"}
{"type":"assistant.message","data":{"content":"DONE","toolRequests":[],"turnId":"1"},"id":"74c877c6","timestamp":"2026-10-04T04:21:55.529Z"}
{"type":"assistant.turn_end","data":{"turnId":"1"},"id":"8d1a423b","timestamp":"2026-10-04T04:21:55.531Z"}
{"type":"session.shutdown","data":{"shutdownType":"routine"},"id":"3b403d38","timestamp":"2026-10-04T04:22:58.501Z"}
{"type":"session.resume","data":{"eventCount":17,"selectedModel":"stub-model","context":{"cwd":"/private/tmp/proj"}},"id":"ddde3bb2","timestamp":"2026-10-04T04:23:21.674Z"}
{"type":"user.message","data":{"content":"please run touch","transformedContent":"<current_datetime>2026-10-03T21:21:17-07:00</current_datetime>\n\nplease run touch","turnId":"0"},"id":"b36e9175","timestamp":"2026-10-04T04:23:25.544Z"}
{"type":"assistant.turn_start","data":{"turnId":"0"},"id":"fff1967d","timestamp":"2026-10-04T04:23:25.551Z"}
{"type":"assistant.message","data":{"content":"","toolRequests":[{"toolCallId":"call_d7a51f0b","name":"bash","arguments":{"command":"touch made-by-stub.txt","description":"Run a command"},"type":"function","intentionSummary":"Run a command"}],"turnId":"0"},"id":"e2d4d232","timestamp":"2026-10-04T04:23:26.089Z"}
{"type":"tool.execution_start","data":{"toolCallId":"call_d7a51f0b","toolName":"bash","turnId":"0"},"id":"9bf36ca6","timestamp":"2026-10-04T04:23:26.100Z"}
{"type":"permission.requested","data":{"requestId":"9416764b","permissionRequest":{"kind":"shell","toolCallId":"call_d7a51f0b","fullCommandText":"touch made-by-stub.txt","intention":"Run a command"}},"id":"c6519a5b","timestamp":"2026-10-04T04:23:26.133Z"}
{"type":"permission.completed","data":{"requestId":"9416764b","result":{"kind":"approved"},"decisionSource":"human_response"},"id":"efea1753","timestamp":"2026-10-04T04:23:27.298Z"}
{"type":"tool.execution_complete","data":{"toolCallId":"call_d7a51f0b","success":true,"result":{"content":"\n<shellId: 0 completed with exit code 0>"},"turnId":"0"},"id":"a98bfd7d","timestamp":"2026-10-04T04:23:27.329Z"}
{"type":"assistant.turn_end","data":{"turnId":"0"},"id":"1e5882b0","timestamp":"2026-10-04T04:23:27.331Z"}
{"type":"assistant.turn_start","data":{"turnId":"1"},"id":"fb2ef2e7","timestamp":"2026-10-04T04:23:27.331Z"}
{"type":"assistant.message","data":{"content":"DONE","toolRequests":[],"turnId":"1"},"id":"96b4c067","timestamp":"2026-10-04T04:23:27.870Z"}
{"type":"assistant.turn_end","data":{"turnId":"1"},"id":"1f7d45fe","timestamp":"2026-10-04T04:23:27.872Z"}
{"type":"session.shutdown","data":{"shutdownType":"routine"},"id":"f88abfa5","timestamp":"2026-10-04T04:23:29.963Z"}"#;

    #[test]
    fn its_record_says_which_tools_it_runs() {
        let calls = |line: &str| tool_calls_in(&serde_json::from_str(line).unwrap());
        // Copilot 1.0.91's own lines, on BYOK with qwen3:4b; its end doesn't name the tool.
        let start = r#"{"type":"tool.execution_start","data":{"toolCallId":"call_14gc1jbg","toolName":"glob","arguments":{"pattern":"*.md"},"turnId":"0","model":"qwen3:4b","toolTitle":"Finding files"},"id":"f2732c74-b788-4da6-836d-c8ca3b5a862a","timestamp":"2026-10-04T15:19:59.308Z"}"#;
        let end = r#"{"type":"tool.execution_complete","data":{"toolCallId":"call_14gc1jbg","model":"qwen3:4b","turnId":"0","rte":false,"success":true,"result":{"content":"./README.md"}},"id":"f5bfc5f8","timestamp":"2026-10-04T15:19:59.400Z"}"#;
        assert_eq!(calls(start), [("glob".to_string(), true)]);
        assert_eq!(calls(end), [(String::new(), false)]);
        // An MCP server's tool, with the fields its session events give one.
        let mcp = r#"{"type":"tool.execution_start","data":{"toolCallId":"c2","toolName":"open-computer-use-list_apps","mcpServerName":"open-computer-use","mcpToolName":"list_apps","arguments":{}}}"#;
        assert_eq!(calls(mcp), [("open-computer-use-list_apps".to_string(), true)]);
        let unprefixed = r#"{"type":"tool.execution_start","data":{"toolCallId":"c3","toolName":"browser_navigate","mcpServerName":"playwright","mcpToolName":"browser_navigate"}}"#;
        assert_eq!(calls(unprefixed), [("playwright-browser_navigate".to_string(), true)]);
        assert!(EVENTS.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).flat_map(|v| tool_calls_in(&v)).all(|(n, started)| n == "bash" || !started));
    }

    #[test]
    fn its_record_reads_as_turns() {
        let turns: Vec<(String, String)> = turns_in(EVENTS).into_iter().map(|t| (t.role, t.text)).collect();
        let want = [
            ("user", "say hi"),
            ("assistant", "hi"),
            ("user", "please run ls"),
            ("tool", "bash ls"),
            ("assistant", "DONE"),
            ("user", "please run touch"),
            ("tool", "bash touch made-by-stub.txt"),
            ("assistant", "DONE"),
        ];
        assert_eq!(turns, want.map(|(r, t)| (r.to_string(), t.to_string())));
        assert_eq!(meta_in(EVENTS), Meta { title: Some("say hi".into()), cwd: Some("/private/tmp/proj".into()), hidden: false });
    }

    #[test]
    fn its_record_says_where_the_turn_is() {
        let events: Vec<LogEvent> = EVENTS.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).map(|v| event_of(&v)).collect();
        assert!(events.contains(&LogEvent::Needs("Run: touch made-by-stub.txt?".into())));
        let needs = events.iter().position(|e| matches!(e, LogEvent::Needs(_))).unwrap();
        assert_eq!(events[needs + 1], LogEvent::Other, "answering is something it does");
        let (asking, _) = EVENTS.split_once(r#"{"type":"permission.completed""#).unwrap();
        assert!(busy_in(asking), "waiting on the user is still its turn");
        let (between, _) = EVENTS.split_once(r#"{"type":"assistant.turn_start","data":{"turnId":"1"},"id":"11e76276""#).unwrap();
        assert!(busy_in(between), "a model call that asked for a tool isn't the end of the turn");
        let (answered, _) = EVENTS.split_once(r#"{"type":"user.message","data":{"content":"please run ls""#).unwrap();
        assert!(!busy_in(answered), "an answer that asks for no tool ends it");
        assert!(!busy_in(EVENTS));
        // Stopped mid-turn (Esc): its `abort` ends the turn.
        let stopped = format!("{answered}{}", r#"{"type":"user.message","data":{"content":"go on"}}
{"type":"assistant.turn_start","data":{"turnId":"0"}}
{"type":"abort","data":{"reason":"user_initiated"}}"#);
        assert!(!busy_in(&stopped));
        assert_eq!(turns_in(&stopped).last().map(|t| t.text.as_str()), Some("Interrupted"));
    }

    #[test]
    fn subagents_and_injected_prompts_are_not_the_conversation() {
        let sub = r#"{"type":"assistant.message","agentId":"explore-1","data":{"content":"sub","toolRequests":[]}}"#;
        let v: Value = serde_json::from_str(sub).unwrap();
        assert_eq!(event_of(&v), LogEvent::Other, "a subagent's answer doesn't end the turn");
        let skill = r#"{"type":"user.message","data":{"content":"PDF skill instructions","source":"skill-pdf"}}"#;
        assert!(turns_in(skill).is_empty());
    }

    #[test]
    fn its_workspace_file() {
        let yaml = "id: 3a145fb3-8670-4968-990c-d7b4c897e6e3\ncwd: /private/tmp/proj\nclient_name: github/cli\nuser_named: false\nname: \"Fix auth: expiry\"\nsummary_count: 0\n";
        assert_eq!(yaml_field(yaml, "cwd").as_deref(), Some("/private/tmp/proj"));
        assert_eq!(yaml_field(yaml, "name").as_deref(), Some("Fix auth: expiry"));
        assert_eq!(yaml_field(yaml, "summary"), None);
        assert!(session_dir("../x").is_none() && session_dir("").is_none());
    }

    #[test]
    fn modes_and_the_same_flag_starts_and_continues_it() {
        let c = Copilot;
        for m in ["edits", "plan", "bypass"] {
            let a = c.mode_args(m);
            let (name, value) = a[0].split_once('=').map_or((a[0].as_str(), None), |(n, v)| (n, Some(v)));
            assert_eq!(c.control_of(name, value), Some(ControlKind::Mode), "{m}");
            assert_eq!(c.read_mode(&[(name, value)]).as_deref(), Some(m));
        }
        assert_eq!(c.control_of("--allow-tool", Some("shell(git:*)")), None, "only edits is a mode");
        let mut none = None;
        let (before, _) = c.session_args(&mut none, false);
        assert_eq!(before, [format!("--session-id={}", none.unwrap())]);
        let args: Vec<String> = ["--resume", "abc", "--model", "gpt-5.5", "--yolo", "--continue", "-i", "hi"].iter().map(|s| s.to_string()).collect();
        assert_eq!(c.portable_flags(&args), ["--model", "gpt-5.5", "--yolo"]);
    }

    #[test]
    fn its_dialogs_on_screen() {
        let trust = " Confirm folder trust\n /tmp/x\n Do you trust the files in this folder?\n ❯ 1. Yes\n   2. Yes, and remember this folder for future sessions\n   3. No (Esc)";
        assert_eq!(Copilot.asking(trust).as_deref(), Some("Trust this folder?"));
        let run = " Run the test suite\n npm test\n Do you want to run this command?\n ❯ 1. Yes\n   2. Yes, and approve `npm` for the rest of the running session\n   3. No, and tell Copilot what to do differently (Esc)";
        assert!(Copilot.asking(run).is_some());
        assert_eq!(Copilot.asking("> fix the tests").as_deref(), None);
    }

    #[test]
    fn its_answers_count_as_calls_on_the_model_chosen() {
        let mut t = Tally::default();
        let used = usage_in(EVENTS, "b2fc", Some("/p"), &mut t);
        assert_eq!(used.len(), 5, "each answer, the subagent's none here");
        assert!(used.iter().all(|u| u.model.as_deref() == Some("stub-model") && u.input == 0));
        assert_eq!((used[0].id.as_str(), used[0].cwd.as_deref()), ("b2fc:a67dff70", Some("/p")));
        // Where it writes each call's usage, those are its calls.
        let metered = r#"{"type":"assistant.message","data":{"content":"x"},"id":"m1","timestamp":"2026-10-04T04:21:21.396Z"}
{"type":"assistant.usage","data":{"model":"gpt-x","inputTokens":100,"outputTokens":9,"cacheReadTokens":50,"cacheWriteTokens":2},"id":"u1","timestamp":"2026-10-04T04:21:21.400Z"}"#;
        let mut t = Tally::default();
        let used = usage_in(metered, "b2fc", None, &mut t);
        assert_eq!(used.len(), 1);
        assert_eq!((used[0].input, used[0].cache_read, used[0].cache_write, used[0].output, used[0].model.as_deref()), (100, 50, 2, 9, Some("gpt-x")));
        assert!(t.metered);
    }
}
