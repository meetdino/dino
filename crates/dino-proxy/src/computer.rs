//! An agent using the Mac or a browser, told from the tools it calls: Claude Code's computer use
//! and Claude in Chrome, Codex's Computer Use plugin, open-computer-use, and the common browser
//! MCP servers (Playwright, Chrome DevTools). Nothing is added to the agent: its hooks or its own
//! record already name every tool it calls, and each agent names an MCP tool in its own way
//! (`mcp__<server>__<tool>`, Codex's namespace and name, `mcp_<server>_<tool>`, `<server>_<tool>`).

use std::time::{Duration, Instant};

use crate::{SessionStats, Stats};

/// What the agent reaches outside its terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reach {
    /// Apps on this Mac: it sees the screen and clicks and types.
    Computer,
    /// A web browser.
    Browser,
}

impl Reach {
    pub fn word(self) -> &'static str {
        match self {
            Reach::Computer => "computer",
            Reach::Browser => "browser",
        }
    }
}

/// MCP servers that drive the Mac's apps, by name (lowercase, `-` as `_`).
/// (`cua_repl`: Codex's newer Computer Use, a REPL of its own.)
const COMPUTER_SERVERS: &[&str] = &["open_computer_use", "computer_use", "computeruse", "cua_repl"];
/// MCP servers that drive a browser.
const BROWSER_SERVERS: &[&str] = &["claude_in_chrome", "chrome_devtools", "playwright", "puppeteer", "browser_use", "browsermcp", "browser"];

/// open-computer-use's and Codex Computer Use's own tools, for agents that call an MCP tool by its
/// bare name. Only these can't be anything else; `click`, `scroll` and the like count only once
/// one of these was called (see `GENERIC`).
const COMPUTER_TOOLS: &[&str] = &["get_app_state", "list_apps", "perform_secondary_action"];
const GENERIC: &[&str] = &["click", "scroll", "drag", "type_text", "press_key", "set_value"];

/// How long the agent counts as using the Mac after its last such call ended: its next call is
/// usually seconds away, and the banner shouldn't flicker between them.
pub const LINGER: Duration = Duration::from_secs(5);
/// A call still open after this long went unanswered (the agent was stopped mid-call and said nothing).
const STALE: Duration = Duration::from_secs(120);
/// How long after a call by a distinctive name a bare generic one (`click`) still counts.
const GENERIC_AFTER: Duration = Duration::from_secs(600);

/// What a tool's name says it reaches, if anything. `known` says the session called a
/// computer-use tool by its distinctive bare name not long ago. Only the tool's name counts, never
/// what it was asked to do: a Bash `screencapture` or a Read of a screenshot isn't computer use.
pub fn reach_of(tool: &str, known: bool) -> Option<Reach> {
    let name = tool.to_ascii_lowercase().replace('-', "_");
    // `mcp__<server>__<tool>`: the server by its whole name (`browser_history` isn't `browser`),
    // or a plugin's (`plugin_<plugin>_<server>`).
    if let Some((server, tool)) = name.strip_prefix("mcp__").and_then(|r| r.split_once("__")) {
        let is = |servers: &[&str]| {
            servers.iter().any(|s| server == *s || (server.starts_with("plugin_") && server.strip_suffix(s).is_some_and(|p| p.ends_with('_'))))
        };
        return if is(COMPUTER_SERVERS) {
            Some(Reach::Computer)
        } else if is(BROWSER_SERVERS) || playwright(tool) {
            Some(Reach::Browser)
        } else {
            None
        };
    }
    // `mcp_<server>_<tool>` (Hermes), `<server>_<tool>` (OpenCode), or a bare name.
    let rest = name.strip_prefix("mcp__").or_else(|| name.strip_prefix("mcp_")).unwrap_or(&name);
    let server = |servers: &[&str]| {
        servers.iter().any(|s| rest.strip_prefix(s).is_some_and(|after| after.starts_with('_') && after.len() > 1))
    };
    // Longest first, so `open_computer_use` isn't read as some other server's tool.
    if server(COMPUTER_SERVERS) {
        return Some(Reach::Computer);
    }
    if server(BROWSER_SERVERS) {
        return Some(Reach::Browser);
    }
    if COMPUTER_TOOLS.contains(&rest) {
        return Some(Reach::Computer);
    }
    if known && GENERIC.contains(&rest) {
        return Some(Reach::Computer);
    }
    playwright(rest).then_some(Reach::Browser)
}

