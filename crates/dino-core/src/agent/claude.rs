//! Claude Code, and Claude Code on dino's free tier (`free`), which answers as the Anthropic API.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{Agent, ControlKind, StatusSource, Wiring, strings};
use crate::claude_config;
use crate::found::{self, FoundSession, Source};
use crate::history::{self, Turn};
use crate::models::{self, Catalog};
use crate::providers::Format;

pub(crate) struct Claude {
    pub(crate) free: bool,
}

/// The `~/.claude/sessions/<pid>.json` Claude process `pid` keeps while it runs, in Claude's
/// config folder `config`.
fn session_file_in(config: &Path, pid: u32) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(config.join(format!("sessions/{pid}.json"))).ok()?).ok()
}

/// That file, in whichever of the config folders dino knows of it is (see `claude_config`).
fn session_file(pid: u32) -> Option<Value> {
    claude_config::homes().iter().find_map(|c| session_file_in(c, pid))
}

/// `v`, if it's the session file of `pid` as an interactive Claude.
fn interactive(v: Value, pid: u32) -> Option<Value> {
    (v["pid"].as_u64() == Some(pid as u64) && v["kind"].as_str().is_none_or(|k| k == "interactive")).then_some(v)
}

/// The live session file of an interactive Claude, if `pid` is one.
fn live(pid: u32) -> Option<Value> {
    interactive(session_file(pid)?, pid)
}

/// Whether Claude Code would take bypass in its cycle without asking first, for a session in
/// `cwd` with config folder `config`: the user accepted its warning about bypass once (it then
/// writes `skipDangerousModePermissionPrompt` to their settings), or the organization's settings
/// (`managed`) skip it; and no settings turn bypass off. A repository's own settings can't skip
/// the warning, so only these two are read for that.
fn bypass_ready(config: &Path, managed: &Path, cwd: &Path) -> bool {
    let read = |p: &Path| std::fs::read_to_string(p).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok());
    let (user, managed) = (read(&config.join("settings.json")), read(managed));
    let project = [read(&cwd.join(".claude/settings.json")), read(&cwd.join(".claude/settings.local.json"))];
    let off = |v: &Option<Value>| v.as_ref().and_then(|v| v.pointer("/permissions/disableBypassPermissionsMode")?.as_str()) == Some("disable");
    let skips = |v: &Option<Value>| v.as_ref().and_then(|v| v.get("skipDangerousModePermissionPrompt")?.as_bool()) == Some(true);
    !(off(&user) || off(&managed) || project.iter().any(off)) && (skips(&user) || skips(&managed))
}

/// Claude Code's command line (`claude --help`, 2.1), for finding a prompt in it.
const CLI: super::Cli = super::Cli {
    value: &[
        "--agent", "--agents", "--append-system-prompt", "--append-system-prompt-file", "--autocompact", "--debug-file", "--effort", "--environment",
        "--fallback-model", "--input-format", "--json-schema", "--max-budget-usd", "--max-turns", "--model", "-n", "--name", "--output-format",
        "--permission-mode", "--permission-prompts", "--permission-prompt-tool", "--plugin-dir", "--plugin-url", "--remote-control-session-name-prefix",
        "--session-id", "--setting-sources", "--settings", "--system-prompt", "--system-prompt-file", "--system-prompt-snapshot",
    ],
    optional: &["--cloud", "-d", "--debug", "--from-pr", "--prompt-suggestions", "--remote-control", "-r", "--resume", "--teleport", "-w", "--worktree"],
    variadic: &["--add-dir", "--allowedTools", "--allowed-tools", "--betas", "--disallowedTools", "--disallowed-tools", "--file", "--mcp-config", "--tools"],
    flags: &[
        "--allow-dangerously-skip-permissions", "--ax-screen-reader", "--bg", "--background", "--bare", "--brief", "--chrome", "-c", "--continue",
        "--dangerously-skip-permissions", "--desktop", "--disable-slash-commands", "--exclude-dynamic-system-prompt-sections", "--fork-session",
        "--forward-subagent-text", "-h", "--help", "--ide", "--include-hook-events", "--include-partial-messages", "--no-chrome",
        "--no-session-persistence", "-p", "--print", "--replay-user-messages", "--restricted", "--safe-mode", "--strict-mcp-config", "--tmux",
        "--verbose", "-v", "--version",
    ],
    commands: &[
        "agents", "attach", "auth", "auto-mode", "config", "doctor", "gateway", "import", "install", "logs", "mcp", "migrate-installer", "plugin",
        "plugins", "purge", "respawn", "rm", "setup-token", "stop", "kill", "ultrareview", "update", "upgrade",
    ],
};

