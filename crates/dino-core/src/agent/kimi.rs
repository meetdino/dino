//! Kimi Code (Moonshot's `kimi`), and Kimi Code on dino's free tier (`free`), which it reaches as
//! an OpenAI-compatible provider given in its environment (`KIMI_MODEL_*`: a model of its own for
//! that process; its config is left alone). It keeps each conversation in
//! `~/.kimi-code/sessions/<workspace>/<session>/`: `state.json`, and `agents/main/wire.jsonl`, the
//! record of everything that happens in it, which dino follows for status. `session_index.jsonl`
//! says which folder each is for. It can't be given a conversation id up front, so dino claims the
//! first one it starts; its process shows as `kimi-code`.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{Agent, ControlKind, LogEvent, StatusSource, Wiring, strings};
use crate::found::{self, FoundSession, Source};
use crate::history::{self, Turn, one_line, turn};
use crate::models::{Catalog, ModelInfo};

pub(crate) struct Kimi {
    pub(crate) free: bool,
}

/// Where Kimi keeps its own files: `$KIMI_CODE_HOME`, else `~/.kimi-code`.
fn kimi_home() -> PathBuf {
    std::env::var_os("KIMI_CODE_HOME").map(PathBuf::from).unwrap_or_else(|| std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(".kimi-code"))
}

/// A conversation as its index lists it.
struct Entry {
    id: String,
    dir: PathBuf,
    work_dir: String,
}

/// Every conversation, the latest entry for each.
fn index() -> Vec<Entry> {
    let text = std::fs::read_to_string(kimi_home().join("session_index.jsonl")).unwrap_or_default();
    let mut out: Vec<Entry> = vec![];
    for v in text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
        let (Some(id), Some(dir), Some(work_dir)) = (v["sessionId"].as_str(), v["sessionDir"].as_str(), v["workDir"].as_str()) else { continue };
        out.retain(|e| e.id != id);
        out.push(Entry { id: id.into(), dir: dir.into(), work_dir: work_dir.into() });
    }
    out.retain(|e| e.dir.is_dir());
    out
}

