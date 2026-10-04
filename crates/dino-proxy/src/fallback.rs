//! Fallbacks: when the route a session's agent talks to says its quota is spent (a plan's window,
//! a subscription's limit, a balance), dino sends the same request on to the next route the user
//! listed for that agent, with that route's model and credentials, and streams its answer back, so
//! the agent never sees the failure. The session stays there until the first route's limit
//! resets, then goes back at the start of a turn. Short rate limits the agent waits out itself
//! don't count; outages only when the user asked for that.
//!
//! Nothing is translated: a route is only listed for an agent when it serves the API shape the
//! agent speaks. The agent's own credentials never go to another route (a Claude subscription is
//! Claude Code's, for Anthropic alone): each route gets the credentials dino holds for it.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::http::HeaderMap;
use bytes::Bytes;
use serde_json::Value;

use crate::plan;

/// One route to try: where the proxy serves it (`or`, `siwc`, `free`, `plan/<id>`,
/// `local/<runtime>`) and the model to ask it for.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub route: String,
    pub model: String,
    /// As people know it: "GLM Coding Plan".
    pub name: String,
}

/// What a session falls back to, in order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Chain {
    pub steps: Vec<Step>,
    /// Also when a route is down (server errors, overloaded, unreachable), not only at its limit.
    pub on_outage: bool,
}

/// Why a route can't be used for now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A window's allowance (5 hours, a week, a month) or a subscription's limit is spent.
    Quota,
    /// A pay-as-you-go balance or credit ran out.
    Balance,
    /// Server errors several times in a row, or unreachable.
    Outage,
}

impl Kind {
    pub fn word(self) -> &'static str {
        match self {
            Kind::Quota => "limit",
            Kind::Balance => "balance",
            Kind::Outage => "outage",
        }
    }
}

/// An answer that means the route is spent.
#[derive(Clone, Debug, PartialEq)]
pub struct Trigger {
    pub kind: Kind,
    /// When it resets, in Unix seconds, if the answer says so exactly.
    pub resets_at: Option<u64>,
    /// The route's own words.
    pub said: String,
}

/// A route found spent, until when dino leaves it alone.
#[derive(Clone, Debug, PartialEq)]
pub struct Limited {
    pub name: String,
    pub kind: Kind,
    pub said: String,
    pub resets_at: Option<u64>,
    /// When to try it again: its reset, or a while from now when it doesn't say.
    pub retry_at: u64,
}

/// A session answered by a fallback route.
#[derive(Clone, Debug, PartialEq)]
pub struct OnFallback {
    /// The step answering, its name and model.
    pub route: String,
    pub name: String,
    pub model: String,
    /// The route it fell back from: its key (see `route_key`) and name.
    pub from: String,
    pub from_name: String,
    pub kind: Kind,
    pub said: String,
    pub resets_at: Option<u64>,
    /// When the first route is tried again (at the start of a turn).
    pub retry_at: u64,
    /// Unix seconds.
    pub since: u64,
    /// Which step of the chain: a session only moves up the chain at the start of a turn.
    pub(crate) step: usize,
}

/// How long a route that didn't say when it resets is left alone before it's tried again.
pub const RETRY_UNKNOWN: u64 = 15 * 60;
/// How long a route that's down is left alone.
pub const RETRY_OUTAGE: u64 = 5 * 60;
/// Server errors in a row (the agent's own retries among them) that make a route down.
pub const OUTAGE_AFTER: u32 = 3;
/// Further apart than this, failures aren't "in a row".
pub const OUTAGE_WINDOW: u64 = 120;

pub(crate) fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// The API shape of a model call, by its path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Api {
    Anthropic,
    Chat,
    Responses,
}

impl Api {
    pub(crate) fn of(rest: &str) -> Option<Api> {
        if rest.ends_with("messages") {
            Some(Api::Anthropic)
        } else if rest.ends_with("chat/completions") {
            Some(Api::Chat)
        } else if rest.ends_with("responses") {
            Some(Api::Responses)
        } else {
            None
        }
    }

    /// Where it's served under a provider's base.
    pub(crate) fn path(self) -> &'static str {
        match self {
            Api::Anthropic => "v1/messages",
            Api::Chat => "v1/chat/completions",
            Api::Responses => "v1/responses",
        }
    }
}

