//! The free tier's models, found rather than listed: the provider's own list (`/v1/models`), its
//! featured list for a ranking and output limits, and one small request to each model to see
//! whether it answers and calls tools. Kept in a file, so a restart starts with what's known.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use dino_router::Probe;
use serde_json::{Value, json};

use crate::{AppState, log};

pub(crate) const NIM_BASE: &str = "https://integrate.api.nvidia.com/v1";
/// NVIDIA's featured models: a ranking, and each one's context and output limits.
const FEATURED: &str = "https://assets.ngc.nvidia.com/products/api-catalog/featured-models.json";
/// A model is tried again after this long.
const STALE: u64 = 24 * 3600;
/// Between tries, to stay well inside the free tier's rate limit.
const PACE: Duration = Duration::from_secs(2);

/// What was learned about one model, and when.
struct Tried {
    answers: bool,
    tools: bool,
    ms: f64,
    at: u64,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn load(path: &Path) -> HashMap<String, Tried> {
    let Some(v) = std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) else { return HashMap::new() };
    v["tried"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(id, t)| {
            let tried =
                Tried { answers: t["answers"].as_bool().unwrap_or(false), tools: t["tools"].as_bool().unwrap_or(false), ms: t["ms"].as_f64().unwrap_or(f64::MAX), at: t["at"].as_u64().unwrap_or(0) };
            (id.clone(), tried)
        })
        .collect()
}

fn save(path: &Path, listed: &[String], tried: &HashMap<String, Tried>, featured: &[(String, Option<u64>)]) {
    let tried: serde_json::Map<String, Value> = tried.iter().map(|(id, t)| (id.clone(), json!({"answers": t.answers, "tools": t.tools, "ms": t.ms, "at": t.at}))).collect();
    let featured: Vec<Value> = featured.iter().map(|(id, max)| json!({"id": id, "max_output": max})).collect();
    let v = json!({"listed": listed, "featured": featured, "tried": tried});
    let _ = std::fs::write(path, serde_json::to_vec_pretty(&v).unwrap_or_default());
}

/// The router's view: every listed model with what was learned, ranked and limited as the
/// provider says.
fn probes(listed: &[String], tried: &HashMap<String, Tried>, featured: &[(String, Option<u64>)]) -> Vec<Probe> {
    listed
        .iter()
        .map(|id| {
            let t = tried.get(id);
            let rank = featured.iter().position(|(f, _)| f == id);
            Probe {
                id: id.clone(),
                answers: t.is_some_and(|t| t.answers),
                tools: t.is_some_and(|t| t.tools),
                ms: t.map_or(f64::MAX, |t| t.ms),
                rank,
                tried: t.is_some(),
                max_output: rank.and_then(|r| featured[r].1),
            }
        })
        .collect()
}

fn cached(path: &Path) -> (Vec<String>, Vec<(String, Option<u64>)>) {
    let Some(v) = std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) else { return (vec![], vec![]) };
    let listed = v["listed"].as_array().into_iter().flatten().filter_map(|x| x.as_str().map(String::from)).collect();
    let featured = v["featured"].as_array().into_iter().flatten().filter_map(|f| Some((f["id"].as_str()?.to_string(), f["max_output"].as_u64()))).collect();
    (listed, featured)
}

/// Keep the router's view of the free models current, for as long as dinod runs.
pub(crate) async fn keep_fresh(st: AppState, path: PathBuf) {
    let (listed, featured) = cached(&path);
    st.router.set_probes(probes(&listed, &load(&path), &featured));
    loop {
        // Only while the free tier is on (Settings → Experimental).
        let on = st.free_models.load(std::sync::atomic::Ordering::Relaxed);
        let key = st.keys.read().unwrap().get("NVIDIA_API_KEY").cloned().filter(|_| on);
        let done = match key {
            Some(key) => refresh(&st, &key, &path).await,
            None => false,
        };
        // Until a full look has gone through (no key yet, offline, rate limited), look again soon,
        // and at once when the free tier is turned on or a key comes (`free_wake`).
        if done {
            tokio::time::sleep(Duration::from_secs(6 * 3600)).await;
        } else {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(60)) => {}
                _ = st.free_wake.notified() => {}
            }
        }
    }
}

