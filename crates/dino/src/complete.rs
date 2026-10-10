//! Tab completion: `dino completions zsh|bash|fish` prints a script that registers it, and the
//! script asks `dino __complete -- <the words on the line>` what fits the last one. One list of
//! commands and flags here, so the three shells complete alike.
//!
//! What `__complete` prints, a line each: `value<TAB>what it is`, and `:dirs` or `:files` for the
//! shell to add folders or files of its own. The shell filters by what's typed so far.

use std::io::{BufWriter, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use dino_core::ipc::{self, Request, Response};

pub const COMPLETION_ZSH: &str = include_str!("../shell/completions/dino.zsh");
pub const COMPLETION_BASH: &str = include_str!("../shell/completions/dino.bash");
pub const COMPLETION_FISH: &str = include_str!("../shell/completions/dino.fish");

/// The completion script for `shell`, standalone: for a folder the shell loads completions from
/// (Homebrew's), or to source.
pub fn script(shell: &str) -> Option<&'static str> {
    match shell {
        "zsh" => Some(COMPLETION_ZSH),
        "bash" => Some(COMPLETION_BASH),
        "fish" => Some(COMPLETION_FISH),
        _ => None,
    }
}

/// `dino completions zsh|bash|fish`.
pub fn completions(shell: Option<&str>) -> anyhow::Result<()> {
    let shell = shell.ok_or_else(|| anyhow::anyhow!("usage: dino completions zsh|bash|fish"))?;
    let text = script(shell).ok_or_else(|| anyhow::anyhow!("dino has completions for zsh, bash and fish, not {}", crate::printable(shell)))?;
    // The dino on the PATH: a Homebrew install's path changes with each version.
    print!("{}", text.replace("__DINO_BIN__", "dino"));
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    /// Nothing to offer: a name, a prompt, a number.
    Free,
    Sessions,
    /// Agents dino can start, as `dino new` takes them.
    Agents,
    /// The agents fallbacks are for (no shell).
    KnownAgents,
    Automations,
    Providers,
    Dirs,
    Files,
    Words(&'static [(&'static str, &'static str)]),
}

/// A flag, what it does, and what follows it (`None`: nothing).
type Flag = (&'static str, &'static str, Option<Kind>);

const HELP: Flag = ("--help", "show its usage", None);
const SHELLS: Kind = Kind::Words(&[("zsh", ""), ("bash", ""), ("fish", "")]);

/// Each command: what it does, its flags, and what its words after the flags are, in order (the
/// last repeats when `rest` is set).
struct Command {
    name: &'static str,
    about: &'static str,
    flags: &'static [Flag],
    args: &'static [Kind],
}

const COMMANDS: &[Command] = &[
    Command { name: "ls", about: "list sessions, the ones that need you first", flags: &[("--usage", "add each session's tokens", None), ("--json", "print JSON", None), HELP], args: &[] },
    Command { name: "status", about: "sum up agents in a line", flags: &[("--tmux", "short, for tmux's status bar", None), HELP], args: &[] },
    Command {
        name: "new",
        about: "start a session in the background and print its id",
        flags: &[("--worktree", "in a new git worktree", None), ("--stay", "this agent even at its limit", None), HELP],
        args: &[Kind::Agents],
    },
    Command { name: "attach", about: "open a session here", flags: &[HELP], args: &[Kind::Sessions] },
    Command { name: "resume", about: "resume an ended session", flags: &[HELP], args: &[Kind::Sessions] },
    Command { name: "kill", about: "close a session", flags: &[HELP], args: &[Kind::Sessions] },
    Command {
        name: "fork",
        about: "start a new session on a copy of a session's conversation",
        flags: &[("--no-worktree", "not in a new worktree", None), ("--name", "the new session's name", Some(Kind::Free)), HELP],
        args: &[Kind::Sessions],
    },
    Command { name: "rm", about: "delete a session and the worktree dino made for it", flags: &[("--force", "even with uncommitted changes", None), HELP], args: &[Kind::Sessions] },
    Command { name: "found", about: "list agent sessions started outside dino", flags: &[("--all", "every recent one", None), ("--json", "print JSON", None), HELP], args: &[] },
    Command { name: "continue", about: "continue a session `dino found` lists", flags: &[HELP], args: &[Kind::Free] },
    Command {
        name: "stats",
        about: "show usage across all agents",
        flags: &[
            ("--range", "the last 7 days, 30, or all", Some(Kind::Words(&[("7d", "the last 7 days"), ("30d", "the last 30 days"), ("all", "everything")]))),
            ("--json", "print JSON", None),
            ("--clear", "forget every statistic", None),
            ("--yes", "don't ask first", None),
            HELP,
        ],
        args: &[Kind::Words(&[("overview", ""), ("models", ""), ("agents", ""), ("projects", ""), ("routes", ""), ("speed", ""), ("all", "")])],
    },
    Command { name: "automations", about: "agents and commands that run on their own", flags: AUTOMATION_FLAGS, args: &[AUTOMATION_ACTIONS, Kind::Automations] },
    Command {
        name: "login",
        about: "sign in, or connect a provider",
        flags: &[("--email", "get a sign-in link by email", None), ("--device", "show a code to enter on another device", None), ("--base", "the plan's base URL", Some(Kind::Free)), HELP],
        args: &[Kind::Providers],
    },
    Command { name: "logout", about: "sign out, or disconnect a provider", flags: &[HELP], args: &[Kind::Providers] },
    Command {
        name: "sync",
        about: "settings sync",
        flags: &[HELP],
        args: &[Kind::Words(&[("status", "what's synced"), ("now", "sync now"), ("resolve", "settle a conflict"), ("undo", "undo the last sync")])],
    },
    Command {
        name: "claude-token",
        about: "the Claude subscription token",
        flags: &[HELP],
        args: &[Kind::Words(&[
            ("status", "show it"),
            ("create", "run `claude setup-token` and keep the token"),
            ("set", "read a token from stdin"),
            ("remove", "forget the token"),
            ("add-account", "add another Claude account: sign in in the browser"),
            ("rename-account", "name an account: rename-account <n> <name>"),
            ("remove-account", "forget an account"),
        ])],
    },
    Command {
        name: "fallback",
        about: "choose what an agent switches to when it hits a limit",
        flags: &[("--outages", "also when its provider is down", None), ("--new-sessions", "start new sessions with another agent", Some(Kind::KnownAgents)), HELP],
        args: &[Kind::KnownAgents, Kind::Words(&[("off", "no fallbacks")])],
    },
    Command {
        name: "build-cache",
        about: "share one Rust build cache across sessions",
        flags: &[HELP],
        args: &[Kind::Words(&[("status", "hits and size"), ("on", "turn it on"), ("off", "turn it off"), ("size", "how much disk it may use"), ("install", "install sccache")])],
    },
    Command { name: "init", about: "print the shell integration", flags: &[HELP], args: &[SHELLS] },
    Command { name: "completions", about: "print the shell's completions", flags: &[HELP], args: &[SHELLS] },
    Command {
        name: "shell",
        about: "add or remove the shell integration",
        flags: &[HELP],
        args: &[Kind::Words(&[("install", "load it from the shell's startup file"), ("uninstall", "stop loading it")]), SHELLS],
    },
    Command {
        name: "ai",
        about: "the shell's AI line",
        flags: &[
            ("--agent", "which agent", Some(Kind::Agents)),
            ("--shell", "the shell to answer for", Some(SHELLS)),
            ("--cwd", "the folder it's for", Some(Kind::Dirs)),
            ("--last", "the command before", Some(Kind::Free)),
            ("--status", "its exit status", Some(Kind::Free)),
            HELP,
        ],
        args: &[Kind::Words(&[("suggest", "one command for a request"), ("agent", "hand a request to an agent"), ("risky", "whether a command could destroy something")])],
    },
    Command {
        name: "search",
        about: "search history and dino's sessions",
        flags: &[("--json", "print JSON", None), ("--pick", "pick one", None), ("--query", "what to look for", Some(Kind::Free)), ("--history", "the shell's history", Some(Kind::Files)), HELP],
        args: &[],
    },
    Command { name: "mcp", about: "serve dino's sessions to agents over MCP", flags: &[("--read-only", "no changes", None), HELP], args: &[] },
    Command { name: "ping", about: "start dino's background service", flags: &[], args: &[] },
    Command { name: "stop", about: "stop dino's background service", flags: &[], args: &[] },
    Command { name: "version", about: "print dino's version", flags: &[], args: &[] },
];

/// The commands only this OS has, as `main` runs them: macOS's sleep and privacy permissions,
/// Linux's systemd service.
#[cfg(target_os = "macos")]
const OS_COMMANDS: &[Command] = &[
    Command {
        name: "power",
        about: "see what keeps your Mac awake",
        flags: &[HELP],
        args: &[Kind::Words(&[("status", "what keeps it awake now"), ("setup", "keep agents running with the lid closed"), ("remove", "take that permission back")])],
    },
    Command { name: "permissions", about: "see what macOS lets programs here do", flags: &[("--json", "print JSON", None), HELP], args: &[] },
];
#[cfg(target_os = "linux")]
const OS_COMMANDS: &[Command] = &[Command {
    name: "service",
    about: "run dinod as a systemd user service",
    flags: &[HELP],
    args: &[Kind::Words(&[("install", "install and start it"), ("uninstall", "stop and remove it"), ("status", "whether it's running")])],
}];
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
const OS_COMMANDS: &[Command] = &[];

/// Every command dino runs here.
fn commands() -> impl Iterator<Item = &'static Command> {
    COMMANDS.iter().chain(OS_COMMANDS)
}

