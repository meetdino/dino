//! How agents reach `dino mcp`, dino's MCP server for seeing and driving other sessions.

use std::path::Path;

/// Tools that only look; sessions may use them without asking.
pub const READ_TOOLS: &[&str] = &["mcp__dino__list_sessions", "mcp__dino__read_session"];

/// An `--mcp-config` document that runs `dino mcp`. `session` is the session whose agent uses it
/// (so what it starts and messages is marked as its doing); `read_only` leaves out the tools that act.
pub fn config(dino: &Path, session: Option<&str>, read_only: bool) -> String {
    let mut env = serde_json::Map::new();
    env.insert("DINO_HOME".into(), crate::config_dir().display().to_string().into());
    if let Some(id) = session {
        env.insert("DINO_SESSION".into(), id.into());
    }
    let args: Vec<&str> = if read_only { vec!["mcp", "--read-only"] } else { vec!["mcp"] };
    serde_json::json!({
        "mcpServers": { "dino": { "type": "stdio", "command": dino.display().to_string(), "args": args, "env": env } }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_runs_dino_mcp() {
        let v: serde_json::Value = serde_json::from_str(&config(Path::new("/bin/dino"), Some("7"), false)).unwrap();
        let s = &v["mcpServers"]["dino"];
        assert_eq!(s["command"], "/bin/dino");
        assert_eq!(s["args"], serde_json::json!(["mcp"]));
        assert_eq!(s["env"]["DINO_SESSION"], "7");
        let ro: serde_json::Value = serde_json::from_str(&config(Path::new("/bin/dino"), None, true)).unwrap();
        assert_eq!(ro["mcpServers"]["dino"]["args"], serde_json::json!(["mcp", "--read-only"]));
        assert!(ro["mcpServers"]["dino"]["env"].get("DINO_SESSION").is_none());
    }
}
