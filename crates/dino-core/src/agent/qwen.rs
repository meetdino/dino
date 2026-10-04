//! Qwen Code, and Qwen Code on dino's free tier (`free`), which it reaches as the Anthropic API.
//! It reports its lifecycle through hooks in Claude Code's format, taken from a settings layer
//! dino writes per session (`QWEN_CODE_SYSTEM_DEFAULTS_PATH`), and keeps its conversations at
//! `~/.qwen/projects/<cwd>/chats/<id>.jsonl` and a record of each running process at
//! `~/.qwen/sessions/<pid>.json`.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::{Agent, ControlKind, StatusSource, Wiring, strings};
use crate::providers::Format;
use crate::found::{self, FoundSession, Source};
use crate::history::{self, Meta, Turn, one_line, turn, typed};

pub(crate) struct Qwen {
    pub(crate) free: bool,
}

/// Where Qwen keeps its own files: `$QWEN_HOME`, else `~/.qwen`.
fn qwen_home() -> PathBuf {
    std::env::var_os("QWEN_HOME").map(PathBuf::from).unwrap_or_else(|| std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(".qwen"))
}

/// The system defaults layer Qwen reads when nothing points it elsewhere.
const SYSTEM_DEFAULTS: &str = "/Library/Application Support/QwenCode/system-defaults.json";

/// `hooks` added to the system defaults layer the user may have: dino's go after theirs, event by
/// event, and everything else in it is kept.
fn with_hooks(mut layer: Value, hooks: &Value) -> Value {
    if !layer.is_object() {
        layer = json!({});
    }
    let ours = hooks["hooks"].as_object().cloned().unwrap_or_default();
    let theirs = layer["hooks"].as_object_mut().map(std::mem::take).unwrap_or_default();
    let mut merged = theirs;
    for (event, entries) in ours {
        let list = merged.entry(event).or_insert_with(|| json!([]));
        if let (Some(list), Some(entries)) = (list.as_array_mut(), entries.as_array()) {
            list.extend(entries.iter().cloned());
        }
    }
    layer["hooks"] = Value::Object(merged);
    layer
}

/// Write the settings layer that reports session `hook_url`'s lifecycle to dino, and return its
/// path. It stands in for the user's own system defaults, so those are carried into it.
fn hook_layer(hook_url: &str) -> Option<PathBuf> {
    let dir = crate::config_dir().join("qwen");
    std::fs::create_dir_all(&dir).ok()?;
    // One file per session (`…/s/<id>/hook`), rewritten each start whatever the proxy's port.
    let session = hook_url.split("/s/").nth(1).and_then(|r| r.split('/').next()).unwrap_or("session");
    let path = layer_path(session);
    let own = std::env::var_os("QWEN_CODE_SYSTEM_DEFAULTS_PATH").map(PathBuf::from).filter(|p| !p.starts_with(&dir));
    let theirs = std::fs::read_to_string(own.unwrap_or_else(|| PathBuf::from(SYSTEM_DEFAULTS))).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null);
    let hooks: Value = serde_json::from_str(&crate::claude_hook_settings(hook_url, None)).ok()?;
    std::fs::write(&path, serde_json::to_vec_pretty(&with_hooks(theirs, &hooks)).ok()?).ok()?;
    Some(path)
}

/// Where dino session `session`'s settings layer is written.
fn layer_path(session: &str) -> PathBuf {
    let name: String = session.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    crate::config_dir().join("qwen").join(format!("{name}.json"))
}

/// Remove dino session `session`'s settings layer, once the session is gone for good.
pub fn forget(session: &str) {
    let _ = std::fs::remove_file(layer_path(session));
}

/// Every conversation file: `projects/<cwd>/chats/<id>.jsonl`.
fn transcripts() -> Vec<PathBuf> {
    std::fs::read_dir(qwen_home().join("projects"))
        .into_iter()
        .flatten()
        .flatten()
        .flat_map(|d| std::fs::read_dir(d.path().join("chats")).into_iter().flatten().flatten())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl") && !p.to_string_lossy().ends_with(".ledger.jsonl"))
        .collect()
}