const AUTOMATION_ACTIONS: Kind = Kind::Words(&[
    ("show", "one, with its runs"),
    ("add", "a new one"),
    ("edit", "change one"),
    ("run", "run one now"),
    ("pause", "stop it running"),
    ("resume", "let it run again"),
    ("rm", "delete one"),
]);

const AUTOMATION_FLAGS: &[Flag] = &[
    ("--json", "print JSON", None),
    ("--daily", "every day at HH:MM", Some(Kind::Free)),
    ("--weekdays", "Monday to Friday at HH:MM", Some(Kind::Free)),
    ("--weekly", "a day of the week, then HH:MM", Some(Kind::Words(&[("mon", ""), ("tue", ""), ("wed", ""), ("thu", ""), ("fri", ""), ("sat", ""), ("sun", "")]))),
    ("--hourly", "at a minute past each hour", Some(Kind::Free)),
    ("--manual", "only when you run it", None),
    ("--on-pr", "a pull request opens", None),
    ("--on-merge", "a pull request merges", None),
    ("--on-review", "your review is requested", None),
    ("--on-ci-fail", "a check fails", None),
    ("--on-label", "an issue gets the label", Some(Kind::Free)),
    ("--on-comment", "a comment says the words", Some(Kind::Free)),
    ("--on-commits", "new commits after a fetch", None),
    ("--on-behind", "the branch falls behind its upstream", None),
    ("--on-files", "files change in the folder", None),
    ("--after", "another automation's run finishes", Some(Kind::Automations)),
    ("--outcome", "only when it succeeded or failed", Some(Kind::Words(&[("success", ""), ("failure", "")]))),
    ("--repo", "owner/name", Some(Kind::Free)),
    ("--branch", "the branch", Some(Kind::Free)),
    ("--mine", "your PRs only", None),
    ("--interval", "minutes between looks", Some(Kind::Free)),
    ("--path", "the part of the folder to watch", Some(Kind::Dirs)),
    ("--agent", "which agent", Some(Kind::Agents)),
    ("--continue", "send the prompt into a session", Some(Kind::Sessions)),
    ("--agents", "several agents, a worktree each", Some(Kind::Free)),
    ("--command", "run a command", Some(Kind::Free)),
    ("--then-agent", "the agent after the command", Some(Kind::Words(&[("failure", ""), ("always", "")]))),
    ("--cwd", "where it runs", Some(Kind::Dirs)),
    ("--worktree", "each run in a new worktree", None),
    ("--no-worktree", "each run in the folder", None),
    ("--args", "the agent's options", Some(Kind::Free)),
    ("--if-changed", "only when the repo changed", None),
    ("--on-limit", "when the agent is at its limit", Some(Kind::Words(&[("skip", ""), ("fallback", ""), ("run", "")]))),
    ("--ac-power", "only on power", None),
    ("--lid-open", "only with the lid open", None),
    ("--parallel", "even while the last run goes on", None),
    ("--retries", "try a failed run again", Some(Kind::Free)),
    ("--backoff", "seconds to wait first", Some(Kind::Free)),
    ("--max-runs", "pause after this many runs", Some(Kind::Free)),
    ("--comment", "post the summary on the PR", None),
    ("--no-notify", "no notification", None),
    HELP,
];

