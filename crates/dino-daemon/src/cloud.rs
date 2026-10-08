//! The dino account: signing in to dino-cloud and talking to it as this Mac. The server only keeps
//! the account, the devices signed in to it and their synced settings (see `sync`); agent
//! traffic never goes there. Its tokens go to dino's key store and are never printed or logged.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::providers::{percent, unpercent};

/// dino's own account server; `DINO_CLOUD_URL` or `dino login <url>` choose another (self-hosted).
pub const DEFAULT_SERVER: &str = "https://cloud.meetdino.com";
/// dino's app and CLI, as the server knows them.
const CLIENT: &str = "dino";
const SCOPE: &str = "account sync";

// The sign-in, in the key store. They start with `dino_core::settings::ACCOUNT_TOKEN_PREFIX`, so
// the lists of keys (Settings, Welcome) leave them out.
pub const ACCESS_KEY: &str = "DINO_CLOUD_ACCESS_TOKEN";
pub const REFRESH_KEY: &str = "DINO_CLOUD_REFRESH_TOKEN";
const EXPIRES_KEY: &str = "DINO_CLOUD_EXPIRES_AT";

/// The server to use when none was chosen at sign-in.
pub fn default_server() -> String {
    std::env::var("DINO_CLOUD_URL").ok().filter(|u| !u.is_empty()).unwrap_or_else(|| DEFAULT_SERVER.into())
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn random() -> String {
    use base64::Engine;
    let mut bytes = [0u8; 32];
    let _ = std::io::Read::read_exact(&mut std::fs::File::open("/dev/urandom").expect("urandom"), &mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn form(fields: &[(&str, String)]) -> String {
    fields.iter().map(|(k, v)| format!("{k}={}", percent(v))).collect::<Vec<_>>().join("&")
}

/// A client with no timeouts of its own beyond the request's.
fn http() -> &'static reqwest::blocking::Client {
    crate::providers::http()
}

/// The server said no to the sign-in itself (not a network hiccup): this Mac is signed out.
#[derive(Debug)]
pub struct SignedOut(pub String);

impl std::fmt::Display for SignedOut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SignedOut {}

/// What the token endpoint gave.
struct Tokens {
    access: String,
    refresh: String,
    expires_at: u64,
}

fn save_tokens(t: &Tokens) -> anyhow::Result<()> {
    dino_core::settings::set_key(ACCESS_KEY, Some(&t.access))?;
    dino_core::settings::set_key(REFRESH_KEY, Some(&t.refresh))?;
    dino_core::settings::set_key(EXPIRES_KEY, Some(&t.expires_at.to_string()))
}

/// Forget this Mac's sign-in.
pub fn forget_tokens() {
    for k in [ACCESS_KEY, REFRESH_KEY, EXPIRES_KEY] {
        let _ = dino_core::settings::set_key(k, None);
    }
}

pub fn signed_in() -> bool {
    stored(REFRESH_KEY).is_some()
}

fn stored(name: &str) -> Option<String> {
    dino_core::settings::stored().into_iter().find(|(k, _)| k == name).map(|(_, v)| v).filter(|v| !v.is_empty())
}

fn token_post(server: &str, fields: &[(&str, String)]) -> anyhow::Result<Value> {
    let r = http().post(format!("{server}/oauth/token")).header("content-type", "application/x-www-form-urlencoded").header("accept", "application/json").timeout(Duration::from_secs(20)).body(form(fields)).send()?;
    let status = r.status();
    let v: Value = r.json().unwrap_or(Value::Null);
    if !status.is_success() {
        let code = v["error"].as_str().unwrap_or("");
        let why = v["error_description"].as_str().unwrap_or(code);
        // Too many requests or a timeout is the server being busy, not a refusal.
        let busy = matches!(status.as_u16(), 408 | 429);
        if status.is_client_error() && !busy && !matches!(code, "authorization_pending" | "slow_down") {
            return Err(SignedOut(format!("the account server said {} ({why})", status.as_u16())).into());
        }
        anyhow::bail!("{code}");
    }
    Ok(v)
}

fn tokens(v: &Value) -> anyhow::Result<Tokens> {
    let access = v["access_token"].as_str().filter(|t| !t.is_empty()).ok_or_else(|| anyhow::anyhow!("the server gave no token"))?;
    let refresh = v["refresh_token"].as_str().filter(|t| !t.is_empty()).ok_or_else(|| anyhow::anyhow!("the server gave no refresh token"))?;
    Ok(Tokens { access: access.into(), refresh: refresh.into(), expires_at: now() + v["expires_in"].as_u64().unwrap_or(900) })
}

/// What the server shows for this Mac on its devices page.
fn device_fields() -> Vec<(&'static str, String)> {
    #[cfg(target_os = "macos")]
    let name = std::process::Command::new("scutil").args(["--get", "ComputerName"]).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).filter(|n| !n.is_empty()).unwrap_or_else(|| "Mac".into());
    #[cfg(target_os = "macos")]
    let os = std::process::Command::new("sw_vers").arg("-productVersion").output().ok().map(|o| format!("macOS {}", String::from_utf8_lossy(&o.stdout).trim())).unwrap_or_else(|| "macOS".into());
    // Linux: the host name, and the distribution as os-release names it.
    #[cfg(not(target_os = "macos"))]
    let name = std::fs::read_to_string("/proc/sys/kernel/hostname").ok().map(|n| n.trim().to_string()).filter(|n| !n.is_empty()).unwrap_or_else(|| "Linux".into());
    #[cfg(not(target_os = "macos"))]
    let os = ["/etc/os-release", "/usr/lib/os-release"]
        .iter()
        .find_map(|p| std::fs::read_to_string(p).ok()?.lines().find_map(|l| Some(l.strip_prefix("PRETTY_NAME=")?.trim_matches('"').to_string())))
        .unwrap_or_else(|| "Linux".into());
    vec![("device_name", std::env::var("DINO_DEVICE_NAME").unwrap_or(name)), ("device_os", os), ("dino_version", env!("CARGO_PKG_VERSION").into())]
}

/// How long the sign-in page may take.
const WAIT: Duration = Duration::from_secs(10 * 60);

/// Start signing in at `server` in the browser (code + PKCE, back to a loopback port): returns the
/// page to open. With a `provider` (`github`), the page goes straight to that provider's sign-in.
/// `done` runs once the tokens are stored, or with why it didn't finish.
pub fn login(server: String, provider: Option<&str>, done: impl FnOnce(anyhow::Result<()>) + Send + 'static) -> anyhow::Result<String> {
    use base64::Engine;
    use sha2::Digest;
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    let redirect = format!("http://127.0.0.1:{}/callback", listener.local_addr()?.port());
    let verifier = random();
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(verifier.as_bytes()));
    let state = random();
    let mut query = vec![
        ("response_type", "code".to_string()),
        ("client_id", CLIENT.into()),
        ("redirect_uri", redirect.clone()),
        ("scope", SCOPE.into()),
        ("state", state.clone()),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256".into()),
    ];
    if let Some(p) = provider {
        query.push(("provider", p.into()));
    }
    query.extend(device_fields());
    let url = format!("{server}/oauth/authorize?{}", form(&query));
    std::thread::spawn(move || {
        let result = (|| -> anyhow::Result<()> {
            let code = wait_for_code(&listener, &state, &server)?;
            let v = token_post(&server, &[("grant_type", "authorization_code".into()), ("client_id", CLIENT.into()), ("code", code), ("redirect_uri", redirect), ("code_verifier", verifier)])?;
            save_tokens(&tokens(&v)?)
        })();
        done(result);
    });
    Ok(url)
}

