//! The free tier: requests routed to free OpenAI-compatible models (see `catalog`), in two shapes.
//! `/s/<session>/free/v1/messages` takes Anthropic Messages and translates them there and back,
//! so Claude Code and anything speaking Anthropic's API run on it unchanged;
//! `/s/<session>/free/v1/chat/completions` takes OpenAI chat requests as they are.

use std::time::{Duration, Instant};

use anyllm_translate::anthropic::MessageCreateRequest;
use anyllm_translate::mapping::message_map::anthropic_to_openai_request;
use anyllm_translate::openai::{ChatCompletionChunk, ChatCompletionResponse};
use anyllm_translate::{new_stream_translator, translate_response};
use axum::body::Body;
use axum::http::{Response, StatusCode};
use bytes::Bytes;
use dino_router::{JEV_URL, Model, Tier, classifier_request, classifier_state, heuristic_tier, jev_request, parse_jev, parse_label};
use futures_util::StreamExt;
use serde_json::{Value, json};

use crate::catalog::NIM_BASE;
use crate::{AppState, Call, CallStatus, InFlight, Usage, log, now_ms};

const OPENAI_PARAMS: &[&str] = &[
    "model",
    "messages",
    "tools",
    "tool_choice",
    "parallel_tool_calls",
    "max_tokens",
    "max_completion_tokens",
    "temperature",
    "top_p",
    "stop",
    "stream",
    "stream_options",
    "response_format",
    "seed",
    "n",
    "frequency_penalty",
    "presence_penalty",
    "reasoning_effort",
];

pub(crate) async fn handle(st: AppState, session: String, rest: &str, body: Bytes) -> Response<Body> {
    // Off: nothing goes to a free model or to the classifier, whatever the session sends.
    if !st.free_models.load(std::sync::atomic::Ordering::Relaxed) {
        const OFF: &str = "dino: free models are off; turn them on in dino's Settings → Experimental";
        return if rest.ends_with("chat/completions") { openai_error(StatusCode::FORBIDDEN, OFF) } else { anthropic_error(StatusCode::FORBIDDEN, "permission_error", OFF) };
    }
    if rest.ends_with("count_tokens") {
        // Rough estimate; agents only use it for context-window bookkeeping.
        return json_response(StatusCode::OK, json!({ "input_tokens": body.len() / 4 }));
    }
    if rest.ends_with("chat/completions") {
        return chat(st, session, body).await;
    }
    if rest.ends_with("models") {
        // One model: the free tier picks the real one for each request.
        return json_response(StatusCode::OK, json!({"object": "list", "data": [{"id": "auto", "object": "model", "owned_by": "dino"}]}));
    }
    if !rest.ends_with("messages") {
        return anthropic_error(StatusCode::NOT_FOUND, "not_found_error", &format!("dino free tier has no /{rest}"));
    }
    let Some(key) = st.keys.read().unwrap().get("NVIDIA_API_KEY").cloned() else {
        return anthropic_error(StatusCode::UNAUTHORIZED, "authentication_error", "dino: no NVIDIA_API_KEY for the free tier");
    };
    let Ok(raw) = serde_json::from_slice::<Value>(&body) else {
        return anthropic_error(StatusCode::BAD_REQUEST, "invalid_request_error", "invalid JSON");
    };
    let Ok(req) = serde_json::from_value::<MessageCreateRequest>(raw.clone()) else {
        return anthropic_error(StatusCode::BAD_REQUEST, "invalid_request_error", "unsupported request shape");
    };
    let stream = raw["stream"].as_bool().unwrap_or(false);
    let original_model = req.model.clone();

    let tier = choose_tier(&st, &session, &raw, &key).await;
    let in_flight = start(&st, &session, tier);
    // The whole request, prompt and all: a debugging aid only a debug build has.
    if cfg!(debug_assertions) && std::env::var_os("DINO_PROXY_LOG").is_some() {
        let _ = std::fs::write(std::env::temp_dir().join("dino-last-anthropic.json"), raw.to_string());
    }
    let oai = serde_json::to_value(anthropic_to_openai_request(&req)).unwrap_or_default();
    let Some((resp, model)) = send(&st, &session, tier, oai, stream, &key).await else {
        st.stats.update(&session, |s| s.errors += 1);
        record(&st, &session, None, Usage::default(), CallStatus::Error);
        // 529 makes Claude Code back off and retry rather than give up.
        return anthropic_error(StatusCode::from_u16(529).unwrap(), "overloaded_error", "dino: every free model for this request is unavailable right now");
    };
    if stream {
        return stream_back(st, session, resp, original_model, model, in_flight);
    }
    let parsed = resp.json::<ChatCompletionResponse>().await;
    drop(in_flight);
    match parsed {
        Ok(r) => {
            let out = translate_response(&r, &original_model);
            record_usage(&st, &session, &model.id, &out.usage);
            json_response(StatusCode::OK, serde_json::to_value(out).unwrap_or_default())
        }
        Err(e) => anthropic_error(StatusCode::BAD_GATEWAY, "api_error", &format!("dino: bad upstream response: {e}")),
    }
}

