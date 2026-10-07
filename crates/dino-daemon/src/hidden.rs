//! What the user hid from "On this Mac" and the session browser: a finished conversation, by its
//! agent and id, until it's shown again; an agent running in another terminal, by its process,
//! until that process ends. Kept in dino's folder (`hidden.json`), never in the agents' own files.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use dino_core::found::{FoundSession, Source};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
pub(crate) struct Hidden {
    /// Finished conversations: `agent:session id`.
    #[serde(default)]
    conversations: Vec<String>,
    /// Running agents: the process, and when it started (a pid is reused once its process is gone).
    #[serde(default)]
    processes: Vec<(u32, u64)>,
}

fn file(home: &Path) -> PathBuf {
    home.join("hidden.json")
}

fn started(pid: u32) -> Option<u64> {
    dino_core::procinfo::process(pid).map(|p| p.started_us)
}

/// A running one is hidden by its process, anything else by its conversation.
fn process(f: &FoundSession) -> Option<u32> {
    f.pid.filter(|_| f.source == Source::Running)
}

fn key(f: &FoundSession) -> String {
    format!("{}:{}", f.agent, f.session_id)
}

pub(crate) fn load(home: &Path) -> Hidden {
    std::fs::read(file(home)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

impl Hidden {
    /// `f` is hidden: a running one while the process it was hidden in still runs.
    pub(crate) fn has(&self, f: &FoundSession) -> bool {
        match process(f) {
            Some(pid) => self.processes.iter().any(|&(p, at)| p == pid && started(pid) == Some(at)),
            None => !f.session_id.is_empty() && self.conversations.contains(&key(f)),
        }
    }

    /// How many conversations are hidden.
    pub(crate) fn conversations(&self) -> usize {
        self.conversations.len()
    }
}

/// Hide `f`, or (`hide` false) show it again.
pub(crate) fn set(home: &Path, f: &FoundSession, hide: bool) -> anyhow::Result<()> {
    static ONE: Mutex<()> = Mutex::new(());
    let _one = ONE.lock().unwrap();
    let mut h = load(home);
    // Processes that ended are hidden no more.
    h.processes.retain(|&(p, at)| started(p) == Some(at));
    match process(f) {
        Some(pid) => {
            h.processes.retain(|&(p, _)| p != pid);
            if hide {
                let Some(at) = started(pid) else { anyhow::bail!("It isn't running anymore.") };
                h.processes.push((pid, at));
            }
        }
        None => {
            anyhow::ensure!(!f.session_id.is_empty(), "There's no conversation to hide.");
            let k = key(f);
            h.conversations.retain(|c| c != &k);
            if hide {
                h.conversations.push(k);
            }
        }
    }
    let tmp = file(home).with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&h)?)?;
    std::fs::rename(tmp, file(home))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(source: Source, id: &str, pid: Option<u32>) -> FoundSession {
        FoundSession {
            source,
            agent: "claude".into(),
            session_id: id.into(),
            title: String::new(),
            cwd: None,
            updated_at: 0,
            pid,
            status: None,
            terminal: None,
            args: vec![],
            url: None,
            tmux: None,
            unsure: None,
        }
    }

    #[test]
    fn hidden_conversations_stay_hidden_and_processes_until_they_end() {
        let home = std::env::temp_dir().join(format!("dino-hidden-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        let old = found(Source::Recent, "c1", None);
        set(&home, &old, true).unwrap();
        assert!(load(&home).has(&old));
        assert!(!load(&home).has(&found(Source::Recent, "c2", None)), "only that one");
        // This test's own process stands in for an agent running elsewhere.
        let me = found(Source::Running, "c3", Some(std::process::id()));
        set(&home, &me, true).unwrap();
        assert!(load(&home).has(&me));
        // A process that has ended (or a pid since given to another) isn't hidden.
        let mut h = load(&home);
        h.processes = vec![(std::process::id(), 1)];
        assert!(!h.has(&me));
        set(&home, &old, false).unwrap();
        assert!(!load(&home).has(&old));
        assert!(set(&home, &found(Source::Recent, "", None), true).is_err());
        std::fs::remove_dir_all(&home).unwrap();
    }
}
