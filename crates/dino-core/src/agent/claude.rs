//! Claude Code, and Claude Code on dino's free tier (`free`), which answers as the Anthropic API.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{Agent, ControlKind, StatusSource, Wiring, strings};
use crate::found::{self, FoundSession, Source};
use crate::history::{self, Turn};
use crate::models::{self, Catalog};
use crate::providers::Format;

pub(crate) struct Claude {
    pub(crate) free: bool,
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

/// The live `~/.claude/sessions/<pid>.json` of an interactive Claude, if `pid` is one.
fn live(pid: u32) -> Option<Value> {
    let text = std::fs::read_to_string(home().join(format!(".claude/sessions/{pid}.json"))).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    (v["pid"].as_u64() == Some(pid as u64) && v["kind"].as_str().is_none_or(|k| k == "interactive")).then_some(v)
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

    fn catalog_key(&self) -> &'static str {
        "claude"
    }

    fn catalog_sources(&self) -> Vec<PathBuf> {
        models::claude_sources()
    }

    fn catalog(&self, program: &str) -> Option<Catalog> {
        let out = std::process::Command::new(program).arg("--version").stdin(std::process::Stdio::null()).output().ok();
        let version = out.and_then(|o| models::version_of(&String::from_utf8_lossy(&o.stdout)));
        models::claude_from_files(version.as_deref())
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
        let text = std::fs::read_to_string(home().join(format!(".claude/sessions/{pid}.json"))).ok()?;
        let v: Value = serde_json::from_str(&text).ok()?;
        Some(v["status"].as_str()? == "busy")
    }

    fn portable_flags(&self, args: &[String]) -> Vec<String> {
        found::drop_flags(
            args,
            &["--resume", "-r", "--session-id", "--settings", "--teleport", "--from-pr", "--output-format", "--input-format"],
            &["--continue", "-c", "--fork-session", "--print", "-p"],
        )
    }

    /// Claude Code writes `~/.claude/sessions/<pid>.json` for every live process.
    fn running(&self, procs: &crate::procinfo::Procs) -> Vec<FoundSession> {
        let mut out = vec![];
        for e in std::fs::read_dir(home().join(".claude/sessions")).into_iter().flatten().flatten() {
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

    // Found by its session file: its native binary is named after its version.
    fn inside(&self, pid: u32, _comm: &str, _args: &dyn Fn() -> Vec<String>) -> Option<FoundSession> {
        let v = live(pid)?;
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

    fn recent(&self, running: &dyn Fn(&str) -> bool) -> Vec<FoundSession> {
        let mut out = vec![];
        for p in history::claude_transcripts() {
            let Some(sid) = p.file_stem().and_then(|s| s.to_str()).map(String::from) else { continue };
            let meta = history::claude_meta(&p);
            if meta.hidden || running(&sid) {
                continue;
            }
            let title = meta.title.unwrap_or_else(|| "Claude Code session".into());
            out.push(history::recent("claude", sid, title, meta.cwd, history::modified(&p)));
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