/// An OpenAI chat request, answered as the model that took it answers it.
async fn chat(st: AppState, session: String, body: Bytes) -> Response<Body> {
    let Some(key) = st.keys.read().unwrap().get("NVIDIA_API_KEY").cloned() else {
        return openai_error(StatusCode::UNAUTHORIZED, "dino: no NVIDIA_API_KEY for the free tier");
    };
    let Ok(raw) = serde_json::from_slice::<Value>(&body) else {
        return openai_error(StatusCode::BAD_REQUEST, "invalid JSON");
    };
    let stream = raw["stream"].as_bool().unwrap_or(false);
    let tier = choose_tier(&st, &session, &as_anthropic(&raw), &key).await;
    let in_flight = start(&st, &session, tier);
    let mut oai = raw;
    if stream {
        // The usage comes in a last chunk only when asked for.
        oai["stream_options"] = json!({"include_usage": true});
    }
    let Some((resp, model)) = send(&st, &session, tier, oai, stream, &key).await else {
        st.stats.update(&session, |s| s.errors += 1);
        record(&st, &session, None, Usage::default(), CallStatus::Error);
        return openai_error(StatusCode::SERVICE_UNAVAILABLE, "dino: every free model for this request is unavailable right now");
    };
    if !stream {
        let v: Value = resp.json().await.unwrap_or_default();
        drop(in_flight);
        record_openai_usage(&st, &session, &model.id, &v["usage"]);
        return json_response(StatusCode::OK, v);
    }
    // Passed through as it comes; the usage is read from the chunk that carries it.
    let mut line = Vec::<u8>::new();
    let mut guard = Some(in_flight);
    let events = resp.bytes_stream().map(Some).chain(futures_util::stream::once(async { None })).map(move |item| match item {
        Some(Ok(bytes)) => {
            for &b in bytes.iter() {
                if b != b'\n' {
                    line.push(b);
                    continue;
                }
                let l = std::mem::take(&mut line);
                if let Some(v) = l.strip_prefix(b"data:").and_then(|d| serde_json::from_slice::<Value>(d.trim_ascii()).ok()) {
                    if v["usage"].is_object() {
                        record_openai_usage(&st, &session, &model.id, &v["usage"]);
                    }
                    if v.get("error").is_some() {
                        // Failed partway: the agent's retry goes to another model.
                        st.router.record_failure(&model);
                    }
                }
            }
            Ok::<Bytes, std::io::Error>(bytes)
        }
        Some(Err(e)) => {
            log(format_args!("{session} free stream error: {e}"));
            st.router.record_failure(&model);
            guard.take();
            Ok(Bytes::from(format!("data: {}\n\n", json!({"error": {"message": format!("free tier: {e}")}}))))
        }
        None => {
            guard.take();
            Ok(Bytes::new())
        }
    });
    Response::builder().status(200).header("content-type", "text/event-stream").header("cache-control", "no-cache").body(Body::from_stream(events)).unwrap()
}

/// Count a model call for `session`, in flight until the guard drops.
fn start(st: &AppState, session: &str, tier: Tier) -> InFlight {
    // Background chores run on the fast tier; the sidebar should show what's answering the user.
    let headline = tier != Tier::Fast;
    st.stats.update(session, |s| {
        s.requests += 1;
        s.in_flight += 1;
        s.last_request = Some(Instant::now());
        if headline || s.tier.is_none() {
            s.tier = Some(tier.name().to_string());
        }
    });
    InFlight { stats: st.stats.clone(), session: session.to_string() }
}