/// Short flags dino takes as well as the long ones above.
fn long(flag: &str) -> &str {
    match flag {
        "-u" => "--usage",
        "-w" => "--worktree",
        "-f" => "--force",
        "-h" => "--help",
        other => other,
    }
}

/// What a completion offers.
#[derive(Debug, Default, PartialEq)]
pub struct Offer {
    pub values: Vec<(String, String)>,
    pub dirs: bool,
    pub files: bool,
}

impl Offer {
    fn words(&mut self, words: &[(&str, &str)]) {
        self.values.extend(words.iter().map(|(w, d)| (w.to_string(), d.to_string())));
    }
}

/// Where dinod's answers come from: the real one, or a test's.
pub trait Source {
    fn sessions(&self) -> Vec<(String, String)>;
    fn agents(&self) -> Vec<(String, String)>;
    fn automations(&self) -> Vec<(String, String)>;
}

/// What fits the last of `words` (the program's name first, the word being typed last).
pub fn offer(words: &[String], src: &dyn Source) -> Offer {
    let mut o = Offer::default();
    let Some((current, done)) = words.split_last() else { return o };
    let done = done.get(1..).unwrap_or_default();
    let Some((first, rest)) = done.split_first() else {
        // The first word: a command, an agent, or a folder.
        if current.starts_with('-') {
            o.words(&[("--help", "list every command"), ("--version", "print dino's version")]);
        } else {
            o.values.extend(commands().map(|c| (c.name.to_string(), c.about.to_string())));
            // `dino shell` is the command: the agent called that starts as `dino . shell`.
            o.values.extend(src.agents().into_iter().filter(|(a, _)| !commands().any(|c| c.name == a)));
            o.dirs = true;
        }
        return o;
    };
    let name = if first == "automation" { "automations" } else { first.as_str() };
    let Some(cmd) = commands().find(|c| c.name == name) else {
        // `dino <folder> [<agent> [args…]]`, `dino <agent> [args…]`: the agent's own args are files.
        let folder = first.starts_with(['.', '~', '/']) || first.contains('/') || std::path::Path::new(first).is_dir();
        if folder && rest.is_empty() {
            o.values.extend(src.agents());
        } else {
            o.files = true;
        }
        return o;
    };
    // Past `--` there's a prompt or a request.
    if rest.iter().any(|w| w == "--") {
        return o;
    }
    // A flag that takes a value, and the words that aren't flags or their values.
    let flag = |w: &str| cmd.flags.iter().find(|f| f.0 == long(w));
    let mut args = vec![];
    let mut it = rest.iter();
    while let Some(w) = it.next() {
        match flag(w) {
            Some((_, _, Some(_))) => {
                it.next();
            }
            Some(_) => {}
            None if w.starts_with('-') => {}
            None => args.push(w.as_str()),
        }
    }
    if let Some((_, _, Some(kind))) = rest.last().and_then(|w| flag(w)) {
        add(&mut o, *kind, src);
        return o;
    }
    // `dino new`: everything after the agent is the agent's, `--on <provider> <model>` first.
    if name == "new" && !args.is_empty() {
        if args.len() == 1 && rest.last().is_some_and(|w| w == args[0]) && current.starts_with('-') {
            o.words(&[("--on", "a provider's model instead of the agent's own account")]);
        }
        o.files = true;
        return o;
    }
    // `dino login <plan>` takes `--base`; plain `dino login`, the rest.
    let flags: Vec<&Flag> = cmd.flags.iter().filter(|f| name != "login" || (f.0 == "--base") != args.is_empty() || f.0 == "--help").collect();
    // Only `show`, `edit`, `run`, `pause`, `resume` and `rm` take an automation, and only
    // `add` and `edit` the options.
    let kind = match (name, args.first().copied()) {
        ("automations", Some("add")) => None,
        ("automations", Some(_)) if args.len() > 1 => None,
        ("automations", None) if !current.starts_with('-') => Some(AUTOMATION_ACTIONS),
        ("automations", None) => None,
        _ => cmd.args.get(args.len()).copied(),
    };
    let options = name != "automations" || matches!(args.first().copied(), Some("add" | "edit"));
    if current.starts_with('-') || kind.is_none() {
        let used = |f: &str| rest.iter().any(|w| long(w) == f);
        if options {
            o.values.extend(flags.iter().filter(|f| !used(f.0)).map(|f| (f.0.to_string(), f.1.to_string())));
        } else if name == "automations" && args.is_empty() {
            o.words(&[("--json", "print JSON"), ("--help", "show its usage")]);
        }
    }
    if let Some(kind) = kind
        && !current.starts_with('-')
    {
        add(&mut o, kind, src);
    }
    o
}

