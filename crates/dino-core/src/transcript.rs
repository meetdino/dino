//! A session's conversation as plain text, for other agents to read (`dino mcp`'s `read_session`).

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::ipc::TurnInfo;

/// How much of the end of a transcript file is read; enough for the last several turns.
const READ_TAIL: u64 = 2 << 20;

/// Claude's transcript for conversation `uuid`: `~/.claude/projects/<dir>/<uuid>.jsonl`.
pub fn claude_path(uuid: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    std::fs::read_dir(home.join(".claude/projects")).ok()?.flatten().map(|d| d.path().join(format!("{uuid}.jsonl"))).find(|p| p.exists())
}

/// The last turns of Claude conversation `uuid`, at most `budget` characters.
pub fn claude_tail(uuid: &str, budget: usize) -> Option<String> {
    let text = read_tail(&claude_path(uuid)?)?;
    Some(render(&text, budget)).filter(|t| !t.is_empty())
}

fn read_tail(p: &Path) -> Option<String> {
    read_last(p, READ_TAIL)
}

/// The whole lines in the last `bytes` of `p`.
fn read_last(p: &Path, bytes: u64) -> Option<String> {
    let mut f = std::fs::File::open(p).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(bytes))).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    // Started mid-file: the first line is a fragment.
    Some(if len > bytes { text.split_once('\n').map_or(String::new(), |(_, rest)| rest.to_string()) } else { text })
}

/// Codex's rollout for conversation `id`: `~/.codex/sessions/Y/M/D/rollout-<time>-<id>.jsonl`.
pub fn codex_path(id: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let suffix = format!("-{id}.jsonl");
    let mut stack = vec![home.join(".codex/sessions")];
    while let Some(dir) = stack.pop() {
        for p in std::fs::read_dir(&dir).into_iter().flatten().flatten().map(|e| e.path()) {
            if p.is_dir() {
                stack.push(p);
            } else if p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("rollout-") && n.ends_with(&suffix)) {
                return Some(p);
            }
        }
    }
    None
}

/// How full Codex's context window is, as its rollout last said: (tokens in it, its size).
pub fn codex_context(rollout: &Path) -> Option<(u64, u64)> {
    // A turn writes other events after its token count, but not this many bytes of them.
    codex_context_in(&read_last(rollout, 256 << 10)?)
}

/// From the last `token_count` event: `info.model_context_window`, and the tokens of the last
/// call (`last_token_usage.total_tokens`, what Codex itself counts against the window).
fn codex_context_in(jsonl: &str) -> Option<(u64, u64)> {
    jsonl.lines().rev().filter(|l| l.contains("\"token_count\"")).find_map(|line| {
        let v = serde_json::from_str::<Value>(line).ok()?;
        let info = &v["payload"]["info"];
        let window = info["model_context_window"].as_u64().filter(|w| *w > 0)?;
        Some((info["last_token_usage"]["total_tokens"].as_u64().unwrap_or(0), window))
    })
}

/// The transcript of Claude subagent `id`: `~/.claude/projects/<dir>/<parent>/subagents/agent-<id>.jsonl`,
/// looked for under every conversation when the parent's `uuid` isn't known.
pub fn claude_subagent_path(parent: Option<&str>, id: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let file = format!("agent-{id}.jsonl");
    let projects = std::fs::read_dir(home.join(".claude/projects")).ok()?.flatten().map(|d| d.path());
    projects
        .flat_map(|project| match parent {
            Some(uuid) => vec![project.join(uuid)],
            None => std::fs::read_dir(&project).into_iter().flatten().flatten().map(|d| d.path()).collect(),
        })
        .map(|conversation| conversation.join("subagents").join(&file))
        .find(|p| p.exists())
}

/// Characters of a conversation `claude_turns` gives, the newest kept.
const TURNS_BUDGET: usize = 200_000;
/// How much of the end of a transcript is read: screenshots take most of the bytes.
const TURNS_TAIL: u64 = 8 << 20;
/// Longer lines are tool results carrying images or files, never something shown.
const LONGEST_SHOWN_LINE: usize = 256 << 10;

