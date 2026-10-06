//! `/s/<session>/siwc/…`: the ChatGPT plan, through Sign in with ChatGPT (dinod signs in and keeps
//! the tokens). The plan is spent on the Responses API only, stateless and streamed: every
//! request goes out with `store: false` and `stream: true`, without the fields and hosted tools
//! the plan doesn't take. The agent's own credentials are swapped for dino's. When the plan's
//! cap is reached the answer says so; nothing falls back to other billing.

use std::collections::HashMap;

use serde_json::Value;

pub(crate) const PROVIDER: &str = "siwc";
/// Where the plan is spent: OpenAI's API, or in a debug build what `DINO_SIWC_UPSTREAM` names (a
/// stand-in to try the route against).
pub(crate) fn upstream() -> &'static str {
    static AT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    AT.get_or_init(|| std::env::var("DINO_SIWC_UPSTREAM").ok().filter(|u| cfg!(debug_assertions) && !u.is_empty()).unwrap_or_else(|| "https://api.openai.com".into()))
}
const ACCESS_KEY: &str = "CHATGPT_ACCESS_TOKEN";
const PLAN_KEY: &str = "CHATGPT_PLAN_USAGE";

pub(crate) fn is_credential(name: &str) -> bool {
    matches!(name, "authorization" | "x-api-key" | "openai-organization" | "openai-project" | "chatgpt-account-id")
}

