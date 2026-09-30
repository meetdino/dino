//! Where models come from, as dinod last looked: OpenRouter and model servers on this Mac. Asked
//! in the background and kept; an IPC request only ever reads what's kept, and at most starts a
//! fetch. Everything about a model is what its provider says (`dino_core::providers`).

use std::collections::{HashMap, HashSet};
use std::net::{SocketAddr, TcpStream};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use dino_core::providers::{self, Format, ProviderInfo, ProviderModel};
use serde_json::Value;

pub const OPENROUTER_KEY: &str = "OPENROUTER_API_KEY";
const OPENROUTER: &str = "https://openrouter.ai/api";

/// Model servers people run on their Macs, at their default ports.
const LOCAL: &[(&str, &str, &str)] = &[
    ("ollama", "Ollama", "127.0.0.1:11434"),
    ("lmstudio", "LM Studio", "127.0.0.1:1234"),
    ("llamacpp", "llama.cpp", "127.0.0.1:8080"),
    ("vllm", "vLLM", "127.0.0.1:8000"),
];

/// A local server's formats and models, asked again this often while it runs.
const LOCAL_EVERY: Duration = Duration::from_secs(60);
/// OpenRouter's list changes a few times a day.
const HOSTED_EVERY: Duration = Duration::from_secs(6 * 3600);

#[derive(Default)]
struct Cache {
    providers: HashMap<String, (Instant, ProviderInfo)>,
    models: HashMap<String, Fetched>,
    /// Being fetched now.
    busy: HashSet<String>,
}

struct Fetched {
    at: Instant,
    models: Vec<ProviderModel>,
    error: Option<String>,
}

fn cache() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(Mutex::default)
}

pub(crate) fn http() -> &'static reqwest::blocking::Client {
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    CLIENT.get_or_init(|| reqwest::blocking::Client::builder().timeout(Duration::from_secs(20)).connect_timeout(Duration::from_secs(3)).build().expect("http client"))
}

/// Look now and then, in the background.
pub fn start() {
    std::thread::Builder::new()
        .name("providers".into())
        .spawn(|| {
            loop {
                refresh(false);
                std::thread::sleep(Duration::from_secs(15));
            }
        })
        .expect("providers thread");
}

/// Every provider, local ones whether or not they run.
pub fn list() -> Vec<ProviderInfo> {
    let c = cache().lock().unwrap();
    let mut out: Vec<ProviderInfo> = std::iter::once("openrouter").chain(LOCAL.iter().map(|l| l.0)).filter_map(|id| c.providers.get(id).map(|(_, p)| p.clone())).collect();
    if out.is_empty() {
        out = std::iter::once(openrouter_bare()).chain(LOCAL.iter().map(|(id, name, addr)| local_bare(id, name, addr))).collect();
    }
    out
}

pub fn find(id: &str) -> Option<ProviderInfo> {
    list().into_iter().find(|p| p.id == id)
}

/// What `id` serves as last fetched; a stale or missing list is asked for again, off this thread.
pub fn models(id: &str) -> (Vec<ProviderModel>, bool, Option<String>) {
    let every = if id == "openrouter" { HOSTED_EVERY } else { LOCAL_EVERY };
    let mut c = cache().lock().unwrap();
    let (models, error, stale) = match c.models.get(id) {
        Some(f) => (f.models.clone(), f.error.clone(), f.at.elapsed() >= every),
        None => (vec![], None, true),
    };
    let loading = c.busy.contains(id) || (stale && find_in(&c, id).is_some_and(|p| p.connected || p.id == "openrouter"));
    if loading && c.busy.insert(id.to_string()) {
        let id = id.to_string();
        std::thread::spawn(move || fetch_models(&id));
    }
    (models, loading, error)
}

fn find_in(c: &Cache, id: &str) -> Option<ProviderInfo> {
    c.providers.get(id).map(|(_, p)| p.clone())
}

/// Look at every provider again; `now` skips the waits (a key was just added or removed).
pub fn refresh(now: bool) {
    let key = dino_core::load_keys().remove(OPENROUTER_KEY);
    let seen = |id: &str, every: Duration| {
        let c = cache().lock().unwrap();
        c.providers.get(id).map(|(at, p)| (at.elapsed(), p.clone())).filter(|(age, _)| now.then_some(()).is_none() && *age < every).map(|(_, p)| p)
    };

    // OpenRouter: what it serves rarely changes; the account, every minute while there's a key.
    let mut or = seen("openrouter", HOSTED_EVERY).unwrap_or_else(|| ProviderInfo { formats: probe(OPENROUTER), ..openrouter_bare() });
    let had_key = or.connected;
    or.connected = key.is_some();
    or.account = match &key {
        Some(k) if now || !had_key || cache().lock().unwrap().providers.get("openrouter").is_none_or(|(at, _)| at.elapsed() >= Duration::from_secs(60)) => {
            match account(k) {
                Ok(a) => {
                    or.error = None;
                    Some(a)
                }
                Err(e) => {
                    or.error = Some(e);
                    or.account.clone()
                }
            }
        }
        Some(_) => or.account.clone(),
        None => {
            or.error = None;
            None
        }
    };
    store(or);

    for (id, name, addr) in LOCAL {
        let up = TcpStream::connect_timeout(&addr.parse::<SocketAddr>().unwrap(), Duration::from_millis(200)).is_ok();
        let p = if !up {
            local_bare(id, name, addr)
        } else if let Some(p) = seen(id, LOCAL_EVERY).filter(|p| p.connected) {
            p
        } else {
            let base = format!("http://{addr}");
            let mut p = ProviderInfo { formats: probe(&base), connected: true, version: version(id, &base), ..local_bare(id, name, addr) };
            // Something else on the port (a dev server on 8000): not this one.
            if !is(id, &base) {
                p = local_bare(id, name, addr);
            }
            p
        };
        let running = p.connected;
        store(p);
        if !running {
            cache().lock().unwrap().models.remove(*id);
        }
    }
}

