//! `/s/<session>/free/v1/messages`: Anthropic Messages in, routed to free OpenAI-compatible
//! models, translated back. Lets Claude Code run on the free pool unchanged.

use std::time::{Duration, Instant};

use anyllm_translate::anthropic::MessageCreateRequest;
use anyllm_translate::mapping::message_map::anthropic_to_openai_request;
use anyllm_translate::openai::{ChatCompletionChunk, ChatCompletionResponse};
use anyllm_translate::{new_stream_translator, translate_response};
use axum::body::Body;
use axum::http::{Response, StatusCode};
use bytes::Bytes;
use dino_router::{JEV_URL, Tier, classifier_request, classifier_state, heuristic_tier, jev_request, parse_jev, parse_label};
use futures_util::StreamExt;
use serde_json::{Value, json};

use crate::{AppState, InFlight, Usage, log};

const NIM_BASE: &str = "https://integrate.api.nvidia.com/v1";
const OPENAI_PARAMS: &[&str] = &[
    "model", "messages", "tools", "tool_choice", "parallel_tool_calls", "max_tokens", "max_completion_tokens",
    "temperature", "top_p", "stop", "stream", "stream_options", "response_format", "seed", "n",
    "frequency_penalty", "presence_penalty", "reasoning_effort",
];

/// Free-tier models reject very large output budgets.
const MAX_OUTPUT_TOKENS: u64 = 16_384;

pub(crate) async fn handle(st: AppState, session: String, rest: &str, body: Bytes) -> Response<Body> {
    if rest.ends_with("count_tokens") {
        // Rough estimate; agents only use it for context-window bookkeeping.
        return json_response(StatusCode::OK, json!({ "input_tokens": body.len() / 4 }));
    }
    if !rest.ends_with("messages") {
        return anthropic_error(StatusCode::NOT_FOUND, "not_found_error", &format!("dino free tier has no /{rest}"));
    }
    let Some(key) = st.keys.get("NVIDIA_API_KEY").cloned() else {
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
    // Background chores run on the fast tier; the sidebar should show what's answering the user.
    let headline = tier != Tier::Fast;
    st.stats.update(&session, |s| {
        s.requests += 1;
        s.in_flight += 1;
        s.last_request = Some(Instant::now());
        if headline || s.tier.is_none() {
            s.tier = Some(tier.name().to_string());
        }
    });
    let in_flight = InFlight { stats: st.stats.clone(), session: session.clone() };

    if std::env::var_os("DINO_PROXY_LOG").is_some() {
        let _ = std::fs::write(std::env::temp_dir().join("dino-last-anthropic.json"), raw.to_string());
    }
    let mut oai = serde_json::to_value(anthropic_to_openai_request(&req)).unwrap_or_default();
    // The translator passes unknown Anthropic fields through; OpenAI-compatible servers reject them.
    if let Some(obj) = oai.as_object_mut() {
        obj.retain(|k, _| OPENAI_PARAMS.contains(&k.as_str()));
    }
    for field in ["max_tokens", "max_completion_tokens"] {
        if let Some(n) = oai[field].as_u64() {
            oai[field] = json!(n.min(MAX_OUTPUT_TOKENS));
        }
    }

    // Try candidates until one starts answering; failures before the first byte are invisible to the agent.
    for model in st.router.candidates(tier) {
        oai["model"] = json!(model.id);
        set_identity(&mut oai, &model);
        let started = Instant::now();
        let sent = st
            .client
            .post(format!("{NIM_BASE}/chat/completions"))
            .bearer_auth(&key)
            .timeout(Duration::from_secs(if stream { 600 } else { 120 }))
            .json(&oai)
            .send();
        let resp = match tokio::time::timeout(Duration::from_secs(25), sent).await {
            Ok(Ok(r)) if r.status().is_success() => r,
            Ok(Ok(r)) => {
                let status = r.status();
                let body = r.text().await.unwrap_or_default();
                log(format_args!("{session} free {tier:?} {} -> {status} {}", model.id, &body[..body.len().min(600)]));
                // Only model-side trouble cools a model down; a rejected request may still suit the next one.
                if matches!(status.as_u16(), 404 | 408 | 429) || status.is_server_error() {
                    st.router.record_failure(&model);
                }
                continue;
            }
            Ok(Err(e)) => {
                log(format_args!("{session} free {tier:?} {} -> error {e}", model.id));
                st.router.record_failure(&model);
                continue;
            }
            Err(_) => {
                log(format_args!("{session} free {tier:?} {} -> no response in 25s", model.id));
                st.router.record_failure(&model);
                continue;
            }
        };
        st.router.record_ok(&model, started.elapsed());
        log(format_args!("{session} free {tier:?} {} -> 200 in {:?}", model.id, started.elapsed()));
        st.stats.update(&session, |s| {
            if headline || s.last_model.is_none() {
                s.last_model = Some(model.short().to_string());
            }
        });

        return if stream {
            stream_back(st, session, resp, original_model, in_flight)
        } else {
            let parsed = resp.json::<ChatCompletionResponse>().await;
            drop(in_flight);
            match parsed {
                Ok(r) => {
                    let out = translate_response(&r, &original_model);
                    record_usage(&st, &session, &out.usage);
                    json_response(StatusCode::OK, serde_json::to_value(out).unwrap_or_default())
                }
                Err(e) => anthropic_error(StatusCode::BAD_GATEWAY, "api_error", &format!("dino: bad upstream response: {e}")),
            }
        };
    }
    st.stats.update(&session, |s| s.errors += 1);
    // 529 makes Claude Code back off and retry rather than give up.
    anthropic_error(StatusCode::from_u16(529).unwrap(), "overloaded_error", "dino: every free model for this request is unavailable right now")
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
    let key = st.keys.get("TYPESAFE_API_KEY")?;
    let call = st.client.post(JEV_URL).bearer_auth(key).json(&jev_request(state)).send();
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
    let call = st.client.post(format!("{NIM_BASE}/chat/completions")).bearer_auth(key).json(&classifier_request(text)).send();
    match tokio::time::timeout(Duration::from_secs(6), call).await {
        Ok(Ok(r)) if r.status().is_success() => r.json::<Value>().await.ok().and_then(|v| {
            let msg = &v["choices"][0]["message"];
            msg["content"].as_str().filter(|s| !s.trim().is_empty()).or(msg["reasoning_content"].as_str()).and_then(parse_label)
        }),
        _ => None,
    }
}

fn stream_back(st: AppState, session: String, resp: reqwest::Response, model: String, in_flight: InFlight) -> Response<Body> {
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
                    if let Ok(chunk) = serde_json::from_slice::<ChatCompletionChunk>(data) {
                        emit(translator.process_chunk(&chunk));
                    } else if let Some(err) = serde_json::from_slice::<Value>(data).ok().filter(|v| v.get("error").is_some()) {
                        // Upstream failed mid-answer and said so in the stream.
                        let msg = err["error"]["message"].as_str().unwrap_or("upstream error").to_string();
                        log(format_args!("{session} free stream error: {msg}"));
                        out.push_str(&stream_error(&msg));
                        done = true;
                        guard.take();
                        break;
                    }
                }
            }
            // Ending with a plain stop would look like a complete answer (or a malformed one);
            // an Anthropic error event lets Claude Code say so and retry.
            Some(Err(e)) if !done => {
                log(format_args!("{session} free stream error: {e}"));
                out.push_str(&stream_error(&e.to_string()));
                done = true;
                guard.take();
            }
            Some(Err(_)) => {}
            None if !done => {
                done = true;
                emit(translator.finish());
                if let Some(u) = translator.usage() {
                    record_usage(&st, &session, u);
                }
                guard.take();
            }
            None => {}
        }
        Ok::<Bytes, std::io::Error>(Bytes::from(out))
    });

    Response::builder()
        .status(200)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .body(Body::from_stream(events))
        .unwrap()
}

