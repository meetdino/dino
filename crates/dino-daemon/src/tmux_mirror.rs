//! "Show my agents in tmux" (Settings → tmux): dino's agents as windows in the user's
//! own tmux, each running `dino attach`, so `tmux attach` from anywhere reaches them.
//!
//! dinod keeps owning the agents: a window is only a view. Closing it, or the whole server, leaves
//! the agent running, and its window comes back. dino never starts a tmux server, never edits its
//! config, and only adds, renames and removes the windows it made, which carry its
//! `@dino-session` window option; it tells its windows apart by that, never by name.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use dino_core::settings::{Settings, Tmux};

use crate::Daemon;

/// The window option marking a window as dino's, with the session id it shows.
const MARK: &str = "@dino-session";
/// How often it looks again while on: agents end and rename themselves without a request.
const LOOK_ON: Duration = Duration::from_secs(3);
/// While off: only the settings file's date is looked at.
const LOOK_OFF: Duration = Duration::from_secs(10);

pub fn start(d: Arc<Daemon>) {
    std::thread::Builder::new().name("tmux-mirror".into()).spawn(move || run(&d)).expect("tmux mirror thread");
}

fn run(d: &Daemon) {
    let mut cfg = Settings::load().tmux;
    let mut seen = modified();
    let mut loaded = Instant::now();
    // As if it was on: a dinod that starts with it off clears what an earlier one left, once.
    let mut was_on = true;
    loop {
        if was_on && !cfg.show_agents {
            if let Some(t) = Server::find() {
                t.remove_all();
            }
        }
        if cfg.show_agents {
            if let Some(t) = Server::find() {
                t.sync(d, &cfg);
            }
        }
        was_on = cfg.show_agents;
        wait(d, if cfg.show_agents { LOOK_ON } else { LOOK_OFF });
        // The settings file only: reread when it changed (and now and then, for an organization's).
        let now = modified();
        if now != seen || loaded.elapsed() > Duration::from_secs(60) {
            seen = now;
            loaded = Instant::now();
            cfg = Settings::load().tmux;
        }
    }
}

/// Until a request changed something (a session started, ended, was renamed or archived) or
/// `most` has passed.
fn wait(d: &Daemon, most: Duration) {
    let guard = d.asked.0.lock().unwrap();
    let before = *guard;
    let _ = d.asked.1.wait_timeout_while(guard, most, |n| *n == before);
    // A burst of requests (a fan-out starting) settles before it looks.
    std::thread::sleep(Duration::from_millis(300));
}

fn modified() -> Option<SystemTime> {
    std::fs::metadata(Settings::path()).and_then(|m| m.modified()).ok()
}

/// A window dino wants: the session it shows and its name.
#[derive(Debug, Clone, PartialEq)]
struct Want {
    id: String,
    name: String,
}

/// A window of dino's that tmux has.
#[derive(Debug, Clone, PartialEq)]
struct Have {
    window: String,
    id: String,
    name: String,
}

/// The user's tmux: the binary and their default server's socket, when that server is running.
struct Server {
    bin: PathBuf,
    socket: PathBuf,
}

