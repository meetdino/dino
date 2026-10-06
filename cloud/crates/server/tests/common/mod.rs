//! A real server on a random port, its own fresh Postgres database, stand-ins for GitHub and
//! Google, a mail file, and every log line the server writes kept in memory for inspection.

#![allow(dead_code)]

use std::io::Write;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, OnceLock};

use axum::Json;
use axum::routing::{get, post};
use base64::Engine;
use dino_cloud::config::{Config, MailConfig, Platform, Upstream};
use dino_cloud::{AppState, serve};
use serde_json::{Value, json};
use sha2::Digest;
use sqlx::postgres::PgPoolOptions;


static LOGS: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();

#[derive(Clone)]
struct LogWriter(Arc<Mutex<Vec<u8>>>);

impl Write for LogWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Everything logged so far, by every server in this test binary.
pub fn logs() -> String {
    String::from_utf8_lossy(&LOGS.get().unwrap().lock().unwrap()).into_owned()
}

fn init_logs() {
    LOGS.get_or_init(|| {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let w = LogWriter(buf.clone());
        // Debug, not info: whatever a verbose production setting would print is checked too.
        let _ = tracing_subscriber::fmt()
            .json()
            .with_env_filter("debug,hyper=info,hyper_util=info,h2=info,rustls=info,reqwest=info,tower=info")
            .with_writer(move || w.clone())
            .try_init();
        buf
    });
}

pub struct Server {
    pub base: String,
    pub state: AppState,
    pub mail: std::path::PathBuf,
    db_url: String,
}

async fn upstream_mock() -> SocketAddr {
    let app = axum::Router::new()
        .route("/gh/token", post(|body: String| async move {
            let ok = body.contains("code=GHCODE") && body.contains("code_verifier=");
            Json(if ok { json!({"access_token": "gh-access", "token_type": "bearer"}) } else { json!({"error": "bad_verification_code"}) })
        }))
        .route("/gh/user", get(|| async { Json(json!({"id": 4242, "login": "dino-tester"})) }))
        .route("/gh/emails", get(|| async { Json(json!([{"email": "gh-and-google@example.com", "primary": true, "verified": true}])) }))
        .route("/g/token", post(|body: String| async move {
            Json(if body.contains("code=GCODE") { json!({"access_token": "g-access"}) } else { json!({"error": "invalid_grant"}) })
        }))
        .route("/g/userinfo", get(|| async { Json(json!({"sub": "g-123", "email": "gh-and-google@example.com", "email_verified": true})) }));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    addr
}

pub const CRON_SECRET: &str = "cron-secret-for-tests";

pub async fn start() -> Server {
    start_with(Platform::default()).await
}

/// As on Vercel: no push socket or LISTEN, limits counted in Postgres, cleanup only through
/// `/internal/cron`.
pub async fn start_serverless() -> Server {
    start_with(Platform { push: false, shared_limits: true, background_jobs: false, cron_secret: Some(CRON_SECRET.into()), migrate_url: None, db_max_connections: 5 }).await
}

