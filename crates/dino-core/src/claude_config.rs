//! Claude Code's config folder, where it keeps its records: a transcript per conversation in
//! `projects/`, a file per running process in `sessions/`, and the user's settings. Set,
//! `CLAUDE_CONFIG_DIR` moves all of it ("every `~/.claude` path … lives under that directory
//! instead", https://code.claude.com/docs/en/claude-directory).
//!
//! The Claudes dinod starts keep theirs where dinod's own environment says, unless a session's
//! environment says otherwise: an agent continued from outside dino keeps the account it had (its
//! `CLAUDE_CONFIG_DIR` among it), and a repo's environment can set one. dinod notes each such
//! folder, and Claude's records are looked for in every folder noted, after the default one.
//! A session's conversation is named by an id of its own, so finding it in any of them is
//! finding the session's.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The variable that moves Claude's config folder.
pub const ENV: &str = "CLAUDE_CONFIG_DIR";

/// Where Claude keeps its records when nothing says otherwise: dinod's own `CLAUDE_CONFIG_DIR`,
/// else `~/.claude`.
pub fn home() -> PathBuf {
    std::env::var_os(ENV).filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(".claude"))
}

/// The folders noted besides the default one, for as long as dinod runs.
static NOTED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// A Claude on this Mac keeps its records in `dir`: look for them there too from now on.
pub fn note(dir: &Path) {
    if dir.as_os_str().is_empty() {
        return;
    }
    let mut noted = NOTED.lock().unwrap();
    if !noted.iter().any(|d| d == dir) {
        noted.push(dir.to_path_buf());
    }
}

/// Every folder Claude's records may be in: the default one first, then those noted.
pub fn homes() -> Vec<PathBuf> {
    let mut out = vec![home()];
    for dir in NOTED.lock().unwrap().iter() {
        if !out.contains(dir) {
            out.push(dir.clone());
        }
    }
    out
}

/// The folder an environment sets, given as `(name, value)` pairs in the order they apply (a
/// later one wins), if it sets one.
pub fn set_in<'a>(env: impl IntoIterator<Item = (&'a str, &'a str)>) -> Option<PathBuf> {
    env.into_iter().filter(|(k, v)| *k == ENV && !v.is_empty()).last().map(|(_, v)| PathBuf::from(v))
}

/// The folder process `pid` was started with in its environment, if any.
pub fn of_process(pid: u32) -> Option<PathBuf> {
    let (_, env) = crate::procinfo::args_and_env(pid)?;
    set_in(env.iter().filter_map(|kv| kv.split_once('=')))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noted_folders_come_after_the_default_once_each() {
        let dir = std::env::temp_dir().join(format!("dino-claude-config-noted-{}", std::process::id()));
        assert!(!homes().contains(&dir));
        note(&dir);
        note(&dir.join(""));
        note(Path::new(""));
        let homes = homes();
        assert_eq!(homes.iter().filter(|h| **h == dir).count(), 1, "{homes:?}");
        assert_ne!(homes[0], dir, "the default first");
        assert!(homes.iter().all(|h| !h.as_os_str().is_empty()));
    }

    #[test]
    fn the_folder_an_environment_sets() {
        assert_eq!(set_in([("HOME", "/h"), (ENV, "/a"), (ENV, "/b")]), Some(PathBuf::from("/b")), "the later one wins");
        assert_eq!(set_in([(ENV, "/a"), (ENV, "")]), Some(PathBuf::from("/a")), "an empty one sets nothing");
        assert_eq!(set_in([("HOME", "/h")]), None);
    }
}