/// A code to enter at a page, for a Mac without a browser at hand (RFC 8628). `done` runs once
/// the person approved (tokens stored) or it ran out.
pub fn login_device(server: String, done: impl FnOnce(anyhow::Result<()>) + Send + 'static) -> anyhow::Result<(String, String)> {
    device_grant(server, None, done)
}

/// A sign-in link by email: the server mails one to `email`, and opening it approves this Mac's
/// request the way entering the code would. `done` runs once it was opened or ran out.
pub fn login_email(server: String, email: &str, done: impl FnOnce(anyhow::Result<()>) + Send + 'static) -> anyhow::Result<()> {
    device_grant(server, Some(email), done).map(|_| ())
}

fn device_grant(server: String, email: Option<&str>, done: impl FnOnce(anyhow::Result<()>) + Send + 'static) -> anyhow::Result<(String, String)> {
    let mut fields = vec![("client_id", CLIENT.to_string()), ("scope", SCOPE.into())];
    if let Some(e) = email {
        fields.push(("email", e.into()));
    }
    fields.extend(device_fields());
    let r = http().post(format!("{server}/oauth/device_authorization")).header("content-type", "application/x-www-form-urlencoded").body(form(&fields)).send()?;
    if r.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
        anyhow::bail!("too many sign-in emails; try again in a while");
    }
    anyhow::ensure!(r.status().is_success(), "the account server said {}", r.status().as_u16());
    let v: Value = r.json()?;
    let device_code = v["device_code"].as_str().ok_or_else(|| anyhow::anyhow!("no device code"))?.to_string();
    let user_code = v["user_code"].as_str().unwrap_or("").to_string();
    let page = v["verification_uri_complete"].as_str().or(v["verification_uri"].as_str()).unwrap_or("").to_string();
    let mut interval = v["interval"].as_u64().unwrap_or(5).max(1);
    let until = Instant::now() + Duration::from_secs(v["expires_in"].as_u64().unwrap_or(600));
    let by_email = email.is_some();
    std::thread::spawn(move || {
        let result = (|| -> anyhow::Result<()> {
            while Instant::now() < until {
                std::thread::sleep(Duration::from_secs(interval));
                match token_post(&server, &[("grant_type", "urn:ietf:params:oauth:grant-type:device_code".into()), ("client_id", CLIENT.into()), ("device_code", device_code.clone())]) {
                    Ok(v) => return save_tokens(&tokens(&v)?),
                    Err(e) if e.to_string() == "slow_down" => interval += 5,
                    Err(e) if e.to_string() == "authorization_pending" => {}
                    Err(e) if e.is::<SignedOut>() => return Err(e),
                    Err(_) => {}
                }
            }
            anyhow::bail!("{}", if by_email { "the sign-in link wasn't opened in time" } else { "the code wasn't entered in time" })
        })();
        done(result);
    });
    Ok((user_code, page))
}

