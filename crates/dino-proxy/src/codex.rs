//! Codex model fallback: ChatGPT's backend sometimes rejects a model the account lists
//! ("The model `gpt-5.5` does not exist or you do not have access to it"; openai/codex#43543),
//! and Codex then fails every turn. Retry with the next model the account lists instead.

use std::path::PathBuf;

use bytes::Bytes;
use serde_json::Value;

pub(crate) fn model_not_found(body: &[u8]) -> bool {
    String::from_utf8_lossy(body).contains("does not exist or you do not have access")
}

/// The account's selectable models from Codex's own cache, best first, except `rejected`.
pub(crate) fn fallbacks(rejected: &str) -> Vec<String> {
    let home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".codex")));
    let Some(cache) = home.and_then(|h| std::fs::read(h.join("models_cache.json")).ok()) else { return vec![] };
    let Ok(v) = serde_json::from_slice::<Value>(&cache) else { return vec![] };
    let mut models: Vec<(i64, String)> = v["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m["visibility"] == "list")
        .filter_map(|m| Some((m["priority"].as_i64().unwrap_or(i64::MAX), m["slug"].as_str()?.to_string())))
        .filter(|(_, slug)| slug != rejected)
        .collect();
    models.sort();
    models.into_iter().map(|(_, slug)| slug).collect()
}

/// `body` with its `model` swapped for `model`.
pub(crate) fn with_model(body: &Bytes, model: &str) -> Option<Bytes> {
    let mut v: Value = serde_json::from_slice(body).ok()?;
    v.as_object_mut()?.insert("model".into(), model.into());
    serde_json::to_vec(&v).ok().map(Bytes::from)
}

/// The human part of an upstream error body: `{"error":{"message":…}}`, `{"detail":…}` or the text.
pub(crate) fn error_message(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    let msg = serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().or(v["detail"].as_str()).or(v["message"].as_str()).map(String::from))
        .unwrap_or_else(|| text.trim().to_string());
    msg.chars().take(200).collect()
}