/// The text of a Gemini-style `Content`'s parts, without its thoughts.
fn text_of(message: &Value) -> Vec<&str> {
    message["parts"].as_array().into_iter().flatten().filter(|p| p["thought"] != true).filter_map(|p| p["text"].as_str()).collect()
}

/// A person's own message: not a runtime note, an expansion, or a command.
fn said(v: &Value) -> Option<String> {
    if v["type"] != "user" || v["subtype"].is_string() {
        return None;
    }
    let text = text_of(&v["message"]).join("\n");
    typed(&text).filter(|t| !t.starts_with('/')).map(String::from)
}

/// Title as Qwen shows it: its latest title (given with `/rename`, or its own), else the first
/// prompt. Folder from its records; one with no prompt was never a conversation.
fn meta_in(jsonl: &str) -> Meta {
    let (mut title, mut first, mut cwd) = (None, None, None);
    for v in jsonl.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
        cwd = cwd.or_else(|| v["cwd"].as_str().map(String::from));
        if v["type"] == "system" && v["subtype"] == "custom_title" {
            title = v["systemPayload"]["customTitle"].as_str().and_then(one_line).or(title);
        } else if first.is_none() {
            first = said(&v).as_deref().and_then(one_line);
        }
    }
    Meta { hidden: first.is_none(), title: title.or(first), cwd }
}

fn meta(p: &Path) -> Meta {
    history::cached(p, |p| meta_in(&history::peek(p)))
}

