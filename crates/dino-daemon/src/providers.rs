//! Where models come from, as dinod last looked: OpenRouter, model servers on this Mac and coding
//! plans. Asked
//! in the background and kept; an IPC request only ever reads what's kept, and at most starts a
//! fetch. Everything about a model is what its provider says (`dino_core::providers`).

use std::collections::{HashMap, HashSet};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use dino_core::compat::Compat;
use dino_core::ipc::ModelRow;
use dino_core::plans::{self, Preset};
use dino_core::providers::{self, Format, PlanInfo, ProviderInfo, ProviderModel};
use serde_json::Value;

pub const OPENROUTER_KEY: &str = "OPENROUTER_API_KEY";
const OPENROUTER: &str = "https://openrouter.ai/api";

/// Model servers people run on their Macs, at their default addresses; the proxy's `local` route
/// serves the same ones.
const LOCAL: &[(&str, &str, &str)] = dino_proxy::local::RUNTIMES;

/// A local server's formats and models, asked again this often while it runs.
const LOCAL_EVERY: Duration = Duration::from_secs(60);
/// OpenRouter's list changes a few times a day.
const HOSTED_EVERY: Duration = Duration::from_secs(6 * 3600);

#[derive(Default)]
struct Cache {
    /// Each provider, with when it was last asked what it serves.
    providers: HashMap<String, (Instant, ProviderInfo)>,
    /// When OpenRouter's account was last read.
    account_at: Option<Instant>,
    /// Local runtimes whose port something else answers on (a dev server on 8000), and when that
    /// was found: that server is asked one GET, then nothing for `LOCAL_EVERY`.
    others: HashMap<String, Instant>,
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
    let plans: Vec<String> = plans::presets().iter().map(Preset::provider_id).collect();
    let ids = ["openrouter", "chatgpt"].into_iter().chain(LOCAL.iter().map(|l| l.0)).chain(plans.iter().map(String::as_str));
    let mut out: Vec<ProviderInfo> = ids.filter_map(|id| c.providers.get(id).map(|(_, p)| p.clone())).collect();
    if out.is_empty() {
        let keys = dino_core::load_keys();
        out = [openrouter_bare(), chatgpt_bare()]
            .into_iter()
            .chain(LOCAL.iter().filter_map(|(id, _, _)| dino_proxy::local::runtime(id).map(|(name, base)| local_bare(id, name, base))))
            .chain(plans::presets().iter().map(|p| plan_bare(p, &keys)))
            .collect();
    }
    out
}

pub fn find(id: &str) -> Option<ProviderInfo> {
    list().into_iter().find(|p| p.id == id)
}

/// `compat.json`, read once.
fn compat() -> &'static Compat {
    static COMPAT: OnceLock<Compat> = OnceLock::new();
    COMPAT.get_or_init(Compat::load)
}

/// What `id` serves, each with what every agent can make of it.
pub fn rows(id: &str) -> (Vec<ModelRow>, bool, Option<String>) {
    let (models, loading, error) = models(id);
    let p = find(id).unwrap_or_default();
    let rows = models.into_iter().map(|m| ModelRow { agents: compat().judge(&m, &p), model: m }).collect();
    (rows, loading, error)
}