fn add(o: &mut Offer, kind: Kind, src: &dyn Source) {
    match kind {
        Kind::Free => {}
        Kind::Sessions => o.values.extend(src.sessions()),
        Kind::Agents => o.values.extend(src.agents()),
        Kind::KnownAgents => o.values.extend(dino_core::KNOWN_AGENTS.iter().map(|k| (k.id.to_string(), k.name.to_string()))),
        Kind::Automations => o.values.extend(src.automations()),
        Kind::Providers => {
            o.words(&[("openrouter", "OpenRouter"), ("chatgpt", "ChatGPT")]);
            o.values.extend(dino_core::plans::presets().iter().map(|p| (p.id.clone(), p.name.clone())));
        }
        Kind::Dirs => o.dirs = true,
        Kind::Files => o.files = true,
        Kind::Words(words) => o.words(words),
    }
}

/// dinod, when it's running: completing never starts it, and gives up on one that doesn't answer.
struct Dinod;

impl Dinod {
    fn ask(&self, req: &Request) -> Option<Response> {
        let mut s = UnixStream::connect(ipc::socket_path()).ok()?;
        s.set_read_timeout(Some(Duration::from_secs(1))).ok()?;
        s.set_write_timeout(Some(Duration::from_secs(1))).ok()?;
        ipc::write_json(&mut s, req).ok()?;
        let (_, payload) = ipc::read_frame(&mut s).ok()?;
        serde_json::from_slice(&payload).ok()
    }
}

