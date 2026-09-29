//! Side chat: a question about a session, answered by Claude headlessly with dino's read-only
//! session tools and read access to the session's folder. The session itself is never disturbed.

use std::path::Path;
use std::time::Duration;

use crate::review::DISALLOWED;

const MODEL: &str = "sonnet";
const TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// What the answerer may use: dino's tools that only look, and reading files.
const ALLOWED: &str = "mcp__dino__list_sessions,mcp__dino__read_session,Read,Grep,Glob";

/// The session the question is about, as the answerer is told.
pub struct About<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub agent: &'a str,
    pub cwd: &'a Path,
}

/// Answer `question` about session `about`. `dino` is the dino binary, run as the MCP server.
/// Blocks until Claude answers, `review::cancel` is called for `key`, or it times out.
pub fn run(key: &str, about: &About, question: &str, dino: &Path) -> anyhow::Result<String> {
    let args = [
        "--model", MODEL,
        "--allowedTools", ALLOWED,
        "--disallowedTools", &format!("{DISALLOWED},mcp__dino__send_message,mcp__dino__create_session"),
        // Only dino's server, whatever the user configured.
        "--strict-mcp-config", "--mcp-config", &crate::mcp::config(dino, None, true),
    ]
    .map(String::from);
    let prompt = format!(
        r#"The user is asking about the agent session "{name}" (id {id}, {agent}), working in {cwd}. Use the dino tools to see what it has been doing (read_session with id "{id}"), and read files in its folder if that helps. You can't change anything. Answer briefly and concretely, in Markdown.

Question: {question}"#,
        name = about.name,
        id = about.id,
        agent = about.agent,
        cwd = about.cwd.display(),
    );
    let dir = if about.cwd.is_dir() { about.cwd } else { Path::new("/") };
    crate::review::headless(key, dir, &args, prompt, TIMEOUT, "answer").map(|a| a.trim().to_string())
}