/// A Claude conversation (a session's or a subagent's), oldest first: its task (from the start
/// of the file, however long it got), then as much of the end as fits.
pub fn claude_turns(path: &Path) -> Option<Vec<TurnInfo>> {
    let whole = std::fs::metadata(path).ok()?.len() <= TURNS_TAIL;
    let mut all = turns(&read_last(path, TURNS_TAIL)?, whole);
    let task = if whole {
        all.first().is_some_and(|t| t.role == "task").then(|| all.remove(0))
    } else {
        turns(&read_first(path, 1 << 20).unwrap_or_default(), true).into_iter().find(|t| t.role == "task")
    };
    Some(fit(task, all, !whole, TURNS_BUDGET))
}

/// The task, then the newest of `rest` that fit in `budget` characters. `cut`: turns before
/// `rest` were already left out.
fn fit(task: Option<TurnInfo>, rest: Vec<TurnInfo>, cut: bool, budget: usize) -> Vec<TurnInfo> {
    let mut used = 0;
    let kept = rest.iter().rev().take_while(|t| {
        used += t.text.len();
        used <= budget
    });
    let from = rest.len() - kept.count();
    let gap = (cut || from > 0).then(|| TurnInfo { role: "note".into(), text: "Earlier turns left out".into() });
    task.into_iter().chain(gap).chain(rest.into_iter().skip(from)).collect()
}

/// The whole lines in the first `bytes` of `p`.
fn read_first(p: &Path, bytes: u64) -> Option<String> {
    let mut buf = Vec::new();
    std::fs::File::open(p).ok()?.take(bytes).read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    Some(text.rsplit_once('\n').map_or(String::new(), |(whole, _)| whole.to_string()))
}