/// A conversation's entries as turns.
fn turns_in(jsonl: &str) -> Vec<Turn> {
    let mut out = vec![];
    for v in jsonl.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
        match v["type"].as_str() {
            Some("user") => out.extend(said(&v).map(|t| turn("user", t))),
            Some("assistant") => {
                let text = text_of(&v["message"]).join("\n");
                if !text.trim().is_empty() {
                    out.push(turn("assistant", text.trim()));
                }
                for p in v["message"]["parts"].as_array().into_iter().flatten() {
                    if let Some(call) = p.get("functionCall") {
                        out.push(turn("tool", format!("{}{}", call["name"].as_str().unwrap_or("tool"), history::hint(&call["args"]))));
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// `sessions/<pid>.json`, what a live Qwen says about itself, if `pid` is one in its terminal UI.
fn live(pid: u32) -> Option<Value> {
    let text = std::fs::read_to_string(qwen_home().join(format!("sessions/{pid}.json"))).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    (v["pid"].as_u64() == Some(pid as u64) && v["kind"].as_str().is_none_or(|k| k == "tui")).then_some(v)
}

impl Qwen {
    fn title(&self, session_id: &str, v: &Value) -> String {
        self.transcript(session_id).and_then(|p| meta(&p).title).or_else(|| v["name"].as_str().map(String::from)).unwrap_or_else(|| "Qwen Code".into())
    }
}

impl Agent for Qwen {
    fn id(&self) -> &'static str {
        if self.free { "qwen-free" } else { "qwen" }
    }

    fn modes(&self) -> &'static [&'static str] {
        &["ask", "edits", "plan", "auto", "bypass"]
    }

    // Its approval modes, as it names them.
    fn mode_label(&self, mode: &str) -> Option<&'static str> {
        Some(match mode {
            "ask" => "Default",
            "edits" => "Auto-edit",
            "plan" => "Plan",
            "auto" => "Auto",
            "bypass" => "YOLO",
            _ => return None,
        })
    }

    fn mode_args(&self, mode: &str) -> Vec<String> {
        let m = match mode {
            "ask" => "default",
            "edits" => "auto-edit",
            "plan" => "plan",
            "auto" => "auto",
            _ => "yolo",
        };
        strings(&["--approval-mode", m])
    }

    // The free tier picks the model for each turn.
    fn picks_model(&self) -> bool {
        !self.free
    }

    fn free(&self) -> bool {
        self.free
    }

    fn provider_formats(&self) -> &'static [Format] {
        // Its own way to a custom endpoint is its OpenAI mode.
        if self.free { &[] } else { &[Format::Chat, Format::Anthropic, Format::Responses] }
    }

    // Each of its auth types reads a base URL of its own from the environment (the URL carries the
    // proxy's secret, so not `--openai-base-url`); the key is a placeholder for dino's.
    fn provider_wiring(&self, url: &str, format: Format, model: &str) -> Option<Wiring> {
        if self.free {
            return None;
        }
        let (auth, env) = match format {
            Format::Anthropic => ("anthropic", [("ANTHROPIC_BASE_URL", url.to_string()), ("ANTHROPIC_API_KEY", "dino".into()), ("ANTHROPIC_MODEL", model.into())]),
            Format::Chat => ("openai", [("OPENAI_BASE_URL", format!("{url}/v1")), ("OPENAI_API_KEY", "dino".into()), ("OPENAI_MODEL", model.into())]),
            Format::Responses => ("openai-responses", [("OPENAI_BASE_URL", format!("{url}/v1")), ("OPENAI_API_KEY", "dino".into()), ("OPENAI_MODEL", model.into())]),
        };
        Some((env.into_iter().map(|(k, v)| (k.to_string(), v)).collect(), strings(&["--auth-type", auth, "-m", model])))
    }

    fn model_args(&self, model: &str) -> Vec<String> {
        strings(&["-m", model])
    }

    // No effort to choose.
    fn effort_args(&self, _effort: &str) -> Vec<String> {
        vec![]
    }

    fn value_flags(&self) -> &'static [&'static str] {
        &["--approval-mode", "-m", "--model", "--auth-type"]
    }

    fn control_of(&self, name: &str, _value: Option<&str>) -> Option<ControlKind> {
        match name {
            "--approval-mode" | "-y" | "--yolo" => Some(ControlKind::Mode),
            "-m" | "--model" => Some(ControlKind::Model),
            _ => None,
        }
    }

    fn read_mode(&self, flags: &[(&str, Option<&str>)]) -> Option<String> {
        let &(name, value) = flags.last()?;
        if name == "--approval-mode" { value.and_then(|v| self.reported_mode(v)) } else { Some("bypass".into()) }
    }

    /// Its approval modes: the flag says `auto-edit`, its hooks `auto_edit`.
    fn reported_mode(&self, mode: &str) -> Option<String> {
        let id = match mode {
            "default" => "ask",
            "auto-edit" | "auto_edit" => "edits",
            "plan" => "plan",
            "auto" => "auto",
            "yolo" => "bypass",
            _ => return None,
        };
        Some(id.into())
    }

    // On its own, status only: which provider it talks to is its own setting. On the free tier it
    // talks to dino as the Anthropic API (through Anthropic's SDK, which adds `/v1/messages`); the
    // key is a placeholder, the proxy holds the real ones.
    fn wiring(&self, _route: bool, base: &dyn Fn(&str) -> String, _status_line: Option<String>) -> Wiring {
        let mut env: Vec<(String, String)> = hook_layer(&base("hook")).map(|p| ("QWEN_CODE_SYSTEM_DEFAULTS_PATH".into(), p.display().to_string())).into_iter().collect();
        if !self.free {
            return (env, vec![]);
        }
        env.extend([("ANTHROPIC_BASE_URL", base("free")), ("ANTHROPIC_API_KEY", "dino-free".into()), ("ANTHROPIC_MODEL", "auto".into())].map(|(k, v)| (k.to_string(), v)));
        (env, strings(&["--auth-type", "anthropic", "-m", "auto"]))
    }

    // A prompt on its own runs once and exits.
    fn prompt_args(&self, prompt: String) -> Vec<String> {
        vec!["-i".into(), prompt]
    }

    // The option `prompt_args` gives it with.
    fn launch_prompt(&self, args: &[String]) -> Option<(Vec<String>, String)> {
        super::option_prompt(args, &["-i", "--prompt-interactive"])
    }

    fn session_args(&self, session: &mut Option<String>, restoring: bool) -> (Vec<String>, Vec<String>) {
        let id = session.get_or_insert_with(crate::new_uuid).clone();
        // A conversation is only saved once it has a prompt; resuming one that isn't fails.
        let flag = if restoring && self.transcript(&id).is_some() { "--resume" } else { "--session-id" };
        (vec![], vec![flag.into(), id])
    }

    fn status_source(&self) -> StatusSource {
        StatusSource::Hooks
    }

    // `-p` (not `-i`, its TUI with a first prompt), its ACP server, `serve`.
    fn headless(&self, args: &[String]) -> bool {
        super::runs_with(args, &["-p", "--prompt", "--acp", "--experimental-acp", "--input-format"], &["serve", "mcp", "extensions"])
    }

    fn portable_flags(&self, args: &[String]) -> Vec<String> {
        found::drop_flags(
            args,
            &[
                "--resume", "-r", "--session-id", "-p", "--prompt", "-i", "--prompt-interactive", "-o", "--output-format", "--input-format",
                "--json-file", "--json-fd", "--input-file", "--worktree",
            ],
            &["--continue", "-c", "--fork-session", "--acp"],
        )
    }

    fn running(&self, procs: &crate::procinfo::Procs) -> Vec<FoundSession> {
        let mut out = vec![];
        for e in std::fs::read_dir(qwen_home().join("sessions")).into_iter().flatten().flatten() {
            let Some(pid) = e.path().file_stem().and_then(|s| s.to_str()?.parse::<u32>().ok()) else { continue };
            let Some(v) = live(pid) else { continue };
            let Some(sid) = v["sessionId"].as_str().filter(|s| !s.is_empty()) else { continue };
            if !found::started_before(procs.get(&pid), &v["startedAt"]) {
                continue;
            }
            let (terminal, args) = found::terminal_and_flags(self, pid);
            let updated = self.transcript(sid).map_or(0, |p| history::modified(&p));
            out.push(FoundSession {
                source: Source::Running,
                agent: "qwen".into(),
                session_id: sid.into(),
                title: self.title(sid, &v),
                cwd: v["cwd"].as_str().map(String::from),
                updated_at: if updated > 0 { updated } else { v["startedAt"].as_u64().map_or(0, |ms| ms / 1000) },
                pid: Some(pid),
                status: None,
                terminal,
                args,
                url: None,
                tmux: None,
            });
        }
        out
    }

    // Found by its process record: it runs as `node`.
    fn inside(&self, pid: u32, _comm: &str, args: &dyn Fn() -> Vec<String>) -> Option<FoundSession> {
        let v = live(pid)?;
        let mut s = found::by_hand("qwen", pid);
        s.session_id = v["sessionId"].as_str().unwrap_or_default().into();
        s.title = self.title(&s.session_id, &v);
        s.cwd = v["cwd"].as_str().map(String::from);
        s.updated_at = self.transcript(&s.session_id).map_or(0, |p| history::modified(&p));
        s.args = self.portable_flags(&args());
        Some(s)
    }

    fn recent(&self, running: &dyn Fn(&str) -> bool) -> Vec<FoundSession> {
        let mut out = vec![];
        for p in transcripts() {
            let Some(sid) = p.file_stem().and_then(|s| s.to_str()).map(String::from) else { continue };
            let meta = meta(&p);
            if meta.hidden || running(&sid) {
                continue;
            }
            let title = meta.title.unwrap_or_else(|| "Qwen Code session".into());
            out.push(history::recent("qwen", sid, title, meta.cwd, history::modified(&p)));
        }
        out
    }

    // No cloud work of its own.
    fn cloud_args(&self, _session_id: &str) -> Vec<String> {
        vec![]
    }

    fn transcript(&self, session_id: &str) -> Option<PathBuf> {
        if session_id.is_empty() {
            return None;
        }
        std::fs::read_dir(qwen_home().join("projects")).ok()?.flatten().map(|d| d.path().join("chats").join(format!("{session_id}.jsonl"))).find(|p| p.exists())
    }

    fn usage(&self, seen: &mut crate::usage::Seen) -> Vec<crate::usage::Used> {
        if self.free {
            return vec![];
        }
        transcripts().iter().filter_map(|p| seen.new_lines(p, b"\"usageMetadata\"")).flat_map(|(t, _)| usage_in(&t)).collect()
    }

    fn turns(&self, text: &str, _path: &Path, _start: u64) -> Vec<Turn> {
        turns_in(text)
    }
}

