//! Scheduled tasks: a prompt dinod starts a new session with, on a schedule or on demand, like
//! Claude Desktop's local scheduled tasks but for any agent. Kept in `schedule.json`.

use serde::{Deserialize, Serialize};

/// When a task runs, in local time. Weekdays count from Sunday (0) to Saturday (6).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(tag = "every", rename_all = "snake_case")]
pub enum Frequency {
    /// Only when the user asks (Run now).
    Manual,
    Hourly { minute: u8 },
    Daily { hour: u8, minute: u8 },
    /// Monday to Friday.
    Weekdays { hour: u8, minute: u8 },
    Weekly { weekday: u8, hour: u8, minute: u8 },
}

impl Default for Frequency {
    fn default() -> Self {
        Frequency::Daily { hour: 9, minute: 0 }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default)]
pub struct ScheduledTask {
    /// Empty in a `SchedulePut` for a new task; dinod assigns one.
    pub id: String,
    /// Unique; also the name of the sessions it starts.
    pub name: String,
    pub prompt: String,
    /// Which agent, by launcher short name.
    pub launcher: String,
    /// The folder it runs in.
    pub cwd: String,
    /// Each run in a new worktree and branch of the repo at `cwd`.
    pub worktree: bool,
    /// Extra arguments for the agent (`--model haiku`, `--permission-mode plan`), split like a shell would.
    pub args: String,
    pub frequency: Frequency,
    /// Paused tasks only run when asked to.
    pub enabled: bool,
    pub created_at: u64,
    /// The last scheduled time dinod dealt with (ran, or chose not to); later ones are still owed.
    pub last_due: Option<u64>,
    /// Newest last, at most `MAX_HISTORY`.
    pub history: Vec<ScheduledRun>,
    /// When it runs next; filled in by dinod, ignored on the way in.
    pub next_run: Option<u64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default)]
pub struct ScheduledRun {
    /// When dinod acted.
    pub at: u64,
    /// The scheduled time it was for; none for Run now.
    pub due: Option<u64>,
    /// The session it started.
    pub session: Option<String>,
    /// "started", "skipped" or "failed".
    pub outcome: String,
    pub reason: Option<String>,
    /// Started late, for a time missed while the Mac slept or dinod wasn't running.
    pub catch_up: bool,
}

pub const MAX_HISTORY: usize = 20;

/// Words of `s`, split like a shell: spaces separate, quotes group, backslash escapes.
pub fn split_args(s: &str) -> Vec<String> {
    let mut out = vec![];
    let mut word: Option<String> = None;
    let mut quote: Option<char> = None;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') | (None, '\\') => word.get_or_insert_default().extend(chars.next()),
            (Some(_), c) => word.get_or_insert_default().push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                word.get_or_insert_default();
            }
            (None, c) if c.is_whitespace() => out.extend(word.take()),
            (None, c) => word.get_or_insert_default().push(c),
        }
    }
    out.extend(word);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_like_a_shell() {
        assert_eq!(split_args(""), Vec::<String>::new());
        assert_eq!(split_args("  --model  haiku "), ["--model", "haiku"]);
        assert_eq!(split_args(r#"--append-system-prompt "be brief" 'a b' c\ d"#), ["--append-system-prompt", "be brief", "a b", "c d"]);
        assert_eq!(split_args(r#"--x """#), ["--x", ""]);
    }
}