/// Send OpenAI request `oai` to the models for `tier` until one starts answering; failures before
/// the first byte are invisible to the agent. A model that refuses the output budget says how much
/// it takes, and is asked again with that.
async fn send(st: &AppState, session: &str, tier: Tier, mut oai: Value, stream: bool, key: &str) -> Option<(reqwest::Response, Model)> {
    shape(&mut oai);
    let headline = tier != Tier::Fast;
    let candidates = st.router.candidates(tier);
    if candidates.is_empty() {
        log(format_args!("{session} free {tier:?}: no models known yet"));
    }
    for model in candidates {
        let mut body = oai.clone();
        body["model"] = json!(model.id);
        set_identity(&mut body, &model);
        let mut limit = st.router.max_output(&model);
        for _ in 0..3 {
            clamp_output(&mut body, limit);
            let started = Instant::now();
            let sent = st.client().post(format!("{NIM_BASE}/chat/completions")).bearer_auth(key).timeout(Duration::from_secs(if stream { 600 } else { 120 })).json(&body).send();
            match tokio::time::timeout(Duration::from_secs(25), sent).await {
                Ok(Ok(r)) if r.status().is_success() => {
                    st.router.record_ok(&model, started.elapsed());
                    log(format_args!("{session} free {tier:?} {} -> 200 in {:?}", model.id, started.elapsed()));
                    st.stats.update(session, |s| {
                        if headline || s.last_model.is_none() {
                            s.last_model = Some(model.short().to_string());
                        }
                    });
                    return Some((r, model));
                }
                Ok(Ok(r)) => {
                    let status = r.status();
                    let text = r.text().await.unwrap_or_default();
                    log(format_args!("{session} free {tier:?} {} -> {status} {}", model.id, clip(&text, 600)));
                    if status.as_u16() == 400
                        && let Some(n) = output_limit(&text, asked_output(&body))
                    {
                        st.router.learn_max_output(&model, n);
                        limit = Some(n);
                        continue;
                    }
                    // Only model-side trouble cools a model down; a rejected request may still suit the next one.
                    if matches!(status.as_u16(), 404 | 408 | 429) || status.is_server_error() {
                        st.router.record_failure(&model);
                    }
                }
                Ok(Err(e)) => {
                    log(format_args!("{session} free {tier:?} {} -> error {e}", model.id));
                    st.router.record_failure(&model);
                }
                Err(_) => {
                    log(format_args!("{session} free {tier:?} {} -> no response in 25s", model.id));
                    st.router.record_failure(&model);
                }
            }
            break;
        }
    }
    None
}

/// Request `oai` as the models take it. Agents pass fields OpenAI-compatible servers reject, and
/// an empty tools list (Claude Code's background requests carry one) that some refuse; the choice
/// of tool goes with the tools.
fn shape(oai: &mut Value) {
    let Some(obj) = oai.as_object_mut() else { return };
    obj.retain(|k, _| OPENAI_PARAMS.contains(&k.as_str()));
    if obj.get("tools").and_then(Value::as_array).is_some_and(Vec::is_empty) {
        obj.remove("tools");
    }
    if !obj.contains_key("tools") {
        obj.remove("tool_choice");
        obj.remove("parallel_tool_calls");
    }
}

