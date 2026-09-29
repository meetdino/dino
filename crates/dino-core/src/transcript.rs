//! A session's conversation as plain text, for other agents to read (`dino mcp`'s `read_session`).

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;

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
    let mut f = std::fs::File::open(p).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(READ_TAIL))).ok()?;
    let mut bytes = Vec::new();
    f.read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    // Started mid-file: the first line is a fragment.
    Some(if len > READ_TAIL { text.split_once('\n').map_or(String::new(), |(_, rest)| rest.to_string()) } else { text })
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
}
