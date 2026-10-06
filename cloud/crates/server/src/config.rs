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
    /// Behind a proxy that sets `X-Forwarded-For` (a load balancer, Cloudflare): take the client address
    /// from it. Off, the header is ignored, since anyone can send it.
    pub trust_proxy: bool,
    pub github: Option<Upstream>,
    pub google: Option<Upstream>,
    pub mail: MailConfig,
    pub turnstile: Option<Turnstile>,
    /// JSON logs (production) or readable ones (development).
    pub json_logs: bool,
    /// Requests per second and burst, per client address (`DINO_IP_LIMIT=30,120`).
    pub ip_limit: (u32, u32),
    /// `/v1` requests per second and burst, per account (`DINO_ACCOUNT_LIMIT=20,60`).
    pub account_limit: (u32, u32),
    /// How it's hosted: a long-running server, or a serverless platform.
    pub platform: Platform,
}

/// What changes between a long-running server (dev.sh, Docker, a VM) and a serverless host
/// (Vercel), where many short-lived instances share nothing but the database.
#[derive(Clone, Debug)]
pub struct Platform {
    /// Nudge devices over a WebSocket, fanned out between nodes with Postgres LISTEN/NOTIFY.
    /// Off, devices look every minute on their own (`/v1/meta` says which).
    pub push: bool,
    /// Keep the limits that must hold across instances (sign-in attempts, sync writes) in
    /// Postgres instead of each instance's memory.
    pub shared_limits: bool,
    /// Run the cleanup job inside the server every ten minutes. Off, it runs when
    /// `/internal/cron` is called (Vercel Cron) with `cron_secret`.
    pub background_jobs: bool,
    /// The bearer token `/internal/cron` wants (`CRON_SECRET`, which Vercel Cron sends).
    pub cron_secret: Option<String>,
    /// A direct, unpooled database URL for migrations: they hold a session advisory lock, which a
    /// transaction-mode pooler can't keep (Neon's `DATABASE_URL_UNPOOLED`).
    pub migrate_url: Option<String>,
    pub db_max_connections: u32,
}

impl Default for Platform {
    /// A long-running server.
    fn default() -> Self {
        Platform { push: true, shared_limits: false, background_jobs: true, cron_secret: None, migrate_url: None, db_max_connections: 32 }
    }
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

/// Plain http is for running a copy on this machine: production needs https, and development
/// only serves http on a loopback address, where sign-in codes and cookies stay on the machine.
fn check_public_url(url: &url::Url, production: bool) -> anyhow::Result<()> {
    if url.scheme() == "https" {
        return Ok(());
    }
    if production {
        bail!("DINO_CLOUD_URL must be https in production");
    }
    let loopback = match url.host() {
        Some(url::Host::Domain(d)) => d == "localhost",
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    };
    if url.scheme() != "http" || !loopback {
        bail!("DINO_CLOUD_URL must be https, or http on 127.0.0.1 or localhost for a copy on this machine (got {url})");
    }
    Ok(())
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
        check_public_url(&public_url, production)?;
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
        // Vercel sets VERCEL=1 for every deployment and function.
        let serverless = var("VERCEL").is_some();
        let flag = |name: &str, default: bool| var(name).map_or(default, |v| v == "1" || v == "true");
        let platform = Platform {
            push: flag("DINO_PUSH", !serverless),
            shared_limits: flag("DINO_SHARED_LIMITS", serverless),
            background_jobs: flag("DINO_BACKGROUND_JOBS", !serverless),
            cron_secret: var("CRON_SECRET"),
            migrate_url: var("DATABASE_URL_UNPOOLED"),
            db_max_connections: var("DINO_DB_MAX_CONNECTIONS").map(|v| v.parse()).transpose().context("DINO_DB_MAX_CONNECTIONS")?.unwrap_or(if serverless { 5 } else { 32 }),
        };
        // A key alone means Resend, whose free tier covers a few thousand codes a month.
        let mail_url = var("DINO_MAIL_URL").or_else(|| var("DINO_MAIL_KEY").map(|_| "https://api.resend.com/emails".into()));
        let mail = match (mail_url, var("DINO_MAIL_KEY")) {
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
            // A host that assigns the port (Vercel, most container platforms) sets PORT and
            // expects the server on every interface.
            bind: var("DINO_BIND").or_else(|| var("PORT").map(|p| format!("0.0.0.0:{p}"))).unwrap_or_else(|| "127.0.0.1:8787".into()).parse().context("DINO_BIND")?,
            metrics_bind: var("DINO_METRICS_BIND").map(|v| v.parse()).transpose().context("DINO_METRICS_BIND")?,
            database_url: var("DATABASE_URL").context("DATABASE_URL isn't set")?,
            secret,
            // Vercel's edge sets the client address; nothing else can reach the function.
            trust_proxy: flag("DINO_TRUST_PROXY", serverless),
            github: upstream("GITHUB", "https://github.com/login/oauth/authorize", "https://github.com/login/oauth/access_token", "https://api.github.com/user", Some("https://api.github.com/user/emails")),
            google: upstream("GOOGLE", "https://accounts.google.com/o/oauth2/v2/auth", "https://oauth2.googleapis.com/token", "https://openidconnect.googleapis.com/v1/userinfo", None),
            mail,
            turnstile,
            json_logs: production || var("DINO_JSON_LOGS").as_deref() == Some("1"),
            ip_limit: pair("DINO_IP_LIMIT", (30, 120))?,
            account_limit: pair("DINO_ACCOUNT_LIMIT", (20, 60))?,
            platform,
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

#[cfg(test)]
mod url_tests {
    use super::check_public_url;

    #[test]
    fn plain_http_only_on_this_machine_and_not_in_production() {
        let ok = |u: &str, prod| check_public_url(&u.parse().unwrap(), prod).is_ok();
        assert!(ok("https://cloud.meetdino.com", true));
        assert!(ok("https://cloud.meetdino.com", false));
        assert!(ok("http://127.0.0.1:8787", false));
        assert!(ok("http://localhost:8787", false));
        assert!(ok("http://[::1]:8787", false));
        assert!(!ok("http://127.0.0.1:8787", true));
        assert!(!ok("http://cloud.meetdino.com", false));
        assert!(!ok("http://10.0.0.5:8787", false));
        assert!(!ok("ftp://127.0.0.1", false));
    }
}