/// A subagent's transcript as turns. Its first message is its task (a fork's comes after the
/// boilerplate that says it's a fork); what Claude Code adds on its side (reminders, notes about
/// its setup, screenshots' captions, tool results) is left out. `from_start`: `jsonl` is where
/// the file begins, not somewhere past the task.
fn turns(jsonl: &str, from_start: bool) -> Vec<TurnInfo> {
    let mut out: Vec<TurnInfo> = Vec::new();
    let mut tasked = !from_start;
    let turn = |role: &str, text: &str| TurnInfo { role: role.into(), text: text.trim().to_string() };
    for line in jsonl.lines().filter(|l| l.len() <= LONGEST_SHOWN_LINE) {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if v["isMeta"] == true {
            continue;
        }
        let content = &v["message"]["content"];
        match v["type"].as_str() {
            Some("user") => {
                let blocks = content.as_array().into_iter().flatten().filter(|b| b["type"] == "text");
                for text in content.as_str().into_iter().chain(blocks.filter_map(|b| b["text"].as_str())) {
                    let text = text.trim();
                    if let Some((_, rest)) = text.split_once("</fork-boilerplate>") {
                        let rest = rest.trim();
                        out.push(turn("task", rest.strip_prefix("Your directive:").unwrap_or(rest)));
                        tasked = true;
                    } else if text.starts_with("This session is being continued") {
                        out.push(turn("note", "Its context was compacted"));
                    } else if text.starts_with("[Request interrupted") {
                        out.push(turn("note", "Interrupted"));
                    } else if let Some(msg) = text.strip_prefix("The coordinator sent a message while you were working:") {
                        out.push(turn("user", msg));
                    } else if !(text.is_empty()
                        || text.starts_with('<')
                        || text.starts_with("[Image")
                        || text.starts_with("You've inherited the conversation context")
                        || text.starts_with("Your response above was cut off"))
                    {
                        out.push(turn(if tasked { "user" } else { "task" }, text));
                        tasked = true;
                    }
                }
            }
            // Before its task, a fork's transcript repeats the call that started it.
            Some("assistant") if tasked => {
                for block in content.as_array().into_iter().flatten() {
                    match block["type"].as_str() {
                        Some("text") => out.push(turn("agent", block["text"].as_str().unwrap_or(""))),
                        Some("tool_use") => {
                            let name = block["name"].as_str().unwrap_or("tool");
                            out.push(turn("tool", &format!("{name}{}", tool_hint(&block["input"]))));
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    out.retain(|t| !t.text.is_empty());
    out
}

/// Transcript JSONL as "User: …" / "Claude: …" lines, tool calls in brackets, newest last,
/// keeping whole entries from the end up to `budget` characters.
pub fn render(jsonl: &str, budget: usize) -> String {
    let mut entries: Vec<String> = Vec::new();
    let mut used = 0;
    for line in jsonl.lines().rev() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let Some(entry) = entry(&v) else { continue };
        used += entry.len() + 1;
        if used > budget && !entries.is_empty() {
            break;
        }
        entries.push(entry);
    }
    entries.reverse();
    let mut out = entries.join("\n");
    if out.len() > budget {
        let cut = out.char_indices().map(|(i, _)| i).find(|&i| i >= out.len() - budget).unwrap_or(0);
        out = format!("…{}", &out[cut..]);
    }
    out
}

fn entry(v: &Value) -> Option<String> {
    // Subagents' own turns and Claude Code's bookkeeping aren't the conversation.
    if v["isSidechain"] == true || v["isMeta"] == true {
        return None;
    }
    let who = match v["type"].as_str()? {
        "user" => "User",
        "assistant" => "Claude",
        _ => return None,
    };
    let content = &v["message"]["content"];
    let mut parts: Vec<String> = Vec::new();
    if let Some(s) = content.as_str() {
        parts.push(s.trim().to_string());
    }
    for block in content.as_array().into_iter().flatten() {
        match block["type"].as_str() {
            Some("text") => parts.push(block["text"].as_str().unwrap_or("").trim().to_string()),
            Some("tool_use") => parts.push(format!("[{}{}]", block["name"].as_str().unwrap_or("tool"), tool_hint(&block["input"]))),
            _ => {}
        }
    }
    parts.retain(|p| !p.is_empty() && !p.starts_with("<command-") && !p.starts_with("<local-command-"));
    (!parts.is_empty()).then(|| format!("{who}: {}", parts.join("\n")))
}

/// The telling argument of a tool call, shortened: the command, the file, the pattern.
fn tool_hint(input: &Value) -> String {
    let arg = ["command", "file_path", "pattern", "description", "url", "prompt"].iter().find_map(|k| input[*k].as_str());
    match arg {
        Some(a) => {
            let a = a.lines().next().unwrap_or("");
            let short: String = a.chars().take(100).collect();
            format!(" {short}{}", if short.len() < a.len() { "…" } else { "" })
        }
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_conversation_reads_as_turns() {
        let jsonl = [
            r#"{"type":"summary","summary":"x"}"#,
            r#"{"type":"user","message":{"role":"user","content":"fix the build"}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Looking."},{"type":"tool_use","name":"Bash","input":{"command":"cargo build\nmore"}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"ok"}]}}"#,
            r#"{"type":"assistant","isSidechain":true,"message":{"content":[{"type":"text","text":"subagent"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Fixed."}]}}"#,
        ]
        .join("\n");
        assert_eq!(render(&jsonl, 10_000), "User: fix the build\nClaude: Looking.\n[Bash cargo build]\nClaude: Fixed.");
        // Over budget, the newest entries stay.
        assert_eq!(render(&jsonl, 20), "Claude: Fixed.");
        assert!(render(&jsonl, 5).starts_with('…'));
    }

    #[test]
    fn a_subagents_conversation() {
        let fork = [
            r#"{"type":"fork-context-ref","agentId":"a1"}"#,
            r#"{"type":"assistant","isSidechain":true,"message":{"content":[{"type":"tool_use","name":"Agent","input":{"description":"Polish"}}]}}"#,
            r#"{"type":"user","isSidechain":true,"message":{"content":[{"type":"tool_result","content":"started"},{"type":"text","text":"<fork-boilerplate>\nYou are a fork.\n</fork-boilerplate>\n\nYour directive: Fix the sidebar."}]}}"#,
            r#"{"type":"user","isSidechain":true,"message":{"content":"You've inherited the conversation context above"}}"#,
            r#"{"type":"assistant","isSidechain":true,"message":{"content":[{"type":"text","text":"On it."},{"type":"tool_use","name":"Bash","input":{"command":"swift build"}}]}}"#,
            r#"{"type":"user","isSidechain":true,"message":{"content":[{"type":"tool_result","content":"ok"}]}}"#,
            r#"{"type":"attachment","isSidechain":true}"#,
            r#"{"type":"user","isSidechain":true,"message":{"content":"[Image: original 3000x1716]"}}"#,
            r#"{"type":"user","isSidechain":true,"message":{"content":"This session is being continued from a previous conversation"}}"#,
            r#"{"type":"user","isSidechain":true,"message":{"content":"The coordinator sent a message while you were working: also the toolbar"}}"#,
            r#"{"type":"assistant","isSidechain":true,"message":{"content":[{"type":"text","text":"Done."}]}}"#,
        ]
        .join("\n");
        let t = |role: &str, text: &str| TurnInfo { role: role.into(), text: text.into() };
        let all = turns(&fork, true);
        assert_eq!(all, vec![
            t("task", "Fix the sidebar."),
            t("agent", "On it."),
            t("tool", "Bash swift build"),
            t("note", "Its context was compacted"),
            t("user", "also the toolbar"),
            t("agent", "Done."),
        ]);
        // Read from somewhere in the middle, nothing is taken for the task.
        assert!(turns(&fork.lines().skip(4).collect::<Vec<_>>().join("\n"), false).iter().all(|t| t.role != "task"));
        // A plain subagent's first message is its task.
        let plain = r#"{"type":"user","isSidechain":true,"message":{"content":"Find the bug"}}"#;
        assert_eq!(turns(plain, true), vec![t("task", "Find the bug")]);

        // Over budget: the task, a note, then the newest.
        let rest = vec![t("agent", "aaaa"), t("agent", "bbbb"), t("agent", "cccc")];
        assert_eq!(fit(Some(t("task", "x")), rest.clone(), false, 9), vec![t("task", "x"), t("note", "Earlier turns left out"), t("agent", "bbbb"), t("agent", "cccc")]);
        assert_eq!(fit(None, rest.clone(), false, 100), rest);
        assert_eq!(fit(None, rest.clone(), true, 100)[0], t("note", "Earlier turns left out"));
    }

    #[test]
    fn codex_context_from_the_last_token_count() {
        let count = |total: u64, window: &str| {
            format!(r#"{{"type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"total_tokens":99999}},"last_token_usage":{{"input_tokens":16188,"cached_input_tokens":3328,"output_tokens":39,"total_tokens":{total}}},"model_context_window":{window}}},"rate_limits":{{}}}}}}"#)
        };
        let jsonl = [
            r#"{"type":"session_meta","payload":{"id":"x"}}"#.to_string(),
            count(16227, "258400"),
            count(40000, "258400"),
            // Before a model answers, Codex reports a count without info.
            r#"{"type":"event_msg","payload":{"type":"token_count","info":null,"rate_limits":{}}}"#.into(),
            r#"{"type":"response_item","payload":{"type":"message"}}"#.into(),
        ]
        .join("\n");
        assert_eq!(codex_context_in(&jsonl), Some((40000, 258400)));
        assert_eq!(codex_context_in(&count(5, "null")), None, "no window, no guess");
        assert_eq!(codex_context_in(r#"{"type":"session_meta"}"#), None);
    }
}