/// The route a call went to, as fallbacks track it: the proxy's path for it, and for the routes
/// that carry the agent's own credentials (`anthropic`, `openai`, `chatgpt`) which credentials, so
/// a spent subscription isn't taken for another account's.
pub(crate) fn route_key(route: &str, headers: &HeaderMap) -> String {
    if !matches!(route, "anthropic" | "openai" | "chatgpt") {
        return route.to_string();
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for name in ["authorization", "x-api-key", "chatgpt-account-id"] {
        headers.get(name).map(|v| v.as_bytes()).hash(&mut h);
    }
    format!("{route}#{:08x}", h.finish() as u32)
}

/// What a route is called where the session shows it: the agent's own account by what signs in.
pub(crate) fn primary_name(route: &str, headers: &HeaderMap) -> String {
    let auth = headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or_default();
    match route {
        "anthropic" if auth.contains("sk-ant-oat") => "Claude".into(),
        "anthropic" => "Anthropic API".into(),
        "openai" => "OpenAI API".into(),
        "chatgpt" => "ChatGPT".into(),
        "or" => "OpenRouter".into(),
        "siwc" => "ChatGPT plan".into(),
        "free" => "free models".into(),
        other => other.to_string(),
    }
}

/// The agent's credentials and account ids: none of them go to a fallback route.
pub(crate) fn is_credential(name: &str) -> bool {
    matches!(name, "authorization" | "x-api-key" | "openai-organization" | "openai-project" | "chatgpt-account-id" | "cookie")
}

/// A Claude subscription token: Claude Code's, for Anthropic alone.
pub(crate) fn is_claude_subscription(value: &str) -> bool {
    value.contains("sk-ant-oat")
}

/// Whether an answer means the route is spent: its quota, its window, its balance. A short rate
/// limit isn't: the agent waits it out (its retry-after) as it would without dino.
pub fn classify(status: u16, headers: &HeaderMap, body: &[u8]) -> Option<Trigger> {
    let header = |k: &str| headers.get(k).and_then(|v| v.to_str().ok()).map(str::trim).filter(|v| !v.is_empty());
    let v: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let said = crate::codex::error_message(body);
    let trigger = |kind, resets_at| Some(Trigger { kind, resets_at, said: said.clone() });

    // A Claude subscription (Pro, Max): its unified limiter turned the call down. Which window did
    // says when it's back; the overall reset and Retry-After can name the longest one.
    if header("anthropic-ratelimit-unified-status") == Some("rejected") {
        let rejected = headers
            .iter()
            .filter_map(|(n, val)| {
                let w = n.as_str().strip_prefix("anthropic-ratelimit-unified-")?.strip_suffix("-status")?;
                (val.to_str().ok()? == "rejected").then(|| header(&format!("anthropic-ratelimit-unified-{w}-reset")).and_then(|r| r.parse::<u64>().ok()))?
            })
            .max();
        let reset = rejected.or_else(|| header("anthropic-ratelimit-unified-reset").and_then(|r| r.parse().ok())).or_else(|| retry_after(headers));
        return trigger(Kind::Quota, reset);
    }

    // ChatGPT's plan (Codex signed in with ChatGPT): its usage limit, flat, under `error`, or twice.
    for e in [&v, &v["error"], &v["error"]["error"], &v["detail"]] {
        if let Some(kind) = e["type"].as_str().or(e["code"].as_str())
            && matches!(kind, "usage_limit_reached" | "usage_not_included")
        {
            let at = e["resets_at"].as_u64().or_else(|| e["resets_in_seconds"].as_u64().map(|s| now() + s));
            return trigger(Kind::Quota, at);
        }
    }

    let code = v["error"]["code"].as_str().or(v["error"]["type"].as_str()).unwrap_or_default();
    let lower = said.to_lowercase();
    // OpenAI's API: the account's quota or credit.
    if code == "insufficient_quota" || lower.contains("exceeded your current quota") {
        return trigger(Kind::Balance, None);
    }
    // Anthropic's API: no credit left, or the spend limit the organization set for the month.
    if lower.contains("credit balance is too low") {
        return trigger(Kind::Balance, None);
    }
    if lower.contains("reached your specified api usage limits") {
        return trigger(Kind::Quota, regain_access(&said));
    }
    // OpenRouter: no credits for a paid model, or the day's free requests used up.
    if status == 402 {
        return trigger(Kind::Balance, None);
    }
    if status == 429 && lower.contains("per-day") {
        let reset = header("x-ratelimit-reset").and_then(|r| r.parse::<u64>().ok()).map(|ms| if ms > 1 << 40 { ms / 1000 } else { ms });
        return trigger(Kind::Quota, reset);
    }
    // Coding plans differ in status (Kimi's spent window is a 403), so their words count.
    if matches!(status, 403 | 429) {
        return match plan::limited(status, &said) {
            Some(plan::Limit::Usage) => trigger(Kind::Quota, None),
            Some(plan::Limit::Balance) => trigger(Kind::Balance, None),
            Some(plan::Limit::Rate) | None => None,
        };
    }
    None
}

/// A server error or overload: an outage, if it goes on.
pub fn is_outage(status: u16) -> bool {
    status >= 500
}

/// `Retry-After` in seconds or as a date, from now.
fn retry_after(headers: &HeaderMap) -> Option<u64> {
    let v = headers.get("retry-after")?.to_str().ok()?.trim();
    v.parse::<u64>().ok().map(|s| now() + s)
}

/// "You will regain access on 2026-11-01 at 00:00 UTC."
fn regain_access(said: &str) -> Option<u64> {
    let at = said.split(" on ").nth(1)?;
    let mut words = at.split_whitespace();
    let date = words.next()?;
    let time = match (words.next(), words.next()) {
        (Some("at"), Some(t)) => t,
        _ => "00:00",
    };
    utc(date, time)
}

/// `YYYY-MM-DD` and `HH:MM[:SS]`, in UTC, as Unix seconds.
fn utc(date: &str, time: &str) -> Option<u64> {
    let mut d = date.split('-').map(|x| x.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let mut t = time.trim_end_matches(['.', ',']).split(':').map(|x| x.parse::<i64>().ok());
    let (hh, mm, ss) = (t.next()??, t.next()??, t.next().flatten().unwrap_or(0));
    if !(1..=12).contains(&m) || !(1..=31).contains(&day) {
        return None;
    }
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + hh * 3600 + mm * 60 + ss).ok()
}

/// When to try a spent route again.
pub(crate) fn retry_at(t: &Trigger) -> u64 {
    let now = now();
    match (t.kind, t.resets_at) {
        (Kind::Outage, _) => now + RETRY_OUTAGE,
        (_, Some(at)) if at > now => at,
        _ => now + RETRY_UNKNOWN,
    }
}

/// The request starts a turn: the person wrote, rather than a tool answered. A session goes back
/// to its first route (or up its chain) only then, so one turn isn't answered by two models.
/// Notes the agent adds after the conversation (Claude Code 2.1 ends each call with a system
/// message of the tokens left) aren't anyone writing: the message before them says.
pub fn turn_start(api: Api, body: &[u8]) -> bool {
    let Ok(v) = serde_json::from_slice::<Value>(body) else { return false };
    let last_said = |messages: &Value| messages.as_array().and_then(|m| m.iter().rev().find(|m| !matches!(m["role"].as_str(), Some("system" | "developer")))).cloned();
    match api {
        Api::Anthropic => last_said(&v["messages"]).is_some_and(|m| {
            m["role"] == "user" && (m["content"].is_string() || m["content"].as_array().is_some_and(|c| !c.iter().any(|b| b["type"] == "tool_result")))
        }),
        Api::Chat => last_said(&v["messages"]).is_some_and(|m| m["role"] == "user"),
        Api::Responses => match &v["input"] {
            Value::String(_) => true,
            Value::Array(items) => items.last().is_some_and(|i| i["role"] == "user" && i["type"].as_str().is_none_or(|t| t == "message")),
            _ => false,
        },
    }
}

/// The request as a fallback route takes it: its model, and nothing only the first route can
/// read (Claude's signed thinking, OpenAI's encrypted reasoning).
pub(crate) fn for_step(body: &[u8], api: Api, model: &str) -> Option<Bytes> {
    let mut v: Value = serde_json::from_slice(body).ok()?;
    let o = v.as_object_mut()?;
    o.insert("model".into(), Value::String(model.into()));
    match api {
        Api::Anthropic => {
            for m in o.get_mut("messages").and_then(Value::as_array_mut).into_iter().flatten() {
                if let Some(content) = m.get_mut("content").and_then(Value::as_array_mut) {
                    content.retain(|b| b["type"] != "redacted_thinking");
                }
            }
        }
        Api::Responses => {
            if let Some(items) = o.get_mut("input").and_then(Value::as_array_mut) {
                items.retain(|i| !(i["type"] == "reasoning" && i["encrypted_content"].is_string()));
            }
            if let Some(include) = o.get_mut("include").and_then(Value::as_array_mut) {
                include.retain(|x| x != "reasoning.encrypted_content");
            }
        }
        Api::Chat => {}
    }
    serde_json::to_vec(&v).ok().map(Bytes::from)
}

/// The request as the first route takes it again after a fallback answered part of the
/// conversation: what the fallback thought can't be verified there (Anthropic checks the
/// signature of every thinking block; OpenAI looks up reasoning it didn't write). Only turns
/// before this one carry any, and their thinking is optional there. `None`: nothing to change.
pub(crate) fn for_primary(body: &[u8], api: Api) -> Option<Bytes> {
    // Most calls have none: not worth reading a whole conversation to find out.
    let mark: &[u8] = if api == Api::Responses { b"\"reasoning\"" } else { b"thinking\"" };
    memchr::memmem::find(body, mark)?;
    let mut v: Value = serde_json::from_slice(body).ok()?;
    let mut changed = false;
    match api {
        Api::Anthropic => {
            let messages = v.get_mut("messages").and_then(Value::as_array_mut)?;
            let opens_turn = |m: &Value| m["role"] == "user" && (m["content"].is_string() || m["content"].as_array().is_some_and(|c| !c.iter().any(|b| b["type"] == "tool_result")));
            let this_turn = messages.iter().rposition(opens_turn).unwrap_or(0);
            for m in &mut messages[..this_turn] {
                if m["role"] != "assistant" {
                    continue;
                }
                if let Some(content) = m.get_mut("content").and_then(Value::as_array_mut) {
                    let before = content.len();
                    content.retain(|b| !matches!(b["type"].as_str(), Some("thinking" | "redacted_thinking")));
                    changed |= content.len() != before;
                    // An assistant message that was only thinking still says something.
                    if content.is_empty() {
                        content.push(serde_json::json!({"type": "text", "text": "…"}));
                    }
                }
            }
        }
        Api::Responses => {
            let items = v.get_mut("input").and_then(Value::as_array_mut)?;
            let before = items.len();
            items.retain(|i| i["type"] != "reasoning" || i["encrypted_content"].is_string());
            changed = items.len() != before;
        }
        Api::Chat => {}
    }
    changed.then(|| serde_json::to_vec(&v).ok().map(Bytes::from)).flatten()
}

/// Every spent route, by its key (see `route_key`).
pub(crate) type LimitedRoutes = HashMap<String, Limited>;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, v.parse().unwrap());
        }
        h
    }

    /// Every shape that means "spent", as each provider documents or sends it, and the ones that
    /// don't: a short rate limit, an overload, a bad request.
    #[test]
    fn what_counts_as_a_spent_route() {
        let none = HeaderMap::new();
        // A Claude subscription's 5-hour window: the unified limiter's headers, its own words.
        let claude = headers(&[
            ("anthropic-ratelimit-unified-status", "rejected"),
            ("anthropic-ratelimit-unified-reset", "1790000000"),
            ("anthropic-ratelimit-unified-5h-status", "rejected"),
            ("anthropic-ratelimit-unified-5h-reset", "1789990000"),
            ("anthropic-ratelimit-unified-7d-status", "allowed"),
            ("anthropic-ratelimit-unified-7d-reset", "1790500000"),
            ("anthropic-ratelimit-unified-representative-claim", "five_hour"),
            ("retry-after", "86400"),
        ]);
        let body = br#"{"type":"error","error":{"type":"rate_limit_error","message":"This request would exceed your account's rate limit. Please try again later."}}"#;
        let t = classify(429, &claude, body).unwrap();
        assert_eq!((t.kind, t.resets_at), (Kind::Quota, Some(1_789_990_000)), "the window that refused, not the longest");
        let overall = headers(&[("anthropic-ratelimit-unified-status", "rejected"), ("anthropic-ratelimit-unified-reset", "1790000000")]);
        assert_eq!(classify(429, &overall, body).unwrap().resets_at, Some(1_790_000_000));
        // The same words from an API key's ordinary rate limit: the agent retries it.
        assert_eq!(classify(429, &headers(&[("retry-after", "20"), ("anthropic-ratelimit-requests-remaining", "0")]), body), None);
        // Overloaded, a server error: not a limit.
        assert_eq!(classify(529, &none, br#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#), None);
        assert!(is_outage(529) && is_outage(500) && is_outage(503) && !is_outage(429) && !is_outage(400));

        // ChatGPT's plan, as Codex gets it: no Retry-After, the reset in the body.
        let chatgpt = br#"{"error":{"type":"usage_limit_reached","message":"The usage limit has been reached","plan_type":"plus","resets_at":1789676095,"eligible_promo":null,"resets_in_seconds":4182}}"#;
        assert_eq!(classify(429, &none, chatgpt), Some(Trigger { kind: Kind::Quota, resets_at: Some(1_789_676_095), said: "The usage limit has been reached".into() }));
        let flat = br#"{"type":"usage_limit_reached","message":"The usage limit has been reached","resets_in_seconds":60}"#;
        let t = classify(429, &none, flat).unwrap();
        assert!(t.resets_at.unwrap().abs_diff(now() + 60) <= 2);

        // OpenAI's API out of quota, and its ordinary rate limit.
        let quota = br#"{"error":{"message":"You exceeded your current quota, please check your plan and billing details.","type":"insufficient_quota","param":null,"code":"insufficient_quota"}}"#;
        assert_eq!(classify(429, &none, quota).unwrap().kind, Kind::Balance);
        let rate = br#"{"error":{"message":"Rate limit reached for gpt-5.5 in organization org-x on tokens per min (TPM): Limit 30000, Used 29000. Please try again in 20s.","type":"tokens","code":"rate_limit_exceeded"}}"#;
        assert_eq!(classify(429, &none, rate), None);

        // Anthropic's API: no credit, the organization's monthly limit.
        let credit = br#"{"type":"error","error":{"type":"invalid_request_error","message":"Your credit balance is too low to access the Anthropic API. Please go to Plans & Billing to upgrade or purchase credits."}}"#;
        assert_eq!(classify(400, &none, credit).unwrap().kind, Kind::Balance);
        let spend = br#"{"type":"error","error":{"type":"invalid_request_error","message":"You have reached your specified API usage limits. You will regain access on 2026-11-01 at 00:00 UTC."}}"#;
        let t = classify(400, &none, spend).unwrap();
        assert_eq!((t.kind, t.resets_at), (Kind::Quota, Some(1_793_491_200)));
        assert_eq!(classify(400, &none, br#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 250000 tokens > 200000 maximum"}}"#), None);

        // OpenRouter: out of credits; the day's free requests; the minute's (waited out).
        assert_eq!(classify(402, &none, br#"{"error":{"message":"Insufficient credits. Add more using https://openrouter.ai/settings/credits","code":402}}"#).unwrap().kind, Kind::Balance);
        let day = br#"{"error":{"message":"Rate limit exceeded: free-models-per-day. Add 10 credits to unlock 1000 free model requests per day","code":429}}"#;
        let t = classify(429, &headers(&[("x-ratelimit-reset", "1790006400000")]), day).unwrap();
        assert_eq!((t.kind, t.resets_at), (Kind::Quota, Some(1_790_006_400)));
        assert_eq!(classify(429, &none, br#"{"error":{"message":"Rate limit exceeded: free-models-per-min. ","code":429}}"#), None);

        // Coding plans, each as its docs give it.
        let zai = br#"{"error":{"code":"1308","message":"Usage limit reached for 5 hour. Your limit will reset at 2026-10-04 02:00:00"}}"#;
        let t = classify(429, &none, zai).unwrap();
        assert_eq!(t.kind, Kind::Quota);
        assert!(t.said.contains("reset at 2026-10-04 02:00:00"), "its words come along: {}", t.said);
        assert_eq!(classify(429, &none, br#"{"error":{"code":"1310","message":"Weekly/Monthly Limit Exhausted. Your limit will reset at 2026-10-06 10:00:00"}}"#).unwrap().kind, Kind::Quota);
        assert_eq!(classify(403, &none, br#"{"error":{"message":"You've reached your 5-hour usage limit. Upgrade at kimi.com","type":"rate_limit_reached_error"}}"#).unwrap().kind, Kind::Quota);
        assert_eq!(classify(429, &none, br#"{"base_resp":{"status_code":2056},"error":{"message":"usage limit exceeded, 5-hour usage limit reached for Token Plan (2056)"}}"#).unwrap().kind, Kind::Quota);
        assert_eq!(classify(429, &none, br#"{"error":{"code":"1113","message":"Insufficient balance or no resource package. Please recharge."}}"#).unwrap().kind, Kind::Balance);
        assert_eq!(classify(429, &none, br#"{"error":{"code":"1302","message":"Rate limit reached for requests"}}"#), None);
        assert_eq!(classify(403, &none, br#"{"error":{"message":"You don't have access to this model"}}"#), None);
        assert_eq!(classify(401, &none, br#"{"error":{"message":"Invalid Authentication"}}"#), None);
    }

    #[test]
    fn when_a_spent_route_is_tried_again() {
        let t = |kind, resets_at| Trigger { kind, resets_at, said: String::new() };
        let n = now();
        assert_eq!(retry_at(&t(Kind::Quota, Some(n + 3600))), n + 3600, "at its reset");
        assert!(retry_at(&t(Kind::Quota, None)).abs_diff(n + RETRY_UNKNOWN) <= 2, "a while, when it doesn't say");
        assert!(retry_at(&t(Kind::Quota, Some(n - 10))).abs_diff(n + RETRY_UNKNOWN) <= 2, "a reset already past says nothing");
        assert!(retry_at(&t(Kind::Outage, Some(n + 99_999))).abs_diff(n + RETRY_OUTAGE) <= 2);
        assert_eq!(utc("1970-01-02", "00:00"), Some(86_400));
        assert_eq!(utc("2026-10-04", "14:30:15"), Some(1_791_124_215));
        assert_eq!(utc("2026-13-01", "00:00"), None);
    }

    #[test]
    fn a_turn_starts_when_the_person_writes() {
        let a = |m: Value| serde_json::to_vec(&json!({"model": "m", "messages": m})).unwrap();
        assert!(turn_start(Api::Anthropic, &a(json!([{"role": "user", "content": "hi"}]))));
        assert!(turn_start(Api::Anthropic, &a(json!([{"role": "user", "content": [{"type": "text", "text": "hi"}]}]))));
        assert!(!turn_start(Api::Anthropic, &a(json!([{"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t", "content": "ok"}]}]))));
        assert!(turn_start(Api::Chat, &a(json!([{"role": "user", "content": "hi"}]))));
        assert!(!turn_start(Api::Chat, &a(json!([{"role": "tool", "content": "ok"}]))));
        let r = |input: Value| serde_json::to_vec(&json!({"model": "m", "input": input})).unwrap();
        assert!(turn_start(Api::Responses, &r(json!([{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}]))));
        assert!(!turn_start(Api::Responses, &r(json!([{"type": "function_call_output", "call_id": "c", "output": "ok"}]))));
        assert!(turn_start(Api::Responses, &r(json!("hi"))));
        assert!(!turn_start(Api::Anthropic, b"not json"));
        // Claude Code 2.1 ends every call with a note of the tokens left, as a system message.
        let left = json!({"role": "system", "content": [{"type": "text", "text": "<total_tokens>15000000 tokens left</total_tokens>"}]});
        assert!(turn_start(Api::Anthropic, &a(json!([{"role": "user", "content": [{"type": "text", "text": "hi"}]}, left]))));
        assert!(!turn_start(Api::Anthropic, &a(json!([{"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t", "content": "ok"}]}, left]))));
        assert!(!turn_start(Api::Anthropic, &a(json!([left]))));
        assert!(!turn_start(Api::Chat, &a(json!([{"role": "tool", "content": "ok"}, {"role": "system", "content": "note"}]))));
    }

    #[test]
    fn what_each_side_cant_read_stays_behind() {
        let claude = json!({"model": "claude-opus-5-5", "messages": [
            {"role": "user", "content": "one"},
            {"role": "assistant", "content": [{"type": "thinking", "thinking": "t", "signature": "sig"}, {"type": "redacted_thinking", "data": "x"}, {"type": "text", "text": "1"}]},
            {"role": "user", "content": "two"},
            {"role": "assistant", "content": [{"type": "thinking", "thinking": "t2", "signature": ""}, {"type": "tool_use", "id": "t", "name": "Bash", "input": {}}]},
            {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t", "content": "ok"}]},
        ]});
        let body = serde_json::to_vec(&claude).unwrap();
        let out: Value = serde_json::from_slice(&for_step(&body, Api::Anthropic, "glm-x").unwrap()).unwrap();
        assert_eq!(out["model"], "glm-x");
        assert_eq!(out["messages"][1]["content"].as_array().unwrap().len(), 2, "redacted thinking only Anthropic reads");
        // Back on Claude: earlier turns' thinking goes, this turn's (its tool loop) stays.
        let back: Value = serde_json::from_slice(&for_primary(&body, Api::Anthropic).unwrap()).unwrap();
        assert_eq!(back["messages"][1]["content"], json!([{"type": "text", "text": "1"}]));
        assert_eq!(back["messages"][3]["content"][0]["type"], "thinking");
        assert_eq!(back["model"], "claude-opus-5-5");
        let plain = serde_json::to_vec(&json!({"model": "m", "messages": [{"role": "user", "content": "hi"}]})).unwrap();
        assert_eq!(for_primary(&plain, Api::Anthropic), None, "nothing to change, nothing rewritten");

        let codex = json!({"model": "gpt-5.5", "include": ["reasoning.encrypted_content"], "input": [
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]},
            {"type": "reasoning", "id": "rs_1", "summary": [], "encrypted_content": "gAAA"},
            {"type": "reasoning", "id": "rs_2", "summary": [{"type": "summary_text", "text": "s"}]},
            {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "x"}]},
        ]});
        let body = serde_json::to_vec(&codex).unwrap();
        let out: Value = serde_json::from_slice(&for_step(&body, Api::Responses, "qwen3:4b").unwrap()).unwrap();
        assert_eq!((out["input"].as_array().unwrap().len(), out["include"].as_array().unwrap().len()), (3, 0));
        let back: Value = serde_json::from_slice(&for_primary(&body, Api::Responses).unwrap()).unwrap();
        assert_eq!(back["input"].as_array().unwrap().len(), 3);
        assert_eq!(back["input"][1]["id"], "rs_1", "OpenAI's own reasoning stays");
    }

    #[test]
    fn a_route_is_known_by_its_credentials() {
        let a = headers(&[("authorization", "Bearer sk-ant-oat01-a")]);
        let b = headers(&[("authorization", "Bearer sk-ant-oat01-b")]);
        assert_ne!(route_key("anthropic", &a), route_key("anthropic", &b));
        assert_eq!(route_key("anthropic", &a), route_key("anthropic", &a.clone()));
        assert!(!route_key("anthropic", &a).contains("sk-ant"), "the key isn't in it");
        assert_eq!(route_key("plan/zai", &a), "plan/zai");
        assert_eq!(primary_name("anthropic", &a), "Claude");
        assert_eq!(primary_name("anthropic", &headers(&[("x-api-key", "sk-ant-api03-x")])), "Anthropic API");
        assert!(is_credential("authorization") && is_credential("chatgpt-account-id") && is_credential("cookie") && !is_credential("anthropic-version"));
        assert!(is_claude_subscription("Bearer sk-ant-oat01-x") && !is_claude_subscription("Bearer sk-or-v1"));
    }
}