pub async fn start_with(platform: Platform) -> Server {
    init_logs();
    let admin_url = std::env::var("DINO_TEST_DATABASE_URL").unwrap_or_else(|_| "postgres://dino:dino@127.0.0.1:55432/postgres".into());
    let db_name = format!("dino_test_{}", uuid::Uuid::new_v4().simple());
    let admin = PgPoolOptions::new().max_connections(1).connect(&admin_url).await.expect("test Postgres (see README)");
    // The name is ours: a fixed prefix and a UUID.
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {db_name}"))).execute(&admin).await.unwrap();
    admin.close().await;
    let db_url = format!("{}/{}", admin_url.rsplit_once('/').unwrap().0, db_name);

    let mock = upstream_mock().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let mail = std::env::temp_dir().join(format!("{db_name}.mail"));
    let up = |p: &str, authorize: &str, token: &str, user: &str, emails: Option<&str>| Upstream {
        client_id: format!("{p}-client"),
        client_secret: format!("{p}-secret"),
        authorize_url: format!("http://{mock}{authorize}"),
        token_url: format!("http://{mock}{token}"),
        userinfo_url: format!("http://{mock}{user}"),
        emails_url: emails.map(|e| format!("http://{mock}{e}")),
    };
    let cfg = Config {
        public_url: format!("http://127.0.0.1:{port}").parse().unwrap(),
        bind: format!("127.0.0.1:{port}").parse().unwrap(),
        metrics_bind: None,
        database_url: db_url.clone(),
        secret: rand_secret(),
        trust_proxy: false,
        github: Some(up("gh", "/gh/authorize", "/gh/token", "/gh/user", Some("/gh/emails"))),
        google: Some(up("g", "/g/authorize", "/g/token", "/g/userinfo", None)),
        mail: MailConfig::File(mail.clone()),
        turnstile: None,
        json_logs: true,
        ip_limit: (30, 120),
        account_limit: (20, 60),
        platform,
    };
    let pool = PgPoolOptions::new().max_connections(16).acquire_timeout(std::time::Duration::from_secs(3)).connect(&db_url).await.unwrap();
    let state = AppState::with_pool(cfg, pool).await.unwrap();
    let s2 = state.clone();
    tokio::spawn(async move { serve(s2, listener).await.unwrap() });
    Server { base: format!("http://127.0.0.1:{port}"), state, mail, db_url }
}

impl Server {
    /// Another node on the same database, as behind a load balancer.
    pub async fn second_node(&self) -> Server {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut cfg = (*self.state.cfg).clone();
        cfg.public_url = format!("http://127.0.0.1:{port}").parse().unwrap();
        let pool = PgPoolOptions::new().max_connections(16).connect(&self.db_url).await.unwrap();
        let state = AppState::with_pool(cfg, pool).await.unwrap();
        let s2 = state.clone();
        tokio::spawn(async move { serve(s2, listener).await.unwrap() });
        Server { base: format!("http://127.0.0.1:{port}"), state, mail: self.mail.clone(), db_url: self.db_url.clone() }
    }

    /// Every row of every table, as text: what someone with the database would see.
    pub async fn dump(&self) -> String {
        let tables: Vec<(String,)> = sqlx::query_as("SELECT table_name::text FROM information_schema.tables WHERE table_schema = 'public'").fetch_all(&self.state.db).await.unwrap();
        let mut out = String::new();
        for (t,) in tables {
            let rows: (Option<String>,) = sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT string_agg(to_jsonb(x)::text, E'\\n') FROM \"{t}\" x"))).fetch_one(&self.state.db).await.unwrap();
            out.push_str(&rows.0.unwrap_or_default());
            out.push('\n');
        }
        out
    }
}

fn rand_secret() -> [u8; 32] {
    let mut s = [0u8; 32];
    for (i, b) in uuid::Uuid::new_v4().as_bytes().iter().chain(uuid::Uuid::new_v4().as_bytes()).enumerate() {
        s[i] = *b;
    }
    s
}

/// A browser: keeps cookies, doesn't follow redirects (tests look at them).
pub fn browser() -> reqwest::Client {
    reqwest::Client::builder().cookie_store(true).redirect(reqwest::redirect::Policy::none()).build().unwrap()
}

/// A native app talking to the token endpoint.
pub fn app() -> reqwest::Client {
    reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap()
}

pub fn location(r: &reqwest::Response) -> String {
    r.headers().get("location").map(|v| v.to_str().unwrap().to_owned()).unwrap_or_default()
}