impl Server {
    /// The default server, if one is running. dino never starts one for this.
    fn find() -> Option<Self> {
        let bin = tmux_bin()?;
        let dir = std::env::var_os("TMUX_TMPDIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"));
        let socket = dir.join(format!("tmux-{}", unsafe { libc::getuid() })).join("default");
        if !socket.exists() {
            return None;
        }
        let t = Self { bin, socket };
        t.run(&["list-sessions", "-F", "#{session_id}"])?;
        Some(t)
    }

    /// `tmux -S <socket> args`: its output when it succeeded, given up on when the server doesn't
    /// answer (see [`crate::tmux::ask`]).
    fn run(&self, args: &[&str]) -> Option<String> {
        crate::tmux::ask(&self.bin, &self.socket, args)
    }

    /// dino's windows, wherever the user moved them.
    fn have(&self) -> Vec<Have> {
        let fmt = format!("#{{window_id}}\t#{{{MARK}}}\t#{{window_name}}");
        self.run(&["list-windows", "-a", "-F", &fmt]).map(|out| parse_windows(&out)).unwrap_or_default()
    }

    fn remove_all(&self) {
        for h in self.have() {
            self.run(&["kill-window", "-t", &h.window]);
        }
    }

    fn sync(&self, d: &Daemon, cfg: &Tmux) {
        let want = wanted(d);
        let have = self.have();
        let (remove, rename, add) = plan(&want, &have);
        for w in remove {
            self.run(&["kill-window", "-t", &w]);
        }
        for (w, name) in rename {
            self.run(&["rename-window", "-t", &w, &name]);
        }
        if add.is_empty() {
            return;
        }
        let Some(session) = self.target(cfg) else { return };
        for w in add {
            self.add(&session, &w);
        }
    }

    /// Where new windows go: the named session (made when needed), or the one the user was in
    /// last; `None` when that's asked for and nobody is attached.
    fn target(&self, cfg: &Tmux) -> Option<Target> {
        if cfg.session.is_empty() {
            let clients = self.run(&["list-clients", "-F", "#{client_activity}\t#{session_id}"])?;
            let id = clients.lines().filter_map(|l| l.split_once('\t')).max_by_key(|(at, _)| at.parse::<u64>().unwrap_or(0))?.1.to_string();
            return Some(Target::Existing(format!("{id}:")));
        }
        if !Tmux::valid_name(&cfg.session) {
            return None;
        }
        // `=name`: exactly this session, not one whose name starts with it.
        let exact = format!("={}", cfg.session);
        Some(match self.run(&["has-session", "-t", &exact]) {
            Some(_) => Target::Existing(format!("{exact}:")),
            None => Target::New(cfg.session.clone()),
        })
    }

    fn add(&self, target: &Target, w: &Want) {
        let command = attach_command(&w.id, std::env::var("DINO_HOME").ok().as_deref());
        let exact;
        let args: Vec<&str> = match target {
            Target::Existing(s) => vec!["new-window", "-d", "-P", "-F", "#{window_id}", "-t", s, "-n", &w.name, &command],

            // Its first window is this one; the session goes when its last window does. Nobody may
            // be attached to it: a config's `destroy-unattached` would end it as this command
            // returns (and it would be made again every look), so in the same command, it's off.
            Target::New(s) => {
                exact = format!("={s}:");
                vec!["new-session", "-d", "-P", "-F", "#{window_id}", "-s", s, "-n", &w.name, &command, ";", "set-option", "-t", &exact, "destroy-unattached", "off"]
            }
        };
        let Some(window) = self.run(&args).map(|o| o.trim().to_string()).filter(|w| !w.is_empty()) else { return };
        self.run(&["set-option", "-w", "-t", &window, MARK, &w.id]);
        // Its name follows the agent, from dino: not from what runs in it, nor its title.
        self.run(&["set-option", "-w", "-t", &window, "automatic-rename", "off"]);
        self.run(&["set-option", "-w", "-t", &window, "allow-rename", "off"]);
    }
}

/// Where a new window goes: `session:` of one that exists (the next free index there), or a
/// session to make.
enum Target {
    Existing(String),
    New(String),
}

/// The tmux on this Mac: on PATH, else where Homebrew and MacPorts put it (dinod started from the
/// app has a short PATH).
fn tmux_bin() -> Option<PathBuf> {
    let path = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect::<Vec<_>>()).unwrap_or_default();
    path.into_iter().chain(["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin"].map(PathBuf::from)).map(|d| d.join("tmux")).find(|p| p.is_file())
}

/// The command a window runs: this dino, attached to the session, from a clean screen; with this
/// dinod's own folder when it isn't the usual one. Through `env`, which any tmux and any
/// `default-shell` (fish too) runs the same way.
fn attach_command(id: &str, home: Option<&str>) -> String {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("dino"));
    let home = home.map(|h| format!("env {} ", quote(&format!("DINO_HOME={h}")))).unwrap_or_default();
    format!("{home}{} attach --fresh {id}", quote(&exe.display().to_string()))
}

fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// The agents to show: every session of dino's that runs an agent (shells are tabs, not agents).
fn wanted(d: &Daemon) -> Vec<Want> {
    d.sessions
        .lock()
        .unwrap()
        .iter()
        .filter(|s| s.agent_id != "shell")
        .map(|s| {
            let label = s.label.lock().unwrap().clone();
            Want { id: s.id.clone(), name: window_name(label.or_else(|| s.pane.title()).as_deref(), &s.name) }
        })
        .collect()
}