/// Playwright's tools (`browser_click`), wherever its server is registered under another name.
fn playwright(tool: &str) -> bool {
    tool.starts_with("browser_") && tool.len() > 8
}

/// The agent's use of the Mac or a browser, as its tool calls say.
#[derive(Clone, Debug)]
pub struct ComputerUse {
    pub reach: Reach,
    /// The last such tool, as the agent names it.
    pub tool: String,
    /// When the last such call started or ended.
    pub last: Instant,
    /// Calls started and not yet ended.
    pub open: u32,
    /// When it last called a tool only computer use has (`get_app_state`), for the generic names.
    known: Option<Instant>,
}

impl ComputerUse {
    /// In use now: a call is out, or one ended a moment ago.
    pub fn active(&self) -> bool {
        let since = self.last.elapsed();
        (self.open > 0 && since < STALE) || since < LINGER
    }
}

/// Where in a call the agent is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Started,
    Ended,
    /// Seen without its start or end (a record that only lists calls made).
    Called,
}

impl Stats {
    /// The agent called (or finished calling) tool `name`: noted only when it reaches the Mac or a browser.
    pub fn tool_call(&self, session: &str, name: &str, phase: Phase) {
        // A record that doesn't repeat the tool's name when its call ends: one of the calls out ended.
        if name.is_empty() {
            if phase == Phase::Ended {
                self.tools_ended(session, 1);
            }
            return;
        }
        // Nothing to lock for the tools nearly every call is (Bash, Read, Edit…).
        if reach_of(name, true).is_none() {
            return;
        }
        let known_now = |c: &Option<ComputerUse>| c.as_ref().and_then(|c| c.known).is_some_and(|t| t.elapsed() < GENERIC_AFTER);
        let mut sessions = self.sessions.lock().unwrap();
        let known = sessions.get(session).is_some_and(|s| known_now(&s.computer));
        let Some(reach) = reach_of(name, known) else { return };
        let s = sessions.entry(session.to_string()).or_default();
        let now = Instant::now();
        let distinctive = COMPUTER_TOOLS.iter().any(|t| name.ends_with(t));
        let c = s.computer.get_or_insert_with(|| ComputerUse { reach, tool: String::new(), last: now, open: 0, known: None });
        c.reach = reach;
        c.tool = name.to_string();
        c.last = now;
        if distinctive {
            c.known = Some(now);
        }
        match phase {
            Phase::Started => c.open += 1,
            Phase::Ended => c.open = c.open.saturating_sub(1),
            Phase::Called => {}
        }
    }

    /// The agent's turn is over (or it was stopped): no call of its is still out.
    pub fn tools_done(&self, session: &str) {
        self.tools_ended(session, u32::MAX);
    }

    fn tools_ended(&self, session: &str, n: u32) {
        if let Some(s) = self.sessions.lock().unwrap().get_mut(session) {
            s.calls_ended(n);
        }
    }

    /// What the agent reaches right now, if anything.
    pub fn using(&self, session: &str) -> Option<Reach> {
        let sessions = self.sessions.lock().unwrap();
        sessions.get(session)?.computer.as_ref().filter(|c| c.active()).map(|c| c.reach)
    }
}

impl SessionStats {
    /// Its turn ended: none of its calls is out any more, whatever was said last.
    pub(crate) fn calls_over(&mut self) {
        self.calls_ended(u32::MAX);
    }