/// dino's token for the request, or why there isn't one.
pub(crate) fn headers(keys: &HashMap<String, String>) -> Result<[(&'static str, String); 1], &'static str> {
    let token = keys.get(ACCESS_KEY).filter(|t| !t.is_empty()).ok_or("Sign in with ChatGPT isn't connected: sign in in dino's Settings → Providers")?;
    if keys.get(PLAN_KEY).is_none_or(|p| p != "granted") {
        return Err("Sign in with ChatGPT didn't allow dino to use your plan: sign in again in Settings → Providers and allow it");
    }
    Ok([("authorization", format!("Bearer {token}"))])
}

/// Only the Responses API and the model list take the plan: these paths exactly, or one model's
/// (a single segment, so nothing like `v1/models/../../v1/files` gets through).
pub(crate) fn allowed(rest: &str) -> bool {
    match rest.strip_prefix("v1/models/") {
        Some(model) => !matches!(model, "" | "." | "..") && !model.contains(['/', '\\', '%', '?', '#']),
        None => matches!(rest, "v1/responses" | "v1/models"),
    }
}

/// Fields the plan doesn't take (it keeps nothing between requests).
const DROPPED: &[&str] = &["previous_response_id", "background", "conversation", "prompt"];

/// A Responses request as the plan takes it: stateless and streamed, only the agent's own tools.
pub(crate) fn shape(body: &[u8]) -> Option<Vec<u8>> {
    let mut v: Value = serde_json::from_slice(body).ok()?;
    let o = v.as_object_mut()?;
    o.insert("store".into(), Value::Bool(false));
    o.insert("stream".into(), Value::Bool(true));
    for f in DROPPED {
        o.remove(*f);
    }
    // Hosted tools (web search, file search, code interpreter, image generation…) run on OpenAI's
    // side and aren't part of the plan; the agent's own function tools are.
    if let Some(tools) = o.get_mut("tools").and_then(Value::as_array_mut) {
        tools.retain(|t| matches!(t["type"].as_str(), Some("function" | "custom")));
        if tools.is_empty() {
            o.remove("tools");
            o.remove("tool_choice");
        }
    }
    serde_json::to_vec(&v).ok()
}

/// The agent asked for a stream (the default is one JSON answer).
pub(crate) fn wants_stream(body: &[u8]) -> bool {
    serde_json::from_slice::<Value>(body).ok().is_some_and(|v| v["stream"].as_bool() == Some(true))
}

/// One JSON answer from the stream the plan sent: the finished response its last event carries.
pub(crate) fn collect(sse: &[u8]) -> Option<Vec<u8>> {
    let text = std::str::from_utf8(sse).ok()?;
    let done = text
        .lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .filter_map(|d| serde_json::from_str::<Value>(d.trim()).ok())
        .filter(|v| matches!(v["type"].as_str(), Some("response.completed" | "response.incomplete" | "response.failed")))
        .last()?;
    serde_json::to_vec(&done["response"]).ok()
}

/// What to say when the plan turns a request down.
pub(crate) fn refused(status: u16, said: &str) -> String {
    match status {
        429 => format!("429 {said} (your ChatGPT plan's limit, or the weekly cap you set for dino in ChatGPT → Settings → Usage)"),
        401 => format!("401 {said} (sign in with ChatGPT again in dino's Settings → Providers)"),
        _ => format!("{status} {said}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dinos_token_only_when_plan_usage_was_allowed() {
        assert!(headers(&HashMap::new()).unwrap_err().contains("isn't connected"));
        let mut keys = HashMap::from([(ACCESS_KEY.to_string(), "at".to_string())]);
        assert!(headers(&keys).unwrap_err().contains("didn't allow"));
        keys.insert(PLAN_KEY.into(), "not granted".into());
        assert!(headers(&keys).is_err());
        keys.insert(PLAN_KEY.into(), "granted".into());
        assert_eq!(headers(&keys).unwrap()[0], ("authorization", "Bearer at".to_string()));
        assert!(is_credential("authorization") && is_credential("chatgpt-account-id") && !is_credential("content-type"));
    }

    #[test]
    fn requests_go_out_stateless_and_streamed_with_the_agents_tools() {
        let body = serde_json::json!({
            "model": "gpt-6-luna", "input": "hi", "store": true, "stream": false, "previous_response_id": "resp_1",
            "tools": [{"type": "function", "name": "shell"}, {"type": "web_search"}, {"type": "custom", "name": "apply_patch"}, {"type": "image_generation"}],
        });
        let out: Value = serde_json::from_slice(&shape(&serde_json::to_vec(&body).unwrap()).unwrap()).unwrap();
        assert_eq!((out["store"].clone(), out["stream"].clone()), (Value::Bool(false), Value::Bool(true)));
        assert!(out.get("previous_response_id").is_none());
        let kinds: Vec<&str> = out["tools"].as_array().unwrap().iter().filter_map(|t| t["type"].as_str()).collect();
        assert_eq!(kinds, ["function", "custom"]);
        assert_eq!(out["model"], "gpt-6-luna");

        let hosted_only = serde_json::json!({"model": "m", "input": "x", "tools": [{"type": "web_search"}], "tool_choice": "auto"});
        let out: Value = serde_json::from_slice(&shape(&serde_json::to_vec(&hosted_only).unwrap()).unwrap()).unwrap();
        assert!(out.get("tools").is_none() && out.get("tool_choice").is_none());
        assert!(shape(b"not json").is_none());
    }

    #[test]
    fn a_stream_put_together_for_an_agent_that_wanted_one_answer() {
        assert!(wants_stream(br#"{"stream":true}"#) && !wants_stream(br#"{"model":"m"}"#));
        let sse = b"event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"r\",\"status\":\"in_progress\"}}\n\n\
event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"PELICAN\"}\n\n\
event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"r\",\"status\":\"completed\",\"output\":[{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"PELICAN\"}]}],\"usage\":{\"input_tokens\":12,\"output_tokens\":3}}}\n\n";
        let v: Value = serde_json::from_slice(&collect(sse).unwrap()).unwrap();
        assert_eq!(v["status"], "completed");
        assert_eq!(v["output"][0]["content"][0]["text"], "PELICAN");
        assert!(collect(b"data: {\"type\":\"response.created\"}\n").is_none());
    }

    #[test]
    fn only_responses_and_models() {
        assert!(allowed("v1/responses") && allowed("v1/models") && allowed("v1/models/gpt-5.5"));
        assert!(!allowed("v1/chat/completions") && !allowed("v1/embeddings") && !allowed("v1/responses/resp_1"));
        for sneaky in ["v1/models/../../v1/files", "v1/models/..", "v1/models/", "v1/models/x/../../files", "v1/models/%2e%2e", "v1/models/..\\files", "v1/responses/", "v1/responses/.."] {
            assert!(!allowed(sneaky), "{sneaky}");
        }
        assert!(refused(429, "rate limited").contains("weekly cap"));
    }
}