/// One look: the list, the ranking, and a try of every model not tried lately. `true` when it got
/// through all of them.
async fn refresh(st: &AppState, key: &str, path: &Path) -> bool {
    let listed: Vec<String> = match get(st, &format!("{NIM_BASE}/models"), Some(key)).await {
        Some(v) => v["data"].as_array().into_iter().flatten().filter_map(|m| m["id"].as_str().map(String::from)).collect(),
        None => return false,
    };
    // Its featured list sometimes leaves out the owner's prefix: match on what's listed.
    let featured: Vec<(String, Option<u64>)> = get(st, FEATURED, None)
        .await
        .map(|v| {
            v["featured-models"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|f| {
                    let name = f["model"].as_str()?;
                    let id = listed.iter().find(|id| *id == name || id.ends_with(&format!("/{name}")))?;
                    Some((id.clone(), f["max-output"].as_u64()))
                })
                .collect()
        })
        .unwrap_or_else(|| cached(path).1);
    let mut tried = load(path);
    tried.retain(|id, _| listed.contains(id));
    st.router.set_probes(probes(&listed, &tried, &featured));
    save(path, &listed, &tried, &featured);

    // The provider's picks first, then those that worked before, then the rest.
    let mut order: Vec<&String> = listed.iter().filter(|id| tried.get(*id).is_none_or(|t| now().saturating_sub(t.at) > STALE)).collect();
    order.sort_by_key(|id| (!featured.iter().any(|(f, _)| f == *id), !tried.get(*id).is_some_and(|t| t.tools)));
    let order: Vec<String> = order.into_iter().cloned().collect();
    for id in order {
        match try_model(st, key, &id).await {
            Some(t) => {
                log(format_args!("free models: {id} answers={} tools={} in {:.0}ms", t.answers, t.tools, t.ms));
                tried.insert(id, t);
                st.router.set_probes(probes(&listed, &tried, &featured));
                save(path, &listed, &tried, &featured);
            }
            // Rate limited or offline: pick up from here next time.
            None => return false,
        }
        tokio::time::sleep(PACE).await;
    }
    true
}

async fn get(st: &AppState, url: &str, key: Option<&str>) -> Option<Value> {
    let mut req = st.client().get(url).timeout(Duration::from_secs(20));
    if let Some(key) = key {
        req = req.bearer_auth(key);
    }
    let r = req.send().await.ok()?;
    if !r.status().is_success() {
        log(format_args!("free models: {url} -> {}", r.status()));
        return None;
    }
    r.json().await.ok()
}

/// Ask `id` to call a tool. `None` when the provider can't say right now (rate limit, network).
async fn try_model(st: &AppState, key: &str, id: &str) -> Option<Tried> {
    let body = json!({
        "model": id,
        "messages": [{"role": "user", "content": "Call the ping tool."}],
        "tools": [{"type": "function", "function": {"name": "ping", "description": "Answer a ping.", "parameters": {"type": "object", "properties": {}}}}],
        "max_tokens": 512,
    });
    let started = Instant::now();
    let sent = st.client().post(format!("{NIM_BASE}/chat/completions")).bearer_auth(key).timeout(Duration::from_secs(45)).json(&body).send().await;
    let ms = started.elapsed().as_secs_f64() * 1000.0;
    let at = now();
    let r = match sent {
        Ok(r) => r,
        // Too slow to use is as good as not answering.
        Err(e) if e.is_timeout() => return Some(Tried { answers: false, tools: false, ms, at }),
        Err(_) => return None,
    };
    let status = r.status();
    if status.as_u16() == 429 {
        return None;
    }
    if !status.is_success() {
        return Some(Tried { answers: false, tools: false, ms, at });
    }
    let v: Value = r.json().await.unwrap_or_default();
    let message = &v["choices"][0]["message"];
    let tools = message["tool_calls"].as_array().is_some_and(|c| !c.is_empty());
    Some(Tried { answers: message.is_object(), tools, ms, at })
}