fn state(dir: &Path) -> Value {
    std::fs::read_to_string(dir.join("state.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null)
}

/// Seconds, from its milliseconds.
fn secs(v: &Value) -> u64 {
    v.as_u64().unwrap_or(0) / 1000
}

/// As Kimi shows it: its title (given, or its first prompt), else the latest prompt.
fn title_of(state: &Value) -> Option<String> {
    state["title"].as_str().and_then(one_line).or_else(|| state["lastPrompt"].as_str().and_then(one_line))
}

/// Folders compared as the same folder, whatever links lead to them.
fn same_dir(a: &str, b: &Path) -> bool {
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    canon(Path::new(a)) == canon(b)
}

/// A conversation's record as turns: what the person typed, what it answered and the tools it ran.
fn turns_in(jsonl: &str) -> Vec<Turn> {
    let mut out = vec![];
    for v in jsonl.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
        match v["type"].as_str() {
            Some("turn.prompt") if v["origin"]["kind"] == "user" => {
                let text: Vec<&str> = v["input"].as_array().into_iter().flatten().filter_map(|p| p["text"].as_str()).collect();
                if let Some(t) = history::typed(&text.join("\n")) {
                    out.push(turn("user", t));
                }
            }
            Some("agent.message.appended") if v["message"]["message"]["role"] == "assistant" => {
                let m = &v["message"]["message"];
                let text: Vec<&str> = m["content"].as_array().into_iter().flatten().filter(|p| p["type"] == "text").filter_map(|p| p["text"].as_str()).collect();
                let text = text.join("\n");
                if !text.trim().is_empty() {
                    out.push(turn("assistant", text.trim()));
                }
                for call in m["toolCalls"].as_array().into_iter().flatten() {
                    let args = call["arguments"].as_str().and_then(|a| serde_json::from_str::<Value>(a).ok()).unwrap_or(Value::Null);
                    out.push(turn("tool", format!("{}{}", call["name"].as_str().unwrap_or("tool"), history::hint(&args))));
                }
            }
            _ => {}
        }
    }
    out
}

/// What one line of its record says about its turn.
fn event_of(v: &Value) -> LogEvent {
    match v["type"].as_str().unwrap_or_default() {
        "turn.prompt" | "agent.turn.started" => LogEvent::TurnStarted,
        "turn.ended" | "agent.turn.ended" | "prompt.completed" => LogEvent::TurnEnded,
        "interaction.request" => {
            let r = &v["request"];
            let what = r["action"].as_str().or(r["display"]["command"].as_str()).or(r["toolName"].as_str()).unwrap_or("an answer");
            LogEvent::Needs(what.to_string())
        }
        "usage.record" | "token_counting.measured" | "token_counting.turn_recorded" | "llm.request" | "llm.tools_snapshot" | "metadata" => LogEvent::Bookkeeping,
        _ => LogEvent::Other,
    }
}

/// Whether the latest turn in a record is still going.
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

/// Its model aliases, from its own config: `[models.<alias>]` and `default_model`.
fn catalog_in(config: &str) -> Option<Catalog> {
    let v: toml::Value = toml::from_str(config).ok()?;
    let models: Vec<ModelInfo> = v
        .get("models")?
        .as_table()?
        .iter()
        .map(|(alias, m)| ModelInfo {
            id: alias.clone(),
            label: m.get("display_name").and_then(|n| n.as_str()).unwrap_or(alias).to_string(),
            efforts: vec![],
            default_effort: None,
            group: None,
            aliases: vec![],
        })
        .collect();
    (!models.is_empty()).then(|| Catalog { models, default_model: v.get("default_model").and_then(|d| d.as_str()).map(String::from) })
}

/// The conversation a live process is on: the one it began (created in its folder since it
/// started). Nothing before then, and nothing for one it continued: its process title overwrites
/// its arguments, so which one can't be told from another Kimi's in the same folder.
fn conversation_in(pid: u32) -> Option<(Entry, Value)> {
    let cwd = crate::procinfo::cwd_of(pid)?;
    let since = crate::procinfo::started(pid).unwrap_or(0);
    index()
        .into_iter()
        .filter(|e| same_dir(&e.work_dir, Path::new(&cwd)))
        .map(|e| {
            let st = state(&e.dir);
            (e, st)
        })
        .filter(|(_, st)| secs(&st["createdAt"]) + 1 >= since)
        .min_by_key(|(_, st)| st["createdAt"].as_u64().unwrap_or(0))
}

/// The conversation ran on dino's free tier: its record says its requests went to the model its
/// environment gave it, named as dino names the free tier's. (Its process can't say: Kimi's
/// process title overwrites its environment.)
fn on_free_tier(dir: &Path) -> bool {
    let text = std::fs::read_to_string(dir.join("agents/main/wire.jsonl")).unwrap_or_default();
    text.lines()
        .filter(|l| l.contains("\"llm.request\""))
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .next_back()
        .is_some_and(|v| v["modelAlias"] == "__kimi_env_model__" && v["model"] == "auto")
}

impl Kimi {
    fn found(&self, pid: u32, source: Source) -> FoundSession {
        let mut s = found::by_hand("kimi", pid);
        s.source = source;
        s.cwd = crate::procinfo::cwd_of(pid);
        if let Some((e, st)) = conversation_in(pid) {
            if on_free_tier(&e.dir) {
                s.agent = "kimi-free".into();
            }
            s.session_id = e.id;
            s.title = title_of(&st).unwrap_or_else(|| "Kimi Code".into());
            s.updated_at = secs(&st["updatedAt"]);
        } else {
            s.title = "Kimi Code".into();
        }
        s
    }
}

impl Agent for Kimi {
    fn id(&self) -> &'static str {
        if self.free { "kimi-free" } else { "kimi" }
    }

    // Its own words: plan mode, "Ask When Needed" (`--yolo`), "Never Ask" (`--auto`).
    fn modes(&self) -> &'static [&'static str] {
        &["ask", "plan", "auto", "bypass"]
    }

    fn mode_args(&self, mode: &str) -> Vec<String> {
        match mode {
            "plan" => strings(&["--plan"]),
            "auto" => strings(&["--yolo"]),
            "bypass" => strings(&["--auto"]),
            // It asks by default.
            _ => vec![],
        }
    }

    // The free tier picks the model for each turn.
    fn picks_model(&self) -> bool {
        !self.free
    }

    fn free(&self) -> bool {
        self.free
    }

    fn model_args(&self, model: &str) -> Vec<String> {
        strings(&["-m", model])
    }

    // Thinking is set in its config, not on its command line.
    fn effort_args(&self, _effort: &str) -> Vec<String> {
        vec![]
    }

    fn value_flags(&self) -> &'static [&'static str] {
        &["-m", "--model", "-S", "--session", "-p", "--prompt", "--output-format", "--agent", "--agent-file", "--add-dir", "--skills-dir"]
    }

    fn control_of(&self, name: &str, _value: Option<&str>) -> Option<ControlKind> {
        match name {
            "--plan" | "-y" | "--yolo" | "--auto" => Some(ControlKind::Mode),
            "-m" | "--model" => Some(ControlKind::Model),
            _ => None,
        }
    }

    fn read_mode(&self, flags: &[(&str, Option<&str>)]) -> Option<String> {
        let mode = match flags.last()?.0 {
            "--plan" => "plan",
            "-y" | "--yolo" => "auto",
            _ => "bypass",
        };
        Some(mode.into())
    }

    fn catalog_sources(&self) -> Vec<PathBuf> {
        vec![kimi_home().join("config.toml")]
    }

    fn catalog(&self, _program: &str) -> Option<Catalog> {
        if self.free {
            return None;
        }
        catalog_in(&std::fs::read_to_string(kimi_home().join("config.toml")).ok()?)
    }

    // On its own, nothing: which provider it talks to is its own setting, and dino follows its
    // record for status. On the free tier, a model of its own for this process, talking to dino
    // as OpenAI's chat API; the key is a placeholder, the proxy holds the real ones.
    fn wiring(&self, _route: bool, base: &dyn Fn(&str) -> String, _status_line: Option<String>) -> Wiring {
        if !self.free {
            return (vec![], vec![]);
        }
        let env = [
            ("KIMI_MODEL_NAME", "auto".to_string()),
            ("KIMI_MODEL_API_KEY", "dino-free".into()),
            ("KIMI_MODEL_PROVIDER_TYPE", "openai".into()),
            ("KIMI_MODEL_BASE_URL", format!("{}/v1", base("free"))),
        ];
        (env.map(|(k, v)| (k.to_string(), v)).into(), vec![])
    }

    // It takes no prompt to start on (only `-p`, which runs once and exits).
    fn prompt_args(&self, _prompt: String) -> Vec<String> {
        vec![]
    }

    fn session_args(&self, session: &mut Option<String>, restoring: bool) -> (Vec<String>, Vec<String>) {
        match session {
            Some(id) if restoring => (vec![], vec!["-S".into(), id.clone()]),
            _ => (vec![], vec![]),
        }
    }

    fn status_source(&self) -> StatusSource {
        StatusSource::Log
    }

    fn log_path(&self, session: &str) -> Option<PathBuf> {
        let e = index().into_iter().find(|e| e.id == session)?;
        Some(e.dir.join("agents/main/wire.jsonl"))
    }

    fn log_event(&self, line: &Value) -> LogEvent {
        event_of(line)
    }

    fn new_conversation(&self, cwd: &Path, since: u64, claimed: &[String]) -> Option<String> {
        index()
            .into_iter()
            .filter(|e| same_dir(&e.work_dir, cwd) && !claimed.contains(&e.id))
            .map(|e| {
                let created = secs(&state(&e.dir)["createdAt"]);
                (e, created)
            })
            .filter(|(_, created)| *created + 1 >= since)
            .min_by_key(|(_, created)| *created)
            .map(|(e, _)| e.id)
    }

    fn busy(&self, pid: u32) -> Option<bool> {
        let (e, _) = conversation_in(pid)?;
        Some(busy_in(&std::fs::read_to_string(e.dir.join("agents/main/wire.jsonl")).ok()?))
    }

    fn portable_flags(&self, args: &[String]) -> Vec<String> {
        found::drop_flags(args, &["-S", "--session", "-p", "--prompt", "--output-format", "--agent", "--agent-file"], &["-c", "--continue"])
    }

    fn may_be(&self, comm: &str) -> bool {
        comm.rsplit('/').next() == Some("kimi-code")
    }

    fn running(&self) -> Vec<FoundSession> {
        let mut out = vec![];
        for pid in crate::procinfo::pids_named("kimi-code") {
            let mut s = self.found(pid, Source::Running);
            if s.session_id.is_empty() {
                continue;
            }
            let (terminal, args) = found::terminal_and_flags(self, pid);
            s.terminal = terminal;
            s.args = args;
            s.status = self.busy(pid).map(|b| if b { "busy" } else { "idle" }.into());
            out.push(s);
        }
        out
    }

    fn inside(&self, pid: u32, comm: &str, args: &dyn Fn() -> Vec<String>) -> Option<FoundSession> {
        if !self.may_be(comm) || self.free {
            return None;
        }
        let mut s = self.found(pid, Source::Running);
        s.args = self.portable_flags(&args());
        s.status = self.busy(pid).map(|b| if b { "busy" } else { "idle" }.into());
        Some(s)
    }

    fn recent(&self, running: &dyn Fn(&str) -> bool) -> Vec<FoundSession> {
        if self.free {
            return vec![];
        }
        let mut out = vec![];
        for e in index() {
            if running(&e.id) {
                continue;
            }
            let st = state(&e.dir);
            if st["archived"] == true {
                continue;
            }
            let Some(title) = title_of(&st) else { continue };
            out.push(history::recent("kimi", e.id, title, st["cwd"].as_str().map(String::from).or(Some(e.work_dir)), secs(&st["updatedAt"])));
        }
        out
    }

    // No cloud work of its own.
    fn cloud_args(&self, _session_id: &str) -> Vec<String> {
        vec![]
    }

    fn transcript(&self, session_id: &str) -> Option<PathBuf> {
        self.log_path(session_id).filter(|p| p.exists())
    }

    fn turns(&self, text: &str, _path: &Path, _start: u64) -> Vec<Turn> {
        turns_in(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed from Kimi Code 2.1.1's own record of a session: a prompt, an answer, then a prompt
    /// that ran a command after asking.
    const WIRE: &str = r#"{"type":"metadata","protocol_version":"1.5","created_at":1790739703474}
{"type":"permission.set_mode","agentId":"main","mode":"manual","time":1790739703509}
{"type":"turn.prompt","agentId":"main","input":[{"type":"text","text":"Reply with only the word PELICAN."}],"origin":{"kind":"user"},"promptId":"m1","turnId":0,"time":1790739703532}
{"type":"context.append_message","agentId":"main","message":{"role":"user","content":[{"type":"text","text":"<system-reminder>date</system-reminder>"}],"origin":{"kind":"injection"}},"time":1790739703558}
{"turnId":0,"type":"agent.turn.started","time":1790739703525,"kind":"event"}
{"type":"usage.record","agentId":"main","usage":{"inputOther":19360,"output":3},"time":1790739705969}
{"message":{"message":{"role":"assistant","content":[{"type":"text","text":"PELICAN"}],"toolCalls":[]},"meta":{"source":"llm"}},"type":"agent.message.appended","time":1790739705973,"kind":"event"}
{"turnId":0,"outcome":"done","type":"agent.turn.ended","time":1790739705973,"kind":"event"}
{"type":"turn.ended","agentId":"main","turnId":0,"reason":"completed","time":1790739705975}
{"type":"turn.prompt","agentId":"main","input":[{"type":"text","text":"List the files here with your shell tool, then say DONE."}],"origin":{"kind":"user"},"promptId":"m2","turnId":1,"time":1790740051671}
{"turnId":1,"type":"agent.turn.started","time":1790740051670,"kind":"event"}
{"type":"interaction.request","agentId":"main","id":"approval_1","kind":"approval","request":{"toolName":"Bash","action":"Running: ls -la","display":{"kind":"command","command":"ls -la"}},"time":1790740055000}
{"type":"interaction.resolved","agentId":"main","id":"approval_1","response":{"decision":"approved"},"time":1790740055187}
{"message":{"message":{"role":"assistant","content":[],"toolCalls":[{"type":"function","id":"t1","name":"Bash","arguments":"{\"command\":\"ls -la\",\"description\":\"List files\"}"}]},"meta":{"source":"llm"}},"type":"agent.message.appended","time":1790740055538,"kind":"event"}
{"message":{"message":{"role":"tool","content":[{"type":"text","text":"total 0"}],"toolCallId":"t1"},"meta":{"source":"tool"}},"type":"agent.message.appended","time":1790740055538,"kind":"event"}
{"message":{"message":{"role":"assistant","content":[{"type":"text","text":"DONE"}],"toolCalls":[]},"meta":{"source":"llm"}},"type":"agent.message.appended","time":1790740055538,"kind":"event"}
{"turnId":1,"outcome":"done","type":"agent.turn.ended","time":1790740055538,"kind":"event"}
{"type":"turn.ended","agentId":"main","turnId":1,"reason":"completed","time":1790740055539}"#;

    #[test]
    fn its_record_reads_as_turns() {
        let turns: Vec<(String, String)> = turns_in(WIRE).into_iter().map(|t| (t.role, t.text)).collect();
        let want = [
            ("user", "Reply with only the word PELICAN."),
            ("assistant", "PELICAN"),
            ("user", "List the files here with your shell tool, then say DONE."),
            ("tool", "Bash ls -la"),
            ("assistant", "DONE"),
        ];
        assert_eq!(turns, want.map(|(r, t)| (r.to_string(), t.to_string())));
    }

    #[test]
    fn its_record_says_where_the_turn_is() {
        let events: Vec<LogEvent> = WIRE.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).map(|v| event_of(&v)).collect();
        assert!(events.contains(&LogEvent::Needs("Running: ls -la".into())));
        let needs = events.iter().position(|e| matches!(e, LogEvent::Needs(_))).unwrap();
        assert_eq!(events[needs + 1], LogEvent::Other, "answering is something it does");
        assert!(!busy_in(WIRE));
        let (cut, _) = WIRE.split_once(r#"{"type":"interaction.resolved""#).unwrap();
        assert!(busy_in(cut), "cut mid-turn");
    }

    #[test]
    fn modes_both_ways() {
        let k = Kimi { free: false };
        assert!(k.mode_args("ask").is_empty(), "it asks unless told otherwise");
        for m in ["plan", "auto", "bypass"] {
            let args = k.mode_args(m);
            assert_eq!(k.read_mode(&[(args[0].as_str(), None)]).as_deref(), Some(m));
        }
        assert_eq!(k.read_mode(&[("-y", None)]).as_deref(), Some("auto"));
    }

    #[test]
    fn models_from_its_config() {
        let c = catalog_in("default_model = \"k3\"\n[providers.moonshot]\ntype = \"kimi\"\n[models.k3]\nprovider = \"moonshot\"\nmodel = \"kimi-k3\"\ndisplay_name = \"Kimi K3\"\n[models.fast]\nprovider = \"moonshot\"\nmodel = \"kimi-k3-turbo\"\n").unwrap();
        assert_eq!(c.default_model.as_deref(), Some("k3"));
        let ids: Vec<(&str, &str)> = c.models.iter().map(|m| (m.id.as_str(), m.label.as_str())).collect();
        assert!(ids.contains(&("k3", "Kimi K3")) && ids.contains(&("fast", "fast")));
        assert!(catalog_in("# empty\n").is_none(), "no models: typed by hand");
    }

    #[test]
    fn continuing_drops_what_picks_the_session() {
        let args: Vec<String> = ["-S", "session_x", "-m", "k3", "--plan", "-c"].iter().map(|s| s.to_string()).collect();
        assert_eq!(Kimi { free: false }.portable_flags(&args), ["-m", "k3", "--plan"]);
    }

    #[test]
    fn the_free_tier_is_a_model_of_its_own() {
        let (env, args) = Kimi { free: true }.wiring(true, &|p| format!("http://127.0.0.1:9/s/3/{p}"), None);
        assert!(args.is_empty());
        let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        assert_eq!(get("KIMI_MODEL_BASE_URL"), Some("http://127.0.0.1:9/s/3/free/v1"));
        assert_eq!(get("KIMI_MODEL_PROVIDER_TYPE"), Some("openai"));
        assert!(Kimi { free: false }.wiring(true, &|p| p.into(), None).0.is_empty(), "its own provider, untouched");
    }
}