impl Source for Dinod {
    fn sessions(&self) -> Vec<(String, String)> {
        let Some(Response::State { mut sessions, .. }) = self.ask(&Request::State) else { return vec![] };
        // As `dino ls` lists them.
        sessions.sort_by_key(|s| (dino_core::status::Status::of(s), crate::id_order(&s.id)));
        sessions.iter().map(|s| (s.id.clone(), format!("{} · {}", crate::name(s), dino_core::status::Status::of(s).key().replace('_', " ")))).collect()
    }

    fn agents(&self) -> Vec<(String, String)> {
        if let Some(Response::Launchers { launchers }) = self.ask(&Request::Launchers) {
            return launchers.into_iter().map(|l| (l.short, l.label)).collect();
        }
        // Without dinod, the agents on this Mac, as it would find them.
        dino_core::detect_agents().into_iter().map(|d| (d.kind.id.to_string(), d.kind.name.to_string())).chain([("shell".to_string(), "a shell".to_string())]).collect()
    }

    fn automations(&self) -> Vec<(String, String)> {
        let Some(Response::Schedule { tasks }) = self.ask(&Request::ScheduleList) else { return vec![] };
        tasks.into_iter().map(|t| (t.name, String::new())).collect()
    }
}

/// `dino __complete -- <words>`: what the shell scripts ask.
pub fn run(args: &[String]) -> anyhow::Result<()> {
    let words = match args.first().map(String::as_str) {
        Some("--") => &args[1..],
        _ => args,
    };
    let o = offer(words, &Dinod);
    let mut out = BufWriter::new(std::io::stdout().lock());
    for (value, about) in &o.values {
        // One line each: a tab or newline of its own would split it.
        let clean = |s: &str| s.replace(['\t', '\n', '\r'], " ");
        writeln!(out, "{}\t{}", clean(value), clean(&crate::printable(about)))?;
    }
    if o.dirs {
        writeln!(out, ":dirs")?;
    }
    if o.files {
        writeln!(out, ":files")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake;

    impl Source for Fake {
        fn sessions(&self) -> Vec<(String, String)> {
            vec![("3".into(), "fix the build · needs you".into()), ("1".into(), "zsh · idle".into())]
        }
        fn agents(&self) -> Vec<(String, String)> {
            vec![("claude".into(), "Claude Code".into()), ("shell".into(), "Shell".into())]
        }
        fn automations(&self) -> Vec<(String, String)> {
            vec![("nightly".into(), String::new())]
        }
    }

    fn values(line: &str) -> Vec<String> {
        let mut words: Vec<String> = line.split(' ').map(String::from).collect();
        if line.ends_with(' ') {
            words.pop();
            words.push(String::new());
        }
        offer(&words, &Fake).values.into_iter().map(|(v, _)| v).collect()
    }

    fn has(line: &str, want: &[&str]) {
        let got = values(line);
        for w in want {
            assert!(got.iter().any(|g| g == w), "`{line}` should offer {w}: {got:?}");
        }
    }

    fn lacks(line: &str, unwanted: &[&str]) {
        let got = values(line);
        for w in unwanted {
            assert!(!got.iter().any(|g| g == w), "`{line}` shouldn't offer {w}: {got:?}");
        }
    }

    #[test]
    fn commands_agents_and_folders_come_first() {
        has("dino ", &["ls", "attach", "new", "completions", "claude", "shell"]);
        assert_eq!(values("dino ").iter().filter(|v| *v == "shell").count(), 1);
        assert!(offer(&["dino".into(), String::new()], &Fake).dirs);
        has("dino -", &["--help", "--version"]);
        lacks("dino -", &["ls"]);
    }

    #[test]
    fn sessions_for_the_commands_that_take_one() {
        for c in ["attach", "kill", "resume", "rm", "fork"] {
            has(&format!("dino {c} "), &["3", "1"]);
        }
        has("dino rm --force ", &["3"]);
        has("dino fork --name x ", &["3"]);
        lacks("dino fork --name ", &["3"]);
        lacks("dino attach 3 ", &["3", "1"]);
        lacks("dino fork 3 -- ", &["3"]);
    }

    #[test]
    fn flags_by_command_and_not_twice() {
        has("dino ls -", &["--usage", "--json", "--help"]);
        lacks("dino ls --json -", &["--json"]);
        lacks("dino ls -u -", &["--usage"]);
        has("dino ls ", &["--usage"]);
        has("dino stats --range ", &["7d", "30d", "all"]);
        has("dino stats ", &["overview", "models"]);
        has("dino rm -", &["--force"]);
        has("dino new -", &["--worktree", "--stay"]);
    }

    #[test]
    fn new_and_folders_take_agents() {
        has("dino new ", &["claude"]);
        has("dino new --worktree ", &["claude"]);
        has("dino new claude -", &["--on"]);
        lacks("dino new claude --model -", &["--on"]);
        assert!(offer(&["dino".into(), "new".into(), "claude".into(), String::new()], &Fake).files);
        has("dino . ", &["claude"]);
        has("dino ~/src ", &["claude"]);
        lacks("dino claude ", &["claude"]);
        assert!(offer(&["dino".into(), "claude".into(), String::new()], &Fake).files);
    }

    #[test]
    fn subcommands_and_their_words() {
        has("dino init ", &["zsh", "bash", "fish"]);
        has("dino completions ", &["zsh", "bash", "fish"]);
        has("dino shell ", &["install", "uninstall"]);
        has("dino shell install ", &["zsh", "bash", "fish"]);
        has("dino fallback ", &["claude", "codex"]);
        has("dino fallback claude ", &["off"]);
        has("dino fallback claude --new-sessions ", &["codex"]);
        has("dino login ", &["openrouter", "chatgpt", "zai"]);
        has("dino login zai -", &["--base"]);
        lacks("dino login -", &["--base"]);
        has("dino ai suggest --agent ", &["claude"]);
        assert!(offer(&["dino".into(), "ai".into(), "--cwd".into(), String::new()], &Fake).dirs);
    }

    #[test]
    fn automations() {
        has("dino automations ", &["show", "add", "rm"]);
        has("dino automation ", &["show"]);
        has("dino automations -", &["--json"]);
        lacks("dino automations -", &["--daily"]);
        has("dino automations show ", &["nightly"]);
        has("dino automations rm ", &["nightly"]);
        lacks("dino automations add ", &["nightly"]);
        has("dino automations add x -", &["--daily", "--agent"]);
        has("dino automations add x --agent ", &["claude"]);
        has("dino automations edit nightly -", &["--daily"]);
        lacks("dino automations show nightly -", &["--daily"]);
    }

    #[test]
    fn every_command_dino_runs_is_offered() {
        #[cfg(target_os = "linux")]
        let usage = crate::linux_usage();
        #[cfg(not(target_os = "linux"))]
        let usage = crate::USAGE.to_string();
        // The ones `dino --help` lists here.
        let mut names = vec![
            "ls",
            "status",
            "new",
            "attach",
            "resume",
            "kill",
            "fork",
            "rm",
            "found",
            "continue",
            "stats",
            "automations",
            "login",
            "logout",
            "sync",
            "claude-token",
            "fallback",
            "build-cache",
            "init",
            "shell",
            "ai",
            "search",
            "mcp",
            "ping",
            "stop",
            "completions",
        ];
        if cfg!(target_os = "macos") {
            names.extend(["power", "permissions"]);
        }
        if cfg!(target_os = "linux") {
            names.push("service");
        }
        for name in names {
            assert!(commands().any(|c| c.name == name), "{name}");
            assert!(usage.contains(&format!("dino {name}")) || usage.contains(&format!("| {name}")), "--help lists {name}");
        }
    }

    /// macOS's commands on a Mac only, Linux's on Linux only, as `main` runs them.
    #[test]
    fn os_commands_only_where_they_run() {
        if cfg!(target_os = "macos") {
            has("dino ", &["power", "permissions"]);
            has("dino power ", &["status", "setup", "remove"]);
            has("dino permissions -", &["--json"]);
        } else {
            lacks("dino ", &["power", "permissions"]);
            lacks("dino power ", &["status", "setup", "remove"]);
        }
        if cfg!(target_os = "linux") {
            has("dino ", &["service"]);
            has("dino service ", &["install", "uninstall", "status"]);
        } else {
            lacks("dino ", &["service"]);
            lacks("dino service ", &["install", "uninstall"]);
        }
    }
}
