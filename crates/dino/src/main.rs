mod account;
mod ai;
mod automations;
mod client;
mod launchd;
mod mcp;
mod out;
mod search;
mod shell;
mod stats;

use std::time::Instant;

use crossterm::terminal;

use dino_core::discover;
use dino_core::ipc::{Request, Response, SessionInfo};
use dino_core::providers::ProviderRoute;
use dino_core::status::{self, Status as SessionStatus};
use out::{Cell, Column, Paint};

fn tokens(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => format!("{:.1}k", n as f64 / 1e3),
        _ => format!("{:.1}M", n as f64 / 1e6),
    }
}

fn duration(secs: u64) -> String {
    match secs {
        0..3600 => format!("{}m", secs / 60),
        3600..86400 => format!("{}h{:02}m", secs / 3600, secs % 3600 / 60),
        _ => format!("{}d{}h", secs / 86400, secs % 86400 / 3600),
    }
}

/// `s` safe to print to a terminal: control characters (C0, DEL and C1), with which a title or
/// a folder name could move the cursor, retitle the window or write to the clipboard, become `?`.
fn printable(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { '?' } else { c }).collect()
}

/// What `dino --help` says before the full list: what dino is, and where to start.
const INTRO: &str = "dino runs your coding agents (Claude Code, Codex, …) and keeps them running:
in the dino app, or here.

  dino                the dino app; in a dino session or piped, the sessions
  dino claude         start Claude Code in this folder, or any agent dino knows
  dino .              a shell in this folder, in the dino app
  dino status         what's working and what needs you
  dino found          agents already running on this Mac, in any terminal
";

const USAGE: &str = "Sessions
  dino [<agent> [args...]]          the app; with an agent, that agent in this folder
  dino <folder> [<agent> [args...]] a shell or that agent there, in the dino app
  dino ls [--usage] [--json]        every session, what needs you first
  dino status [--tmux]              in a line; --tmux for tmux's status-right
  dino new [--worktree] [--stay] <agent> [--on <provider> <model>] [args...]
                                    start one in the background; prints its id
  dino attach | resume | kill <id>
  dino rm [--force] <id>            delete it, and the worktree dino made for it
  dino found [--all] [--json]       agents dino didn't start, to continue here
  dino continue <id>                continue one of those in dino
  dino stats [--range 7d|30d|all] [--json]
                                    usage across every agent: tokens, models, streaks

Fan-out: one prompt to several agents, a git worktree each
  dino fan [--agents claude,codex,...] <prompt>
  dino groups | diff <id> | keep <id> | discard <group>

Automations: dino does something by itself when something happens
  dino automations [show|add|edit|run|pause|resume|rm] …
                                    on a schedule, a PR, failed CI, changed files, …

Setup
  dino login [--email | --device] | logout | sync [status|now|resolve|undo]
  dino login openrouter|chatgpt     connect a provider in your browser
  dino login <plan> [--base <url>]  connect a coding plan with its key, read from stdin
  dino claude-token [status|create|set|remove|add-account|remove-account <n>]
  dino fallback [<agent> [<provider>:<model>... | off] [--outages] [--new-sessions <agent>[:<model>]]]
                                    where an agent goes when it hits a limit
  dino power [status|setup|remove]  keep agents running with the lid closed
  dino init zsh|bash|fish | shell install|uninstall [zsh|bash|fish]
  dino ai suggest|agent -- <request> | search [--json|--pick]
                                    the shell's AI line and history search
  dino mcp [--read-only]            an MCP server on stdio, for agents
  dino ping | stop | daemon | --version

`dino <command> --help` says more about ls, rm, found, stats, login, fan and automations.";

/// The build this is: the commit app/build.sh and scripts/release.sh built it from. Read here, in
/// the crate built last, so a new commit recompiles only this one.
const BUILD: Option<&str> = option_env!("DINO_BUILD");

fn main() {
    // Piped into `head`, stop quietly when it has enough, as other commands do; Rust otherwise
    // ignores SIGPIPE and the next print panics. Not dinod: a client hanging up mustn't end it.
    if !matches!(std::env::args().nth(1).as_deref(), Some("daemon" | "lid-watchdog")) {
        unsafe {
            libc::signal(libc::SIGPIPE, libc::SIG_DFL);
        }
    }
    if let Err(e) = dino() {
        out::error(&e);
        std::process::exit(1);
    }
}

