//! Code review: Claude reads a session's changes headlessly and reports the bugs worth fixing.
//!
//! Always Claude, whichever agent made the changes: `claude -p` in the checkout, read-only. It may
//! open files for context but can't edit, run commands or reach the web.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// One issue the reviewer found. `line` is in the new version of `file`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub file: String,
    pub line: u32,
    /// "high", "medium" or "low".
    pub severity: String,
    pub message: String,
}

const MODEL: &str = "sonnet";
/// Nothing the reviewer runs may change the checkout or leave it.
pub(crate) const DISALLOWED: &str = "Write,Edit,MultiEdit,NotebookEdit,Bash,Artifact,WebFetch,WebSearch";
/// Past this, the patch is cut and the reviewer reads the rest from the files.
const MAX_PATCH: usize = 300_000;
const TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Running reviews by session, as process group ids, so they can be cancelled.
static RUNNING: Mutex<Option<HashMap<String, u32>>> = Mutex::new(None);

/// Each line of each hunk prefixed with its number in the new file (blank for removed lines), so
/// findings point at the line the diff viewer shows rather than a count the model guessed.
fn number_lines(patch: &str) -> String {
    let (mut new, mut in_hunk) = (0u32, false);
    let mut out = String::with_capacity(patch.len() + patch.len() / 4);
    for line in patch.lines() {
        if let Some(h) = line.strip_prefix("@@ ") {
            new = h.split(' ').nth(1).and_then(|r| r.get(1..)?.split(',').next()?.parse().ok()).unwrap_or(0);
            in_hunk = true;
        } else if line.starts_with("diff --git ") {
            in_hunk = false;
        } else if in_hunk && !line.starts_with('\\') {
            if line.starts_with('-') {
                out.push_str("      ");
            } else {
                out.push_str(&format!("{new:>5} "));
                new += 1;
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn prompt(patch: &str) -> String {
    let (patch, cut) = match patch.char_indices().nth(MAX_PATCH) {
        Some((i, _)) => (&patch[..i], "\n[The diff is cut here; read the remaining changed files yourself.]\n"),
        None => (patch, ""),
    };
    let patch = number_lines(patch);
    format!(
        r#"You are reviewing uncommitted and branch changes in this repository. The diff is below; you may read files in the working directory for context.

Report only high-signal issues: code that won't compile or parse, logic errors, security problems, and obvious bugs that will misbehave at runtime. Do NOT report style, naming, formatting, missing tests, missing docs, refactoring ideas or speculative concerns. If you're not confident it's a real problem, leave it out. An empty list is a fine answer.

Reply with ONLY a JSON array, no prose and no code fence:
[{{"file": "path/relative/to/repo/root", "line": 42, "severity": "high" | "medium" | "low", "message": "what's wrong and why, in one or two sentences"}}]
`line` is the line number in the new version of the file. In the diff, each kept or added line starts with that number; use it exactly (for a removed line, the number of the nearest line that remains).

<diff>
{patch}{cut}</diff>"#
    )
}

/// Review what changed in `dir` since `base`. Blocks until Claude answers, `cancel` is called for
/// `key`, or it times out.
pub fn run(key: &str, dir: &Path, base: &str) -> anyhow::Result<Vec<Finding>> {
    let patch = crate::worktree::changes_patch(dir, base)?;
    anyhow::ensure!(!patch.trim().is_empty(), "Nothing has changed, so there's nothing to review");
    // No MCP servers.
    let args = ["--model", MODEL, "--disallowedTools", DISALLOWED, "--strict-mcp-config"].map(String::from);
    let out = headless(key, dir, &args, prompt(&patch), TIMEOUT, "review")?;
    let mut findings = parse_findings(&out)?;
    // Paths as the diff has them, should the model answer with absolute ones.
    let root = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    for f in findings.iter_mut().filter(|f| f.file.starts_with('/')) {
        // Through symlinks (/tmp is /private/tmp), and for files since deleted, their folder.
        let p = Path::new(&f.file);
        let real = p.canonicalize().ok().or_else(|| Some(p.parent()?.canonicalize().ok()?.join(p.file_name()?)));
        if let Some(rel) = real.as_deref().and_then(|p| p.strip_prefix(&root).ok()) {
            f.file = rel.to_string_lossy().into_owned();
        }
    }
    Ok(findings)
}

/// Run `claude -p` in `dir` with `args` and `input` on stdin, and answer with its reply. Blocks
/// until Claude answers, `cancel` is called for `key`, or `timeout` passes. `what` names the job
/// in errors ("review").
pub(crate) fn headless(key: &str, dir: &Path, args: &[String], input: String, timeout: Duration, what: &str) -> anyhow::Result<String> {
    let claude = crate::which("claude").ok_or_else(|| {
        anyhow::anyhow!("This needs Claude Code, and `claude` isn't installed. Install it (npm install -g @anthropic-ai/claude-code), then try again.")
    })?;
    let mut child = Command::new(claude)
        .args(["-p", "--output-format", "json"])
        .args(args)
        // No entry in the user's resumable sessions.
        .arg("--no-session-persistence")
        .current_dir(dir)
        .env("NO_COLOR", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()?;
    let pid = child.id();
    {
        let mut running = RUNNING.lock().unwrap();
        if let Some(old) = running.get_or_insert_with(HashMap::new).insert(key.to_string(), pid) {
            kill_group(old);
        }
    }
    let mut stdin = child.stdin.take().unwrap();
    std::thread::spawn(move || {
        let _ = stdin.write_all(input.as_bytes());
    });
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let out = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        s
    });
    let err = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    let start = Instant::now();
    let status = loop {
        if let Some(st) = child.try_wait()? {
            break Some(st);
        }
        if start.elapsed() > timeout {
            kill_group(pid);
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    let cancelled = {
        let mut running = RUNNING.lock().unwrap();
        let map = running.get_or_insert_with(HashMap::new);
        // Gone, or replaced by a newer run: someone cancelled this one.
        let ours = map.get(key) == Some(&pid);
        if ours {
            map.remove(key);
        }
        !ours
    };
    let (out, err) = (out.join().unwrap_or_default(), err.join().unwrap_or_default());
    anyhow::ensure!(!cancelled, "Cancelled");
    let status = status.ok_or_else(|| anyhow::anyhow!("The {what} took longer than {} minutes and was stopped", timeout.as_secs() / 60))?;
    result_text(&out, what).map_err(|e| if status.success() || err.trim().is_empty() { e } else { anyhow::anyhow!("claude: {}", last_line(&err)) })
}

/// Stop the review (or other headless run) going for `key`, if any.
pub fn cancel(key: &str) -> bool {
    let pid = RUNNING.lock().unwrap().get_or_insert_with(HashMap::new).remove(key);
    if let Some(pid) = pid {
        kill_group(pid);
    }
    pid.is_some()
}

fn kill_group(pgid: u32) {
    let _ = Command::new("kill").args(["-TERM", &format!("-{pgid}")]).stdout(Stdio::null()).stderr(Stdio::null()).status();
}

fn last_line(s: &str) -> &str {
    s.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim()
}

/// `claude -p --output-format json` prints one result object; its `result` holds the answer text.
fn result_text(out: &str, what: &str) -> anyhow::Result<String> {
    #[derive(Deserialize)]
    struct Result {
        #[serde(default)]
        is_error: bool,
        #[serde(default)]
        result: String,
    }
    let r: Result = serde_json::from_str(out.trim()).map_err(|_| anyhow::anyhow!("Claude's answer wasn't readable: {}", last_line(out)))?;
    anyhow::ensure!(!r.is_error, "Claude couldn't {what}: {}", if r.result.is_empty() { "unknown error" } else { r.result.trim() });
    Ok(r.result)
}

#[cfg(test)]
fn parse_output(out: &str) -> anyhow::Result<Vec<Finding>> {
    parse_findings(&result_text(out, "review")?)
}

/// The JSON array in the answer, even if the model wrapped it in a fence or a sentence.
fn parse_findings(text: &str) -> anyhow::Result<Vec<Finding>> {
    #[derive(Deserialize)]
    struct Raw {
        file: String,
        #[serde(default)]
        line: serde_json::Value,
        #[serde(default)]
        severity: String,
        message: String,
    }
    let bad = || anyhow::anyhow!("Claude didn't answer with a list of findings: {}", text.trim().chars().take(200).collect::<String>());
    let (start, end) = (text.find('[').ok_or_else(bad)?, text.rfind(']').ok_or_else(bad)?);
    anyhow::ensure!(start < end, bad());
    let raw: Vec<Raw> = serde_json::from_str(&text[start..=end]).map_err(|_| bad())?;
    Ok(raw
        .into_iter()
        .map(|r| Finding {
            file: r.file.trim_start_matches("./").trim_start_matches("b/").to_string(),
            line: r.line.as_u64().or_else(|| r.line.as_str()?.parse().ok()).unwrap_or(0) as u32,
            severity: match r.severity.to_lowercase().as_str() {
                s @ ("high" | "medium" | "low") => s.to_string(),
                "critical" | "error" => "high".into(),
                _ => "medium".into(),
            },
            message: r.message.trim().to_string(),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn findings_from_a_result() {
        let out = r#"{"type":"result","subtype":"success","is_error":false,"result":"```json\n[{\"file\":\"./src/a.rs\",\"line\":\"12\",\"severity\":\"Critical\",\"message\":\" off by one \"}]\n```"}"#;
        let f = parse_output(out).unwrap();
        assert_eq!(f, vec![Finding { file: "src/a.rs".into(), line: 12, severity: "high".into(), message: "off by one".into() }]);
        assert_eq!(parse_output(r#"{"is_error":false,"result":"[]"}"#).unwrap(), vec![]);
    }

    #[test]
    fn errors_are_said() {
        assert!(parse_output(r#"{"is_error":true,"result":"Credit balance is too low"}"#).unwrap_err().to_string().contains("Credit balance"));
        assert!(parse_output(r#"{"is_error":false,"result":"Looks good to me!"}"#).is_err());
        assert!(parse_output("Error: not logged in").unwrap_err().to_string().contains("not logged in"));
    }

    #[test]
    fn lines_are_numbered_as_in_the_new_file() {
        let patch = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -3,3 +7,3 @@ fn f\n a\n-b\n+c\n d\n";
        let n = number_lines(patch);
        assert!(n.contains("+++ b/x\n@@ -3,3 +7,3 @@ fn f\n    7  a\n      -b\n    8 +c\n    9  d\n"), "{n}");
    }

    #[test]
    fn a_long_diff_is_cut() {
        let p = prompt(&"x".repeat(MAX_PATCH + 10));
        assert!(p.contains("The diff is cut here"));
        assert!(!prompt("small").contains("cut here"));
    }
}
