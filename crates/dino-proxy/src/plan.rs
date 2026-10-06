//! `/s/<session>/plan/<id>/…`: a coding plan (the GLM Coding Plan, Kimi Code, DeepSeek…) with the
//! key the user pasted. Agents talk to it in their own shape: Anthropic Messages go to the plan's
//! Anthropic base (`{anthropic}/v1/messages`), the OpenAI APIs to its OpenAI base, version
//! included (`v1/chat/completions` → `{openai}/chat/completions`). The agent's own credentials
//! (a claude.ai login among them) are dropped for the plan's key. When the plan's limit is
//! reached the answer says so; other billing takes over only where the user listed it as the
//! agent's fallback (see `fallback`).

use std::collections::HashMap;

pub(crate) const PROVIDER: &str = "plan";

/// A plan as dinod configured it (see `Proxy::set_plans`).
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    /// As people know it: "GLM Coding Plan".
    pub name: String,
    /// Where it serves Anthropic Messages, without `/v1`.
    pub anthropic: Option<String>,
    /// Its OpenAI-compatible base, version included.
    pub openai: Option<String>,
    pub key: String,
}

/// The agent's own credentials, dropped on the way out.
pub(crate) fn is_credential(name: &str) -> bool {
    matches!(name, "authorization" | "x-api-key" | "openai-organization" | "openai-project" | "chatgpt-account-id")
}

/// Where `rest` (the agent's path, `v1/…`) goes on `plan`, or why nowhere. Only the model APIs
/// and the model list: the key isn't for anything else.
pub(crate) fn url(plan: &Plan, rest: &str) -> Result<String, String> {
    let anthropic = rest == "v1/messages" || rest == "v1/messages/count_tokens";
    let model = rest.strip_prefix("v1/models/").is_some_and(|m| !m.is_empty() && !m.contains('/'));
    let openai = matches!(rest, "v1/chat/completions" | "v1/responses" | "v1/models") || model;
    if anthropic {
        let base = plan.anthropic.as_deref().ok_or_else(|| format!("{} doesn't support Anthropic Messages", plan.name))?;
        return Ok(format!("{base}/{rest}"));
    }
    if !openai {
        return Err(format!("dino only sends model calls to {}", plan.name));
    }
    match (&plan.openai, &plan.anthropic) {
        (Some(base), _) => Ok(format!("{base}/{}", &rest[3..])),
        // A plan that only serves Anthropic Messages lists its models there.
        (None, Some(base)) if rest.starts_with("v1/models") => Ok(format!("{base}/{rest}")),
        _ => Err(format!("{} doesn't support the OpenAI APIs", plan.name)),
    }
}

/// What goes out instead of the agent's credentials: the plan's key, as each API takes it. Plans
/// differ on which header their Anthropic endpoint reads, so it goes in both. Never a Claude
/// subscription token: that's Claude Code's alone.
pub(crate) fn headers(plan: &Plan, rest: &str) -> Result<Vec<(&'static str, String)>, String> {
    let key = plan.key.trim();
    if key.is_empty() {
        return Err(format!("{} has no key: add it in dino's Settings → Models & Providers", plan.name));
    }
    if key.starts_with("sk-ant-oat") {
        return Err("A Claude subscription token is for Claude Code alone; dino doesn't send it to a coding plan".into());
    }
    let mut h = vec![("authorization", format!("Bearer {key}"))];
    if rest.starts_with("v1/messages") {
        h.push(("x-api-key", key.to_string()));
    }
    Ok(h)
}

/// Why a call to `name` failed, said so the user knows what to do. Its own words come along: the
/// plan says best which limit it hit and when it resets.
pub(crate) fn refused(name: &str, status: u16, message: &str) -> String {
    let said = if message.is_empty() { String::new() } else { format!(": {message}") };
    match limited(status, message) {
        Some(Limit::Usage) => format!("{name}'s usage limit was reached ({status}{said}). dino doesn't switch to other billing unless you set a fallback (Settings → Agents): the plan works again once its window resets"),
        Some(Limit::Rate) => format!("{name} is rate limiting ({status}{said})"),
        Some(Limit::Balance) => format!("{name} is out of balance ({status}{said}). Top it up with {name}; dino doesn't switch to other billing unless you set a fallback (Settings → Agents)"),
        None if matches!(status, 401 | 403) => format!("{name} turned the call down ({status}{said}). Check the plan's key and the model in dino's Settings → Models & Providers"),
        None => format!("{name}: {status}{said}"),
    }
}

#[derive(Debug, PartialEq)]
pub(crate) enum Limit {
    /// A window's allowance (5 hours, a week, a month) is spent.
    Usage,
    /// Too many requests for now.
    Rate,
    /// A pay-as-you-go balance ran out.
    Balance,
}