fn dino() -> anyhow::Result<()> {
    let cli: Vec<String> = std::env::args().skip(1).collect();
    match cli.first().map(String::as_str) {
        Some("daemon") => return dino_daemon::run(BUILD),
        Some("lid-watchdog") => {
            dino_daemon::lid_watchdog(cli.get(1).and_then(|p| p.parse().ok()).unwrap_or(0));
            return Ok(());
        }
        Some("power") => return cmd_power(cli.get(1).map(String::as_str).unwrap_or("status")),
        Some("claude-token") => return cmd_claude_token(cli.get(1).map(String::as_str).unwrap_or("status"), cli.get(2).map(String::as_str)),
        Some("fallback") => return cmd_fallback(&cli[1..]),
        Some("attach") => {
            let fresh = cli.iter().any(|a| a == "--fresh");
            let id = cli.iter().skip(1).find(|a| *a != "--fresh").ok_or_else(|| anyhow::anyhow!("usage: dino attach <id>\n`dino ls` lists the sessions."))?;
            return client::attach_raw(id, fresh);
        }
        Some("ls") => return cmd_ls(&cli[1..]),
        Some("status") => return cmd_status(cli.iter().any(|a| a == "--tmux")),
        Some("mcp") => return mcp::serve(cli.iter().any(|a| a == "--read-only")),
        // Wired in by dinod around the user's own statusline (see `dino_core::statusline`).
        Some("statusline") => std::process::exit(dino_core::statusline::run(cli.get(1).map(String::as_str))),
        Some("--version" | "-V" | "version") => {
            match BUILD {
                Some(build) => println!("dino {} ({build})", env!("CARGO_PKG_VERSION")),
                None => println!("dino {}", env!("CARGO_PKG_VERSION")),
            }
            return Ok(());
        }
        Some("found") => return cmd_found(&cli[1..]),
        Some("stats") => return stats::run(&cli[1..]),
        Some("ai") => return ai::run(&cli[1..]),
        Some("search") => return search::run(&cli[1..]),
        Some("init") => return shell::init(cli.get(1).map(String::as_str)),
        Some("shell") => return shell::run(&cli[1..]),
        Some("login") if matches!(cli.get(1).map(String::as_str), Some("openrouter" | "chatgpt")) => return cmd_login(cli.get(1).map(String::as_str)),
        Some("login") if cli.get(1).is_some_and(|p| plan_id(p).is_some()) => return cmd_login_plan(&cli[1..]),
        Some("login") => return account::login(&cli[1..]),
        Some("logout") => {
            let Some(provider) = cli.get(1).cloned() else { return account::logout() };
            let provider = plan_id(&provider).unwrap_or(provider);
            done(client::request(&Request::DisconnectProvider { provider: provider.clone() })?)?;
            say(&format!("Disconnected {}.", printable(&provider)));
            return Ok(());
        }
        Some("sync") => return account::sync(&cli[1..]),
        Some("fan") => return cmd_fan(&cli[1..]),
        Some("automations" | "automation") => return automations::run(&cli[1..]),
        Some("groups") => return cmd_groups(),
        Some("diff") => {
            let session = cli.get(1).ok_or_else(|| anyhow::anyhow!("usage: dino diff <id>\n`dino groups` lists fan-outs and their sessions."))?.clone();
            let Response::Diff { text, .. } = client::request(&Request::Diff { session })? else { return Err(unexpected()) };
            print!("{text}");
            return Ok(());
        }
        Some("keep") => {
            let session = cli.get(1).ok_or_else(|| anyhow::anyhow!("usage: dino keep <session id>\n`dino groups` lists fan-outs and their sessions."))?.clone();
            done(client::request(&Request::Keep { session: session.clone() })?)?;
            say(&format!("Applied session {}'s changes to your checkout, and closed its group.", printable(&session)));
            return Ok(());
        }
        Some("discard") => {
            let group = cli.get(1).ok_or_else(|| anyhow::anyhow!("usage: dino discard <group>\n`dino groups` lists them."))?.clone();
            done(client::request(&Request::Discard { group: group.clone() })?)?;
            say(&format!("Closed group {}: its agents are stopped and their worktrees removed.", printable(&group)));
            return Ok(());
        }
        Some("continue") => return cmd_continue(cli.get(1).ok_or_else(|| anyhow::anyhow!("usage: dino continue <session-id prefix>"))?),
        // Start dinod if needed; used by the app before it attaches surfaces.
        Some("ping") => {
            client::connect()?;
            println!("{}", dino_core::ipc::socket_path().display());
            return Ok(());
        }
        Some("new") => {
            // Its own flags, before the agent: everything after the agent is the agent's.
            let flags = cli[1..].iter().take_while(|a| matches!(a.as_str(), "-w" | "--worktree" | "--stay")).count();
            let worktree = cli[1..=flags].iter().any(|a| a == "-w" || a == "--worktree");
            let stay = cli[1..=flags].iter().any(|a| a == "--stay");
            let rest = &cli[1 + flags..];
            let agent = rest.first().ok_or_else(|| anyhow::anyhow!("usage: dino new [--worktree] [--stay] <agent> [--on <provider> <model>] [args...]"))?.clone();
            let (cols, rows) = terminal::size().unwrap_or((120, 40));
            let cwd = std::env::current_dir().ok().map(|p| p.display().to_string());
            // `--on <provider> <model>`: a provider's model instead of the agent's own account.
            let (route, args) = match &rest[1..] {
                [on, provider, model, args @ ..] if on == "--on" => (Some(ProviderRoute { provider: provider.clone(), model: model.clone(), format: None, name: String::new() }), args.to_vec()),
                args => (None, args.to_vec()),
            };
            let req = Request::New { launcher: agent.clone(), args, cwd, cols, rows, worktree, controls: Default::default(), host: None, prompt: None, by: None, route, reveal: false, tmux: None, stay };
            let id = created(client::request(&req)?)?;
            // Piped, only the id, for `id=$(dino new claude)`.
            if out::tty() {
                // Its agent was at its limit: another one started (Settings → Agents).
                if let Ok(Response::State { sessions, .. }) = client::request(&Request::State)
                    && let Some(s) = sessions.iter().find(|s| s.id == id)
                    && let Some(why) = &s.instead_of
                {
                    let until = why.resets_at.map(|t| format!(", back in {}", duration(t.saturating_sub(out::now())))).unwrap_or_default();
                    println!("{} is at its limit ({}{until}): started {} instead. `dino new --stay {}` starts it anyway.", agent_name(&why.agent_id), printable(&why.name), agent_name(&s.agent_id), printable(&agent));
                }
                println!("Started {} as session {id}. `dino attach {id}` opens it here.", printable(&agent));
            } else {
                println!("{id}");
            }
            return Ok(());
        }
        Some("kill") => {
            let id = cli.get(1).ok_or_else(|| anyhow::anyhow!("usage: dino kill <id>\n`dino ls` lists the sessions."))?.clone();
            done(client::request(&Request::Kill { id: id.clone() })?)?;
            say(&format!("Closed session {}.", printable(&id)));
            return Ok(());
        }
        Some("rm") => return cmd_rm(&cli[1..]),
        Some("resume") => {
            let id = cli.get(1).ok_or_else(|| anyhow::anyhow!("usage: dino resume <id>\n`dino ls` lists the sessions; Ended ones resume."))?.clone();
            done(client::request(&Request::Resume { id: id.clone() })?)?;
            say(&format!("Resumed session {id}. `dino attach {id}` opens it here.", id = printable(&id)));
            return Ok(());
        }
        Some("stop") => {
            if std::os::unix::net::UnixStream::connect(dino_core::ipc::socket_path()).is_err() {
                println!("dinod isn't running.");
                return Ok(());
            }
            done(client::request(&Request::Shutdown)?)?;
            say("Stopped dinod. Its sessions resume when it next starts.");
            return Ok(());
        }
        Some(arg) if is_folder(arg) => return cmd_open(arg, &cli[1..]),
        Some("-h" | "--help" | "help") => {
            println!("{INTRO}\n{USAGE}");
            return Ok(());
        }
        _ => {}
    }
    let Some((agent, args)) = cli.split_first() else { return cmd_home() };
    // `dino claude`: that agent in this folder, as `dino . claude`.
    let Response::Launchers { launchers } = client::request(&Request::Launchers)? else { return Err(unexpected()) };
    let agent = agent.to_lowercase();
    let launcher = launchers.iter().find(|l| l.short == agent).or_else(|| launchers.iter().find(|l| l.label.to_lowercase().starts_with(&agent)));
    let Some(launcher) = launcher else { return Err(unknown_agent(&agent, true)) };
    cmd_open(".", &[&[launcher.short.clone()], args].concat())
}

/// `dino` on its own opens the app, as `code` and `zed` open theirs. Inside a dino session (the app
/// is in front already), piped, or with no app to open: the sessions, and where to go from there.
fn cmd_home() -> anyhow::Result<()> {
    let inside = std::env::var_os("DINO_SESSION").is_some_and(|s| !s.is_empty());
    if !inside && out::tty() && open_app() {
        return Ok(());
    }
    let sessions = sessions()?;
    if sessions.is_empty() {
        println!("No sessions.");
    } else {
        let (cols, rows) = ls_table(&sessions, false);
        print!("{}", out::grid(&cols, &rows));
    }
    println!();
    let w = HOME_COMMANDS.iter().map(|(c, _)| c.len()).max().unwrap_or(0);
    for (command, what) in HOME_COMMANDS {
        println!("  {command:w$}  {what}");
    }
    println!("\n{}", out::paint("`dino --help` for the rest.", Paint::Dim));
    Ok(())
}

/// What plain `dino` suggests under the sessions: the commands used most.
const HOME_COMMANDS: &[(&str, &str)] = &[
    ("dino claude", "start Claude Code in this folder, or any agent dino knows"),
    ("dino .", "a shell in this folder"),
    ("dino attach <id>", "a session, in this terminal"),
    ("dino status", "what's working and what needs you"),
    ("dino found", "agents already running on this Mac, in any terminal"),
];

