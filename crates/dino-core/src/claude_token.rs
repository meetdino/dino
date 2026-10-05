//! The Claude subscription token: the one-year OAuth token `claude setup-token` prints, which
//! Claude Code reads from `CLAUDE_CODE_OAUTH_TOKEN` where it can't sign in in a browser
//! (https://code.claude.com/docs/en/authentication). It's for Claude Code itself, so dino gives
//! it to the real Claude Code it starts and to nothing else: no other agent, no provider route,
//! never on a command line.

use std::collections::HashMap;

use crate::settings::Settings;

/// Its name in dino's key store, and the variable Claude Code reads it from.
pub const KEY: &str = "CLAUDE_CODE_OAUTH_TOKEN";
/// When it was made, for its one-year expiry (unknown for a pasted one).
pub const CREATED_KEY: &str = "CLAUDE_CODE_OAUTH_TOKEN_CREATED";
/// How long `claude setup-token`'s tokens last.
pub const LIFETIME_SECS: u64 = 365 * 24 * 60 * 60;
/// What `claude setup-token`'s tokens start with.
const PREFIX: &str = "sk-ant-oat";

/// Your other Claude accounts in the key store: `CLAUDE_ACCOUNT_<n>`, n from 2 (1 is the account
/// Claude Code signed in with), each a token `claude setup-token` printed. When one is at its
/// limit, dino's proxy has Claude Code go on with the next (see `dino_proxy::accounts`).
pub const ACCOUNT_PREFIX: &str = "CLAUDE_ACCOUNT_";

/// The other accounts, in order: their number and token.
pub fn accounts(keys: &HashMap<String, String>) -> Vec<(u32, String)> {
    let mut v: Vec<(u32, String)> = keys
        .iter()
        .filter_map(|(k, t)| {
            let n: u32 = k.strip_prefix(ACCOUNT_PREFIX)?.parse().ok().filter(|n| *n >= 2)?;
            let t = t.trim();
            t.starts_with(PREFIX).then(|| (n, t.to_string()))
        })
        .collect();
    v.sort();
    v
}

/// Account `n`'s name in the key store.
pub fn account_key(n: u32) -> String {
    format!("{ACCOUNT_PREFIX}{n}")
}

/// Where a Claude Code dino starts runs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Launch {
    /// A session on this Mac (scheduled tasks too).
    Local,
    /// A session on an SSH environment, where Claude Code usually has no sign-in.
    Remote,
    /// dino's own `claude -p`: the AI line, Review, side chat.
    Headless,
}