impl Agent for Claude {
    fn id(&self) -> &'static str {
        if self.free { "claude-free" } else { "claude" }
    }

    fn modes(&self) -> &'static [&'static str] {
        &["ask", "edits", "plan", "auto", "bypass"]
    }

    // The free tier picks the model for each turn.
    fn picks_model(&self) -> bool {
        !self.free
    }

    fn answers_once(&self) -> bool {
        !self.free
    }

    // No tools at all, no MCP servers, nothing saved: a plain answer. In a folder Claude doesn't
    // trust, not the project's settings either: their hooks would run.
    fn one_shot(&self, ask: &super::OneShot) -> Vec<String> {
        let mut out = crate::trust::claude_headless_args(crate::trust::claude_trusts(ask.cwd));
        out.extend(strings(&["-p", "--tools", "", "--strict-mcp-config", "--no-session-persistence", "--output-format", "text", "--system-prompt", ask.instructions]));
        out.extend(ask.controls.iter().cloned());
        out.push(ask.request.into());
        out
    }

    fn free(&self) -> bool {
        self.free
    }

    // After `--`: an option before it that takes several values (`--add-dir`, `--disallowedTools`)
    // would otherwise take the prompt as one more.
    fn prompt_args(&self, prompt: String) -> Vec<String> {
        vec!["--".into(), prompt]
    }

    // `claude [options] [prompt]`, as Claude Code 2.1's own help lists its options.
    fn launch_prompt(&self, args: &[String]) -> Option<(Vec<String>, String)> {
        super::positional_prompt(args, &CLI)
    }

    // Claude's own words, as its footer shows them.
    fn mode_label(&self, mode: &str) -> Option<&'static str> {
        Some(match mode {
            "ask" => "Manual",
            "edits" => "Accept edits",
            "plan" => "Plan",
            "auto" => "Auto",
            "bypass" => "Bypass permissions",
            _ => return None,
        })
    }

    fn mode_args(&self, mode: &str) -> Vec<String> {
        let m = match mode {
            "ask" => "manual",
            "edits" => "acceptEdits",
            "plan" => "plan",
            "auto" => "auto",
            _ => "bypassPermissions",
        };
        strings(&["--permission-mode", m])
    }

    fn model_args(&self, model: &str) -> Vec<String> {
        strings(&["--model", model])
    }

    fn effort_args(&self, effort: &str) -> Vec<String> {
        strings(&["--effort", effort])
    }

    fn value_flags(&self) -> &'static [&'static str] {
        &["--permission-mode", "--model", "--effort"]
    }

    fn control_of(&self, name: &str, _value: Option<&str>) -> Option<ControlKind> {
        match name {
            "--permission-mode" | "--dangerously-skip-permissions" => Some(ControlKind::Mode),
            "--model" => Some(ControlKind::Model),
            "--effort" => Some(ControlKind::Effort),
            _ => None,
        }
    }

    // A session started with `--dangerously-skip-permissions` is in bypass.
    fn read_mode(&self, flags: &[(&str, Option<&str>)]) -> Option<String> {
        let &(name, value) = flags.last()?;
        if name == "--dangerously-skip-permissions" { Some("bypass".into()) } else { value.and_then(|v| self.reported_mode(v)) }
    }

    /// One of Claude's permission modes in dino's words; `dontAsk` has none.
    fn reported_mode(&self, mode: &str) -> Option<String> {
        let id = match mode {
            "default" | "manual" => "ask",
            "acceptEdits" => "edits",
            "plan" => "plan",
            "auto" => "auto",
            "bypassPermissions" => "bypass",
            _ => return None,
        };
        Some(id.into())
    }

    // Its footer, under the prompt: "⏸ manual mode on · ? for shortcuts", "⏵⏵ accept edits on
    // (shift+tab to cycle)". Versions before 2.1.2xx name no mode in the default one, only "? for
    // shortcuts".
    fn screen_mode(&self, screen: &str) -> Option<String> {
        const SAYS: &[(&str, &str)] = &[
            ("manual mode on", "ask"),
            ("default mode on", "ask"),
            ("accept edits on", "edits"),
            ("plan mode on", "plan"),
            ("auto mode on", "auto"),
            ("bypass permissions on", "bypass"),
        ];
        for line in screen.lines().rev().filter(|l| !l.trim().is_empty()).take(3) {
            let t = line.trim_start();
            if let Some(rest) = t.strip_prefix("⏵⏵").or_else(|| t.strip_prefix('⏸')) {
                return SAYS.iter().find(|(says, _)| rest.trim_start().starts_with(says)).map(|(_, m)| m.to_string());
            }
            if t.starts_with("? for shortcuts") {
                return Some("ask".into());
            }
        }
        None
    }

    // Shift+Tab, Claude Code 2.1: manual → accept edits → plan → bypass (when started with it
    // allowed) → auto (when the account and model have it) → manual.
    fn mode_cycle(&self, args: &[String]) -> Option<(&'static str, Vec<&'static str>)> {
        let bypass = args.iter().enumerate().any(|(i, a)| {
            matches!(a.as_str(), "--dangerously-skip-permissions" | "--allow-dangerously-skip-permissions" | "--permission-mode=bypassPermissions")
                || (a == "--permission-mode" && args.get(i + 1).is_some_and(|v| v == "bypassPermissions"))
        });
        let mut order = vec!["ask", "edits", "plan"];
        if bypass {
            order.push("bypass");
        }
        order.push("auto");
        Some(("\x1b[Z", order))
    }

    // Bypass in Shift+Tab's cycle from the start, so switching into it is a keypress. Only once
    // Claude's warning about bypass is out of the way: until the user accepts it, the flag puts
    // that warning up as every session starts, and declining it quits (seen with 2.1.291).
    fn reach_args(&self, mode: &str, cwd: &Path, config: Option<&Path>) -> Vec<String> {
        let config = config.map_or_else(claude_config::home, Path::to_path_buf);
        if mode == "bypass" && bypass_ready(&config, Path::new(models::CLAUDE_MANAGED), cwd) {
            strings(&["--allow-dangerously-skip-permissions"])
        } else {
            vec![]
        }
    }

    fn catalog_key(&self) -> &'static str {
        "claude"
    }

    fn catalog_sources(&self) -> Vec<PathBuf> {
        models::claude_sources()
    }

    fn catalog(&self, program: &str) -> Option<Catalog> {
        let ask = |arg: &str| std::process::Command::new(program).arg(arg).stdin(std::process::Stdio::null()).output().ok();
        let version = ask("--version").and_then(|o| models::version_of(&String::from_utf8_lossy(&o.stdout)));
        // No catalog here (Claude keeps one only once it has fetched it; a new config folder has
        // none): the effort levels its help says `--effort` takes, for whichever model it's on.
        models::claude_from_files(version.as_deref()).or_else(|| {
            let efforts = models::claude_help_efforts(&String::from_utf8_lossy(&ask("--help")?.stdout));
            (!efforts.is_empty()).then(|| Catalog { efforts, ..Catalog::default() })
        })
    }

    fn wiring(&self, route: bool, base: &dyn Fn(&str) -> String, status_line: Option<String>) -> Wiring {
        let hooks = vec!["--settings".into(), crate::claude_hook_settings(&base("hook"), status_line)];
        if self.free {
            // Claude Code on the free pool: dino answers as the Anthropic API and routes each
            // request. The token is a placeholder so Claude Code skips its own login; the proxy
            // holds the real keys.
            let env = [
                ("ANTHROPIC_BASE_URL", base("free")),
                ("ANTHROPIC_AUTH_TOKEN", "dino-free".into()),
                ("ANTHROPIC_MODEL", "auto".into()),
                ("ANTHROPIC_DEFAULT_OPUS_MODEL", "auto".into()),
                ("ANTHROPIC_DEFAULT_SONNET_MODEL", "auto".into()),
                ("ANTHROPIC_DEFAULT_HAIKU_MODEL", "auto-fast".into()),
                ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1".into()),
            ];
            return (env.into_iter().map(|(k, v)| (k.to_string(), v)).collect(), hooks);
        }
        let env = if crate::user_set(route, "ANTHROPIC_BASE_URL") { vec![] } else { vec![("ANTHROPIC_BASE_URL".into(), base("anthropic"))] };
        (env, hooks)
    }

    fn provider_formats(&self) -> &'static [Format] {
        if self.free { &[] } else { &[Format::Anthropic] }
    }

    // Every model it would pick, background ones too, is the chosen one: otherwise its side calls
    // ask the provider for Claude models. The token is a placeholder: dino's proxy swaps whatever
    // Claude sends (its own claude.ai login included) for the provider's credentials.
    fn provider_wiring(&self, url: &str, format: Format, model: &str) -> Option<Wiring> {
        if self.free || format != Format::Anthropic {
            return None;
        }
        let mut env: Vec<(String, String)> = vec![
            ("ANTHROPIC_BASE_URL".into(), url.into()),
            ("ANTHROPIC_AUTH_TOKEN".into(), "dino".into()),
            ("ANTHROPIC_API_KEY".into(), String::new()),
            ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".into(), "1".into()),
        ];
        for var in ["ANTHROPIC_MODEL", "ANTHROPIC_DEFAULT_OPUS_MODEL", "ANTHROPIC_DEFAULT_SONNET_MODEL", "ANTHROPIC_DEFAULT_HAIKU_MODEL", "ANTHROPIC_DEFAULT_FABLE_MODEL", "ANTHROPIC_SMALL_FAST_MODEL"] {
            env.push((var.into(), model.into()));
        }
        Some((env, strings(&["--model", model])))
    }

    fn metered(&self) -> bool {
        true
    }

    fn session_args(&self, session: &mut Option<String>, restoring: bool) -> (Vec<String>, Vec<String>) {
        let uuid = session.get_or_insert_with(crate::new_uuid).clone();
        // Claude only saves a transcript after the first prompt; resuming an unused id fails.
        let flag = if restoring && crate::transcript::claude_path(&uuid).is_some() { "--resume" } else { "--session-id" };
        (vec![], vec![flag.into(), uuid])
    }

    // `--resume <parent> --fork-session` copies the conversation into a new one (Claude Code 2.1),
    // given its id up front with `--session-id`. The copy starts without the original's "allow for
    // this session" grants.
    fn fork_args(&self, parent: &str, _cwd: &Path, session: &mut Option<String>) -> Option<(Vec<String>, Vec<String>)> {
        // Kept when it starts again before the copy is saved.
        let uuid = session.get_or_insert_with(crate::new_uuid).clone();
        Some((vec![], strings(&["--resume", parent, "--fork-session", "--session-id", &uuid])))
    }

    // `/branch` writes the copy with each entry's `forkedFrom` naming the original (Claude Code
    // 2.1.289); `--fork-session` doesn't say.
    fn forked_from(&self, session: &str) -> Option<String> {
        let path = crate::transcript::claude_path(session)?;
        history::claude_forked_from(&history::read_range(&path, 0, 512 * 1024)?).filter(|parent| parent != session)
    }

    // Its live session file names the conversation it's on now: `/clear`, `/resume` and `/branch`
    // change it.
    fn conversation_of(&self, pid: u32) -> Option<String> {
        live(pid)?["sessionId"].as_str().map(String::from)
    }

    fn statusline(&self) -> bool {
        true
    }

    fn session_tools(&self) -> bool {
        true
    }

    /// Its permission and trust dialogs each end in "Esc to cancel".
    fn asking(&self, screen: &str) -> Option<String> {
        screen.contains("Esc to cancel").then(|| "Claude asks".into())
    }

    fn asks_trust(&self) -> bool {
        true
    }

    fn trusted_in(&self, dir: &Path, root: &Path) -> Option<PathBuf> {
        crate::trust::claude_trusted_in(dir, root)
    }

    fn trust(&self, dir: &Path) -> anyhow::Result<()> {
        crate::trust::claude_trust(dir)
    }

    fn status_source(&self) -> StatusSource {
        StatusSource::Hooks
    }

    /// Claude reports `busy`/`idle` in `~/.claude/sessions/<pid>.json`.
    fn busy(&self, pid: u32) -> Option<bool> {
        Some(session_file(pid)?["status"].as_str()? == "busy")
    }

    fn portable_flags(&self, args: &[String]) -> Vec<String> {
        found::drop_flags(
            args,
            &["--resume", "-r", "--session-id", "--settings", "--teleport", "--from-pr", "--output-format", "--input-format"],
            &["--continue", "-c", "--fork-session", "--print", "-p"],
        )
    }

    /// Claude Code writes `~/.claude/sessions/<pid>.json` for every live process, in the config
    /// folder it runs with: those dino knows of.
    fn running(&self, procs: &crate::procinfo::Procs) -> Vec<FoundSession> {
        let mut out = vec![];
        let files = claude_config::homes().into_iter().flat_map(|c| std::fs::read_dir(c.join("sessions")).into_iter().flatten().flatten());
        for e in files {
            let p = e.path();
            if p.extension().is_none_or(|x| x != "json") {
                continue;
            }
            let Some(v) = std::fs::read_to_string(&p).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()) else { continue };
            let (Some(pid), Some(sid)) = (v["pid"].as_u64(), v["sessionId"].as_str()) else { continue };
            let pid = pid as u32;
            if v["kind"].as_str().is_some_and(|k| k != "interactive") || !found::started_before(procs.get(&pid), &v["startedAt"]) {
                continue;
            }
            let title = history::claude_title(sid).or_else(|| v["name"].as_str().map(String::from)).unwrap_or_else(|| "Claude Code session".into());
            let (terminal, args) = found::terminal_and_flags(self, pid);
            out.push(FoundSession {
                source: Source::Running,
                agent: "claude".into(),
                session_id: sid.into(),
                title,
                cwd: v["cwd"].as_str().map(String::from),
                updated_at: v["updatedAt"].as_u64().map_or(0, |ms| ms / 1000),
                pid: Some(pid),
                status: v["status"].as_str().map(String::from),
                terminal,
                args,
                url: None,
                tmux: None,
                unsure: None,
            });
        }
        out
    }

    /// Its native binary is named after its version (`…/claude/versions/2.1.288`); the installer's
    /// link is `claude`.
    // `-p` and its formats (the Agent SDK's way in), and its commands that aren't a conversation.
    fn headless(&self, args: &[String]) -> bool {
        super::runs_with(
            args,
            &["-p", "--print", "--output-format", "--input-format", "--sdk-url"],
            &[
                "agents", "auth", "auto-mode", "doctor", "gateway", "import", "install", "logs", "mcp", "plugin", "plugins", "purge", "respawn", "rm", "setup-token",
                "stop", "kill", "ultrareview", "update", "upgrade",
            ],
        )
    }

    fn may_be(&self, comm: &str) -> bool {
        !self.free && (comm.contains("/claude/versions/") || comm.rsplit('/').next() == Some("claude"))
    }

    // Found by its session file: its native binary is named after its version. One started with
    // a config folder dino doesn't know of has its file there: its environment says which, known
    // from then on.
    fn inside(&self, pid: u32, comm: &str, _args: &dyn Fn() -> Vec<String>) -> Option<FoundSession> {
        let v = live(pid).or_else(|| {
            let own = self.may_be(comm).then(|| claude_config::of_process(pid)).flatten()?;
            let v = interactive(session_file_in(&own, pid)?, pid)?;
            claude_config::note(&own);
            Some(v)
        })?;
        let mut s = found::by_hand("claude", pid);
        s.session_id = v["sessionId"].as_str().unwrap_or_default().into();
        s.title = crate::transcript::claude_path(&s.session_id)
            .and_then(|p| found::tail_title(&p, 512 * 1024))
            .or_else(|| v["name"].as_str().map(String::from))
            .unwrap_or_else(|| "Claude Code".into());
        s.cwd = v["cwd"].as_str().map(String::from);
        s.updated_at = v["updatedAt"].as_u64().map_or(0, |ms| ms / 1000);
        s.status = v["status"].as_str().map(String::from);
        s.args = self.portable_flags(&found::args_of(pid));
        Some(s)
    }

    fn recent(&self, leave_out: &dyn Fn(&str, u64) -> bool) -> Vec<FoundSession> {
        let mut out = vec![];
        for p in history::claude_transcripts() {
            let Some(sid) = p.file_stem().and_then(|s| s.to_str()).map(String::from) else { continue };
            let updated = history::modified(&p);
            if leave_out(&sid, updated) {
                continue;
            }
            let meta = history::claude_meta(&p);
            if meta.hidden {
                continue;
            }
            let title = meta.title.unwrap_or_else(|| "Claude Code session".into());
            out.push(history::recent("claude", sid, title, meta.cwd, updated));
        }
        out
    }

    /// Claude Code web sessions, picked in Claude with `--teleport`.
    fn cloud(&self, _program: &Path) -> Vec<FoundSession> {
        vec![FoundSession {
            source: Source::Cloud,
            agent: "claude".into(),
            session_id: String::new(),
            title: "Claude Code on the web".into(),
            cwd: None,
            updated_at: 0,
            pid: None,
            status: Some("pick a web session to teleport".into()),
            terminal: None,
            args: vec![],
            url: None,
            tmux: None,
            unsure: None,
        }]
    }

    fn cloud_args(&self, session_id: &str) -> Vec<String> {
        let mut args = vec!["--teleport".to_string()];
        if !session_id.is_empty() {
            args.push(session_id.into());
        }
        args
    }

    /// A subagent's id reads its own transcript.
    fn transcript(&self, session_id: &str) -> Option<PathBuf> {
        crate::transcript::claude_path(session_id).or_else(|| crate::transcript::claude_subagent_path(None, session_id))
    }

    fn turns(&self, text: &str, path: &Path, start: u64) -> Vec<Turn> {
        history::claude_turns(text, history::is_subagent(path).then_some(start == 0))
    }

    fn tail(&self, session_id: &str, budget: usize) -> Option<String> {
        crate::transcript::claude_tail(session_id, budget)
    }

    // The free tier's conversations are Claude's own files, read once, for Claude.
    fn usage(&self, seen: &mut crate::usage::Seen) -> Vec<crate::usage::Used> {
        if self.free {
            return vec![];
        }
        let mut out = vec![];
        for p in history::claude_usage_files() {
            if let Some((text, _)) = seen.new_lines(&p, b"\"usage\"") {
                out.extend(history::claude_usage_in(&text));
            }
        }
        out
    }

    // Its config folder holds its sign-in and its conversations; Bedrock and Vertex pick a cloud
    // account instead of Anthropic's.
    fn account_vars(&self) -> &'static [&'static str] {
        &[
            "CLAUDE_CONFIG_DIR",
            "ANTHROPIC_CUSTOM_HEADERS",
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "ANTHROPIC_VERTEX_PROJECT_ID",
            "CLOUD_ML_REGION",
            "AWS_PROFILE",
            "AWS_REGION",
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_SESSION_TOKEN",
            "AWS_BEARER_TOKEN_BEDROCK",
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A conversation kept in a config folder of its own (`CLAUDE_CONFIG_DIR`): once dino knows of
    /// the folder, it's resumed rather than started again with the same id (which Claude refuses,
    /// "Session ID … is already in use"), and its transcript, its subagents', and the file its
    /// process keeps are all read there.
    #[test]
    fn a_conversation_in_a_config_folder_of_its_own_is_found_there() {
        let config = std::env::temp_dir().join(format!("dino-claude-own-config-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&config);
        let (uuid, sub) = (crate::new_uuid(), "a1b2c3");
        let transcript = config.join(format!("projects/-r/{uuid}.jsonl"));
        let subagent = config.join(format!("projects/-r/{uuid}/subagents/agent-{sub}.jsonl"));
        std::fs::create_dir_all(subagent.parent().unwrap()).unwrap();
        std::fs::create_dir_all(config.join("sessions")).unwrap();
        std::fs::write(&transcript, "{}\n").unwrap();
        std::fs::write(&subagent, "{}\n").unwrap();
        // No process has this pid: macOS's go up to 99999.
        let pid = 5_000_000 + std::process::id();
        std::fs::write(config.join(format!("sessions/{pid}.json")), format!(r#"{{"pid":{pid},"sessionId":"{uuid}","status":"busy","kind":"interactive"}}"#)).unwrap();
        let claude = Claude { free: false };
        let resume = |c: &Claude| c.session_args(&mut Some(uuid.clone()), true).1;

        assert_eq!(resume(&claude), ["--session-id", uuid.as_str()], "a folder dino doesn't know of");
        assert_eq!(claude.conversation_of(pid), None);
        claude_config::note(&config);
        assert_eq!(resume(&claude), ["--resume", uuid.as_str()]);
        assert_eq!(claude.transcript(&uuid), Some(transcript.clone()));
        assert_eq!(crate::transcript::claude_subagent_path(Some(&uuid), sub), Some(subagent.clone()));
        assert_eq!(claude.transcript(sub), Some(subagent.clone()), "a subagent's id reads its own");
        assert!(history::claude_transcripts().contains(&transcript));
        let usage = history::claude_usage_files();
        assert!(usage.contains(&transcript) && usage.contains(&subagent));
        assert_eq!(claude.conversation_of(pid).as_deref(), Some(uuid.as_str()));
        assert_eq!(claude.busy(pid), Some(true));
        std::fs::remove_dir_all(&config).unwrap();
    }

    /// A Claude typed by hand into a dino shell with a config folder dino doesn't know of: its
    /// environment says which, and its session file is read there.
    #[test]
    fn a_claude_by_hand_is_found_by_its_own_config_folder() {
        const STAND_IN: &str = "DINO_TEST_CLAUDE_STAND_IN";
        if std::env::var_os(STAND_IN).is_some() {
            // The Claude: a process of ours (macOS hides its own programs' environments), around
            // for a while.
            std::thread::sleep(std::time::Duration::from_secs(30));
            return;
        }
        let config = std::env::temp_dir().join(format!("dino-claude-by-hand-config-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&config);
        std::fs::create_dir_all(config.join("sessions")).unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "agent::claude::tests::a_claude_by_hand_is_found_by_its_own_config_folder", "--test-threads=1"])
            .env(STAND_IN, "1")
            .env(claude_config::ENV, &config)
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        assert_eq!(claude_config::of_process(pid), Some(config.clone()));
        let uuid = crate::new_uuid();
        std::fs::write(config.join(format!("sessions/{pid}.json")), format!(r#"{{"pid":{pid},"sessionId":"{uuid}","cwd":"/r","kind":"interactive"}}"#)).unwrap();
        let claude = Claude { free: false };

        assert!(claude.inside(pid, "/usr/bin/vim", &Vec::new).is_none(), "only a process that may be Claude is asked");
        assert!(!claude_config::homes().contains(&config));
        let found = claude.inside(pid, "/Users/x/.local/share/claude/versions/2.1.292", &Vec::new).unwrap();
        assert_eq!(found.session_id, uuid);
        assert!(claude_config::homes().contains(&config), "known from then on");
        let _ = child.kill();
        let _ = child.wait();
        std::fs::remove_dir_all(&config).unwrap();
    }

    #[test]
    fn bypass_joins_the_cycle_only_once_its_warning_was_accepted_and_nothing_turns_it_off() {
        let root = std::env::temp_dir().join(format!("dino-bypass-ready-{}", std::process::id()));
        let (config, repo, managed) = (root.join("config"), root.join("repo"), root.join("managed-settings.json"));
        std::fs::create_dir_all(&config).unwrap();
        std::fs::create_dir_all(repo.join(".claude")).unwrap();
        let write = |p: &Path, json: &str| std::fs::write(p, json).unwrap();
        let ready = || bypass_ready(&config, &managed, &repo);
        let reach = |config: &Path| Claude { free: false }.reach_args("bypass", &repo, Some(config));

        assert!(!ready(), "never accepted: the flag would put the warning up at every start");
        write(&repo.join(".claude/settings.local.json"), r#"{"skipDangerousModePermissionPrompt": true}"#);
        assert!(!ready(), "a repository's own settings don't skip it");
        write(&config.join("settings.json"), r#"{"model": "opus", "skipDangerousModePermissionPrompt": true}"#);
        assert!(ready(), "accepted once, written to the user's settings");
        assert_eq!(reach(&config), ["--allow-dangerously-skip-permissions"]);
        assert!(Claude { free: false }.reach_args("plan", &repo, Some(&config)).is_empty(), "only bypass needs it");
        assert!(reach(&root.join("another-account")).is_empty(), "another config folder, not accepted there");

        write(&repo.join(".claude/settings.json"), r#"{"permissions": {"disableBypassPermissionsMode": "disable"}}"#);
        assert!(!ready(), "turned off in the repository");
        std::fs::remove_file(repo.join(".claude/settings.json")).unwrap();
        write(&managed, r#"{"permissions": {"disableBypassPermissionsMode": "disable"}}"#);
        assert!(!ready(), "turned off by the organization");
        write(&config.join("settings.json"), "{}");
        write(&managed, r#"{"skipDangerousModePermissionPrompt": true}"#);
        assert!(ready(), "the organization skips it");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
