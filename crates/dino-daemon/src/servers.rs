//! Background commands an agent left running that listen for connections: a dev server the user
//! works with, not work the agent is waiting on. The agent names each background command it runs
//! (its hooks); dinod finds that command's shell among the agent's own children by its command
//! line, and asks the kernel whether anything under it is listening. Stopping one looks again, so
//! only a process still under that agent, running that command, is ever signalled.

use crate::{Daemon, Session};
use dino_core::procinfo;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// A background command that serves: its task id and the ports it listens on.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Server {
    pub task: String,
    pub command: String,
    pub ports: Vec<u16>,
}

/// Look again at every local session with background commands still running.
pub(crate) fn watch(d: &Daemon) {
    let sessions: Vec<Arc<Session>> = d.sessions.lock().unwrap().iter().filter(|s| s.host.is_none() && !s.pane.is_exited()).cloned().collect();
    for s in sessions {
        let running = running_commands(d, &s);
        let found = if running.is_empty() {
            vec![]
        } else {
            s.pane.pid().map(|agent| serving(&processes(agent), agent, &running)).unwrap_or_default()
        };
        let mut servers = s.servers.lock().unwrap();
        if *servers != found {
            *servers = found;
        }
    }
}

/// The session's background shell commands its agent still lists as running: (task id, command).
fn running_commands(d: &Daemon, s: &Session) -> Vec<(String, String)> {
    d.proxy
        .stats
        .session(&s.id)
        .background
        .into_iter()
        .filter(|b| b.running && b.kind == "shell")
        .filter_map(|b| Some((b.id, b.command?)))
        .collect()
}

/// The agent and every process under it: its parent and its command line.
fn processes(agent: u32) -> HashMap<u32, (u32, String)> {
    procinfo::tree(agent).into_iter().map(|(pid, parent)| (pid, (parent, procinfo::args_and_env(pid).map(|(a, _)| a.join(" ")).unwrap_or_default()))).collect()
}

/// The agent's shell running `command`: a direct child of the agent whose command line carries it,
/// as the agent quotes it.
fn shell_for(tree: &HashMap<u32, (u32, String)>, agent: u32, command: &str) -> Option<u32> {
    let quoted = command.replace('\'', r"'\''");
    tree.iter().find(|(_, (ppid, args))| *ppid == agent && (args.contains(command) || args.contains(&quoted))).map(|(pid, _)| *pid)
}

/// `root` and everything under it.
fn subtree(tree: &HashMap<u32, (u32, String)>, root: u32) -> Vec<u32> {
    let mut out = vec![root];
    let mut i = 0;
    while i < out.len() {
        let p = out[i];
        out.extend(tree.iter().filter(|(_, (ppid, _))| *ppid == p).map(|(pid, _)| *pid));
        i += 1;
    }
    out
}

fn serving(tree: &HashMap<u32, (u32, String)>, agent: u32, running: &[(String, String)]) -> Vec<Server> {
    running
        .iter()
        .filter_map(|(task, command)| {
            let shell = shell_for(tree, agent, command)?;
            let ports = listening(&subtree(tree, shell));
            (!ports.is_empty()).then(|| Server { task: task.clone(), command: command.clone(), ports })
        })
        .collect()
}

/// TCP ports any of `pids` listens on.
fn listening(pids: &[u32]) -> Vec<u16> {
    let list = pids.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
    let out = std::process::Command::new("lsof").args(["-a", "-p", &list, "-iTCP", "-sTCP:LISTEN", "-nP", "-Fn"]).output();
    let Ok(out) = out else { return vec![] };
    let mut ports: Vec<u16> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.strip_prefix('n')?.rsplit(':').next()?.parse().ok())
        .collect();
    ports.sort_unstable();
    ports.dedup();
    ports
}

/// Stop the background command `task` of session `s`: its shell and everything under it, found
/// again now under the agent. The agent then sees its command end, as when it stops one itself.
pub(crate) fn stop(d: &Daemon, s: &Session, task: &str) -> anyhow::Result<()> {
    let command = running_commands(d, s).into_iter().find(|(id, _)| id == task).map(|(_, c)| c);
    let command = command.ok_or_else(|| anyhow::anyhow!("that command isn't running any more"))?;
    let agent = s.pane.pid().ok_or_else(|| anyhow::anyhow!("its agent isn't running"))?;
    let tree = processes(agent);
    let shell = shell_for(&tree, agent, &command).ok_or_else(|| anyhow::anyhow!("couldn't find that command's process"))?;
    let pids = subtree(&tree, shell);
    // As ⌃C in its terminal would: a server takes that as asked to stop and exits cleanly, so the
    // agent sees it stopped rather than failed (and doesn't start it again). Then harder, for one
    // that ignores it.
    signal(&pids, libc::SIGINT);
    std::thread::spawn(move || {
        for sig in [libc::SIGTERM, libc::SIGKILL] {
            std::thread::sleep(Duration::from_secs(3));
            // Only ones that are still what they were: the same pid under the same parent.
            let left: Vec<u32> = pids.iter().copied().filter(|p| procinfo::parent_of(*p).is_some_and(|pp| tree.get(p).is_some_and(|(was, _)| *was == pp))).collect();
            if left.is_empty() {
                break;
            }
            signal(&left, sig);
        }
    });
    s.servers.lock().unwrap().retain(|x| x.task != task);
    Ok(())
}

fn signal(pids: &[u32], sig: libc::c_int) {
    for &p in pids.iter().rev() {
        unsafe { libc::kill(p as libc::pid_t, sig) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_agents_shell_for_a_command() {
        let tree: HashMap<u32, (u32, String)> = [
            (10, (1, "claude --model haiku".to_string())),
            (11, (10, "/bin/bash -c source snap.sh && eval 'python3 -m http.server 8765' < /dev/null".to_string())),
            (12, (11, "Python -m http.server 8765".to_string())),
            (13, (10, "/bin/bash -c eval 'echo '\\''hi'\\'''".to_string())),
            // The same command under someone else isn't this agent's.
            (20, (1, "/bin/bash -c eval 'python3 -m http.server 8765'".to_string())),
        ]
        .into_iter()
        .collect();
        assert_eq!(shell_for(&tree, 10, "python3 -m http.server 8765"), Some(11));
        assert_eq!(shell_for(&tree, 10, "echo 'hi'"), Some(13));
        assert_eq!(shell_for(&tree, 10, "npm run dev"), None);
        let mut sub = subtree(&tree, 11);
        sub.sort();
        assert_eq!(sub, [11, 12]);
    }

    #[test]
    fn reads_an_agents_processes() {
        // A command no other test runs: tests share this process, and its children.
        let mut shell = std::process::Command::new("/bin/sh").args(["-c", "eval 'sleep 41'; true"]).spawn().unwrap();
        let me = std::process::id();
        let mut tree = processes(me);
        for _ in 0..50 {
            if subtree(&tree, shell.id()).len() > 1 {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
            tree = processes(me);
        }
        let found = shell_for(&tree, me, "sleep 41");
        let under = subtree(&tree, shell.id());
        let _ = shell.kill();
        let _ = shell.wait();
        assert_eq!(found, Some(shell.id()));
        assert_eq!(under.len(), 2, "the shell and its sleep: {tree:?}");
    }
}
