//! Sign in with ChatGPT: OpenAI's sign-in for open-source apps that run on the person's own Mac,
//! which lets them spend their ChatGPT plan in dino (up to the weekly cap they set for dino in
//! ChatGPT → Settings → Usage). The loopback PKCE flow as OpenAI's partners run it; the tokens
//! it gives go only to dino's key store, and the proxy's `siwc` route adds them to requests.
//! Nothing here is printed or logged.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::providers::{http, percent, set_error, unpercent};

pub const REFRESH_KEY: &str = "CHATGPT_REFRESH_TOKEN";
pub const ACCESS_KEY: &str = "CHATGPT_ACCESS_TOKEN";
/// The app id OpenAI issues dino on this Mac, which the tokens belong to.
const CLIENT_KEY: &str = "CHATGPT_CLIENT_ID";
/// Unix seconds the access token stops working.
const EXPIRES_KEY: &str = "CHATGPT_EXPIRES_AT";
/// "granted" when the person allowed dino to use their plan, which is a separate consent from
/// signing in (`dino_proxy::siwc` reads it).
const PLAN_KEY: &str = "CHATGPT_PLAN_USAGE";
/// This Mac, the same on every sign-in (OpenAI shows each one as its own app host).
const HOST_KEY: &str = "CHATGPT_HOST_ID";

/// The scope that lets requests draw on the ChatGPT plan.
const DIRECT: &str = "chatgpt.tokens.use.direct";
const SCOPE: &str = "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
const RESOURCE: &str = "https://api.openai.com/v1";
/// Where the plan is spent: the Responses API only.
pub const API: &str = "https://api.openai.com/v1";

/// OpenAI's side of the sign-in; a test points these at stand-ins.
pub struct Endpoints {
    pub authorize: String,
    pub token: String,
    /// The loopback port OpenAI sends the browser back to.
    pub port: u16,
    /// This Mac's id, when not the one kept in dino's key store.
    pub host: Option<String>,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            authorize: "https://auth.openai.com/api/accounts/authorize".into(),
            token: "https://auth.openai.com/api/accounts/oauth/token".into(),
            port: 1455,
            host: None,
        }
    }
}

/// What a sign-in or a refresh gave.
#[derive(Debug, Clone, PartialEq)]
pub struct Tokens {
    pub access: String,
    pub refresh: String,
    pub client_id: String,
    pub expires_at: u64,
    /// The person allowed dino to use their plan.
    pub plan: bool,
}