/// At most the first `max` bytes of `s`, cut where a character starts (a byte cut could panic).
fn clip(s: &str, max: usize) -> &str {
    let mut end = s.len().min(max);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// The output budget `body` asks for.
fn asked_output(body: &Value) -> Option<u64> {
    body["max_tokens"].as_u64().or(body["max_completion_tokens"].as_u64())
}

fn clamp_output(body: &mut Value, limit: Option<u64>) {
    let Some(limit) = limit else { return };
    for field in ["max_tokens", "max_completion_tokens"] {
        if let Some(n) = body[field].as_u64() {
            body[field] = json!(n.min(limit));
        }
    }
}

/// From a refusal of output budget `asked`: the most the model takes, when it says (the largest
/// number under `asked` in its message), else half of `asked`.
fn output_limit(error: &str, asked: Option<u64>) -> Option<u64> {
    let asked = asked?;
    let lower = error.to_lowercase();
    if !["max_tokens", "max_completion_tokens", "max_new_tokens", "output tokens", "completion tokens"].iter().any(|w| lower.contains(w)) {
        return None;
    }
    let said = lower.split(|c: char| !c.is_ascii_digit()).filter_map(|n| n.parse::<u64>().ok()).filter(|&n| n >= 256 && n < asked).max();
    let n = said.unwrap_or(asked / 2);
    (n >= 256).then_some(n)
}

/// An OpenAI chat request in the shape the tier choice reads (Anthropic's): its tools, and each
/// message's role and text, tool results as such.
fn as_anthropic(oai: &Value) -> Value {
    let messages: Vec<Value> = oai["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|m| match m["role"].as_str().unwrap_or_default() {
            "tool" => json!({"role": "user", "content": [{"type": "tool_result", "content": ""}]}),
            "system" | "developer" => json!({"role": "system", "content": m["content"]}),
            role => {
                let text = match &m["content"] {
                    Value::String(s) => s.clone(),
                    Value::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("\n"),
                    _ => String::new(),
                };
                json!({"role": role, "content": [{"type": "text", "text": text}]})
            }
        })
        .collect();
    json!({"model": oai["model"], "tools": oai["tools"], "messages": messages})
}

fn record_openai_usage(st: &AppState, session: &str, model: &str, u: &Value) {
    let usage = Usage {
        input: u["prompt_tokens"].as_u64().unwrap_or(0),
        output: u["completion_tokens"].as_u64().unwrap_or(0),
        cache_read: u["prompt_tokens_details"]["cached_tokens"].as_u64().unwrap_or(0),
        cache_write: 0,
    };
    st.stats.update(session, |s| s.metered(Some(&crate::RouteTag { path: "free".into(), name: "free models".into() }), &usage));
    record(st, session, Some(model), usage, CallStatus::Ok);
}

fn openai_error(status: StatusCode, msg: &str) -> Response<Body> {
    json_response(status, json!({"error": {"message": msg, "type": "dino_error"}}))
}

/// Heuristics first; otherwise classify once per user turn and keep that tier for the turn.
/// Jev (TypeSafe) when a key is available, else a small free LLM, else a safe default.
async fn choose_tier(st: &AppState, session: &str, raw: &Value, key: &str) -> Tier {
    if let Some(t) = heuristic_tier(raw) {
        return t;
    }
    let Some(state) = classifier_state(raw) else {
        return st.router.turn_tier(session, None).unwrap_or(Tier::Code);
    };
    // Clients sometimes send the same turn twice; classify once.
    if let Some(tier) = st.router.turn_tier(session, Some(&state)) {
        return tier;
    }
    let started = Instant::now();
    let tier = match jev(st, &state).await {
        Some((tier, confidence)) => {
            log(format_args!("{session} classify jev -> {tier:?} (confidence {confidence:.2}) in {:?}", started.elapsed()));
            st.stats.update(session, |s| s.classifier = Some("jev".into()));
            tier
        }
        None => {
            let tier = llm_classify(st, &state, key).await.unwrap_or(Tier::Code);
            log(format_args!("{session} classify llm -> {tier:?} in {:?}", started.elapsed()));
            st.stats.update(session, |s| s.classifier = Some("llm".into()));
            tier
        }
    };
    st.router.set_turn_tier(session, &state, tier);
    tier
}

async fn jev(st: &AppState, state: &str) -> Option<(Tier, f64)> {
    let key = st.keys.read().unwrap().get("TYPESAFE_API_KEY").cloned()?;
    let call = st.client().post(JEV_URL).bearer_auth(key).json(&jev_request(state)).send();
    match tokio::time::timeout(Duration::from_millis(2500), call).await {
        Ok(Ok(r)) if r.status().is_success() => parse_jev(&r.json::<Value>().await.ok()?),
        Ok(Ok(r)) => {
            log(format_args!("jev -> {}", r.status()));
            None
        }
        Ok(Err(e)) => {
            log(format_args!("jev -> error {e}"));
            None
        }
        Err(_) => {
            log(format_args!("jev -> timed out"));
            None
        }
    }
}

