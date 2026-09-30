//! Rate limits, in memory per node (GCRA via `governor`). Behind a load balancer each node counts
//! on its own; that's fine for abuse protection, and the few limits that must be exact (codes sent
//! per address) are counted in Postgres instead.

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
    Quota::per_second(NonZeroU32::new(per_second).unwrap()).allow_burst(NonZeroU32::new(burst).unwrap())
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            ip: RateLimiter::keyed(quota(30, 120)),
            // One a second, a burst of 20: a person signing in never notices, a script guessing
            // user codes gets about 60 tries a minute against 25 billion codes.
            auth: RateLimiter::keyed(Quota::with_period(std::time::Duration::from_secs(1)).unwrap().allow_burst(NonZeroU32::new(20).unwrap())),
            account: RateLimiter::keyed(quota(20, 60)),
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

pub fn auth(state: &AppState, ip: IpAddr) -> Result<(), Error> {
    check(&state.limits.auth, &ip, "auth")
}

pub fn account(state: &AppState, id: Uuid) -> Result<(), Error> {
    check(&state.limits.account, &id, "account")
}

/// `n` records written by `device`; the seconds to wait when that's too many.
pub fn sync_writes(state: &AppState, device: Uuid, n: NonZeroU32) -> Result<(), u64> {
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
        .or_else(|| h("fly-client-ip"))
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