/// The code the browser brings back, for this sign-in (`state`) only.
fn wait_for_code(listener: &std::net::TcpListener, state: &str, server: &str) -> anyhow::Result<String> {
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
        let Some(query) = target.strip_prefix("/callback?") else {
            reply("404 Not Found", "Not here", "");
            continue;
        };
        let param = |name: &str| query.split('&').find_map(|kv| kv.strip_prefix(&format!("{name}="))).map(unpercent).filter(|v| !v.is_empty());
        if let Some(err) = param("error") {
            reply("200 OK", "dino isn't signed in", "Nothing was changed. You can close this tab.");
            anyhow::bail!("the account server said {err}");
        }
        if param("state").as_deref() != Some(state) {
            reply("400 Bad Request", "Not this sign-in", "Start signing in again from dino.");
            continue;
        }
        // RFC 9207: the answer names the server it came from.
        if param("iss").is_some_and(|iss| iss.trim_end_matches('/') != server.trim_end_matches('/')) {
            reply("400 Bad Request", "Not this sign-in", "That answer came from another server.");
            continue;
        }
        let Some(code) = param("code") else {
            reply("200 OK", "dino isn't signed in", "The server sent no code. You can close this tab.");
            anyhow::bail!("the account server's answer had no code");
        };
        reply("200 OK", "dino is signed in", "You can close this tab and go back to dino.");
        return Ok(code);
    }
}

/// A current access token, refreshed when it's about to run out. `SignedOut` when the server
/// refused the refresh: this Mac was signed out (or the account deleted) elsewhere.
pub fn access(server: &str) -> anyhow::Result<String> {
    let refresh = stored(REFRESH_KEY).ok_or_else(|| SignedOut("not signed in".into()))?;
    let expires = stored(EXPIRES_KEY).and_then(|e| e.parse::<u64>().ok()).unwrap_or(0);
    if let Some(at) = stored(ACCESS_KEY).filter(|_| expires > now() + 60) {
        return Ok(at);
    }
    renew(server, &refresh)
}