async fn llm_classify(st: &AppState, text: &str, key: &str) -> Option<Tier> {
    let model = st.router.classifier()?;
    let call = st.client().post(format!("{NIM_BASE}/chat/completions")).bearer_auth(key).json(&classifier_request(&model, text)).send();
    match tokio::time::timeout(Duration::from_secs(6), call).await {
        Ok(Ok(r)) if r.status().is_success() => r.json::<Value>().await.ok().and_then(|v| {
            let msg = &v["choices"][0]["message"];
            msg["content"].as_str().filter(|s| !s.trim().is_empty()).or(msg["reasoning_content"].as_str()).and_then(parse_label)
        }),
        _ => None,
    }
}

/// `answering` is the model the answer comes from: one that fails partway waits, so the agent's
/// retry goes to another.
fn stream_back(st: AppState, session: String, resp: reqwest::Response, model: String, answering: Model, in_flight: InFlight) -> Response<Body> {
    let mut translator = new_stream_translator(model);
    let mut line = Vec::<u8>::new();
    let mut guard = Some(in_flight);
    let mut done = false;

    let events = resp.bytes_stream().map(Some).chain(futures_util::stream::once(async { None })).map(move |item| {
        let mut out = String::new();
        let mut emit = |evs: Vec<anyllm_translate::anthropic::StreamEvent>| {
            for ev in evs {
                let v = serde_json::to_value(&ev).unwrap_or_default();
                let name = v["type"].as_str().unwrap_or("message").to_string();
                out.push_str(&format!("event: {name}\ndata: {v}\n\n"));
            }
        };
        match item {
            Some(Ok(_)) if done => {}
            Some(Ok(bytes)) => {
                for &b in bytes.iter() {
                    if b != b'\n' {
                        line.push(b);
                        continue;
                    }
                    let l = std::mem::take(&mut line);
                    let Some(data) = l.strip_prefix(b"data:") else { continue };
                    let data = data.trim_ascii();
                    if data == b"[DONE]" {
                        continue;
                    }
                    // An error object reads as a chunk too (all its fields are optional): look for it first.
                    let error = serde_json::from_slice::<Value>(data).ok().filter(|v| v.get("error").is_some());
                    if let Some(err) = error {
                        // Upstream failed mid-answer and said so in the stream.
                        let msg = err["error"]["message"].as_str().unwrap_or("upstream error").to_string();
                        log(format_args!("{session} free stream error: {msg}"));
                        st.router.record_failure(&answering);
                        out.push_str(&stream_error(&msg));
                        done = true;
                        guard.take();
                        break;
                    }
                    if let Ok(chunk) = serde_json::from_slice::<ChatCompletionChunk>(data) {
                        emit(translator.process_chunk(&chunk));
                    }
                }
            }
            // Ending with a plain stop would look like a complete answer (or a malformed one);
            // an Anthropic error event lets Claude Code say so and retry.
            Some(Err(e)) if !done => {
                log(format_args!("{session} free stream error: {e}"));
                st.router.record_failure(&answering);
                out.push_str(&stream_error(&e.to_string()));
                done = true;
                guard.take();
            }
            Some(Err(_)) => {}
            None if !done => {
                done = true;
                emit(translator.finish());
                if let Some(u) = translator.usage() {
                    record_usage(&st, &session, &answering.id, u);
                }
                guard.take();
            }
            None => {}
        }
        Ok::<Bytes, std::io::Error>(Bytes::from(out))
    });

    Response::builder().status(200).header("content-type", "text/event-stream").header("cache-control", "no-cache").body(Body::from_stream(events)).unwrap()
}

fn stream_error(message: &str) -> String {
    let v = serde_json::json!({"type": "error", "error": {"type": "overloaded_error", "message": format!("free tier: {message}")}});
    format!("event: error\ndata: {v}\n\n")
}

/// Claude Code's system prompt tells the model it is Claude; say which model actually answers,
/// so "what model are you?" gets an honest reply.
fn set_identity(oai: &mut Value, model: &dino_router::Model) {
    const MARK: &str = "[dino] ";
    let note = format!("{MARK}Identity: you are {} ({}), served by NVIDIA NIM through dino's free tier. You are not Claude; if asked what model you are, say so plainly.", model.short(), model.id);
    let Some(msgs) = oai["messages"].as_array_mut() else { return };
    // Replace the note from a previous attempt (fallback to another model) instead of stacking them.
    msgs.retain(|m| !(m["role"] == "system" && m["content"].as_str().is_some_and(|c| c.starts_with(MARK))));
    let at = msgs.iter().take_while(|m| m["role"] == "system").count();
    msgs.insert(at, json!({ "role": "system", "content": note }));
}