fn stream_error(message: &str) -> String {
    let v = serde_json::json!({"type": "error", "error": {"type": "overloaded_error", "message": format!("free tier: {message}")}});
    format!("event: error\ndata: {v}\n\n")
}

/// Claude Code's system prompt tells the model it is Claude; say which model actually answers,
/// so "what model are you?" gets an honest reply.
fn set_identity(oai: &mut Value, model: &dino_router::Model) {
    const MARK: &str = "[dino] ";
    let note = format!(
        "{MARK}Identity: you are {} ({}), served by NVIDIA NIM through dino's free tier. You are not Claude; if asked what model you are, say so plainly.",
        model.short(),
        model.id
    );
    let Some(msgs) = oai["messages"].as_array_mut() else { return };
    // Replace the note from a previous attempt (fallback to another model) instead of stacking them.
    msgs.retain(|m| !(m["role"] == "system" && m["content"].as_str().is_some_and(|c| c.starts_with(MARK))));
    let at = msgs.iter().take_while(|m| m["role"] == "system").count();
    msgs.insert(at, json!({ "role": "system", "content": note }));
}

fn record_usage(st: &AppState, session: &str, u: &anyllm_translate::anthropic::Usage) {
    let usage = Usage {
        input: u.input_tokens as u64,
        output: u.output_tokens as u64,
        cache_read: u.cache_read_input_tokens.unwrap_or(0) as u64,
        cache_write: 0,
    };
    st.stats.update(session, |s| s.usage.add(&usage));
}

fn json_response(status: StatusCode, v: Value) -> Response<Body> {
    Response::builder().status(status).header("content-type", "application/json").body(Body::from(v.to_string())).unwrap()
}

fn anthropic_error(status: StatusCode, kind: &str, msg: &str) -> Response<Body> {
    json_response(status, json!({"type": "error", "error": {"type": kind, "message": msg}}))
}