impl Tokens {
    /// As `(key, value)` pairs for dino's key store.
    pub fn keys(&self) -> [(&'static str, String); 5] {
        [
            (ACCESS_KEY, self.access.clone()),
            (REFRESH_KEY, self.refresh.clone()),
            (CLIENT_KEY, self.client_id.clone()),
            (EXPIRES_KEY, self.expires_at.to_string()),
            (PLAN_KEY, if self.plan { "granted" } else { "not granted" }.into()),
        ]
    }
}

/// The key-store entries a sign-out removes. The app id and this Mac's id stay: a later sign-in
/// is the same app on the same Mac.
pub const SIGNED_IN: [&str; 4] = [ACCESS_KEY, REFRESH_KEY, EXPIRES_KEY, PLAN_KEY];

/// Signed in, and whether plan usage was allowed, as the key store says.
pub fn status(keys: &std::collections::HashMap<String, String>) -> Option<bool> {
    keys.get(REFRESH_KEY).filter(|r| !r.is_empty()).map(|_| keys.get(PLAN_KEY).is_some_and(|p| p == "granted"))
}

/// This Mac's id for OpenAI, made once and kept.
fn host_id() -> String {
    let keys = dino_core::load_keys();
    let id = keys.get(HOST_KEY).cloned().unwrap_or_else(|| {
        let mut b = [0u8; 16];
        let _ = std::io::Read::read_exact(&mut std::fs::File::open("/dev/urandom").expect("urandom"), &mut b);
        b[6] = (b[6] & 0x0f) | 0x40;
        b[8] = (b[8] & 0x3f) | 0x80;
        let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
        let id = format!("{}-{}-{}-{}-{}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..]);
        let _ = dino_core::settings::set_key(HOST_KEY, Some(&id));
        id
    });
    format!("urn:uuid:{id}")
}

fn random() -> String {
    use base64::Engine;
    let mut bytes = [0u8; 32];
    let _ = std::io::Read::read_exact(&mut std::fs::File::open("/dev/urandom").expect("urandom"), &mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Start signing in: returns the page to open. When the browser comes back, the code is traded
/// for tokens and `save` stores them; a failure is shown on the provider's row.
pub fn connect(e: Endpoints, save: impl FnOnce(Tokens) -> anyhow::Result<()> + Send + 'static) -> anyhow::Result<String> {
    use base64::Engine;
    use sha2::Digest;
    let listener = std::net::TcpListener::bind(("127.0.0.1", e.port)).map_err(|err| anyhow::anyhow!("port {} is busy ({err}); is another app signing in to ChatGPT?", e.port))?;
    // Port 0 (tests) takes any free port, so parallel sign-ins can't race for one.
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(e.port);
    let redirect = format!("http://127.0.0.1:{port}/auth/callback");
    let verifier = random();
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(verifier.as_bytes()));
    let state = random();
    let query = [
        ("client_id", "dynamic_agent_client".to_string()),
        ("agent_name_hint", "dino".into()),
        ("ext_agent_host_id", e.host.clone().unwrap_or_else(host_id)),
        ("response_type", "code".into()),
        ("redirect_uri", redirect.clone()),
        ("resource", RESOURCE.into()),
        ("scope", SCOPE.into()),
        ("state", state.clone()),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256".into()),
        ("nonce", random()),
    ];
    let url = format!("{}?{}", e.authorize, form(&query));
    set_error("chatgpt", None);
    std::thread::spawn(move || {
        let result = (|| -> anyhow::Result<()> {
            let (code, client_id) = wait_for_code(&listener, &state)?;
            let v = post(
                &e.token,
                &[
                    ("grant_type", "authorization_code".into()),
                    ("client_id", client_id.clone()),
                    ("code", code),
                    ("code_verifier", verifier),
                    ("redirect_uri", redirect),
                    ("resource", RESOURCE.into()),
                ],
            )?;
            save(tokens(&v, &client_id, None)?)
        })();
        if let Err(err) = result {
            set_error("chatgpt", Some(format!("Signing in didn't finish: {err}")));
        }
        crate::providers::refresh(true);
    });
    Ok(url)
}

/// A token reply as `Tokens`; a refresh that doesn't hand out a new refresh token keeps `old`'s.
fn tokens(v: &Value, client_id: &str, old: Option<&str>) -> anyhow::Result<Tokens> {
    let access = v["access_token"].as_str().filter(|t| !t.is_empty()).ok_or_else(|| anyhow::anyhow!("OpenAI gave no token"))?;
    let refresh = v["refresh_token"].as_str().filter(|t| !t.is_empty()).or(old).ok_or_else(|| anyhow::anyhow!("OpenAI gave no refresh token"))?;
    let scopes = v["scope"].as_str().unwrap_or("");
    Ok(Tokens {
        access: access.into(),
        refresh: refresh.into(),
        client_id: client_id.into(),
        expires_at: now() + v["expires_in"].as_u64().unwrap_or(3600),
        plan: scopes.split_whitespace().any(|s| s == DIRECT),
    })
}

/// A new access token before the old one runs out, when one is due: `Ok(Some)` to store,
/// `Ok(None)` when nothing was due, `Err` when OpenAI refused (the person signs in again).
pub fn refresh_if_due(token_url: &str, keys: &std::collections::HashMap<String, String>) -> anyhow::Result<Option<Tokens>> {
    let (Some(refresh), Some(client_id)) = (keys.get(REFRESH_KEY), keys.get(CLIENT_KEY)) else { return Ok(None) };
    let expires = keys.get(EXPIRES_KEY).and_then(|e| e.parse::<u64>().ok()).unwrap_or(0);
    if expires > now() + 5 * 60 {
        return Ok(None);
    }
    let v = post(
        token_url,
        &[("grant_type", "refresh_token".into()), ("client_id", client_id.clone()), ("refresh_token", refresh.clone()), ("resource", RESOURCE.into())],
    )?;
    let mut t = tokens(&v, client_id, Some(refresh))?;
    // A refresh reply may leave the scope out: plan usage stays as it was granted.
    if v["scope"].as_str().is_none() {
        t.plan = keys.get(PLAN_KEY).is_some_and(|p| p == "granted");
    }
    Ok(Some(t))
}

/// OpenAI turned the request down (not a network hiccup): the sign-in is over.
#[derive(Debug)]
pub struct Refused(pub String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refused {}

fn form(fields: &[(&str, String)]) -> String {
    fields.iter().map(|(k, v)| format!("{k}={}", percent(v))).collect::<Vec<_>>().join("&")
}

/// A form POST to OpenAI's token endpoint; its error says why, never what was sent.
fn post(url: &str, fields: &[(&str, String)]) -> anyhow::Result<Value> {
    let r = http().post(url).header("content-type", "application/x-www-form-urlencoded").header("accept", "application/json").timeout(Duration::from_secs(30)).body(form(fields)).send()?;
    let status = r.status();
    let v: Value = r.json().unwrap_or(Value::Null);
    if !status.is_success() {
        let why = v["error_description"].as_str().or(v["error"]["message"].as_str()).or(v["error"].as_str()).unwrap_or("no reason given");
        let said = format!("OpenAI said {} ({why})", status.as_u16());
        if status.is_client_error() {
            return Err(Refused(said).into());
        }
        anyhow::bail!(said);
    }
    Ok(v)
}

/// How long the sign-in page may take.
const WAIT: Duration = Duration::from_secs(10 * 60);

/// The code and the app id the browser brings back, for this sign-in (`state`) only.
fn wait_for_code(listener: &std::net::TcpListener, state: &str) -> anyhow::Result<(String, String)> {
    use std::io::{BufRead, Write};
    listener.set_nonblocking(true)?;
    let until = Instant::now() + WAIT;
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
        let target = line.split_whitespace().nth(1).unwrap_or("");
        let reply = |code: &str, title: &str, body: &str| {
            let page = format!("<!doctype html><meta charset=utf-8><title>{title}</title><body style=\"font:15px -apple-system,sans-serif;margin:15vh auto;max-width:28em;text-align:center\"><h2>{title}</h2><p>{body}</p>");
            let _ = (&stream).write_all(format!("HTTP/1.1 {code}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}", page.len()).as_bytes());
        };
        let Some(query) = target.strip_prefix("/auth/callback?") else {
            reply("404 Not Found", "Not here", "");
            continue;
        };
        let param = |name: &str| query.split('&').find_map(|kv| kv.strip_prefix(&format!("{name}="))).map(unpercent).filter(|v| !v.is_empty());
        if let Some(err) = param("error") {
            reply("200 OK", "dino isn't signed in to ChatGPT", "Nothing was changed. You can close this tab.");
            anyhow::bail!("OpenAI said {err}");
        }
        // Another sign-in's, or a forged one: not this.
        if param("state").as_deref() != Some(state) {
            reply("400 Bad Request", "Not this sign-in", "Start signing in again from dino.");
            continue;
        }
        let (Some(code), Some(client_id)) = (param("code"), param("client_id")) else {
            reply("200 OK", "dino isn't signed in to ChatGPT", "OpenAI didn't say who dino is. You can close this tab.");
            anyhow::bail!("OpenAI's answer had no code or app id");
        };
        reply("200 OK", "dino is signed in to ChatGPT", "You can close this tab and go back to dino.");
        return Ok((code, client_id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    /// OpenAI's token endpoint as a stand-in: answers each request with the next reply and says
    /// what it was sent.
    fn token_server(replies: Vec<(u16, &'static str)>) -> (String, std::sync::mpsc::Receiver<String>) {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}/oauth/token", l.local_addr().unwrap().port());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for (status, reply) in replies {
                let (mut s, _) = l.accept().unwrap();
                let mut buf = vec![0; 8192];
                let n = s.read(&mut buf).unwrap();
                let text = String::from_utf8_lossy(&buf[..n]).to_string();
                tx.send(text.split("\r\n\r\n").nth(1).unwrap_or("").to_string()).unwrap();
                write!(s, "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{reply}", reply.len()).unwrap();
            }
        });
        (url, rx)
    }

    fn fields(body: &str) -> std::collections::HashMap<String, String> {
        body.split('&').filter_map(|kv| kv.split_once('=')).map(|(k, v)| (k.to_string(), unpercent(v))).collect()
    }

    fn free_port() -> u16 {
        0
    }

    /// The port a sign-in page sends the browser back to, from its URL.
    fn redirect_port(url: &str) -> u16 {
        let r = url.split(['?', '&']).find_map(|kv| kv.strip_prefix("redirect_uri=")).map(unpercent).unwrap();
        r.trim_start_matches("http://127.0.0.1:").split('/').next().unwrap().parse().unwrap()
    }

    #[test]
    fn signing_in_trades_the_code_for_tokens() {
        use base64::Engine;
        use sha2::Digest;
        let (token, sent) = token_server(vec![(200, r#"{"access_token":"at-1","refresh_token":"rt-1","expires_in":3600,"scope":"openid offline_access resource.invoke chatgpt.tokens.use.direct"}"#)]);
        let port = free_port();
        let (tx, saved) = std::sync::mpsc::channel();
        let url = connect(Endpoints { authorize: "https://auth.example/authorize".into(), token, port, host: Some("urn:uuid:00000000-0000-4000-8000-000000000000".into()) }, move |t| {
            tx.send(t).unwrap();
            Ok(())
        })
        .unwrap();
        let port = redirect_port(&url);
        let q = |name: &str| url.split(['?', '&']).find_map(|kv| kv.strip_prefix(&format!("{name}="))).map(unpercent).unwrap();
        assert_eq!(q("client_id"), "dynamic_agent_client");
        assert_eq!(q("agent_name_hint"), "dino");
        assert!(q("ext_agent_host_id").starts_with("urn:uuid:"));
        assert_eq!(q("redirect_uri"), format!("http://127.0.0.1:{port}/auth/callback"));
        assert!(q("scope").split(' ').any(|s| s == DIRECT));
        assert_eq!(q("code_challenge_method"), "S256");
        let state = q("state");
        let back = format!("http://127.0.0.1:{port}/auth/callback");

        // Someone else's callback first: ignored.
        let r = http().get(format!("{back}?code=x&state=forged&client_id=oaiapp_x")).send().unwrap();
        assert_eq!(r.status().as_u16(), 400);
        let page = http().get(format!("{back}?code=c%2F1&state={}&client_id=oaiapp_dino", percent(&state))).send().unwrap().text().unwrap();
        assert!(page.contains("signed in to ChatGPT"), "{page}");

        let f = fields(&sent.recv_timeout(Duration::from_secs(5)).unwrap());
        assert_eq!(f["grant_type"], "authorization_code");
        assert_eq!(f["client_id"], "oaiapp_dino", "the issued app id, not the dynamic one");
        assert_eq!(f["code"], "c/1");
        assert_eq!(f["resource"], RESOURCE);
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        assert_eq!(b64.encode(sha2::Sha256::digest(f["code_verifier"].as_bytes())), q("code_challenge"));
        let t = saved.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!((t.access.as_str(), t.refresh.as_str(), t.client_id.as_str(), t.plan), ("at-1", "rt-1", "oaiapp_dino", true));
        assert!(t.expires_at > now() + 3000);
    }

    #[test]
    fn signed_in_without_plan_usage_says_so() {
        let v = serde_json::json!({"access_token": "a", "refresh_token": "r", "scope": "openid profile email offline_access"});
        let t = tokens(&v, "oaiapp_x", None).unwrap();
        assert!(!t.plan);
        let keys: std::collections::HashMap<String, String> = t.keys().into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        assert_eq!(status(&keys), Some(false));
        assert_eq!(status(&Default::default()), None);
    }

    #[test]
    fn a_refused_sign_in_saves_nothing() {
        let port = free_port();
        let (tx, saved) = std::sync::mpsc::channel::<Tokens>();
        let url = connect(Endpoints { authorize: "https://auth.example/authorize".into(), token: "http://127.0.0.1:9/unused".into(), port, host: Some("urn:uuid:00000000-0000-4000-8000-000000000000".into()) }, move |t| {
            tx.send(t).unwrap();
            Ok(())
        })
        .unwrap();
        let port = redirect_port(&url);
        let page = http().get(format!("http://127.0.0.1:{port}/auth/callback?error=access_denied")).send().unwrap().text().unwrap();
        assert!(page.contains("isn't signed in"));
        assert!(saved.recv_timeout(Duration::from_millis(500)).is_err());
    }

    #[test]
    fn refreshing_rotates_the_tokens() {
        let (token, sent) = token_server(vec![
            (200, r#"{"access_token":"at-2","refresh_token":"rt-2","expires_in":3600}"#),
            (200, r#"{"access_token":"at-3","expires_in":3600}"#),
            (400, r#"{"error":"invalid_grant","error_description":"refresh token was already used"}"#),
        ]);
        let mut keys: std::collections::HashMap<String, String> = [(REFRESH_KEY, "rt-1"), (CLIENT_KEY, "oaiapp_dino"), (PLAN_KEY, "granted"), (EXPIRES_KEY, "0")].into_iter().map(|(k, v)| (k.into(), v.into())).collect();

        let t = refresh_if_due(&token, &keys).unwrap().unwrap();
        let f = fields(&sent.recv().unwrap());
        assert_eq!((f["grant_type"].as_str(), f["refresh_token"].as_str(), f["client_id"].as_str()), ("refresh_token", "rt-1", "oaiapp_dino"));
        assert_eq!((t.access.as_str(), t.refresh.as_str(), t.plan), ("at-2", "rt-2", true), "a new refresh token replaces the old; plan usage stays");

        // Not due: nothing asked.
        keys.extend(t.keys().into_iter().map(|(k, v)| (k.to_string(), v)));
        assert_eq!(refresh_if_due(&token, &keys).unwrap(), None);

        // Due, and no new refresh token given: the old one stays.
        keys.insert(EXPIRES_KEY.into(), "0".into());
        let t = refresh_if_due(&token, &keys).unwrap().unwrap();
        assert_eq!((t.access.as_str(), t.refresh.as_str()), ("at-3", "rt-2"));

        keys.insert(REFRESH_KEY.into(), "rt-used".into());
        let e = refresh_if_due(&token, &keys).unwrap_err().to_string();
        assert!(e.contains("400") && e.contains("already used") && !e.contains("rt-used"), "{e}");
    }
}