fn store(p: ProviderInfo) {
    let mut c = cache().lock().unwrap();
    let keep = c.providers.get(&p.id).filter(|(_, old)| old.formats == p.formats && old.connected == p.connected && old.version == p.version).map(|(at, _)| *at);
    c.providers.insert(p.id.clone(), (keep.unwrap_or_else(Instant::now), p));
}

fn openrouter_bare() -> ProviderInfo {
    ProviderInfo { id: "openrouter".into(), name: "OpenRouter".into(), base: OPENROUTER.into(), key: Some(OPENROUTER_KEY.into()), ..Default::default() }
}

fn local_bare(id: &str, name: &str, addr: &str) -> ProviderInfo {
    ProviderInfo { id: id.into(), name: name.into(), base: format!("http://{addr}"), local: true, ..Default::default() }
}

/// Which formats `base` has routes for: each asked once with an empty body.
fn probe(base: &str) -> Vec<Format> {
    Format::ALL
        .into_iter()
        .filter(|f| {
            let status = http().post(format!("{base}/{}", f.path())).header("content-type", "application/json").body("{}").timeout(Duration::from_secs(5)).send().map(|r| r.status().as_u16()).unwrap_or(0);
            providers::serves(status)
        })
        .collect()
}

fn get(url: &str) -> Result<Value, String> {
    let r = http().get(url).send().map_err(|e| e.to_string())?;
    let status = r.status();
    let v: Value = r.json().map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(v["error"]["message"].as_str().or(v["error"].as_str()).map(String::from).unwrap_or_else(|| format!("HTTP {status}")));
    }
    Ok(v)
}

fn version(id: &str, base: &str) -> Option<String> {
    match id {
        "ollama" => get(&format!("{base}/api/version")).ok()?["version"].as_str().map(String::from),
        "llamacpp" => get(&format!("{base}/props")).ok()?["build_info"].as_str().map(String::from),
        _ => None,
    }
}

/// The server on `id`'s port is `id`, as far as it can tell.
fn is(id: &str, base: &str) -> bool {
    match id {
        "ollama" => get(&format!("{base}/api/version")).is_ok(),
        "lmstudio" => get(&format!("{base}/api/v0/models")).is_ok(),
        "llamacpp" => get(&format!("{base}/props")).is_ok(),
        "vllm" => get(&format!("{base}/v1/models")).is_ok_and(|v| v["data"].as_array().is_some_and(|d| d.iter().any(|m| m["owned_by"] == "vllm" || m["max_model_len"].is_u64()))),
        _ => false,
    }
}

fn account(key: &str) -> Result<dino_core::providers::Account, String> {
    let with = |url: &str| -> Result<Value, String> {
        let r = http().get(url).bearer_auth(key).send().map_err(|e| e.to_string())?;
        let status = r.status();
        let v: Value = r.json().map_err(|e| e.to_string())?;
        if status.as_u16() == 401 {
            return Err("OpenRouter doesn't take this key any more; connect again".into());
        }
        status.is_success().then_some(v).ok_or_else(|| format!("OpenRouter said {status}"))
    };
    let key_info = with(&format!("{OPENROUTER}/v1/key"))?;
    // Only a key that can manage the account may read its credits.
    let credits = with(&format!("{OPENROUTER}/v1/credits")).ok();
    Ok(providers::openrouter_account(&key_info, credits.as_ref()))
}

fn fetch_models(id: &str) {
    let base = find(id).map(|p| p.base).unwrap_or_default();
    let got: Result<Vec<ProviderModel>, String> = match id {
        "openrouter" => get(&format!("{OPENROUTER}/v1/models")).map(|v| providers::openrouter_models(&v)),
        "ollama" => get(&format!("{base}/api/tags")).map(|v| {
            v["models"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|tag| {
                    // Only older Ollamas leave these out of the list; ask those per model.
                    let show = if tag["capabilities"].is_array() && tag["details"]["context_length"].is_u64() {
                        Value::Null
                    } else {
                        let name = tag["name"].as_str().unwrap_or_default();
                        http().post(format!("{base}/api/show")).json(&serde_json::json!({"model": name})).send().ok().and_then(|r| r.json().ok()).unwrap_or(Value::Null)
                    };
                    providers::ollama_model(tag, &show)
                })
                .collect()
        }),
        "lmstudio" => get(&format!("{base}/api/v0/models")).map(|v| providers::lmstudio_models(&v)),
        "llamacpp" => get(&format!("{base}/v1/models")).map(|m| providers::llamacpp_models(&m, &get(&format!("{base}/props")).unwrap_or(Value::Null))),
        "vllm" => get(&format!("{base}/v1/models")).map(|v| providers::vllm_models(&v)),
        _ => Err(format!("no provider {id}")),
    };
    let mut c = cache().lock().unwrap();
    c.busy.remove(id);
    let (models, error) = match got {
        Ok(m) => (m, None),
        // Keep the last good list; say why it's old.
        Err(e) => (c.models.get(id).map(|f| f.models.clone()).unwrap_or_default(), Some(e)),
    };
    c.models.insert(id.to_string(), Fetched { at: Instant::now(), models, error });
}