/// Bring the dino app to the front, opening it if it isn't running. False without one installed.
fn open_app() -> bool {
    cfg!(target_os = "macos")
        && std::process::Command::new("open")
            .args(["-b", "dev.dino.app"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
}

/// `.`, `~/x`, `a/b`: a folder, not an agent's name. A bare word is one only when it's a folder
/// here and no agent is called that.
fn is_folder(arg: &str) -> bool {
    if arg == "." || arg == ".." || arg.starts_with('~') || arg.contains('/') {
        return true;
    }
    !arg.starts_with('-')
        && std::path::Path::new(arg).is_dir()
        && !matches!(client::request(&Request::Launchers), Ok(Response::Launchers { launchers }) if launchers.iter().any(|l| l.short == arg))
}

/// `dino <folder> [agent [args...]]`: a session in that folder, shown in the terminal app. Inside
/// it, the app switches to it; elsewhere this opens the app, or attaches here if there isn't one.
fn cmd_open(folder: &str, rest: &[String]) -> anyhow::Result<()> {
    let expanded = match folder.strip_prefix('~') {
        Some(tail) => format!("{}{tail}", std::env::var("HOME").unwrap_or_default()),
        None => folder.to_string(),
    };
    let path = std::fs::canonicalize(&expanded).map_err(|e| anyhow::anyhow!("{folder}: {e}"))?;
    anyhow::ensure!(path.is_dir(), "{folder} isn't a folder");
    let (cols, rows) = terminal::size().unwrap_or((120, 40));
    let req = Request::New {
        launcher: rest.first().cloned().unwrap_or_else(|| "shell".into()),
        args: rest.get(1..).unwrap_or_default().to_vec(),
        cwd: Some(path.display().to_string()),
        cols,
        rows,
        worktree: false,
        controls: Default::default(),
        host: None,
        prompt: None,
        by: None,
        route: None,
        reveal: true,
        tmux: None,
        stay: false,
    };
    let id = match client::request(&req)? {
        Response::Created { id } => id,
        Response::Error { message } => return Err(hinted(message)),
        _ => return Err(unexpected()),
    };
    if std::env::var_os("DINO_SESSION").is_some_and(|s| !s.is_empty()) {
        return Ok(());
    }
    if open_app() { Ok(()) } else { client::attach_raw(&id, false) }
}

/// What a command did, in a sentence, on a terminal; piped, it says nothing, the way `cp` doesn't.
fn say(what: &str) {
    if out::tty() {
        println!("{what}");
    }
}

/// dinod's answer to a command that makes something: its id.
fn created(resp: Response) -> anyhow::Result<String> {
    match resp {
        Response::Created { id } => Ok(id),
        Response::Error { message } => Err(hinted(message)),
        _ => Err(unexpected()),
    }
}

/// dinod's answer to a command that only does something.
fn done(resp: Response) -> anyhow::Result<()> {
    match resp {
        Response::Ok => Ok(()),
        Response::Error { message } => Err(hinted(message)),
        _ => Err(unexpected()),
    }
}

/// A reply this dino doesn't know: dinod is a different version.
fn unexpected() -> anyhow::Error {
    anyhow::anyhow!("dinod answered in a way this dino doesn't understand: it's probably another version.\n`dino stop` stops it (its sessions resume), and the next command starts this one's.")
}

/// dinod's error, with what to do about it when the command line knows better.
fn hinted(message: String) -> anyhow::Error {
    if message.starts_with("no session ") {
        return anyhow::anyhow!("{message}\n`dino ls` lists them.");
    }
    if let Some(name) = message.strip_prefix("unknown agent ") {
        return unknown_agent(name, false);
    }
    anyhow::anyhow!(message)
}

/// `name` isn't an agent dino can start: how to install it, when it's one dino knows, or the ones it
/// can. `command`: it was the first word, so it could have been meant as a command.
fn unknown_agent(name: &str, command: bool) -> anyhow::Error {
    let name = printable(name);
    if let Some(k) = dino_core::KNOWN_AGENTS.iter().find(|k| k.id == name || k.bin == name || k.was.contains(&&*name)) {
        let hint = discover::install_hint(k.id);
        return anyhow::anyhow!("{} isn't installed (no `{}` on the PATH). Install it with\n\n    {hint}\n\nthen run this again.", k.name, k.bin);
    }
    let agents = match client::request(&Request::Launchers) {
        Ok(Response::Launchers { launchers }) => launchers.into_iter().map(|l| l.short).collect::<Vec<_>>().join(", "),
        _ => String::new(),
    };
    if command {
        anyhow::anyhow!("`{name}` isn't a dino command or an agent dino knows.\nAgents here: {agents}. `dino --help` lists the commands.")
    } else {
        anyhow::anyhow!("`{name}` isn't an agent dino knows. Agents here: {agents}.")
    }
}

/// What the agents are up to, in a line: `dino status --tmux` is short, for tmux's status bar
/// (`set -g status-right '#(dino status --tmux)'`), and prints nothing when nothing needs saying.
/// Without it, the agents that need you, with what they ask, and those working, under the line.
/// It never starts dinod: a status bar asks every few seconds.
fn cmd_status(tmux: bool) -> anyhow::Result<()> {
    if std::os::unix::net::UnixStream::connect(dino_core::ipc::socket_path()).is_err() {
        if !tmux {
            println!("No agents: dinod isn't running. Start one with `dino claude`.");
        }
        return Ok(());
    }
    let Response::State { sessions, power, .. } = client::request(&Request::State)? else { return Err(unexpected()) };
    let mut agents: Vec<_> = sessions.iter().filter(|s| status::is_agent(s) && !s.exited).map(|s| (SessionStatus::of(s), s)).collect();
    agents.sort_by_key(|(st, s)| (*st, id_order(&s.id)));
    let count = |want: SessionStatus| agents.iter().filter(|(st, _)| *st == want).count();
    let line = status_line(tmux, agents.len(), count(SessionStatus::NeedsYou), count(SessionStatus::Working));
    if tmux {
        println!("{line}");
        return Ok(());
    }
    let cols = [Column::keep("ID"), Column::keep("STATUS"), Column::end("NAME", 12), Column::end("ASKS", 12)];
    let rows: Vec<_> = agents
        .iter()
        .filter(|(st, _)| matches!(st, SessionStatus::NeedsYou | SessionStatus::Working))
        .map(|(st, s)| vec![Cell::new(printable(&s.id)), Cell::status(*st), Cell::new(name(s)), Cell::new(printable(status::needs(s).unwrap_or("")))])
        .collect();
    if out::tty() {
        println!("{line}");
        if !rows.is_empty() {
            print!("\n{}", out::table(&cols, &rows, false).lines().map(|l| format!("  {l}\n")).collect::<String>());
        }
        let asking: Vec<_> = agents.iter().filter(|(st, _)| *st == SessionStatus::NeedsYou).collect();
        match asking.as_slice() {
            [] => {}
            [(_, s)] => println!("\n{}", out::paint(&format!("`dino attach {}` to answer it.", printable(&s.id)), Paint::Dim)),
            _ => println!("\n{}", out::paint("`dino attach <id>` to answer one.", Paint::Dim)),
        }
        // What keeps the Mac awake, as the sidebar's foot says it; `dino power` lists them all.
        let session_name = |id: &str| sessions.iter().find(|s| s.id == id).map(awake_session_name);
        if let Some(awake) = power.and_then(|p| p.awake_line(session_name)) {
            println!("\n{}", out::paint(&format!("{awake}  (`dino power` for who)"), Paint::Dim));
        }
    } else {
        println!("{line}");
        print!("{}", out::table(&cols, &rows, false));
    }
    Ok(())
}

fn status_line(tmux: bool, agents: usize, needs: usize, working: usize) -> String {
    let mut parts = vec![];
    if needs > 0 {
        parts.push(format!("{needs} {}", if needs == 1 { "needs you" } else { "need you" }));
    }
    if working > 0 {
        parts.push(format!("{working} working"));
    }
    match (tmux, parts.is_empty()) {
        (true, true) => String::new(),
        (true, false) => format!("dino: {}", parts.join(" · ")),
        (false, true) if agents == 0 => "No agents running. Start one with `dino claude`.".into(),
        (false, true) => format!("{agents} {}, none working", if agents == 1 { "agent" } else { "agents" }),
        (false, false) => format!("{agents} {}: {}", if agents == 1 { "agent" } else { "agents" }, parts.join(", ")),
    }
}

/// Sessions in the order dinod numbered them: `10` after `9`.
fn id_order(id: &str) -> (usize, String) {
    (id.parse().unwrap_or(usize::MAX), id.to_string())
}

/// What the app calls a session: the name you gave it, else what its agent calls the conversation,
/// else its own name (`claude-2`).
fn name(s: &SessionInfo) -> String {
    let title = |t: &str| {
        let t = t.trim_start_matches(|c: char| !(c.is_alphanumeric() || matches!(c, '~' | '/' | '.'))).trim();
        (!t.is_empty()).then(|| t.to_string())
    };
    // Until it has a topic, an agent's title is only its own name ("Claude Code").
    let topic = |t: String| {
        let words: Vec<_> = t.split_whitespace().collect();
        let only_agent = words.len() <= 2 && words.first().is_some_and(|w| s.agent_id.to_lowercase().starts_with(&w.to_lowercase()));
        (!only_agent).then_some(t)
    };
    let shown = s.label.clone().or_else(|| match &s.inside {
        Some(f) => title(&f.title),
        None if s.agent_id != "shell" => s.title.as_deref().and_then(title).and_then(topic),
        None => None,
    });
    printable(&shown.unwrap_or_else(|| s.name.clone()))
}

/// Where a session runs: its folder (a shell's, where it is now), `host:` before it over SSH.
fn folder(s: &SessionInfo) -> String {
    let path = s.shell_cwd.as_deref().unwrap_or(&s.cwd);
    printable(&match &s.host {
        Some(h) => format!("{h}:{path}"),
        None => out::short_path(path),
    })
}

const LS_HELP: &str = "usage: dino ls [--usage] [--json]

Every session dinod runs, what needs you first, then what's working, done, idle and ended.
Statuses are the app's, worked out the same way; a turn that ended is Done until the next one.

  -u, --usage   add the tokens each one has used, in and out
      --json    a JSON array for scripts, one object per session, with
                  id, name, agent, status (needs_you, working, done, idle, ended, exited),
                  needs (what it asks for, or null), folder, host (or null),
                  last_active (ISO 8601 UTC, or null), input_tokens, output_tokens
                Fields may be added; these keep their names and meanings.

Piped, it prints a tab-separated line per session and no header: id, name, agent, status,
folder (in full) and last active, with the values --json has.";

/// `dino ls [--usage] [--json]`: every session, what wants you first.
fn cmd_ls(args: &[String]) -> anyhow::Result<()> {
    let usage = args.iter().any(|a| a == "-u" || a == "--usage");
    let json = args.iter().any(|a| a == "--json");
    if let Some(a) = args.iter().find(|a| !matches!(a.as_str(), "-u" | "--usage" | "--json")) {
        if matches!(a.as_str(), "-h" | "--help") {
            println!("{LS_HELP}");
            return Ok(());
        }
        anyhow::bail!("dino ls doesn't take `{}`\n`dino ls --help` says what it does.", printable(a));
    }
    let sessions = sessions()?;
    if json {
        let list: Vec<_> = sessions
            .iter()
            .map(|s| {
                serde_json::json!({
                    "id": s.id,
                    "name": name(s),
                    "agent": s.inside.as_ref().map_or(&s.agent_id, |f| &f.agent),
                    "status": SessionStatus::of(s).key(),
                    "needs": status::needs(s),
                    "folder": s.shell_cwd.as_deref().unwrap_or(&s.cwd),
                    "host": s.host,
                    "last_active": last_active(s).map(out::iso),
                    "input_tokens": s.input_tokens,
                    "output_tokens": s.output_tokens,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&list)?);
        return Ok(());
    }
    if sessions.is_empty() {
        eprintln!("No sessions. Start one with `dino claude`, or `dino .` for a shell here.");
        return Ok(());
    }
    let (cols, rows) = ls_table(&sessions, usage);
    print!("{}", out::table(&cols, &rows, true));
    Ok(())
}

/// Every session dinod runs, what needs you first. Never starts dinod: without it there are none.
fn sessions() -> anyhow::Result<Vec<SessionInfo>> {
    if std::os::unix::net::UnixStream::connect(dino_core::ipc::socket_path()).is_err() {
        return Ok(vec![]);
    }
    let Response::State { mut sessions, .. } = client::request(&Request::State)? else { return Err(unexpected()) };
    sessions.sort_by_key(|s| (SessionStatus::of(s), id_order(&s.id)));
    Ok(sessions)
}

/// When a session last printed something, in seconds since the epoch.
fn last_active(s: &SessionInfo) -> Option<u64> {
    s.output_ms_ago.map(|ms| (out::now() * 1000).saturating_sub(ms) / 1000)
}

/// `dino ls`'s table: plain `dino` shows it too.
fn ls_table(sessions: &[SessionInfo], usage: bool) -> (Vec<Column>, Vec<Vec<Cell>>) {
    let mut cols = vec![Column::keep("ID"), Column::end("NAME", 12), Column::keep("AGENT"), Column::keep("STATUS"), Column::path("FOLDER", 12), Column::keep("ACTIVE")];
    if usage {
        cols.extend([Column::right("IN"), Column::right("OUT")]);
    }
    let rows: Vec<_> = sessions
        .iter()
        .map(|s| {
            let st = SessionStatus::of(s);
            let agent = printable(s.inside.as_ref().map_or(&s.agent_id, |f| &f.agent));
            let when = last_active(s).map_or_else(|| Cell::new("-").raw(""), |t| Cell::new(out::ago(out::now().saturating_sub(t))).raw(out::iso(t)));
            let mut row = vec![
                Cell::new(printable(&s.id)),
                Cell::new(name(s)).paint(if st == SessionStatus::NeedsYou { Paint::Bold } else { Paint::Plain }),
                Cell::new(agent),
                Cell::status(st),
                Cell::new(folder(s)).raw(printable(s.shell_cwd.as_deref().unwrap_or(&s.cwd))),
                when.paint(Paint::Dim),
            ];
            if usage {
                row.push(Cell::new(tokens(s.input_tokens)).raw(s.input_tokens.to_string()));
                row.push(Cell::new(tokens(s.output_tokens)).raw(s.output_tokens.to_string()));
            }
            row
        })
        .collect();
    (cols, rows)
}

/// `dino power [status|setup|remove]`: what keeps the Mac awake, and keeping agents running with
/// the lid closed.
fn cmd_power(action: &str) -> anyhow::Result<()> {
    if !matches!(action, "status" | "setup" | "remove") {
        println!("usage: dino power [status|setup|remove]\n\nstatus says what keeps the Mac awake now: dino while agents work, an agent's caffeinate, an app.\nsetup asks for an administrator's password once, so dino can keep the Mac awake with its lid closed while agents work (Settings → Power).");
        return Ok(());
    }
    let p = match client::request(&Request::Power { action: action.into() })? {
        Response::Power { power } => power,
        Response::Error { message } => return Err(hinted(message)),
        _ => return Err(unexpected()),
    };
    let sessions = match client::request(&Request::State) {
        Ok(Response::State { sessions, .. }) => sessions,
        _ => vec![],
    };
    let session_name = |id: &str| sessions.iter().find(|s| s.id == id).map(awake_session_name);
    let machine = dino_core::settings::Settings::load().machine;
    let mut rows = vec![
        ("Now", p.awake_line(session_name).unwrap_or_else(|| "nothing keeps the Mac awake; it sleeps when idle".into())),
        ("While agents work", if machine.awake_while_working { "dino keeps the Mac awake".to_string() } else { "the Mac may sleep (Settings → Power keeps it awake)".into() }),
        ("Lid closed", if machine.lid.enabled { "agents keep running".to_string() } else { "the Mac sleeps (Settings → Power keeps agents running)".into() }),
        ("Permission", if p.ready == Some(true) { "set up".into() } else { "not set up: `dino power setup`".into() }),
    ];
    if p.external {
        rows.push(("Sleep", "off, but not by dino; dino leaves it alone".into()));
    }
    if let Some(note) = &p.note {
        rows.push(("Last time", printable(note)));
    }
    if let Some(e) = &p.error {
        rows.push(("Error", printable(e)));
    }
    print!("{}", out::fields(&rows));
    if !p.awake.is_empty() {
        let cols = [Column::keep("PROCESS"), Column::right("PID"), Column::end("SESSION", 10), Column::keep("KEEPS AWAY"), Column::keep("SINCE"), Column::end("SAYS", 12)];
        let now = out::now();
        let rows: Vec<_> = p
            .awake
            .iter()
            .map(|h| {
                // macOS's own, dim: not something you started.
                let paint = if h.system { Paint::Dim } else { Paint::Plain };
                vec![
                    Cell::new(printable(&h.process)).paint(if h.ours { Paint::Green } else { paint }),
                    Cell::new(h.pid.to_string()).paint(paint),
                    Cell::new(h.session.as_deref().and_then(session_name).unwrap_or_default()).raw(h.session.clone().unwrap_or_default()),
                    Cell::new(h.kind_label()).raw(h.kind.clone()).paint(paint),
                    Cell::new(h.since.map(|t| out::ago(now.saturating_sub(t))).unwrap_or_default()).raw(h.since.map(out::iso).unwrap_or_default()).paint(paint),
                    Cell::new(printable(&h.name)).paint(paint),
                ]
            })
            .collect();
        if out::tty() {
            print!("\n{}", out::table(&cols, &rows, true).lines().map(|l| format!("  {l}\n")).collect::<String>());
        } else {
            print!("{}", out::table(&cols, &rows, false));
        }
    }
    Ok(())
}

/// A session as the staying-awake line names it: "Claude Code: Fix the build", a shell by its name.
fn awake_session_name(s: &SessionInfo) -> String {
    let agent = s.inside.as_ref().map_or(s.agent_id.as_str(), |f| f.agent.as_str());
    if agent == "shell" { name(s) } else { format!("{}: {}", agent_name(agent), name(s)) }
}

/// An agent's name as people know it: "Claude Code" for `claude`.
fn agent_name(id: &str) -> String {
    dino_core::KNOWN_AGENTS.iter().find(|k| k.id == id).map_or_else(|| printable(id), |k| k.name.to_string())
}

const FALLBACK_USAGE: &str = "usage: dino fallback [<agent> [<provider>:<model>... | off] [--outages] [--new-sessions <agent>[:<model>]]]

When an agent's route hits its limit (its plan's window, its subscription's limit, its balance), dino
sends its calls on to these routes, in order, until it resets: the agent never sees the failure.
Providers are as Settings → Models & Providers lists them (`dino login` connects them): plan-zai,
openrouter, ollama, chatgpt, …, and free (the free models, when they're on). Each must serve the API
the agent speaks. --outages also falls back when a route is down. --new-sessions starts new sessions
and scheduled tasks with another agent while this one is at its limit.

  dino fallback claude plan-zai:glm-4.6 ollama:qwen3:4b --new-sessions codex
  dino fallback codex off";

/// `dino fallback`: Settings → Agents' \"When it hits a limit\", for scripts.
fn cmd_fallback(args: &[String]) -> anyhow::Result<()> {
    use dino_core::settings::{AgentSwitch, Fallback, FallbackStep};
    if args.first().is_some_and(|a| a == "--help" || a == "-h") {
        println!("{FALLBACK_USAGE}");
        return Ok(());
    }
    let (mut settings, locked) = match client::request(&Request::Settings)? {
        Response::Settings { settings, locked, .. } => (settings, locked),
        Response::Error { message } => return Err(hinted(message)),
        _ => return Err(unexpected()),
    };
    let names: std::collections::HashMap<String, String> = match client::request(&Request::Providers)? {
        Response::Providers { providers } => providers.into_iter().map(|p| (p.id, p.name)).chain([("free".to_string(), "free models".to_string())]).collect(),
        _ => Default::default(),
    };
    let provider_name = |id: &str| names.get(id).cloned().unwrap_or_else(|| printable(id));
    let Some(agent) = args.first() else {
        if settings.fallbacks.values().all(|f| f.steps.is_empty() && f.new_sessions.is_none()) {
            println!("No agent falls back to anything. {}", FALLBACK_USAGE.lines().next().unwrap_or_default());
        }
        for id in settings.fallbacks.keys() {
            print!("{}", show_fallback(&settings, id, &provider_name));
        }
        return Ok(());
    };
    let agent = dino_core::KNOWN_AGENTS.iter().find(|k| k.id == agent || k.bin == agent).map(|k| k.id.to_string()).ok_or_else(|| unknown_agent(agent, false))?;
    let rest = &args[1..];
    if rest.is_empty() {
        print!("{}", show_fallback(&settings, &agent, &provider_name));
        return Ok(());
    }
    anyhow::ensure!(!locked.iter().any(|p| p == "fallbacks" || *p == format!("fallbacks.{agent}") || p.starts_with(&format!("fallbacks.{agent}."))), "{}'s fallbacks are set by your organization", agent_name(&agent));
    if rest == ["off"] {
        settings.fallbacks.remove(&agent);
    } else {
        let mut f = Fallback { extra: settings.fallbacks.get(&agent).map(|f| f.extra.clone()).unwrap_or_default(), ..Default::default() };
        let mut words = rest.iter();
        while let Some(w) = words.next() {
            match w.as_str() {
                "--outages" => f.on_outage = true,
                "--new-sessions" => {
                    let to = words.next().ok_or_else(|| anyhow::anyhow!("--new-sessions needs an agent: --new-sessions codex[:<model>]"))?;
                    let (id, model) = to.split_once(':').map_or((to.as_str(), None), |(a, m)| (a, Some(m.to_string())));
                    let id = dino_core::KNOWN_AGENTS.iter().find(|k| k.id == id || k.bin == id).map(|k| k.id.to_string()).ok_or_else(|| unknown_agent(id, false))?;
                    anyhow::ensure!(id != agent, "new sessions can't fall back to the agent itself");
                    f.new_sessions = Some(AgentSwitch { agent: id, model, ..Default::default() });
                }
                step => {
                    // Models have colons of their own (qwen3:4b); provider ids don't.
                    let (provider, model) = step.split_once(':').filter(|(p, m)| !p.is_empty() && !m.is_empty()).ok_or_else(|| anyhow::anyhow!("{} isn't <provider>:<model>\n\n{FALLBACK_USAGE}", printable(step)))?;
                    anyhow::ensure!(names.contains_key(provider), "dino doesn't know a provider called {}. `dino login` connects one.", printable(provider));
                    anyhow::ensure!(settings.policies.allows_fallback(provider), "your policies don't let agents fall back to {}", provider_name(provider));
                    f.steps.push(FallbackStep { provider: provider.into(), model: model.into(), ..Default::default() });
                }
            }
        }
        settings.fallbacks.insert(agent.clone(), f);
    }
    match client::request(&Request::SetSettings { settings: settings.clone() })? {
        Response::Ok => {}
        Response::Error { message } => return Err(hinted(message)),
        _ => return Err(unexpected()),
    }
    print!("{}", show_fallback(&settings, &agent, &provider_name));
    Ok(())
}

/// One agent's fallbacks, and whether it's at its limit now.
fn show_fallback(settings: &dino_core::settings::Settings, agent: &str, provider_name: &dyn Fn(&str) -> String) -> String {
    let f = settings.fallbacks.get(agent).cloned().unwrap_or_default();
    let steps = if f.steps.is_empty() {
        "nothing: its route's own answer".to_string()
    } else {
        f.steps.iter().enumerate().map(|(i, s)| format!("{}. {} · {}", i + 1, provider_name(&s.provider), printable(&s.model))).collect::<Vec<_>>().join("  ")
    };
    let mut rows = vec![("When it hits a limit", steps), ("Also when it's down", if f.on_outage { "yes".into() } else { "no".into() })];
    if let Some(n) = &f.new_sessions {
        let model = n.model.as_deref().map(|m| format!(" · {}", printable(m))).unwrap_or_default();
        rows.push(("New sessions at its limit", format!("{}{model}", agent_name(&n.agent))));
    }
    if let Ok(Response::State { limits, .. }) = client::request(&Request::State)
        && let Some(l) = limits.iter().find(|l| l.agent_id == agent)
    {
        let back = l.resets_at.map(|t| format!(", back in {}", duration(t.saturating_sub(out::now())))).unwrap_or_default();
        rows.push(("Now", format!("at its limit ({}{back})", printable(&l.name))));
    }
    format!("{}\n{}", out::paint(&agent_name(agent), out::Paint::Bold), out::fields(&rows))
}

/// The Claude subscription token (`claude setup-token`), for the Claude Code dino starts where it
/// isn't signed in. `set` reads the token from stdin, so it never sits on a command line.
fn cmd_claude_token(action: &str, arg: Option<&str>) -> anyhow::Result<()> {
    // Another of your Claude accounts, for when the one Claude Code signed in with is at its limit.
    if action == "add-account" || action == "remove-account" {
        return cmd_claude_account(action, arg);
    }
    if !matches!(action, "status" | "create" | "set" | "remove") {
        println!(
            "usage: dino claude-token [status|create|set|remove|add-account|remove-account]\n\n\
             create runs `claude setup-token` in a dino shell and keeps the token it prints; set reads one from stdin.\n\
             Only Claude Code gets it: on SSH environments, and on this Mac when Claude Code here isn't signed in (Settings → Agents).\n\n\
             add-account reads a `claude setup-token` token of another of your Claude accounts from stdin: when the one Claude Code\n\
             signed in with reaches its limit, its calls go on with that account until the limit resets. remove-account <n> forgets one."
        );
        return Ok(());
    }
    let value = if action == "set" {
        let mut t = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut t)?;
        Some(t.trim().to_string())
    } else {
        None
    };
    let t = match client::request(&Request::ClaudeToken { action: action.into(), value })? {
        Response::ClaudeToken { token } => token,
        Response::Error { message } => return Err(hinted(message)),
        _ => return Err(unexpected()),
    };
    let when = |s: u64| match s.saturating_sub(out::now()) / 86_400 {
        0 => "today".to_string(),
        1 => "tomorrow".into(),
        days => format!("in {days} days"),
    };
    let mut rows = vec![(
        "Token",
        match (&t.masked, t.expires) {
            (Some(m), Some(e)) => format!("{} (runs out {})", printable(m), when(e)),
            (Some(m), None) => printable(m),
            _ => "none: `dino claude-token create` makes one".into(),
        },
    )];
    if let Some(s) = t.signed_in {
        rows.push(("Claude Code here", if s { "signed in on its own".into() } else { "not signed in: sessions here use the token".into() }));
    }
    if let Some(id) = t.creating {
        rows.push(("Creating", format!("in session {}: finish signing in in your browser", printable(&id))));
    }
    if let Some(e) = t.error {
        rows.push(("Error", printable(&e)));
    }
    // Your Claude accounts, each by name with what it's doing; what that means, once, after.
    let accounts = match client::request(&Request::ClaudeAccounts { action: "status".into(), value: None, account: None, order: vec![] }) {
        Ok(Response::ClaudeAccounts { accounts }) if accounts.accounts.len() > 1 => accounts.accounts,
        _ => vec![],
    };
    let names: Vec<String> = accounts.iter().map(|a| if a.number == 1 { "Your Claude Code login".to_string() } else { format!("Account {}", a.number) }).collect();
    for (a, name) in accounts.iter().zip(&names) {
        rows.push((name.as_str(), account_state(a)));
    }
    print!("{}", out::fields(&rows));
    if !accounts.is_empty() {
        println!("\nWhen an account hits its limit, Claude Code goes on with the next one until it resets.");
    }
    Ok(())
}

/// What a Claude account is doing now: "answering now", "ready", "at its limit until today 14:00".
fn account_state(a: &dino_core::ipc::ClaudeAccountInfo) -> String {
    match (a.answering, a.spent, a.resets_at, a.retry_at) {
        (true, ..) => "answering now".into(),
        (_, true, Some(t), _) => format!("at its limit until {}", automations::when(t)),
        (_, true, None, Some(t)) => format!("at its limit, tried again {}", automations::when(t)),
        (_, true, None, None) => "at its limit".into(),
        _ => "ready".into(),
    }
}

/// `dino claude-token add-account` (token on stdin) and `remove-account <n>`: your other Claude
/// accounts, which dinod keeps in its key store as `CLAUDE_ACCOUNT_<n>`, never synced or shown.
fn cmd_claude_account(action: &str, arg: Option<&str>) -> anyhow::Result<()> {
    let (action, value, account) = if action == "add-account" {
        let mut t = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut t)?;
        ("add", Some(t), None)
    } else {
        let n: u32 = arg.and_then(|a| a.parse().ok()).ok_or_else(|| anyhow::anyhow!("usage: dino claude-token remove-account <n>"))?;
        ("remove", None, Some(n))
    };
    let info = match client::request(&Request::ClaudeAccounts { action: action.into(), value, account, order: vec![] })? {
        Response::ClaudeAccounts { accounts } => accounts,
        Response::Error { message } => return Err(hinted(message)),
        _ => return Err(unexpected()),
    };
    match (info.added, account) {
        (Some(n), _) => println!("Account {n} added. When an account hits its limit, Claude Code goes on with the next one until it resets."),
        (None, Some(n)) => println!("Account {n} removed."),
        _ => {}
    }
    Ok(())
}

const RM_HELP: &str = "usage: dino rm [--force] <id>

Delete a session: its agent stops, dino forgets it, and the worktree dino made for it is removed,
with its branch unless that has commits that aren't merged. A shell's folder, or a checkout of
yours, is never removed. The agent's own conversation stays: `dino found --all` lists it.

  -f, --force   delete it even when its worktree has uncommitted changes, which are lost

A branch with commits that aren't merged stays, pushed or not.";

/// `dino rm [--force] <id>`: delete a session, and the worktree dino made for it.
fn cmd_rm(args: &[String]) -> anyhow::Result<()> {
    let force = args.iter().any(|a| a == "-f" || a == "--force");
    let mut ids = args.iter().filter(|a| !matches!(a.as_str(), "-f" | "--force"));
    let id = match ids.next() {
        Some(a) if matches!(a.as_str(), "-h" | "--help") => {
            println!("{RM_HELP}");
            return Ok(());
        }
        Some(a) if a.starts_with('-') => anyhow::bail!("dino rm doesn't take `{}`\n`dino rm --help` says what it does.", printable(a)),
        Some(a) => a.clone(),
        None => anyhow::bail!("usage: dino rm [--force] <id>\n`dino ls` lists the sessions."),
    };
    if let Some(a) = ids.next() {
        anyhow::bail!("dino rm takes one session, not `{}` too\n`dino rm --help` says what it does.", printable(a));
    }
    let deletion = |dry_run| match client::request(&Request::Delete { id: id.clone(), dry_run })? {
        Response::Deletion { deletion } => Ok(deletion),
        Response::Error { message } => Err(hinted(message)),
        _ => Err(unexpected()),
    };
    let shown = printable(&id);
    if !force {
        let plan = deletion(true)?;
        if let (true, Some(path)) = (plan.uncommitted > 0, &plan.worktree) {
            anyhow::bail!(
                "session {shown}'s worktree {} has {}, which deleting it would lose.\n`dino rm --force {shown}` deletes it anyway.",
                printable(path),
                count(plan.uncommitted, "uncommitted change")
            );
        }
    }
    let done = deletion(false)?;
    let mut said = match &done.worktree {
        Some(path) => format!("Deleted session {shown} and its worktree {}.", printable(path)),
        None => format!("Deleted session {shown}."),
    };
    if let (true, Some(branch)) = (done.keeps_branch, &done.branch) {
        let unpushed = if done.unpushed > 0 { format!(", with {}", count(done.unpushed, "unpushed commit")) } else { String::new() };
        said += &format!(" Its branch {} stays{unpushed}: it isn't merged.", printable(branch));
    }
    if let Some(other) = &done.kept_for {
        said += &format!(" Its worktree stays: {} is in it.", printable(other));
    }
    say(&said);
    Ok(())
}

/// `n thing` or `n things`.
fn count(n: u32, thing: &str) -> String {
    format!("{n} {thing}{}", if n == 1 { "" } else { "s" })
}

const FOUND_HELP: &str = "usage: dino found [--all] [--json]

Agent sessions dino didn't start, that `dino continue <id>` continues in dino: running in other
terminals (tmux too), recent ones on this Mac, and Claude Code on the web.

  --all    every recent one, not only the newest 25
  --json   a JSON array, one object per session

Piped, it prints a tab-separated line per session and no header: where it was found (running,
recent or cloud), its full id, agent, title, folder, status and when it last changed.";

/// Agent sessions outside dino that it can continue: running elsewhere, recent, cloud.
fn cmd_found(args: &[String]) -> anyhow::Result<()> {
    use dino_core::found::Source;
    let all = args.iter().any(|a| a == "--all");
    let json = args.iter().any(|a| a == "--json");
    if let Some(a) = args.iter().find(|a| !matches!(a.as_str(), "--all" | "--json")) {
        if matches!(a.as_str(), "-h" | "--help") {
            println!("{FOUND_HELP}");
            return Ok(());
        }
        anyhow::bail!("dino found doesn't take `{}`\n`dino found --help` says what it does.", printable(a));
    }
    // Through dinod, so its own sessions aren't listed as "elsewhere".
    let Response::Found { sessions } = client::request(&Request::Found { cloud: true, running_only: false })? else { return Err(unexpected()) };
    if json {
        println!("{}", serde_json::to_string_pretty(&sessions)?);
        return Ok(());
    }
    let now = out::now();
    let shown = |f: &dino_core::found::FoundSession| f.session_id.get(..8).unwrap_or(&f.session_id).to_string();
    if !out::tty() {
        for f in &sessions {
            let source = match f.source {
                Source::Running => "running",
                Source::Recent => "recent",
                Source::Cloud => "cloud",
            };
            let when = if f.updated_at > 0 { out::iso(f.updated_at) } else { String::new() };
            let fields = [source, &f.session_id, &f.agent, &f.title, f.cwd.as_deref().unwrap_or(""), f.status.as_deref().unwrap_or(""), &when];
            println!("{}", fields.map(printable).join("\t"));
        }
        return Ok(());
    }
    // The rest are in the app's session browser, and `dino continue` finds them all.
    const NEWEST: usize = 25;
    let mut first = true;
    let mut section = |heading: &str| {
        if !first {
            println!();
        }
        first = false;
        println!("{}", out::paint(heading, Paint::Bold));
    };
    let row = |f: &dino_core::found::FoundSession| {
        vec![
            Cell::new(printable(&shown(f))),
            Cell::new(printable(&f.agent)),
            Cell::new(printable(&f.title)),
            Cell::new(printable(&out::short_path(f.cwd.as_deref().unwrap_or("")))),
        ]
    };
    let running: Vec<_> = sessions.iter().filter(|f| f.source == Source::Running).collect();
    if !running.is_empty() {
        section("Running in other terminals");
        let place = |f: &dino_core::found::FoundSession| f.tmux.as_ref().map(|t| format!("tmux {}", t.label)).or_else(|| f.terminal.clone());
        // Which terminal, when dino can tell for any of them.
        let any_place = running.iter().any(|f| place(f).is_some());
        let mut cols = vec![Column::keep("ID"), Column::keep("AGENT"), Column::end("TITLE", 16), Column::path("FOLDER", 12), Column::keep("STATUS")];
        if any_place {
            cols.push(Column::end("TERMINAL", 8));
        }
        let rows: Vec<_> = running
            .iter()
            .map(|f| {
                let state = match f.status.as_deref() {
                    Some("busy") => SessionStatus::Working,
                    Some("needs") => SessionStatus::NeedsYou,
                    _ => SessionStatus::Idle,
                };
                let mut r = row(f);
                r.push(Cell::status(state));
                if any_place {
                    r.push(Cell::new(printable(&place(f).unwrap_or_default())).paint(Paint::Dim));
                }
                r
            })
            .collect();
        print!("{}", out::table(&cols, &rows, true));
    }
    let recent: Vec<_> = sessions.iter().filter(|f| f.source == Source::Recent).collect();
    if !recent.is_empty() {
        section("Recent");
        let more = if all { 0 } else { recent.len().saturating_sub(NEWEST) };
        let cols = [Column::keep("ID"), Column::keep("AGENT"), Column::end("TITLE", 16), Column::path("FOLDER", 12), Column::keep("UPDATED")];
        let rows: Vec<_> = recent
            .iter()
            .take(recent.len() - more)
            .map(|f| {
                let mut r = row(f);
                r.push(Cell::new(out::ago(now.saturating_sub(f.updated_at))).paint(Paint::Dim));
                r
            })
            .collect();
        print!("{}", out::table(&cols, &rows, true));
        if more > 0 {
            println!("{}", out::paint(&format!("… and {more} older: dino found --all"), Paint::Dim));
        }
    }
    let cloud: Vec<_> = sessions.iter().filter(|f| f.source == Source::Cloud).collect();
    if !cloud.is_empty() {
        section("Claude Code on the web");
        for f in cloud {
            // Not a session yet: the app's session browser lists the web's to pick from.
            let what = if f.session_id.is_empty() { "Pick a web session to teleport in the dino app: Session → Continue a Session… (⌘K)".into() } else { format!("{}  {}", printable(&shown(f)), printable(&f.title)) };
            println!("{what}");
        }
    }
    if first {
        println!("No agent sessions outside dino on this Mac.");
    } else {
        println!("\n{}", out::paint("`dino continue <id>` continues one in dino.", Paint::Dim));
    }
    Ok(())
}

/// One prompt to several agents, each in its own worktree. Without `--agents`, every agent dino has.
fn cmd_fan(args: &[String]) -> anyhow::Result<()> {
    if args.is_empty() || args.first().is_some_and(|a| a.starts_with('-') && a != "--agents") {
        println!("usage: dino fan [--agents claude,codex,…] <prompt>\n\nOne prompt, several agents, each in its own git worktree of the current repo.");
        return Ok(());
    }
    let (agents, prompt) = match args {
        [flag, list, rest @ ..] if flag == "--agents" => (list.split(',').map(String::from).collect(), rest.join(" ")),
        rest => {
            let Response::Launchers { launchers } = client::request(&Request::Launchers)? else { return Err(unexpected()) };
            (launchers.into_iter().filter(|l| l.agent_id != "shell").map(|l| l.short).collect::<Vec<_>>(), rest.join(" "))
        }
    };
    let cwd = std::env::current_dir().ok().map(|p| p.display().to_string());
    let group = created(client::request(&Request::Fanout { prompt, launchers: agents.clone(), cwd })?)?;
    if out::tty() {
        println!("Fanned out to {} as group {group}. `dino groups` shows how each is doing.", agents.join(", "));
    } else {
        println!("{group}");
    }
    Ok(())
}


/// Connect a hosted provider in the browser (OpenRouter's sign-in, or Sign in with ChatGPT; no key
/// to paste), then wait until dinod has what it gave.
fn cmd_login(provider: Option<&str>) -> anyhow::Result<()> {
    let Some(provider) = provider else {
        println!("usage: dino login openrouter|chatgpt\n\n  openrouter  Connect OpenRouter in your browser; dino keeps the key it gets, and never shows it.\n  chatgpt     Sign in with ChatGPT, so agents in dino can use your ChatGPT plan (up to the weekly\n              cap you set for dino in ChatGPT → Settings → Usage).");
        return Ok(());
    };
    if let Response::Providers { providers } = client::request(&Request::Providers)?
        && let Some(p) = providers.iter().find(|p| p.id == provider && p.connected)
    {
        println!("{} is already connected (dino logout {provider} disconnects it).", p.name);
        return Ok(());
    }
    let Response::Connect { url } = client::request(&Request::ConnectProvider { provider: provider.into() })? else { return Err(unexpected()) };
    println!("Opening your browser to connect {provider}. If it doesn't open, go to:\n\n  {url}\n");
    let _ = std::process::Command::new("open").arg(&url).status();
    let until = Instant::now() + std::time::Duration::from_secs(10 * 60);
    while Instant::now() < until {
        std::thread::sleep(std::time::Duration::from_secs(1));
        let Response::Providers { providers } = client::request(&Request::Providers)? else { continue };
        let Some(p) = providers.into_iter().find(|p| p.id == provider) else { continue };
        if p.connected {
            match p.account.and_then(|a| a.label) {
                Some(label) if provider == "chatgpt" => println!("Signed in with ChatGPT. {label}."),
                _ => println!("Connected {}.", p.name),
            }
            return Ok(());
        }
        if let Some(e) = p.error {
            anyhow::bail!("{e}");
        }
    }
    anyhow::bail!("gave up waiting for the browser")
}

/// The provider id of coding plan `name` (`zai` or `plan-zai`), if it is one.
fn plan_id(name: &str) -> Option<String> {
    let id = if name.starts_with(dino_core::plans::PREFIX) { name.to_string() } else { format!("{}{name}", dino_core::plans::PREFIX) };
    dino_core::plans::preset(&id).map(|_| id)
}

/// Connect a coding plan with its key, read from stdin so it's never on a command line; the
/// generic entry takes `--base <url>` too.
fn cmd_login_plan(args: &[String]) -> anyhow::Result<()> {
    let id = plan_id(&args[0]).ok_or_else(unexpected)?;
    let base = match &args[1..] {
        [flag, url] if flag == "--base" => Some(url.clone()),
        [] => None,
        _ => anyhow::bail!("usage: dino login {} [--base <url>] < key", args[0]),
    };
    let preset = dino_core::plans::preset(&id).ok_or_else(unexpected)?;
    anyhow::ensure!(base.is_some() || preset.id != dino_core::plans::OTHER, "the generic entry needs its base URL: dino login other --base <url> < key");
    if out::tty() {
        eprintln!("Paste {}'s API key, then press Return and Ctrl-D:", preset.name);
    }
    let mut key = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut key)?;
    done(client::request(&Request::ConnectPlan { plan: id, key, base })?)?;
    say(&format!("Connected {}. dino keeps its key on this Mac and never shows it.", preset.name));
    Ok(())
}

/// Fan-outs: each group's prompt, then how each of its agents is doing and what it changed.
fn cmd_groups() -> anyhow::Result<()> {
    let Response::Groups { groups } = client::request(&Request::Groups)? else { return Err(unexpected()) };
    if groups.is_empty() {
        eprintln!("No fan-outs. `dino fan <prompt>` starts one: one prompt, several agents, a worktree each.");
        return Ok(());
    }
    let sessions = match client::request(&Request::State)? {
        Response::State { sessions, .. } => sessions,
        _ => vec![],
    };
    let cols = [Column::keep("ID"), Column::keep("AGENT"), Column::keep("STATUS"), Column::keep("CHANGES"), Column::path("WORKTREE", 12)];
    let rows: Vec<_> = groups
        .iter()
        .flat_map(|g| &g.members)
        .map(|m| {
            let st = sessions.iter().find(|s| s.id == m.session).map_or(SessionStatus::Ended, SessionStatus::of);
            let changes = match &m.stat {
                Some(s) if s.files == 0 => Cell::new("none").raw("0 +0 -0").paint(Paint::Dim),
                Some(s) => Cell::new(format!("{} {}  +{} -{}", s.files, if s.files == 1 { "file" } else { "files" }, s.added, s.removed)).raw(format!("{} +{} -{}", s.files, s.added, s.removed)),
                None => Cell::new("worktree missing").raw("missing").paint(Paint::Red),
            };
            vec![Cell::new(printable(&m.session)), Cell::new(printable(&m.launcher)), Cell::status(st), changes, Cell::new(printable(&out::short_path(&m.worktree))).raw(printable(&m.worktree))]
        })
        .collect();
    // One table for all of them, so their columns line up; each group's rows under its prompt.
    let table = out::table(&cols, &rows, out::tty());
    let mut lines = table.lines();
    let header = if out::tty() { lines.next().unwrap_or_default() } else { "" };
    for (i, g) in groups.iter().enumerate() {
        if out::tty() {
            if i > 0 {
                println!();
            }
            let prompt = out::fit_end(&printable(&g.prompt.split_whitespace().collect::<Vec<_>>().join(" ")), out::width().saturating_sub(2).max(20));
            println!("{}", out::paint(&format!("“{prompt}”"), Paint::Bold));
            println!("{}", out::paint(&format!("group {} in {}", printable(&g.id), printable(&out::short_path(&g.repo))), Paint::Dim));
            println!("  {header}");
        }
        for l in lines.by_ref().take(g.members.len()) {
            if out::tty() { println!("  {l}") } else { println!("{}\t{l}", printable(&g.id)) }
        }
    }
    if out::tty() {
        println!("\n{}", out::paint("`dino diff <id>` shows what one changed, `dino keep <id>` applies it.", Paint::Dim));
        println!("{}", out::paint("`dino discard <group>` stops a group and removes its worktrees.", Paint::Dim));
    }
    Ok(())
}

/// Continue a session dino didn't start (see `dino found`).
fn cmd_continue(prefix: &str) -> anyhow::Result<()> {
    let Response::Found { sessions } = client::request(&Request::Found { cloud: false, running_only: false })? else { return Err(unexpected()) };
    let session = sessions
        .into_iter()
        .find(|f| !f.session_id.is_empty() && f.session_id.starts_with(prefix))
        .ok_or_else(|| anyhow::anyhow!("no session found starting with {}\n`dino found` lists the ones dino can continue.", printable(prefix)))?;
    let title = printable(&session.title);
    if session.pid.is_some() {
        eprintln!("Moving “{title}” into dino; it waits for its current turn to finish…");
    }
    let id = created(client::request(&Request::Adopt { session, cwd: None })?)?;
    if out::tty() {
        println!("Continuing “{title}” as session {id}. `dino attach {id}` opens it here.");
    } else {
        println!("{id}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_characters_are_not_printed() {
        assert_eq!(printable("fix \x1b]0;pwned\x07login\r\n\x7f\u{9b}2J done"), "fix ?]0;pwned?login????2J done");
        assert_eq!(printable("~/src/app · café ✓\t"), "~/src/app · café ✓?");
    }

    #[test]
    fn status_says_what_needs_saying() {
        // tmux's status bar: nothing when nothing is going on.
        assert_eq!(status_line(true, 3, 0, 0), "");
        assert_eq!(status_line(true, 3, 1, 2), "dino: 1 needs you · 2 working");
        assert_eq!(status_line(true, 3, 2, 0), "dino: 2 need you");
        assert_eq!(status_line(false, 1, 0, 0), "1 agent, none working");
        assert_eq!(status_line(false, 4, 0, 1), "4 agents: 1 working");
    }
}
