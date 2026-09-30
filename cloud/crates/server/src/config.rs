//! Everything the server is told, from the environment. Nothing here names a real domain: the
//! public address is `DINO_CLOUD_URL`, and every upstream can be pointed elsewhere (for tests, or a
//! self-hosted setup).

use std::net::SocketAddr;

use anyhow::{Context, bail};
use base64::Engine;

#[derive(Clone, Debug)]
pub struct Config {
    /// Where people reach this server, e.g. `https://account.example.com`. Links and redirects
    /// are built from it.
    pub public_url: url::Url,
    pub bind: SocketAddr,
    /// Prometheus metrics are served on their own address so they aren't public by accident.
    pub metrics_bind: Option<SocketAddr>,
    pub database_url: String,
    /// Keys the HMACs (email codes, CSRF, derived refresh tokens). 32 random bytes.
    pub secret: [u8; 32],
    /// Behind a proxy that sets `X-Forwarded-For` (Fly, Cloudflare): take the client address
    /// from it. Off, the header is ignored, since anyone can send it.
    pub trust_proxy: bool,
    pub github: Option<Upstream>,
    pub google: Option<Upstream>,
    pub mail: MailConfig,
    pub turnstile: Option<Turnstile>,
    /// Shared secret a resource server (the harness) presents to `/oauth/introspect`.
    pub introspect_secret: Option<String>,
    /// JSON logs (production) or readable ones (development).
    pub json_logs: bool,
    /// Requests per second and burst, per client address (`DINO_IP_LIMIT=30,120`).
    pub ip_limit: (u32, u32),
    /// `/v1` requests per second and burst, per account (`DINO_ACCOUNT_LIMIT=20,60`).
    pub account_limit: (u32, u32),
}

#[derive(Clone, Debug)]
pub struct Upstream {
    pub client_id: String,
    pub client_secret: String,
    pub authorize_url: String,
    pub token_url: String,
    pub userinfo_url: String,
    /// GitHub only: where a user's verified addresses are listed.
    pub emails_url: Option<String>,
}

#[derive(Clone, Debug)]
pub enum MailConfig {
    /// Development: codes are appended to this file. Never stdout, so they can't end up in logs.
    File(std::path::PathBuf),
    /// A transactional mail API that takes `{from, to, subject, text}` JSON with a bearer key
    /// (Resend's shape; Postmark and others are a small adapter away).
    Http { url: String, key: String, from: String },
}

#[derive(Clone, Debug)]
pub struct Turnstile {
    pub site_key: String,
    pub secret: String,
    pub verify_url: String,
}

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// `rate,burst`, both at least 1.
fn pair(name: &str, default: (u32, u32)) -> anyhow::Result<(u32, u32)> {
    let Some(v) = var(name) else { return Ok(default) };
    let (a, b) = v.split_once(',').with_context(|| format!("{name} is rate,burst"))?;
    let (a, b): (u32, u32) = (a.trim().parse().with_context(|| name.to_owned())?, b.trim().parse().with_context(|| name.to_owned())?);
    anyhow::ensure!(a > 0 && b > 0, "{name} must be positive");
    Ok((a, b))
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let public_url: url::Url = var("DINO_CLOUD_URL").unwrap_or_else(|| "http://127.0.0.1:8787".into()).parse().context("DINO_CLOUD_URL")?;
        let production = var("DINO_ENV").as_deref() == Some("production");
        let secret = match var("DINO_SECRET_KEY") {
            Some(s) => {
                let bytes = base64::engine::general_purpose::STANDARD.decode(s.trim()).context("DINO_SECRET_KEY must be base64")?;
                <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| anyhow::anyhow!("DINO_SECRET_KEY must be 32 bytes"))?
            }
            None if production => bail!("DINO_SECRET_KEY is required in production"),
            None => {
                tracing::warn!("DINO_SECRET_KEY isn't set: using a random one, so sign-ins won't survive a restart");
                crate::crypto::random_bytes()
            }
        };
        if production && public_url.scheme() != "https" {
            bail!("DINO_CLOUD_URL must be https in production");
        }
        let upstream = |p: &str, authorize: &str, token: &str, userinfo: &str, emails: Option<&str>| -> Option<Upstream> {
            Some(Upstream {
                client_id: var(&format!("DINO_{p}_CLIENT_ID"))?,
                client_secret: var(&format!("DINO_{p}_CLIENT_SECRET"))?,
                authorize_url: var(&format!("DINO_{p}_AUTHORIZE_URL")).unwrap_or_else(|| authorize.into()),
                token_url: var(&format!("DINO_{p}_TOKEN_URL")).unwrap_or_else(|| token.into()),
                userinfo_url: var(&format!("DINO_{p}_USERINFO_URL")).unwrap_or_else(|| userinfo.into()),
                emails_url: emails.map(|e| var(&format!("DINO_{p}_EMAILS_URL")).unwrap_or_else(|| e.into())),
            })
        };
        let mail = match (var("DINO_MAIL_URL"), var("DINO_MAIL_KEY")) {
            (Some(url), Some(key)) => MailConfig::Http { url, key, from: var("DINO_MAIL_FROM").unwrap_or_else(|| "dino <no-reply@localhost>".into()) },
            _ if production => bail!("DINO_MAIL_URL and DINO_MAIL_KEY are required in production"),
            _ => MailConfig::File(var("DINO_MAIL_LOG").unwrap_or_else(|| "dino-cloud-mail.log".into()).into()),
        };
        let turnstile = match (var("DINO_TURNSTILE_SITE_KEY"), var("DINO_TURNSTILE_SECRET")) {
            (Some(site_key), Some(secret)) => Some(Turnstile {
                site_key,
                secret,
                verify_url: var("DINO_TURNSTILE_VERIFY_URL").unwrap_or_else(|| "https://challenges.cloudflare.com/turnstile/v0/siteverify".into()),
            }),
            _ => None,
        };
        Ok(Config {
            public_url,
            bind: var("DINO_BIND").unwrap_or_else(|| "127.0.0.1:8787".into()).parse().context("DINO_BIND")?,
            metrics_bind: var("DINO_METRICS_BIND").map(|v| v.parse()).transpose().context("DINO_METRICS_BIND")?,
            database_url: var("DATABASE_URL").context("DATABASE_URL isn't set")?,
            secret,
            trust_proxy: var("DINO_TRUST_PROXY").as_deref() == Some("1"),
            github: upstream("GITHUB", "https://github.com/login/oauth/authorize", "https://github.com/login/oauth/access_token", "https://api.github.com/user", Some("https://api.github.com/user/emails")),
            google: upstream("GOOGLE", "https://accounts.google.com/o/oauth2/v2/auth", "https://oauth2.googleapis.com/token", "https://openidconnect.googleapis.com/v1/userinfo", None),
            mail,
            turnstile,
            introspect_secret: var("DINO_INTROSPECT_SECRET"),
            json_logs: production || var("DINO_JSON_LOGS").as_deref() == Some("1"),
            ip_limit: pair("DINO_IP_LIMIT", (30, 120))?,
            account_limit: pair("DINO_ACCOUNT_LIMIT", (20, 60))?,
        })
    }

    /// The server's own address with `path` on it.
    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.public_url.as_str().trim_end_matches('/'), path)
    }

    /// Cookies are `Secure` once the server is served over https.
    pub fn secure_cookies(&self) -> bool {
        self.public_url.scheme() == "https"
    }
}