/// Whether `token` looks like one `claude setup-token` makes.
pub fn valid(token: &str) -> bool {
    let t = token.trim();
    t.starts_with(PREFIX) && t.len() > 40 && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// The token in `claude setup-token`'s output (a terminal's screen text): it may wrap across lines.
pub fn find(text: &str) -> Option<String> {
    let at = text.find(PREFIX)?;
    let mut token = String::new();
    for line in text[at..].lines() {
        let part = line.trim();
        let run: String = part.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
        if run.is_empty() {
            break;
        }
        token.push_str(&run);
        // The rest of the line wasn't token, or it filled the line: only a full line wraps.
        if run.len() < part.len() {
            break;
        }
    }
    valid(&token).then_some(token)
}

/// The token for a Claude Code session of `agent_id` started as `launch`, if it should get one:
/// only the real Claude Code (not the free tier's, not one on a provider's model), and on this Mac
/// only when Settings say so or Claude Code here isn't signed in (`signed_in`, as last checked).
pub fn for_launch(agent_id: &str, launch: Launch, routed: bool, settings: &Settings, keys: &HashMap<String, String>, signed_in: Option<bool>) -> Option<String> {
    if agent_id != "claude" || routed {
        return None;
    }
    let token = keys.get(KEY).map(|t| t.trim()).filter(|t| valid(t))?;
    let wanted = match launch {
        Launch::Remote => settings.machine.claude_token.ssh,
        Launch::Local | Launch::Headless => settings.machine.claude_token.local || signed_in == Some(false),
    };
    wanted.then(|| token.to_string())
}

/// Where dinod notes whether Claude Code on this Mac is signed in, for `dino` commands to read.
pub fn signed_in_file() -> std::path::PathBuf {
    crate::config_dir().join("claude-signed-in")
}

/// Whether Claude Code on this Mac was signed in when dinod last looked; `None` if it hasn't.
pub fn signed_in() -> Option<bool> {
    match std::fs::read_to_string(signed_in_file()).ok()?.trim() {
        "yes" => Some(true),
        "no" => Some(false),
        _ => None,
    }
}

/// The token, masked for showing: its kind and last four characters.
pub fn masked(token: &str) -> String {
    let t = token.trim();
    let tail: String = t.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
    format!("{PREFIX}…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: &str = "sk-ant-oat01-AbCdEfGhIjKlMnOpQrStUvWxYz0123456789_abcdefghijklmnop-QRSTUV";

    fn keys() -> HashMap<String, String> {
        HashMap::from([(KEY.to_string(), T.to_string())])
    }

    #[test]
    fn finds_the_token_in_setup_tokens_output_even_wrapped() {
        let out = format!("✓ Long-lived authentication token created successfully!\n\nYour OAuth token (valid for 1 year):\n\n{T}\n\nStore this token securely.");
        assert_eq!(find(&out).as_deref(), Some(T));
        // A narrow terminal wraps it across full lines.
        let (a, b) = T.split_at(30);
        assert_eq!(find(&format!("token:\n{a}\n{b}\n\nStore this")).as_deref(), Some(T));
        assert_eq!(find("no token here"), None);
        assert_eq!(find("sk-ant-oat01-short"), None);
    }

    #[test]
    fn other_accounts_in_order() {
        let keys: HashMap<String, String> = [
            ("CLAUDE_ACCOUNT_3", T),
            ("CLAUDE_ACCOUNT_2", " sk-ant-oat01-two \n"),
            ("CLAUDE_ACCOUNT_4", "sk-ant-api03-not-a-subscription"),
            ("CLAUDE_ACCOUNT_1", "sk-ant-oat01-one-is-claude-codes-own"),
            ("CLAUDE_ACCOUNT_X", "sk-ant-oat01-no-number"),
            (KEY, "sk-ant-oat01-the-subscription-token"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        assert_eq!(accounts(&keys), vec![(2, "sk-ant-oat01-two".to_string()), (3, T.to_string())]);
        assert_eq!(account_key(2), "CLAUDE_ACCOUNT_2");
    }

    #[test]
    fn only_the_real_claude_code_gets_it() {
        let mut s = Settings::default();
        for agent in ["claude-free", "codex", "qwen", "kimi", "pi", "hermes", "codewhale", "opencode", "copilot", "cursor", "amp", "shell"] {
            for launch in [Launch::Local, Launch::Remote, Launch::Headless] {
                assert_eq!(for_launch(agent, launch, false, &s, &keys(), Some(false)), None, "{agent} {launch:?}");
            }
        }
        // On a provider's model Claude Code isn't talking to Anthropic as itself.
        assert_eq!(for_launch("claude", Launch::Remote, true, &s, &keys(), Some(false)), None);
        // SSH environments by default; this Mac only if asked, or when it isn't signed in.
        assert!(s.machine.claude_token.ssh && !s.machine.claude_token.local);
        assert_eq!(for_launch("claude", Launch::Remote, false, &s, &keys(), Some(true)).as_deref(), Some(T));
        assert_eq!(for_launch("claude", Launch::Local, false, &s, &keys(), Some(true)), None);
        assert_eq!(for_launch("claude", Launch::Local, false, &s, &keys(), None), None, "unknown: its own sign-in stays");
        assert_eq!(for_launch("claude", Launch::Headless, false, &s, &keys(), Some(false)).as_deref(), Some(T));
        s.machine.claude_token.local = true;
        assert_eq!(for_launch("claude", Launch::Local, false, &s, &keys(), Some(true)).as_deref(), Some(T));
        s.machine.claude_token.ssh = false;
        assert_eq!(for_launch("claude", Launch::Remote, false, &s, &keys(), Some(false)), None);
        // Something that isn't a setup-token token is never handed on.
        let bad = HashMap::from([(KEY.to_string(), "sk-ant-api03-whatever-this-is-an-api-key-0000000000".to_string())]);
        assert_eq!(for_launch("claude", Launch::Local, false, &s, &bad, Some(false)), None);
    }

    /// No adapter's own wiring, to dino's proxy or to a provider's model, carries the token: it's
    /// added only where `for_launch` says, and only for the real Claude Code.
    #[test]
    fn no_agent_or_provider_route_wiring_carries_it() {
        use crate::providers::Format;
        let ids = ["claude", "claude-free", "codex", "qwen", "qwen-free", "kimi", "kimi-free", "pi", "pi-free", "hermes", "hermes-free", "codewhale", "opencode", "copilot", "cursor", "amp"];
        let base = |p: &str| format!("http://127.0.0.1:1/s/1/{p}");
        for id in ids {
            let a = crate::agent::agent(id).unwrap_or_else(|| panic!("no adapter {id}"));
            let mut wirings = vec![a.wiring(true, &base, None), a.wiring(false, &base, None), a.serve(4096, "pw")];
            for f in [Format::Anthropic, Format::Chat, Format::Responses] {
                wirings.extend(a.provider_wiring("http://127.0.0.1:1/s/1/or", f, "some/model"));
            }
            for (env, args) in wirings {
                assert!(!env.iter().any(|(k, v)| k == KEY || v.contains(PREFIX)), "{id} env {env:?}");
                assert!(!args.iter().any(|x| x.contains(KEY) || x.contains(PREFIX)), "{id} args {args:?}");
            }
        }
    }

    #[test]
    fn masked_shows_only_the_end() {
        assert_eq!(masked(T), "sk-ant-oat…STUV");
        assert!(!masked(T).contains("AbCd"));
    }
}