/// Its answers' usage, as Gemini counts it: the prompt includes what was cached, the answer
/// leaves its thoughts apart.
fn usage_in(jsonl: &str) -> Vec<crate::usage::Used> {
    let mut out = vec![];
    for v in jsonl.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
        let u = &v["usageMetadata"];
        let (Some(id), Some(conversation)) = (v["uuid"].as_str(), v["sessionId"].as_str()) else { continue };
        if v["type"] != "assistant" || !u.is_object() {
            continue;
        }
        let cached = history::count(&u["cachedContentTokenCount"]);
        out.push(crate::usage::Used { undated: false,
            id: id.into(),
            at_ms: history::ms_of(&v["timestamp"]).unwrap_or(0),
            conversation: conversation.into(),
            cwd: v["cwd"].as_str().map(String::from),
            model: v["model"].as_str().map(String::from),
            input: history::count(&u["promptTokenCount"]).saturating_sub(cached),
            cache_read: cached,
            cache_write: 0,
            output: history::count(&u["candidatesTokenCount"]) + history::count(&u["thoughtsTokenCount"]),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records the way Qwen Code 0.24 writes them (`ChatRecordingService`).
    const CHAT: &str = r#"{"uuid":"1","parentUuid":null,"sessionId":"s1","timestamp":"2026-09-29T10:00:00Z","type":"system","subtype":"session_source","cwd":"/r","version":"0.24.7","systemPayload":{"sourceType":"cli"}}
{"uuid":"2","parentUuid":"1","sessionId":"s1","timestamp":"2026-09-29T10:00:01Z","type":"user","provenance":"real_user","cwd":"/r","version":"0.24.7","message":{"role":"user","parts":[{"text":"add a dark mode\nto the settings"}]}}
{"uuid":"3","parentUuid":"2","sessionId":"s1","timestamp":"2026-09-29T10:00:03Z","type":"assistant","cwd":"/r","version":"0.24.7","model":"qwen3-coder-plus","message":{"role":"model","parts":[{"text":"thinking about it","thought":true},{"text":"Let me look."},{"functionCall":{"name":"read_file","args":{"file_path":"/r/settings.ts"}}}]},"usageMetadata":{"promptTokenCount":1200,"candidatesTokenCount":40,"totalTokenCount":1240}}
{"uuid":"4","parentUuid":"3","sessionId":"s1","timestamp":"2026-09-29T10:00:04Z","type":"tool_result","cwd":"/r","version":"0.24.7","message":{"role":"user","parts":[{"functionResponse":{"name":"read_file","response":{"output":"…"}}}]}}
{"uuid":"5","parentUuid":"4","sessionId":"s1","timestamp":"2026-09-29T10:00:06Z","type":"assistant","cwd":"/r","version":"0.24.7","model":"qwen3-coder-plus","message":{"role":"model","parts":[{"text":"Done: a toggle in Settings."}]}}
{"uuid":"6","parentUuid":"5","sessionId":"s1","timestamp":"2026-09-29T10:00:07Z","type":"user","subtype":"goal_runtime","provenance":"goal_runtime","cwd":"/r","version":"0.24.7","message":{"role":"user","parts":[{"text":"goal check"}]}}"#;

    #[test]
    fn a_conversation_reads_as_turns() {
        let turns: Vec<(String, String)> = turns_in(CHAT).into_iter().map(|t| (t.role, t.text)).collect();
        let want = [
            ("user", "add a dark mode\nto the settings"),
            ("assistant", "Let me look."),
            ("tool", "read_file /r/settings.ts"),
            ("assistant", "Done: a toggle in Settings."),
        ];
        assert_eq!(turns, want.map(|(r, t)| (r.to_string(), t.to_string())));
    }

    #[test]
    fn titled_by_its_title_else_the_first_prompt() {
        assert_eq!(meta_in(CHAT), Meta { title: Some("add a dark mode".into()), cwd: Some("/r".into()), hidden: false });
        let renamed = format!("{CHAT}\n{}", r#"{"type":"system","subtype":"custom_title","cwd":"/r","systemPayload":{"customTitle":"Dark mode","titleSource":"auto"}}"#);
        assert_eq!(meta_in(&renamed).title.as_deref(), Some("Dark mode"));
        let empty = r#"{"type":"system","subtype":"session_source","cwd":"/r","systemPayload":{}}"#;
        assert!(meta_in(empty).hidden, "never got a prompt");
    }

    #[test]
    fn modes_both_ways() {
        let q = Qwen { free: false };
        for m in q.modes() {
            let args = q.mode_args(m);
            assert_eq!(q.read_mode(&[(args[0].as_str(), Some(args[1].as_str()))]).as_deref(), Some(*m));
        }
        assert_eq!(q.read_mode(&[("-y", None)]).as_deref(), Some("bypass"));
        assert_eq!(q.reported_mode("auto_edit").as_deref(), Some("edits"), "as its hooks say it");
        assert_eq!(q.reported_mode("dontAsk"), None);
    }

    #[test]
    fn hooks_join_the_users_own_defaults() {
        let theirs = json!({"model": {"name": "x"}, "hooks": {"Stop": [{"hooks": [{"type": "command", "command": "say done"}]}]}});
        let ours: Value = serde_json::from_str(&crate::claude_hook_settings("http://127.0.0.1:1/s/7/hook", None)).unwrap();
        let merged = with_hooks(theirs, &ours);
        assert_eq!(merged["model"]["name"], "x", "the rest is kept");
        let stop = merged["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 2);
        assert_eq!(stop[0]["hooks"][0]["command"], "say done", "theirs first");
        assert_eq!(stop[1]["hooks"][0]["url"], "http://127.0.0.1:1/s/7/hook");
        assert!(merged["hooks"]["SessionStart"].is_array());
        assert!(with_hooks(Value::Null, &ours)["hooks"]["PermissionRequest"].is_array());
    }

    #[test]
    fn continuing_drops_what_picks_the_session() {
        let args: Vec<String> = ["--resume", "abc", "-m", "qwen3-coder-plus", "--approval-mode", "plan", "-i", "hi", "--continue"].iter().map(|s| s.to_string()).collect();
        assert_eq!(Qwen { free: false }.portable_flags(&args), ["-m", "qwen3-coder-plus", "--approval-mode", "plan"]);
    }

    #[test]
    fn its_answers_usage_counts_cached_input_apart() {
        let jsonl = r#"{"uuid":"3","sessionId":"s1","timestamp":"2026-09-29T10:00:03Z","type":"assistant","cwd":"/r","model":"qwen3-coder-plus","message":{"role":"model","parts":[]},"usageMetadata":{"promptTokenCount":1200,"candidatesTokenCount":40,"thoughtsTokenCount":5,"cachedContentTokenCount":200,"totalTokenCount":1245}}"#;
        let used = usage_in(jsonl);
        assert_eq!(used.len(), 1);
        assert_eq!((used[0].id.as_str(), used[0].conversation.as_str(), used[0].model.as_deref()), ("3", "s1", Some("qwen3-coder-plus")));
        assert_eq!((used[0].input, used[0].cache_read, used[0].output), (1000, 200, 45));
    }
}
