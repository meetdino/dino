//! The user's other Claude accounts: `CLAUDE_ACCOUNT_2`, `_3`… in dino's key store, each a token
//! `claude setup-token` printed. When the account Claude Code signed in with reaches its
//! subscription limit, its calls go on with the next account that hasn't, and back to its own
//! once that resets. Only Claude Code's own calls to Anthropic, signed in with a subscription: a
//! Claude subscription token never goes to another route or another agent.
use std::collections::HashMap;

/// The key store names: `CLAUDE_ACCOUNT_<n>`, n from 2 (1 is the account Claude Code signed in with).
pub const PREFIX: &str = "CLAUDE_ACCOUNT_";

/// The other accounts, in order: their number and token.
pub fn others(keys: &HashMap<String, String>) -> Vec<(u32, String)> {
    let mut v: Vec<(u32, String)> = keys
        .iter()
        .filter_map(|(k, t)| {
            let n: u32 = k.strip_prefix(PREFIX)?.parse().ok()?;
            let t = t.trim();
            crate::fallback::is_claude_subscription(t).then(|| (n, t.to_string()))
        })
        .collect();
    v.sort();
    v
}

/// The account signing with `token` as fallbacks track routes (see `fallback::route_key`): by
/// its token, not its number, so a spent account stays spent when the user reorders them.
pub fn key(token: &str) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(token.trim(), &mut h);
    format!("{KEY_PREFIX}{:08x}", std::hash::Hasher::finish(&h) as u32)
}

const KEY_PREFIX: &str = "anthropic#account-";

/// One of the other accounts' keys, not Claude Code's own.
pub fn is_key(key: &str) -> bool {
    key.starts_with(KEY_PREFIX)
}

/// As the session shows it.
pub fn name(n: u32) -> String {
    format!("Claude account {n}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn other_accounts_in_order() {
        let keys: HashMap<String, String> = [
            ("CLAUDE_ACCOUNT_3", "sk-ant-oat01-three"),
            ("CLAUDE_ACCOUNT_2", " sk-ant-oat01-two \n"),
            ("CLAUDE_ACCOUNT_4", "sk-ant-api03-not-a-subscription"),
            ("CLAUDE_ACCOUNT_X", "sk-ant-oat01-no-number"),
            ("ANTHROPIC_API_KEY", "sk-ant-oat01-other-key"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        assert_eq!(others(&keys), vec![(2, "sk-ant-oat01-two".to_string()), (3, "sk-ant-oat01-three".to_string())]);
    }
}
