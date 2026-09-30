//! Where models come from, as dinod last looked: OpenRouter and model servers on this Mac. Asked
//! in the background and kept; an IPC request only ever reads what's kept, and at most starts a
//! fetch. Everything about a model is what its provider says (`dino_core::providers`).

use std::collections::{HashMap, HashSet};
use std::net::{SocketAddr, TcpStream};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use dino_core::compat::Compat;
use dino_core::ipc::ModelRow;
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
    let mut out: Vec<ProviderInfo> = ["openrouter", "chatgpt"].into_iter().chain(LOCAL.iter().map(|l| l.0)).filter_map(|id| c.providers.get(id).map(|(_, p)| p.clone())).collect();
    if out.is_empty() {
        out = [openrouter_bare(), chatgpt_bare()].into_iter().chain(LOCAL.iter().map(|(id, name, addr)| local_bare(id, name, addr))).collect();
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
    let every = if matches!(id, "openrouter" | "chatgpt") { HOSTED_EVERY } else { LOCAL_EVERY };
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
            // An error from connecting stays until the next try.
            or.error = cache().lock().unwrap().providers.get("openrouter").and_then(|(_, p)| p.error.clone());
            None
        }
    };
    store(or);

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
    store(chatgpt);

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
