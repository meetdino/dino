//! Codex model fallback: ChatGPT's backend sometimes rejects a model the account lists
//! ("The model `gpt-5.5` does not exist or you do not have access to it"; openai/codex#43344,
//! #43543), and Codex then fails every turn. Retry with the next model the account lists instead,
//! and say so on the session (`SessionStats::substitute`).

use std::collections::HashMap;
use std::path::PathBuf;

use bytes::Bytes;
use serde_json::Value;

/// A Codex model the backend rejected, and the one that answered instead.
#[derive(Clone, Debug, PartialEq)]
pub struct Substitute {
    pub rejected: String,
    pub using: String,
    /// What the backend said of the rejected one.
    pub said: String,
    /// Since when, in Unix seconds.
    pub since: u64,
}

pub(crate) fn model_not_found(body: &[u8]) -> bool {
    let text = String::from_utf8_lossy(body);
    text.contains("does not exist or you do not have access") || text.contains("\"model_not_found\"")
}

/// Where Codex keeps the models the account lists.
fn cache_path() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(p) = tests::CACHE.lock().unwrap().clone() {
        return Some(p);
    }
    let home = std::env::var_os("CODEX_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".codex")));
    home.map(|h| h.join("models_cache.json"))
}

/// The account's selectable models from Codex's own cache, best first, except `skip`.
pub(crate) fn fallbacks(skip: &[&str]) -> Vec<String> {
    cache_path().and_then(|p| std::fs::read(p).ok()).map_or_else(Vec::new, |c| in_cache(&c, skip))
}

/// `model` is one the account lists (not a hidden one Codex uses on its own, as for reviews).
pub(crate) fn listed(model: &str) -> bool {
    cache_path().and_then(|p| std::fs::read(p).ok()).is_some_and(|c| in_cache(&c, &[]).iter().any(|m| m == model))
}

/// How long a rejected model is skipped before it's asked again: the backend may take it back.
pub(crate) const REMEMBERED_FOR: u64 = 3600;

/// The model answering in place of `model`, if the backend rejected it lately.
pub(crate) fn remembered(subs: &mut HashMap<String, Substitute>, model: &str) -> Option<Substitute> {
    let now = crate::fallback::now();
    subs.retain(|_, s| now.saturating_sub(s.since) < REMEMBERED_FOR);
    subs.get(model).cloned()
}

/// The selectable models in a `models_cache.json`, best first, except `skip`.
fn in_cache(cache: &[u8], skip: &[&str]) -> Vec<String> {
    let Ok(v) = serde_json::from_slice::<Value>(cache) else { return vec![] };
    let mut models: Vec<(i64, String)> = v["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m["visibility"] == "list")
        .filter_map(|m| Some((m["priority"].as_i64().unwrap_or(i64::MAX), m["slug"].as_str()?.to_string())))
        .filter(|(_, slug)| !skip.contains(&slug.as_str()))
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A `models_cache.json` for tests in place of Codex's own.
    pub(crate) static CACHE: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

    pub(crate) const CACHE_JSON: &str = r#"{"models":[
        {"slug":"gpt-5.4","priority":2,"visibility":"list"},
        {"slug":"gpt-5.5","priority":1,"visibility":"list"},
        {"slug":"codex-auto-review","priority":0,"visibility":"hide"},
        {"slug":"gpt-5.4-mini","priority":3,"visibility":"list"}
    ]}"#;

    #[test]
    fn the_next_listed_model_best_first() {
        assert_eq!(in_cache(CACHE_JSON.as_bytes(), &["gpt-5.5"]), ["gpt-5.4", "gpt-5.4-mini"]);
        assert_eq!(in_cache(CACHE_JSON.as_bytes(), &["gpt-5.5", "gpt-5.4"]), ["gpt-5.4-mini"]);
        assert!(in_cache(b"not json", &[]).is_empty());
    }

    #[test]
    fn only_the_model_is_swapped() {
        let body = Bytes::from(r#"{"model":"gpt-5.5","input":[{"role":"user"}],"stream":true}"#);
        let v: Value = serde_json::from_slice(&with_model(&body, "gpt-5.4").unwrap()).unwrap();
        assert_eq!(v["model"], "gpt-5.4");
        assert_eq!(v["stream"], true);
        assert_eq!(v["input"][0]["role"], "user");
        assert!(model_not_found(br#"{"detail":"The model `gpt-5.5` does not exist or you do not have access to it."}"#));
        assert!(!model_not_found(br#"{"detail":"Not Found"}"#));
    }

    #[test]
    fn a_rejected_model_is_asked_again_after_a_while() {
        let sub = |since| Substitute { rejected: "gpt-5.5".into(), using: "gpt-5.4".into(), said: String::new(), since };
        let now = crate::fallback::now();
        let mut subs = HashMap::from([("gpt-5.5".to_string(), sub(now - 60))]);
        assert_eq!(remembered(&mut subs, "gpt-5.5").map(|s| s.using), Some("gpt-5.4".into()));
        assert_eq!(remembered(&mut subs, "gpt-5.4"), None);
        subs.insert("gpt-5.5".into(), sub(now - REMEMBERED_FOR));
        assert_eq!(remembered(&mut subs, "gpt-5.5"), None);
        assert!(subs.is_empty());
    }
}
