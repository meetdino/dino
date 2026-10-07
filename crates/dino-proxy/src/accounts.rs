//! The user's other Claude accounts: `CLAUDE_ACCOUNT_2`, `_3`… in dino's key store, each a token
//! `claude setup-token` printed. When the account Claude Code signed in with reaches its
//! subscription limit, its calls go on with the next account that hasn't, and back to its own
//! once that resets. Only Claude Code's own calls to Anthropic, signed in with a subscription: a
//! Claude subscription token never goes to another route or another agent.
use std::collections::HashMap;

use crate::Quota;
use crate::fallback::{Kind, Limited};

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

/// An account found spent, and when it's back, as far as that's known.
#[derive(Clone, Debug, PartialEq)]
pub struct Spent {
    pub resets_at: Option<u64>,
}

/// Whether an account is spent as calls last found it (`limited`, see `Stats::limited`: a limit's
/// refusal not tried again yet; a route merely down isn't spent) or as its own windows last said
/// (`quota`: one used up, until its reset). Back once all of those reset.
pub fn spent(limited: Option<&Limited>, quota: Option<&Quota>, now: u64) -> Option<Spent> {
    let refused = limited.filter(|l| l.kind != Kind::Outage).map(|l| Some(l.resets_at.unwrap_or(l.retry_at)));
    let windows = quota.into_iter().flat_map(|q| &q.windows).filter(|(_, w)| w.utilization >= 1.0 && w.resets_at.is_none_or(|r| r > now)).map(|(_, w)| w.resets_at);
    let resets: Vec<Option<u64>> = refused.into_iter().chain(windows).collect();
    if resets.is_empty() {
        return None;
    }
    // One that doesn't say when leaves it unknown.
    Some(Spent { resets_at: resets.into_iter().collect::<Option<Vec<u64>>>().and_then(|r| r.into_iter().max()) })
}

/// With every one of `accounts` spent (each as `spent` judges it), the one back first; `None`
/// while any has room, which the proxy signs Claude Code's calls with (see `forward`).
pub fn all_spent<'a>(accounts: impl IntoIterator<Item = (Option<&'a Limited>, Option<&'a Quota>)>, now: u64) -> Option<Spent> {
    let mut first: Option<Spent> = None;
    for (l, q) in accounts {
        let s = spent(l, q, now)?;
        // Unknown counts as last back.
        if first.as_ref().is_none_or(|f| s.resets_at.unwrap_or(u64::MAX) < f.resets_at.unwrap_or(u64::MAX)) {
            first = Some(s);
        }
    }
    first
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

    /// Claude Code is at its limit only with every account spent, each by its own refusal or its
    /// own windows; then it's back when the first of them is.
    #[test]
    fn at_the_limit_only_with_every_account_spent() {
        use crate::Window;
        let now = 1_000_000;
        let refused = |kind, resets_at| Limited { name: "Claude".into(), kind, said: "spent".into(), resets_at, retry_at: now + 60 };
        let window = |name: &str, utilization, resets_at| Quota { windows: vec![(name.into(), Window { utilization, resets_at, status: None })] };
        let full_7d = window("7d", 1.0, Some(now + 7200));
        let room = window("5h", 0.4, Some(now + 600));

        // No added account: its own, as before (its windows, or a refusal).
        assert_eq!(all_spent([(None, Some(&full_7d))], now), Some(Spent { resets_at: Some(now + 7200) }));
        assert_eq!(all_spent([(None, Some(&room))], now), None);
        assert_eq!(all_spent([(None, None)], now), None);
        let quota = refused(Kind::Quota, Some(now + 300));
        assert_eq!(all_spent([(Some(&quota), None)], now), Some(Spent { resets_at: Some(now + 300) }));

        // Its own spent, another account free (no use reported yet, or room left): it runs.
        assert_eq!(all_spent([(None, Some(&full_7d)), (None, None)], now), None);
        assert_eq!(all_spent([(Some(&quota), Some(&full_7d)), (None, Some(&room))], now), None);
        // A route merely down isn't spent, nor a window past its reset.
        let down = refused(Kind::Outage, None);
        assert_eq!(all_spent([(None, Some(&full_7d)), (Some(&down), None)], now), None);
        assert_eq!(all_spent([(None, Some(&full_7d)), (None, Some(&window("5h", 1.0, Some(now - 1))))], now), None);

        // Every one spent: the one back first, by its latest reset.
        let other = window("5h", 1.0, Some(now + 3600));
        let late = refused(Kind::Quota, Some(now + 9000));
        assert_eq!(
            all_spent([(Some(&quota), Some(&full_7d)), (None, Some(&other)), (Some(&late), None)], now),
            Some(Spent { resets_at: Some(now + 3600) })
        );
        // One that doesn't say when is back last.
        let unknown = window("7d", 1.0, None);
        assert_eq!(all_spent([(None, Some(&unknown)), (Some(&late), None)], now), Some(Spent { resets_at: Some(now + 9000) }));
    }
}