fn record_usage(st: &AppState, session: &str, model: &str, u: &anyllm_translate::anthropic::Usage) {
    let usage = Usage { input: u.input_tokens as u64, output: u.output_tokens as u64, cache_read: u.cache_read_input_tokens.unwrap_or(0) as u64, cache_write: 0 };
    st.stats.update(session, |s| s.metered(Some(&crate::RouteTag { path: "free".into(), name: "free models".into() }), &usage));
    record(st, session, Some(model), usage, CallStatus::Ok);
}

/// A free-tier model call, for usage statistics. Its time isn't measured: the free tier tries
/// models in turn, which is no model's speed.
fn record(st: &AppState, session: &str, model: Option<&str>, usage: Usage, status: CallStatus) {
    st.stats.record_call(Call { at_ms: now_ms(), session: session.into(), route: "free".into(), model: model.map(String::from), usage, status, ..Default::default() });
}

fn json_response(status: StatusCode, v: Value) -> Response<Body> {
    Response::builder().status(status).header("content-type", "application/json").body(Body::from(v.to_string())).unwrap()
}

fn anthropic_error(status: StatusCode, kind: &str, msg: &str) -> Response<Body> {
    json_response(status, json!({"type": "error", "error": {"type": kind, "message": msg}}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clips_between_characters() {
        let s = format!("{}é", "a".repeat(599));
        assert_eq!(clip(&s, 600), "a".repeat(599));
        assert_eq!(clip(&s, 601), s);
        assert_eq!(clip("日本", 1), "");
        assert_eq!(clip("short", 600), "short");
    }

    #[test]
    fn an_empty_tools_list_isnt_sent() {
        let sent = |mut v: Value| {
            shape(&mut v);
            v
        };
        let chore = json!({"model": "auto-fast", "messages": [], "tools": [], "tool_choice": "auto", "parallel_tool_calls": false, "metadata": {"user_id": "u"}});
        assert_eq!(sent(chore), json!({"model": "auto-fast", "messages": []}), "no tools, no choice of one, nothing models reject");
        let turn = json!({"model": "auto", "messages": [], "tools": [{"type": "function", "function": {"name": "Bash"}}], "tool_choice": "auto"});
        assert_eq!(sent(turn.clone()), turn, "tools kept as they are");
        let no_tools = json!({"model": "auto", "messages": [], "tool_choice": "none"});
        assert_eq!(sent(no_tools), json!({"model": "auto", "messages": []}));
    }

    #[test]
    fn a_refused_budget_says_what_it_takes() {
        let e = r#"{"error":{"message":"max_tokens must be less than or equal to 16384, got 32000"}}"#;
        assert_eq!(output_limit(e, Some(32000)), Some(16384));
        assert_eq!(output_limit("max_completion_tokens is too large", Some(32000)), Some(16000), "no number: half");
        assert_eq!(output_limit("model not found", Some(32000)), None, "not about the budget");
        assert_eq!(output_limit(e, None), None);
    }

    #[test]
    fn an_openai_request_reads_as_a_turn() {
        let oai = json!({"model": "auto", "tools": [{"type": "function"}], "messages": [
            {"role": "system", "content": "be brief"},
            {"role": "user", "content": [{"type": "text", "text": "add a test"}]},
            {"role": "assistant", "content": null, "tool_calls": [{"id": "1"}]},
            {"role": "tool", "tool_call_id": "1", "content": "ok"}
        ]});
        assert_eq!(dino_router::new_turn_text(&as_anthropic(&oai)), None, "a tool result coming back is mid-turn");
        let mut first = oai.clone();
        first["messages"].as_array_mut().unwrap().truncate(2);
        assert_eq!(dino_router::new_turn_text(&as_anthropic(&first)).as_deref(), Some("add a test"));
        assert_eq!(heuristic_tier(&as_anthropic(&json!({"messages": []}))), Some(Tier::Fast), "no tools: a chore");
    }
}
