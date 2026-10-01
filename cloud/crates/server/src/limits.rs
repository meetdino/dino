//! Rate limits, in memory per node (GCRA via `governor`). Behind a load balancer each node counts
//! on its own; that's fine for abuse protection, and the few limits that must be exact (codes sent
//! per address) are counted in Postgres instead. On a serverless host (`shared_limits`), where
//! instances come and go, the limits that guard against guessing and flooding (sign-in attempts,
//! sync writes) are counted in Postgres too; per-address request volume is left to the host's
//! firewall.

use std::net::{IpAddr, SocketAddr};
use std::num::NonZeroU32;

use axum::extract::{ConnectInfo, FromRequestParts, Request, State};
use axum::http::request::Parts;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use governor::clock::{Clock, DefaultClock};
use governor::{DefaultKeyedRateLimiter, Quota, RateLimiter};
use uuid::Uuid;

use crate::AppState;
use crate::error::Error;

pub struct Limits {
    /// Every request, by client address.
    pub ip: DefaultKeyedRateLimiter<IpAddr>,
    /// Sign-in, token and code-entry endpoints, by client address.
    pub auth: DefaultKeyedRateLimiter<IpAddr>,
    /// The `/v1` API, by account.
    pub account: DefaultKeyedRateLimiter<Uuid>,
    /// Settings written by a device: 2 a second, a burst of a full push and more.
    pub sync_writes: DefaultKeyedRateLimiter<Uuid>,
}

fn quota(per_second: u32, burst: u32) -> Quota {
    Quota::per_second(NonZeroU32::new(per_second.max(1)).unwrap()).allow_burst(NonZeroU32::new(burst.max(1)).unwrap())
}

impl Limits {
    pub fn new(cfg: &crate::config::Config) -> Self {
        let (ip_rate, ip_burst) = cfg.ip_limit;
        let (acct_rate, acct_burst) = cfg.account_limit;
        Limits {
            ip: RateLimiter::keyed(quota(ip_rate, ip_burst)),
            // One a second, a burst of 20: a person signing in never notices, a script guessing
            // user codes gets about 60 tries a minute against 25 billion codes.
            auth: RateLimiter::keyed(Quota::with_period(std::time::Duration::from_secs(1)).unwrap().allow_burst(NonZeroU32::new(20).unwrap())),
            account: RateLimiter::keyed(quota(acct_rate, acct_burst)),
            sync_writes: RateLimiter::keyed(quota(2, 600)),
        }
    }
}

impl Limits {
    pub fn retain_recent(&self) {
        self.ip.retain_recent();
        self.auth.retain_recent();
        self.account.retain_recent();
        self.sync_writes.retain_recent();
    }
}

fn check<K: std::hash::Hash + Eq + Clone>(limiter: &DefaultKeyedRateLimiter<K>, key: &K, name: &'static str) -> Result<(), Error> {
    limiter.check_key(key).map_err(|not_until| {
        metrics::counter!("rate_limited_total", "limit" => name).increment(1);
        Error::RateLimited { retry_after: not_until.wait_time_from(DefaultClock::default().now()).as_secs().max(1) }
    })
}

pub async fn auth(state: &AppState, ip: IpAddr) -> Result<(), Error> {
    if state.cfg.platform.shared_limits {
        // About what the in-memory limit allows: a burst of 20, then one a second.
        return shared(state, &format!("auth:{ip}"), 1, 80, 60, "auth").await?.map_err(|retry_after| Error::RateLimited { retry_after });
    }
    check(&state.limits.auth, &ip, "auth")
}

/// Add `n` to `key`'s count in the current `window`-second window: Ok while it's within `limit`,
/// else the seconds until the window ends.
async fn shared(state: &AppState, key: &str, n: i32, limit: i32, window: i32, name: &'static str) -> Result<std::result::Result<(), u64>, Error> {
    let (count, left): (i32, f64) = sqlx::query_as(
        "INSERT INTO rate_counters (key, window_start, count)
         VALUES ($1, to_timestamp(floor(extract(epoch FROM now()) / $3) * $3), $2)
         ON CONFLICT (key, window_start) DO UPDATE SET count = rate_counters.count + EXCLUDED.count
         RETURNING count, extract(epoch FROM window_start + make_interval(secs => $3) - now())::float8",
    )
    .bind(key)
    .bind(n)
    .bind(window)
    .fetch_one(&state.db)
    .await?;
    if count > limit {
        metrics::counter!("rate_limited_total", "limit" => name).increment(1);
        return Ok(Err(left.ceil().max(1.0) as u64));
    }
    Ok(Ok(()))
}

pub fn account(state: &AppState, id: Uuid) -> Result<(), Error> {
    check(&state.limits.account, &id, "account")
}

/// `n` records written by `device`; the seconds to wait when that's too many.
pub async fn sync_writes(state: &AppState, device: Uuid, n: NonZeroU32) -> Result<std::result::Result<(), u64>, Error> {
    if state.cfg.platform.shared_limits {
        // Two a second on average, counted over ten minutes so a full push fits.
        return shared(state, &format!("sync:{device}"), n.get() as i32, 1200, 600, "sync_writes").await;
    }
    Ok(sync_writes_here(state, device, n))
}

fn sync_writes_here(state: &AppState, device: Uuid, n: NonZeroU32) -> Result<(), u64> {
    match state.limits.sync_writes.check_key_n(&device, n) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(not_until)) => {
            metrics::counter!("rate_limited_total", "limit" => "sync_writes").increment(1);
            Err(not_until.wait_time_from(DefaultClock::default().now()).as_secs().max(1))
        }
        Err(_) => Err(60),
    }
}

/// The client's address. Behind a trusted proxy, from the header it sets; otherwise the socket's.
#[derive(Clone, Copy, Debug)]
pub struct ClientIp(pub IpAddr);

fn client_ip(state: &AppState, req: &Request) -> IpAddr {
    let socket = req.extensions().get::<ConnectInfo<SocketAddr>>().map(|c| c.0.ip()).unwrap_or(IpAddr::from([127, 0, 0, 1]));
    if !state.cfg.trust_proxy {
        return socket;
    }
    let h = |name: &str| req.headers().get(name).and_then(|v| v.to_str().ok()).map(str::trim);
    h("cf-connecting-ip")
        .or_else(|| h("x-vercel-forwarded-for"))
        // The last hop is the one the trusted proxy saw; earlier entries are client-supplied.
        .or_else(|| h("x-forwarded-for").and_then(|v| v.rsplit(',').next()).map(str::trim))
        .and_then(|v| v.parse().ok())
        .unwrap_or(socket)
}

pub async fn per_ip(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let ip = client_ip(&state, &req);
    req.extensions_mut().insert(ClientIp(ip));
    if let Err(e) = check(&state.limits.ip, &ip, "ip") {
        return e.into_response();
    }
    next.run(req).await
}

impl<S: Send + Sync> FromRequestParts<S> for ClientIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(parts.extensions.get::<ClientIp>().copied().unwrap_or(ClientIp(IpAddr::from([127, 0, 0, 1]))))
    }
}