    fn calls_ended(&mut self, n: u32) {
        if let Some(c) = self.computer.as_mut().filter(|c| c.open > 0) {
            c.open = c.open.saturating_sub(n);
            c.last = Instant::now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_agents_way_of_naming_a_tool_is_read() {
        // Claude Code, Qwen Code: mcp__<server>__<tool>.
        assert_eq!(reach_of("mcp__computer-use__screenshot", false), Some(Reach::Computer));
        assert_eq!(reach_of("mcp__open-computer-use__click", false), Some(Reach::Computer));
        assert_eq!(reach_of("mcp__claude-in-chrome__navigate", false), Some(Reach::Browser));
        assert_eq!(reach_of("mcp__claude-in-chrome__computer", false), Some(Reach::Browser), "Chrome's own `computer` tool is the browser");
        assert_eq!(reach_of("mcp__playwright__browser_click", false), Some(Reach::Browser));
        assert_eq!(reach_of("mcp__chrome-devtools__take_screenshot", false), Some(Reach::Browser));
        // Codex: its namespace and the tool's name.
        assert_eq!(reach_of("mcp__computer_use__list_apps", false), Some(Reach::Computer));
        // Hermes (mcp_<server>_<tool>) and OpenCode (<server>_<tool>).
        assert_eq!(reach_of("mcp_open_computer_use_get_app_state", false), Some(Reach::Computer));
        assert_eq!(reach_of("open-computer-use_type_text", false), Some(Reach::Computer));
        assert_eq!(reach_of("playwright_browser_navigate", false), Some(Reach::Browser));
        // Bare names.
        assert_eq!(reach_of("get_app_state", false), Some(Reach::Computer));
        assert_eq!(reach_of("browser_snapshot", false), Some(Reach::Browser));
        assert_eq!(reach_of("click", false), None, "a bare click could be anything");
        assert_eq!(reach_of("click", true), Some(Reach::Computer), "after get_app_state it's the Mac");
        // Everything else.
        // Claude Code's plugins: mcp__plugin_<plugin>_<server>__<tool>; Playwright under any name.
        assert_eq!(reach_of("mcp__plugin_playwright_playwright__browser_click", false), Some(Reach::Browser));
        assert_eq!(reach_of("mcp__pw__browser_navigate", false), Some(Reach::Browser));
        // Everything else: other servers whose names only start like one, and tools that merely
        // look at the screen or files (`screencapture` in Bash, a Read of a screenshot).
        for other in [
            "Bash",
            "Read",
            "mcp__linear__save_issue",
            "mcp__dino__list_sessions",
            "WebFetch",
            "browser",
            "mcp__computer-use",
            "computer_user_lookup",
            "mcp__browser-history__search",
            "mcp__computer-use-docs__search",
            "mcp__playwright-docs__search",
            "mcp__heroku__list_apps",
            "mcp__ide__getDiagnostics",
        ] {
            assert_eq!(reach_of(other, false), None, "{other}");
        }
    }

    #[test]
    fn in_use_while_a_call_is_out_and_a_moment_after() {
        let stats = Stats::default();
        stats.tool_call("s", "Bash", Phase::Started);
        assert_eq!(stats.using("s"), None);
        stats.tool_call("s", "mcp__computer-use__screenshot", Phase::Started);
        assert_eq!(stats.using("s"), Some(Reach::Computer));
        stats.tool_call("s", "mcp__computer-use__screenshot", Phase::Ended);
        assert_eq!(stats.using("s"), Some(Reach::Computer), "lingers");
        stats.update("s", |s| s.computer.as_mut().unwrap().last -= LINGER);
        assert_eq!(stats.using("s"), None);
        // Stopped mid-call: the turn's end closes it.
        stats.tool_call("s", "mcp__claude-in-chrome__navigate", Phase::Started);
        stats.update("s", |s| s.computer.as_mut().unwrap().last -= LINGER * 2);
        assert_eq!(stats.using("s"), Some(Reach::Browser), "still out");
        stats.tools_done("s");
        stats.update("s", |s| s.computer.as_mut().unwrap().last -= LINGER);
        assert_eq!(stats.using("s"), None);
    }

    #[test]
    fn a_bare_click_counts_after_a_look_at_an_app() {
        let stats = Stats::default();
        stats.tool_call("s", "click", Phase::Called);
        assert_eq!(stats.using("s"), None);
        stats.tool_call("s", "get_app_state", Phase::Called);
        stats.update("s", |s| s.computer.as_mut().unwrap().last -= LINGER);
        stats.tool_call("s", "click", Phase::Started);
        assert_eq!(stats.using("s"), Some(Reach::Computer));
        // Its record ends the call without naming it.
        stats.tool_call("s", "", Phase::Ended);
        stats.update("s", |s| s.computer.as_mut().unwrap().last -= LINGER);
        assert_eq!(stats.using("s"), None);
    }
}