/// What kind of limit an answer is, if any. Plans differ in the status they use (Kimi's spent
/// window is a 403, Atlas Cloud's a 402), so their words count too.
pub(crate) fn limited(status: u16, message: &str) -> Option<Limit> {
    let m = message.to_lowercase();
    let says = |words: &[&str]| words.iter().any(|w| m.contains(w));
    if says(&["usage limit", "limit reached", "limit exhausted", "limit exceeded", "quota", "allowance", "hour", "weekly", "monthly"]) && !says(&["too many requests", "rate limit"]) {
        return Some(Limit::Usage);
    }
    if status == 402 || says(&["insufficient balance", "out of balance"]) {
        return Some(Limit::Balance);
    }
    (status == 429).then_some(Limit::Rate)
}

/// The plans dinod configured, by id (`zai`, `other`).
pub(crate) type Plans = HashMap<String, Plan>;

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(anthropic: Option<&str>, openai: Option<&str>) -> Plan {
        Plan { name: "GLM Coding Plan".into(), anthropic: anthropic.map(String::from), openai: openai.map(String::from), key: "k-123".into() }
    }

    #[test]
    fn each_api_goes_to_its_base() {
        let p = plan(Some("https://api.z.ai/api/anthropic"), Some("https://api.z.ai/api/coding/paas/v4"));
        assert_eq!(url(&p, "v1/messages").unwrap(), "https://api.z.ai/api/anthropic/v1/messages");
        assert_eq!(url(&p, "v1/messages/count_tokens").unwrap(), "https://api.z.ai/api/anthropic/v1/messages/count_tokens");
        assert_eq!(url(&p, "v1/chat/completions").unwrap(), "https://api.z.ai/api/coding/paas/v4/chat/completions");
        assert_eq!(url(&p, "v1/models").unwrap(), "https://api.z.ai/api/coding/paas/v4/models");
        assert_eq!(url(&p, "v1/models/glm-x").unwrap(), "https://api.z.ai/api/coding/paas/v4/models/glm-x");
        for not in ["v1/files", "v1/embeddings", "v1/models/", "v1/messages/batches", "api/event_logging/batch"] {
            assert!(url(&p, not).is_err(), "{not}");
        }
        let only_anthropic = plan(Some("https://a.example/anthropic"), None);
        assert!(url(&only_anthropic, "v1/chat/completions").unwrap_err().contains("doesn't support the OpenAI"));
        assert_eq!(url(&only_anthropic, "v1/models").unwrap(), "https://a.example/anthropic/v1/models");
        assert!(url(&plan(None, Some("https://o.example/v1")), "v1/messages").unwrap_err().contains("Anthropic"));
    }

    #[test]
    fn the_plans_key_goes_out_and_never_a_claude_subscription() {
        let p = plan(Some("https://a"), Some("https://o/v1"));
        assert_eq!(headers(&p, "v1/messages").unwrap(), vec![("authorization", "Bearer k-123".to_string()), ("x-api-key", "k-123".to_string())]);
        assert_eq!(headers(&p, "v1/chat/completions").unwrap(), vec![("authorization", "Bearer k-123".to_string())]);
        let claude = Plan { key: "sk-ant-oat01-abc".into(), ..p.clone() };
        assert!(headers(&claude, "v1/messages").unwrap_err().contains("Claude Code alone"));
        assert!(headers(&Plan { key: " ".into(), ..p }, "v1/messages").is_err());
        assert!(is_credential("authorization") && is_credential("x-api-key") && !is_credential("anthropic-version"));
    }

    #[test]
    fn a_limit_says_so_and_that_nothing_else_is_billed() {
        // Each as its plan's docs give it.
        let m = refused("GLM Coding Plan", 429, "Usage limit reached for 5 hour. Your limit will reset at 2026-10-03 18:00:00");
        assert!(m.contains("usage limit was reached") && m.contains("5 hour") && m.contains("doesn't switch to other billing"), "{m}");
        assert_eq!(limited(429, "Weekly/Monthly Limit Exhausted. Your limit will reset at 2026-10-06"), Some(Limit::Usage));
        assert_eq!(limited(403, "You've reached your 5-hour usage limit. Upgrade at kimi.com"), Some(Limit::Usage), "Kimi's is a 403");
        assert_eq!(limited(429, "usage limit exceeded, 5-hour usage limit reached for Token Plan (2056)"), Some(Limit::Usage));
        assert_eq!(limited(402, "Insufficient balance, or a Coding Plan allowance that has run out"), Some(Limit::Usage));
        assert_eq!(limited(402, "Insufficient Balance"), Some(Limit::Balance));
        assert_eq!(limited(429, "Rate Limit Reached"), Some(Limit::Rate));
        assert_eq!(limited(429, "too many requests"), Some(Limit::Rate));
        assert_eq!(limited(401, "Invalid Authentication"), None);
        assert!(refused("DeepSeek", 402, "Insufficient Balance").contains("out of balance"));
        assert!(refused("Kimi Code", 401, "Invalid Authentication").contains("Check the plan's key"));
        assert_eq!(refused("X", 500, ""), "X: 500");
    }
}