fn renew(server: &str, refresh: &str) -> anyhow::Result<String> {
    let v = token_post(server, &[("grant_type", "refresh_token".into()), ("client_id", CLIENT.into()), ("refresh_token", refresh.into())])?;
    let t = tokens(&v)?;
    save_tokens(&t)?;
    Ok(t.access)
}

/// A request to the server as this Mac, refreshing once when the access token was refused.
pub fn call(server: &str, build: impl Fn(&str) -> reqwest::blocking::RequestBuilder) -> anyhow::Result<reqwest::blocking::Response> {
    let at = access(server)?;
    let r = build(&at).header("dino-sync-version", dino_sync::record::PROTOCOL.to_string()).send()?;
    if r.status() != reqwest::StatusCode::UNAUTHORIZED {
        return Ok(r);
    }
    let refresh = stored(REFRESH_KEY).ok_or_else(|| SignedOut("not signed in".into()))?;
    let at = renew(server, &refresh)?;
    let r = build(&at).header("dino-sync-version", dino_sync::record::PROTOCOL.to_string()).send()?;
    if r.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(SignedOut("your dino account no longer recognizes this Mac".into()).into());
    }
    Ok(r)
}

/// JSON from a GET, or None on 404.
pub fn get(server: &str, path: &str) -> anyhow::Result<Option<Value>> {
    let r = call(server, |at| http().get(format!("{server}{path}")).bearer_auth(at))?;
    if r.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    anyhow::ensure!(r.status().is_success(), "the account server said {}", r.status().as_u16());
    Ok(Some(r.json()?))
}

pub fn send(server: &str, method: reqwest::Method, path: &str, body: &Value) -> anyhow::Result<Value> {
    let r = call(server, |at| http().request(method.clone(), format!("{server}{path}")).bearer_auth(at).json(body))?;
    let status = r.status();
    let v: Value = r.json().unwrap_or(Value::Null);
    anyhow::ensure!(status.is_success(), "the account server said {}{}", status.as_u16(), v["reason"].as_str().or(v["error"].as_str()).map(|r| format!(" ({r})")).unwrap_or_default());
    Ok(v)
}

/// Sign this Mac out at the server (its tokens stop working), then here.
pub fn logout(server: &str) {
    if let Some(rt) = stored(REFRESH_KEY) {
        let _ = http().post(format!("{server}/oauth/revoke")).header("content-type", "application/x-www-form-urlencoded").timeout(Duration::from_secs(5)).body(form(&[("token", rt), ("client_id", CLIENT.into())])).send();
    }
    forget_tokens();
}

/// What the server offers, from `/v1/meta` (no sign-in needed).
pub struct Meta {
    /// It nudges over a WebSocket; otherwise the client looks on its own.
    pub push: bool,
}

/// A server from before `/v1/meta` always had the socket.
pub fn meta(server: &str) -> anyhow::Result<Meta> {
    let r = http().get(format!("{server}/v1/meta")).timeout(Duration::from_secs(10)).send()?;
    if r.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(Meta { push: true });
    }
    anyhow::ensure!(r.status().is_success(), "the account server said {}", r.status().as_u16());
    let v: Value = r.json()?;
    Ok(Meta { push: v["push"].as_bool().unwrap_or(false) })
}

/// The nudge socket, as a WebSocket URL.
pub fn ws_url(server: &str) -> String {
    let base = server.strip_prefix("https://").map(|r| format!("wss://{r}")).or_else(|| server.strip_prefix("http://").map(|r| format!("ws://{r}"))).unwrap_or_else(|| server.to_string());
    format!("{base}/v1/sync/ws")
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_sign_in_is_not_listed_as_keys() {
        for k in [super::ACCESS_KEY, super::REFRESH_KEY, super::EXPIRES_KEY] {
            assert!(k.starts_with(dino_core::settings::ACCOUNT_TOKEN_PREFIX), "{k}");
        }
    }
}
