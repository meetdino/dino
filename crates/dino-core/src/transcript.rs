//! A session's conversation as plain text, for other agents to read (`dino mcp`'s `read_session`).

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;

/// How much of the end of a transcript file is read; enough for the last several turns.
const READ_TAIL: u64 = 2 << 20;

/// Claude's transcript for conversation `uuid`: `~/.claude/projects/<dir>/<uuid>.jsonl`, in
/// whichever of Claude's config folders it is (see `claude_config`).
pub fn claude_path(uuid: &str) -> Option<PathBuf> {
    claude_projects().map(|d| d.join(format!("{uuid}.jsonl"))).find(|p| p.exists())
}

/// Every project folder of Claude's (`~/.claude/projects/<dir>`), in each of its config folders.
pub(crate) fn claude_projects() -> impl Iterator<Item = PathBuf> {
    crate::claude_config::homes().into_iter().flat_map(|home| std::fs::read_dir(home.join("projects")).into_iter().flatten().flatten().map(|d| d.path()))
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
    let file = format!("agent-{id}.jsonl");
    claude_projects()
        .flat_map(|project| match parent {
            Some(uuid) => vec![project.join(uuid)],
            None => std::fs::read_dir(&project).into_iter().flatten().flatten().map(|d| d.path()).collect(),
        })
        .map(|conversation| conversation.join("subagents").join(&file))
        .find(|p| p.exists())
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
            Some("tool_use") => parts.push(format!("[{}{}]", block["name"].as_str().unwrap_or("tool"), crate::history::hint(&block["input"]))),
            _ => {}
        }
    }
    parts.retain(|p| !p.is_empty() && !p.starts_with("<command-") && !p.starts_with("<local-command-"));
    (!parts.is_empty()).then(|| format!("{who}: {}", parts.join("\n")))
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