/// A window's name: the agent's title without the spinner or glyphs it puts first, else the
/// session's name; one line, not too long for a status bar.
fn window_name(title: Option<&str>, name: &str) -> String {
    let t = title.map(|t| t.trim_start_matches(|c: char| !c.is_alphanumeric()).trim()).filter(|t| !t.is_empty()).unwrap_or(name);
    let one: String = t.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let short: String = one.chars().take(30).collect();
    if one.chars().count() > 30 { format!("{}…", short.trim_end()) } else { short }
}

/// dino's windows from `list-windows -a` lines; windows without the mark aren't dino's.
fn parse_windows(out: &str) -> Vec<Have> {
    out.lines()
        .filter_map(|l| {
            let mut f = l.splitn(3, '\t');
            let window = f.next()?.to_string();
            let id = f.next()?.to_string();
            let name = f.next().unwrap_or("").to_string();
            (!id.is_empty()).then_some(Have { window, id, name })
        })
        .collect()
}

/// What to do: windows to remove (their session is gone, or a second window for one), windows to
/// rename, and sessions to add a window for.
fn plan(want: &[Want], have: &[Have]) -> (Vec<String>, Vec<(String, String)>, Vec<Want>) {
    let mut remove = vec![];
    let mut rename = vec![];
    let mut shown = std::collections::HashSet::new();
    for h in have {
        match want.iter().find(|w| w.id == h.id) {
            Some(w) if shown.insert(h.id.clone()) => {
                if w.name != h.name {
                    rename.push((h.window.clone(), w.name.clone()));
                }
            }
            _ => remove.push(h.window.clone()),
        }
    }
    let add = want.iter().filter(|w| !shown.contains(&w.id)).cloned().collect();
    (remove, rename, add)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn want(id: &str, name: &str) -> Want {
        Want { id: id.into(), name: name.into() }
    }

    fn have(window: &str, id: &str, name: &str) -> Have {
        Have { window: window.into(), id: id.into(), name: name.into() }
    }

    #[test]
    fn windows_follow_the_agents() {
        let w = [want("1", "fix tests"), want("2", "claude"), want("3", "codex")];
        let h = [have("@1", "1", "fix tests"), have("@2", "2", "old name"), have("@5", "9", "gone"), have("@6", "1", "fix tests")];
        let (remove, rename, add) = plan(&w, &h);
        // Session 9 is gone, and session 1 has a second window: both go.
        assert_eq!(remove, ["@5", "@6"]);
        assert_eq!(rename, [("@2".to_string(), "claude".to_string())]);
        assert_eq!(add, [want("3", "codex")]);
        // Nothing to do when they match.
        let (r, n, a) = plan(&w[..1], &h[..1]);
        assert!(r.is_empty() && n.is_empty() && a.is_empty());
    }

    #[test]
    fn only_marked_windows_are_dinos() {
        let out = "@1\t\tzsh\n@2\t4\tclaude: fix the build\n@3\t\t\n";
        assert_eq!(parse_windows(out), [have("@2", "4", "claude: fix the build")]);
    }

    #[test]
    fn names_fit_a_status_bar() {
        assert_eq!(window_name(Some("⠋ Fixing the flaky test"), "claude"), "Fixing the flaky test");
        assert_eq!(window_name(Some("✳ "), "claude-2"), "claude-2");
        assert_eq!(window_name(None, "codex"), "codex");
        assert_eq!(window_name(Some("a very long title that goes on and on and on"), "x"), "a very long title that goes on…");
        assert_eq!(window_name(Some("two\nlines"), "x"), "two lines");
    }

    #[test]
    fn windows_run_this_dino() {
        let exe = std::env::current_exe().unwrap().display().to_string();
        assert_eq!(attach_command("7", None), format!("'{exe}' attach --fresh 7"));
        assert_eq!(attach_command("7", Some("/tmp/it's here")), format!("env 'DINO_HOME=/tmp/it'\\''s here' '{exe}' attach --fresh 7"));
    }

    #[test]
    fn session_names_are_plain() {
        assert!(Tmux::valid_name("dino") && Tmux::valid_name("my-agents.2"));
        assert!(!Tmux::valid_name("") && !Tmux::valid_name("a b") && !Tmux::valid_name("a:b") && !Tmux::valid_name("$(x)"));
    }
}