pub fn csrf(html: &str) -> String {
    let i = html.find(r#"name="csrf" value=""#).expect("form has a csrf field") + r#"name="csrf" value=""#.len();
    html[i..html[i..].find('"').unwrap() + i].to_owned()
}

impl Server {
    pub fn url(&self, p: &str) -> String {
        format!("{}{p}", self.base)
    }

    /// The newest code mailed to `email`.
    pub fn mailed_code(&self, email: &str) -> String {
        let text = std::fs::read_to_string(&self.mail).unwrap();
        let block = text.split("---").filter(|b| b.contains(&format!("To: {email}"))).last().expect("a mail to that address");
        let i = block.find("code is ").unwrap() + 8;
        block[i..i + 6].to_owned()
    }

    /// The path of the newest sign-in link mailed to `email` (`/login/<token>`).
    pub fn mailed_link(&self, email: &str) -> String {
        let text = std::fs::read_to_string(&self.mail).unwrap();
        let block = text.split("---").filter(|b| b.contains(&format!("To: {email}"))).last().expect("a mail to that address");
        let i = block.find("/login/").unwrap();
        block[i..].split_whitespace().next().unwrap().to_owned()
    }

    /// Sign `b` in with an emailed code; returns where it was sent after.
    pub async fn email_signin(&self, b: &reqwest::Client, email: &str) -> String {
        let page = b.get(self.url("/signin")).send().await.unwrap().text().await.unwrap();
        let r = b.post(self.url("/signin/email")).form(&[("csrf", csrf(&page).as_str()), ("email", email)]).send().await.unwrap();
        assert_eq!(location(&r), "/signin/email/code", "code sent");
        let page = b.get(self.url("/signin/email/code")).send().await.unwrap().text().await.unwrap();
        let code = self.mailed_code(email);
        let r = b.post(self.url("/signin/email/code")).form(&[("csrf", csrf(&page).as_str()), ("code", code.as_str())]).send().await.unwrap();
        assert!(r.status().is_redirection(), "signed in: {}", r.status());
        location(&r)
    }

    /// The whole native sign-in: authorize with PKCE, confirm in the browser, trade the code.
    pub async fn native_login(&self, b: &reqwest::Client, client_id: &str, device: &str) -> Value {
        let (code, verifier, redirect) = self.authorize(b, client_id, device).await;
        let r = app()
            .post(self.url("/oauth/token"))
            .form(&[("grant_type", "authorization_code"), ("client_id", client_id), ("code", code.as_str()), ("redirect_uri", redirect.as_str()), ("code_verifier", verifier.as_str())])
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200, "token exchange");
        r.json().await.unwrap()
    }

    /// Up to the code arriving at the loopback redirect: (code, verifier, redirect).
    pub async fn authorize(&self, b: &reqwest::Client, client_id: &str, device: &str) -> (String, String, String) {
        let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(uuid::Uuid::new_v4().as_bytes().repeat(2));
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(verifier.as_bytes()));
        let redirect = "http://127.0.0.1:53682/callback".to_string();
        let mut u = url::Url::parse(&self.url("/oauth/authorize")).unwrap();
        u.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", client_id)
            .append_pair("redirect_uri", &redirect)
            .append_pair("state", "st-123")
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("scope", "account sync")
            .append_pair("device_name", device)
            .append_pair("device_os", "macOS 26")
            .append_pair("dino_version", "0.1.0");
        let r = b.get(u.as_str()).send().await.unwrap();
        assert_eq!(location(&r), "/oauth/authorize/confirm");
        let page = b.get(self.url("/oauth/authorize/confirm")).send().await.unwrap().text().await.unwrap();
        assert!(page.contains(device), "confirm page names the device");
        let r = b.post(self.url("/oauth/authorize/decide")).form(&[("csrf", csrf(&page).as_str()), ("decision", "allow")]).send().await.unwrap();
        let to = url::Url::parse(&location(&r)).unwrap();
        assert!(to.as_str().starts_with(&redirect));
        let q: std::collections::HashMap<_, _> = to.query_pairs().into_owned().collect();
        assert_eq!(q.get("state").map(String::as_str), Some("st-123"));
        assert_eq!(q.get("iss").map(String::as_str), Some(self.base.as_str()));
        (q["code"].clone(), verifier, redirect)
    }

    pub async fn refresh(&self, client_id: &str, rt: &str) -> reqwest::Response {
        app().post(self.url("/oauth/token")).form(&[("grant_type", "refresh_token"), ("client_id", client_id), ("refresh_token", rt)]).send().await.unwrap()
    }

    pub async fn me(&self, at: &str) -> reqwest::StatusCode {
        app().get(self.url("/v1/me")).bearer_auth(at).send().await.unwrap().status()
    }
}

pub fn sha256(s: &str) -> Vec<u8> {
    sha2::Sha256::digest(s.as_bytes()).to_vec()
}