/// What `id` serves as last fetched; a stale or missing list is asked for again, off this thread.
fn models(id: &str) -> (Vec<ProviderModel>, bool, Option<String>) {
    let every = if matches!(id, "openrouter" | "chatgpt") || id.starts_with(plans::PREFIX) { HOSTED_EVERY } else { LOCAL_EVERY };
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

/// What `provider` says of `model`, from its last list. A server on this Mac that hasn't been
/// listed yet is asked now (it answers at once); a hosted one off this thread, so none this time.
pub fn model(provider: &str, model: &str) -> Option<ProviderModel> {
    let listed = cache().lock().unwrap().models.contains_key(provider);
    if !listed && find(provider).is_some_and(|p| p.local && p.connected) {
        fetch_models(provider);
    }
    models(provider).0.into_iter().find(|m| m.id == model)
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
    let kept = seen("openrouter", HOSTED_EVERY);
    let asked = kept.is_none();
    let mut or = kept.unwrap_or_else(|| ProviderInfo { formats: probe(OPENROUTER), ..openrouter_bare() });
    let had_key = or.connected;
    or.connected = key.is_some();
    or.account = match &key {
        Some(k) if now || !had_key || cache().lock().unwrap().account_at.is_none_or(|at| at.elapsed() >= Duration::from_secs(60)) => {
            cache().lock().unwrap().account_at = Some(Instant::now());
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
            // An error from connecting stays until the next try.
            or.error = cache().lock().unwrap().providers.get("openrouter").and_then(|(_, p)| p.error.clone());
            None
        }
    };
    store(or, asked);

    // Signed in with ChatGPT: what the key store says, and the last error until the next try.
    let keys = dino_core::load_keys();
    let mut chatgpt = chatgpt_bare();
    let signed_in = crate::chatgpt::status(&keys);
    chatgpt.connected = signed_in.is_some();
    chatgpt.account = signed_in.map(|plan| providers::Account {
        label: Some(if plan { "Plan usage allowed" } else { "Signed in, but plan usage wasn't allowed: sign in again and allow it" }.into()),
        ..Default::default()
    });
    {
        let mut c = cache().lock().unwrap();
        let was = c.providers.get("chatgpt").map(|(_, p)| (p.connected, p.error.clone()));
        chatgpt.error = was.as_ref().and_then(|w| w.1.clone());
        // Signed in or out since: the list was for someone else.
        if was.is_some_and(|w| w.0 != chatgpt.connected) {
            c.models.remove("chatgpt");
        }
    }
    store(chatgpt, false);

    for (id, _, _) in LOCAL {
        let Some((name, base)) = dino_proxy::local::runtime(id) else { continue };
        let addr = base.split_once("://").map_or(base, |(_, a)| a);
        let up = addr.to_socket_addrs().ok().and_then(|mut a| a.next()).is_some_and(|a| TcpStream::connect_timeout(&a, Duration::from_millis(200)).is_ok());
        let other = cache().lock().unwrap().others.get(*id).is_some_and(|at| at.elapsed() < LOCAL_EVERY);
        let (p, asked) = if !up {
            cache().lock().unwrap().others.remove(*id);
            (local_bare(id, name, base), false)
        } else if let Some(p) = seen(id, LOCAL_EVERY).filter(|p| p.connected) {
            (p, false)
        } else if other {
            (local_bare(id, name, base), false)
        } else if !is(id, base) {
            // Something else on the port (a dev server on 8000): asked what it is, nothing more.
            cache().lock().unwrap().others.insert(id.to_string(), Instant::now());
            (local_bare(id, name, base), false)
        } else {
            (ProviderInfo { formats: probe(base), connected: true, version: version(id, base), ..local_bare(id, name, base) }, true)
        };
        let running = p.connected;
        store(p, asked);
        if !running {
            cache().lock().unwrap().models.remove(*id);
        }
    }

    // Coding plans: connected while the key store has a key for them. What they serve is what
    // their docs say; the generic entry is asked, once per URL and key.
    for preset in plans::presets() {
        let mut p = plan_bare(preset, &keys);
        let id = p.id.clone();
        let old = cache().lock().unwrap().providers.get(&id).map(|(_, p)| p.clone());
        p.error = old.as_ref().filter(|o| o.connected == p.connected).and_then(|o| o.error.clone());
        if preset.id == plans::OTHER && p.connected {
            let asked = old.filter(|o| o.connected && o.base == p.base && !o.formats.is_empty() && !now);
            p.formats = match asked {
                Some(o) => o.formats,
                None => plan_route(preset, &keys).map(|r| probe_plan(&r)).unwrap_or_default(),
            };
            if p.formats.is_empty() && p.error.is_none() {
                p.error = Some(format!("Nothing at {} answered as Anthropic Messages or the OpenAI API", p.base));
            }
        }
        if !p.connected {
            cache().lock().unwrap().models.remove(&id);
        }
        store(p, false);
    }
}

/// A coding plan as Settings shows it, before anything is asked of it.
fn plan_bare(preset: &Preset, keys: &HashMap<String, String>) -> ProviderInfo {
    let other = preset.id == plans::OTHER;
    let url = keys.get(plans::OTHER_URL_KEY).filter(|u| !u.is_empty());
    let resolved = if other { url.map(|u| Preset::other(u)) } else { Some(preset.clone()) };
    let connected = keys.get(&preset.key_name()).is_some_and(|k| !k.is_empty()) && resolved.is_some();
    ProviderInfo {
        id: preset.provider_id(),
        name: resolved.filter(|_| connected).map_or_else(|| preset.name.clone(), |p| p.name),
        base: if other { url.cloned().unwrap_or_default() } else { preset.anthropic.clone().or(preset.openai.clone()).unwrap_or_default() },
        formats: if other { vec![] } else { preset.formats() },
        connected,
        key: Some(preset.key_name()),
        plan: Some(PlanInfo {
            blurb: preset.blurb.clone(),
            docs: preset.docs.clone(),
            keys_page: preset.keys_page.clone(),
            terms: preset.terms.clone(),
            custom: other,
            models_note: preset.models_note.clone(),
        }),
        ..Default::default()
    }
}

/// Coding plan `preset` as dino's proxy serves it, with its key, if it has one.
fn plan_route(preset: &Preset, keys: &HashMap<String, String>) -> Option<dino_proxy::plan::Plan> {
    let key = keys.get(&preset.key_name()).filter(|k| !k.is_empty() && !plans::is_subscription_token(k))?;
    let p = if preset.id == plans::OTHER { Preset::other(keys.get(plans::OTHER_URL_KEY).filter(|u| !u.is_empty())?) } else { preset.clone() };
    Some(dino_proxy::plan::Plan { name: p.name, anthropic: p.anthropic, openai: p.openai, key: key.clone() })
}

/// Every connected coding plan, by preset id, for the proxy's `plan/<id>` routes.
pub fn plan_routes(keys: &HashMap<String, String>) -> HashMap<String, dino_proxy::plan::Plan> {
    plans::presets().iter().filter_map(|p| Some((p.id.clone(), plan_route(p, keys)?))).collect()
}

/// Where each format would be on `plan`, and the headers its key goes in.
fn plan_request(plan: &dino_proxy::plan::Plan, f: Format) -> Option<(String, reqwest::header::HeaderMap)> {
    let url = match f {
        Format::Anthropic => format!("{}/v1/messages", plan.anthropic.as_deref()?),
        Format::Chat => format!("{}/chat/completions", plan.openai.as_deref()?),
        Format::Responses => format!("{}/responses", plan.openai.as_deref()?),
    };
    Some((url, plan_headers(plan, f == Format::Anthropic)))
}

fn plan_headers(plan: &dino_proxy::plan::Plan, anthropic: bool) -> reqwest::header::HeaderMap {
    let mut h = reqwest::header::HeaderMap::new();
    if let Ok(v) = format!("Bearer {}", plan.key).parse() {
        h.insert(reqwest::header::AUTHORIZATION, v);
    }
    if anthropic {
        if let Ok(v) = plan.key.parse() {
            h.insert("x-api-key", v);
        }
        h.insert("anthropic-version", reqwest::header::HeaderValue::from_static("2023-06-01"));
    }
    h
}

/// Which formats the generic entry serves: each asked once with an empty body, with its key (so
/// a server that checks keys first answers for the route, not the key).
fn probe_plan(plan: &dino_proxy::plan::Plan) -> Vec<Format> {
    Format::ALL
        .into_iter()
        .filter(|f| {
            let Some((url, headers)) = plan_request(plan, *f) else { return false };
            let status = http().post(url).headers(headers).header("content-type", "application/json").body("{}").timeout(Duration::from_secs(5)).send().map(|r| r.status().as_u16()).unwrap_or(0);
            providers::serves(status)
        })
        .collect()
}

/// Coding plan `id`'s models: its own list when it has one, else the ones its docs name, saying
/// so. A key it turns down is an error.
fn plan_models(id: &str) -> Result<(Vec<ProviderModel>, Option<String>), String> {
    let preset = plans::preset(id).ok_or_else(|| format!("no provider {id}"))?;
    let keys = dino_core::load_keys();
    let plan = plan_route(preset, &keys).ok_or_else(|| format!("{} has no key", preset.name))?;
    let listed = match preset.list.as_deref() {
        Some(kind) => list_plan(&plan, kind),
        None => Err(None),
    };
    let documented = plans::documented_models(preset);
    match listed {
        Ok(v) => {
            let models = plans::listed_models(&v, id, preset.tools);
            if models.is_empty() && !documented.is_empty() {
                return Ok((documented, Some(format!("{} listed no models: these are the ones its docs name", preset.name))));
            }
            Ok((models, None))
        }
        Err(Some(refused)) => Err(refused),
        Err(None) if !documented.is_empty() => Ok((documented, Some(format!("{} has no model list dino can read: these are the ones its docs name", preset.name)))),
        Err(None) => Err(format!("{} has no model list dino can read", preset.name)),
    }
}

/// `plan`'s model list (`kind`: "openai" or "anthropic"). `Err(Some(why))`: it turned the key
/// down; `Err(None)`: there's no list to be had.
fn list_plan(plan: &dino_proxy::plan::Plan, kind: &str) -> Result<Value, Option<String>> {
    let (url, anthropic) = match (kind, &plan.openai, &plan.anthropic) {
        ("openai", Some(base), _) => (format!("{base}/models"), false),
        (_, _, Some(base)) => (format!("{base}/v1/models"), true),
        _ => return Err(None),
    };
    let r = http().get(url).headers(plan_headers(plan, anthropic)).timeout(Duration::from_secs(10)).send().map_err(|_| None)?;
    let status = r.status().as_u16();
    let text = r.text().unwrap_or_default();
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    // Z.ai answers some failures with a 200 whose body says so.
    let failed = v["success"] == Value::Bool(false);
    let said = v["error"]["message"].as_str().or(v["msg"].as_str()).or(v["message"].as_str()).map(String::from).unwrap_or_else(|| text.trim().chars().take(200).collect());
    if matches!(status, 401 | 403) || (failed && said.to_lowercase().contains("authenticat")) {
        return Err(Some(format!("{} didn't take this key ({status}: {said}). Add it again", plan.name)));
    }
    if !(200..300).contains(&status) || failed || !v["data"].is_array() {
        return Err(None);
    }
    Ok(v)
}

/// Connect coding plan `id` (`plan-zai`) with `key`, and `base` for the generic entry: checked
/// with the plan when it has a list to ask, then kept in the key store. `save` stores one key.
pub fn connect_plan(id: &str, key: &str, base: Option<&str>, save: impl Fn(&str, Option<&str>) -> anyhow::Result<()>) -> Result<(), String> {
    let preset = plans::preset(id).ok_or_else(|| format!("dino doesn't know a coding plan called {id}"))?;
    let key = plans::check_key(key)?;
    let other = preset.id == plans::OTHER;
    let base = if other { Some(plans::check_base(base.unwrap_or_default())?) } else { None };
    let mut keys = HashMap::from([(preset.key_name(), key.clone())]);
    if let Some(b) = &base {
        keys.insert(plans::OTHER_URL_KEY.into(), b.clone());
    }
    let plan = plan_route(preset, &keys).ok_or("That key can't be used")?;
    if let Some(kind) = preset.list.as_deref()
        && let Err(Some(refused)) = list_plan(&plan, kind)
    {
        return Err(refused);
    }
    if let Some(b) = &base {
        save(plans::OTHER_URL_KEY, Some(b)).map_err(|e| e.to_string())?;
    }
    save(&preset.key_name(), Some(&key)).map_err(|e| e.to_string())?;
    let mut c = cache().lock().unwrap();
    c.models.remove(id);
    if let Some((_, p)) = c.providers.get_mut(id) {
        p.error = None;
    }
    Ok(())
}

/// Disconnect coding plan `id`: its key (and the generic entry's URL) leave the key store.
pub fn disconnect_plan(id: &str, save: impl Fn(&str, Option<&str>) -> anyhow::Result<()>) -> anyhow::Result<()> {
    let preset = plans::preset(id).ok_or_else(|| anyhow::anyhow!("dino doesn't know a coding plan called {id}"))?;
    save(&preset.key_name(), None)?;
    if preset.id == plans::OTHER {
        save(plans::OTHER_URL_KEY, None)?;
    }
    forget(id);
    cache().lock().unwrap().models.remove(id);
    Ok(())
}

/// Keep `p`. `asked`: it's what the provider just said, so its age starts again; otherwise it's
/// what was kept (or nothing was asked), and keeps its age.
fn store(p: ProviderInfo, asked: bool) {
    let mut c = cache().lock().unwrap();
    let at = c.providers.get(&p.id).filter(|_| !asked).map_or_else(Instant::now, |(at, _)| *at);
    c.providers.insert(p.id.clone(), (at, p));
}

fn openrouter_bare() -> ProviderInfo {
    ProviderInfo { id: "openrouter".into(), name: "OpenRouter".into(), base: OPENROUTER.into(), key: Some(OPENROUTER_KEY.into()), ..Default::default() }
}

/// The ChatGPT plan, through Sign in with ChatGPT: the Responses API is what the plan may be spent on.
fn chatgpt_bare() -> ProviderInfo {
    ProviderInfo {
        id: "chatgpt".into(),
        name: "ChatGPT plan".into(),
        base: crate::chatgpt::API.trim_end_matches("/v1").into(),
        formats: vec![Format::Responses],
        key: Some(crate::chatgpt::REFRESH_KEY.into()),
        ..Default::default()
    }
}

fn local_bare(id: &str, name: &str, base: &str) -> ProviderInfo {
    ProviderInfo { id: id.into(), name: name.into(), base: base.into(), local: true, ..Default::default() }
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

/// Connect OpenRouter without pasting a key: OpenRouter's OAuth PKCE. Returns the page to open;
/// when the browser comes back to dinod's one-off listener, the code is exchanged for a key the
/// user controls (on openrouter.ai/keys), which `save` stores. Nothing is shown or logged.
pub fn connect_openrouter(save: impl FnOnce(String) -> anyhow::Result<()> + Send + 'static) -> anyhow::Result<String> {
    connect_with("https://openrouter.ai/auth", &format!("{OPENROUTER}/v1/auth/keys"), save)
}

/// How long the sign-in page may take before the listener gives up.
const CONNECT_WAIT: Duration = Duration::from_secs(10 * 60);

fn connect_with(authorize: &str, exchange: &str, save: impl FnOnce(String) -> anyhow::Result<()> + Send + 'static) -> anyhow::Result<String> {
    use base64::Engine;
    use sha2::Digest;
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let callback = format!("http://127.0.0.1:{port}/callback");
    let mut bytes = [0u8; 32];
    std::io::Read::read_exact(&mut std::fs::File::open("/dev/urandom")?, &mut bytes)?;
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let verifier = b64.encode(bytes);
    let challenge = b64.encode(sha2::Sha256::digest(verifier.as_bytes()));
    let url = format!("{authorize}?callback_url={}&code_challenge={challenge}&code_challenge_method=S256", percent(&callback));
    let exchange = exchange.to_string();
    set_error("openrouter", None);
    std::thread::spawn(move || {
        let result = (|| -> anyhow::Result<()> {
            let code = wait_for_code(&listener)?;
            let r = http()
                .post(&exchange)
                .timeout(Duration::from_secs(30))
                .json(&serde_json::json!({"code": code, "code_verifier": verifier, "code_challenge_method": "S256"}))
                .send()?;
            let status = r.status();
            let v: Value = r.json().unwrap_or(Value::Null);
            let key = v["key"].as_str().filter(|k| !k.is_empty()).ok_or_else(|| anyhow::anyhow!("OpenRouter didn't give a key ({status})"))?;
            save(key.to_string())
        })();
        if let Err(e) = result {
            set_error("openrouter", Some(format!("Connecting didn't finish: {e}")));
        }
        refresh(true);
    });
    Ok(url)
}

/// The `code` the browser brings back to the listener, answering it with a page to close.
fn wait_for_code(listener: &std::net::TcpListener) -> anyhow::Result<String> {
    use std::io::{BufRead, Write};
    listener.set_nonblocking(true)?;
    let until = Instant::now() + CONNECT_WAIT;
    loop {
        let (stream, _) = match listener.accept() {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < until => {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => anyhow::bail!("the sign-in page wasn't finished within 10 minutes"),
            Err(e) => return Err(e.into()),
        };
        stream.set_nonblocking(false)?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        let mut line = String::new();
        std::io::BufReader::new(&stream).read_line(&mut line)?;
        // "GET /callback?code=…&… HTTP/1.1"
        let target = line.split_whitespace().nth(1).unwrap_or("");
        let Some(query) = target.strip_prefix("/callback?").or_else(|| target.strip_prefix("/callback/?")) else {
            // A favicon or a stray request: not the one.
            let _ = (&stream).write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            continue;
        };
        let param = |name: &str| query.split('&').find_map(|kv| kv.strip_prefix(&format!("{name}="))).map(unpercent);
        let (title, body, got) = match param("code") {
            Some(code) if !code.is_empty() => ("dino is connected to OpenRouter", "You can close this tab and go back to dino.", Ok(code)),
            _ => ("OpenRouter wasn't connected", "Nothing was changed. You can close this tab.", Err(anyhow::anyhow!("OpenRouter said {}", param("error").unwrap_or_else(|| "no".into())))),
        };
        let page = format!("<!doctype html><meta charset=utf-8><title>{title}</title><body style=\"font:15px -apple-system,sans-serif;margin:15vh auto;max-width:28em;text-align:center\"><h2>{title}</h2><p>{body}</p>");
        let _ = (&stream).write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}", page.len()).as_bytes());
        return got;
    }
}

pub(crate) fn percent(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

pub(crate) fn unpercent(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                out.push(u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("00"), 16).unwrap_or(b'?'));
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A hosted provider was disconnected: its account and any old error go with its key.
pub fn forget(id: &str) {
    let mut c = cache().lock().unwrap();
    if let Some((_, p)) = c.providers.get_mut(id) {
        p.account = None;
        p.error = None;
        p.connected = false;
    }
}

/// Say why `id` isn't usable (or stop saying it).
pub(crate) fn set_error(id: &str, error: Option<String>) {
    let mut c = cache().lock().unwrap();
    if let Some((_, p)) = c.providers.get_mut(id) {
        p.error = error;
    }
}

fn fetch_models(id: &str) {
    let base = find(id).map(|p| p.base).unwrap_or_default();
    let got: Result<Vec<ProviderModel>, String> = match id {
        "openrouter" => get(&format!("{OPENROUTER}/v1/models")).map(|v| providers::openrouter_models(&v)),
        // The account's own list; Codex's catalog for the same account when that's empty or out of reach.
        "chatgpt" => {
            let access = dino_core::load_keys().remove(crate::chatgpt::ACCESS_KEY).unwrap_or_default();
            let api = http().get(format!("{}/models", crate::chatgpt::API)).bearer_auth(access).send().ok().filter(|r| r.status().is_success()).and_then(|r| r.json::<Value>().ok()).unwrap_or(Value::Null);
            let models = providers::chatgpt_models(&api, dino_core::models::codex_cache().as_deref());
            if models.is_empty() { Err("ChatGPT listed no models, and Codex has no list for this account".into()) } else { Ok(models) }
        }
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
        id if id.starts_with(plans::PREFIX) => match plan_models(id) {
            Ok((models, note)) => {
                let mut c = cache().lock().unwrap();
                c.busy.remove(id);
                c.models.insert(id.to_string(), Fetched { at: Instant::now(), models, error: note });
                return;
            }
            Err(e) => Err(e),
        },
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    /// A provider's age starts again each time it's asked, even when it says the same; what's
    /// only kept keeps its age, so it's asked again once that's up.
    #[test]
    fn age_starts_again_when_asked() {
        let p = ProviderInfo { id: "test-age".into(), ..Default::default() };
        let at = || cache().lock().unwrap().providers["test-age"].0;
        store(p.clone(), true);
        let first = at();
        std::thread::sleep(Duration::from_millis(5));
        store(p.clone(), false);
        assert_eq!(at(), first, "kept, not asked");
        store(p, true);
        assert!(at() > first, "asked again, the same answer");
        cache().lock().unwrap().providers.remove("test-age");
    }

    /// OpenRouter's key exchange, as a local stand-in: answers once and says what it was sent.
    fn exchange_server() -> (String, std::sync::mpsc::Receiver<Value>) {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}/api/v1/auth/keys", l.local_addr().unwrap().port());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut buf = vec![0; 8192];
            let n = s.read(&mut buf).unwrap();
            let text = String::from_utf8_lossy(&buf[..n]).to_string();
            let body = text.split("\r\n\r\n").nth(1).unwrap_or("");
            tx.send(serde_json::from_str::<Value>(body).unwrap_or(Value::Null)).unwrap();
            let reply = r#"{"key":"sk-or-v1-test","user_id":"u"}"#;
            write!(s, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{reply}", reply.len()).unwrap();
        });
        (url, rx)
    }

    #[test]
    fn connecting_openrouter_trades_the_code_for_a_key() {
        use base64::Engine;
        use sha2::Digest;
        let (exchange, sent) = exchange_server();
        let (tx, saved) = std::sync::mpsc::channel();
        let url = connect_with("https://openrouter.ai/auth", &exchange, move |k| {
            tx.send(k).unwrap();
            Ok(())
        })
        .unwrap();
        assert!(url.starts_with("https://openrouter.ai/auth?callback_url=http%3A%2F%2F127.0.0.1%3A"), "{url}");
        assert!(url.contains("code_challenge_method=S256"));
        let q = |name: &str| url.split(['?', '&']).find_map(|kv| kv.strip_prefix(&format!("{name}="))).map(unpercent).unwrap();
        let callback = q("callback_url");
        let challenge = q("code_challenge");

        // A stray request first, then the browser coming back.
        let _ = http().get(format!("{}/../favicon.ico", callback.trim_end_matches("/callback"))).send();
        let page = http().get(format!("{callback}?code=abc%20123")).send().unwrap().text().unwrap();
        assert!(page.contains("connected to OpenRouter"), "{page}");

        let body = sent.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(body["code"], "abc 123");
        assert_eq!(body["code_challenge_method"], "S256");
        let verifier = body["code_verifier"].as_str().unwrap();
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        assert_eq!(b64.encode(sha2::Sha256::digest(verifier.as_bytes())), challenge, "the verifier matches the challenge");
        assert_eq!(saved.recv_timeout(Duration::from_secs(5)).unwrap(), "sk-or-v1-test");
    }

    #[test]
    fn a_refused_sign_in_saves_nothing() {
        let (tx, saved) = std::sync::mpsc::channel::<String>();
        let url = connect_with("https://openrouter.ai/auth", "http://127.0.0.1:9/unused", move |k| {
            tx.send(k).unwrap();
            Ok(())
        })
        .unwrap();
        let callback = url.split(['?', '&']).find_map(|kv| kv.strip_prefix("callback_url=")).map(unpercent).unwrap();
        let page = http().get(format!("{callback}?error=access_denied")).send().unwrap().text().unwrap();
        assert!(page.contains("wasn't connected"));
        assert!(saved.recv_timeout(Duration::from_millis(500)).is_err());
    }
}
