//! The connections out of the proxy. dino sits between an agent and its provider, so it must never
//! fail where a direct connection wouldn't, and when it does fail it must fail the way the agent's
//! own client expects, so the agent's retries still work.
//!
//! - Pooled connections are dropped before an upstream is likely to close them, kept alive with TCP
//!   and HTTP/2 pings, and thrown away after the Mac sleeps or a send fails (a network change or a
//!   peer that hung up leaves dead sockets in the pool, which would fail every retry in a row).
//! - A request is sent again only when it provably never left: the connection couldn't be opened
//!   (DNS, refused, timed out). Once bytes may have reached the provider, the agent decides: a model
//!   call costs money and has side effects.
//! - Giving up answers in the provider's own error shape with `x-should-retry: true`, which the
//!   Anthropic and OpenAI SDKs honour, so Claude Code and Codex retry with their own backoff.

use std::sync::RwLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{Response, StatusCode};
use serde_json::json;

/// Below the idle limit of the load balancers in front of hosted APIs (AWS ALB 60 s, most CDNs
/// 60 s or more), so the pool never hands out a socket the other side is about to close.
const CLOUD_IDLE: Duration = Duration::from_secs(30);
/// Model servers on this Mac run behind uvicorn or Go's server; uvicorn closes idle sockets after 5 s.
const LOCAL_IDLE: Duration = Duration::from_secs(2);
/// Generous: a slow network takes several seconds to open a connection, and the agent's own retry
/// only starts after this.
const CONNECT: Duration = Duration::from_secs(15);
/// Sends of a request that never left, after the first.
const RETRIES: u32 = 3;
/// How long the Mac may have slept before the pool is assumed dead.
const SLEPT: Duration = Duration::from_secs(10);

pub(crate) struct Upstream {
    cloud: RwLock<Pool>,
    local: RwLock<Pool>,
}

struct Pool {
    client: reqwest::Client,
    /// Wall and monotonic clocks at the last use: the monotonic one stops while the Mac sleeps.
    wall: SystemTime,
    mono: Instant,
}

impl Pool {
    fn new(idle: Duration) -> Self {
        Self { client: client(idle), wall: SystemTime::now(), mono: Instant::now() }
    }
}

fn client(idle: Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .pool_idle_timeout(idle)
        .tcp_keepalive(Duration::from_secs(15))
        .connect_timeout(CONNECT)
        .http2_keep_alive_interval(Duration::from_secs(20))
        .http2_keep_alive_timeout(Duration::from_secs(10))
        .http2_keep_alive_while_idle(true)
        .build()
        .expect("http client")
}

impl Default for Upstream {
    fn default() -> Self {
        Self { cloud: RwLock::new(Pool::new(CLOUD_IDLE)), local: RwLock::new(Pool::new(LOCAL_IDLE)) }
    }
}

impl Upstream {
    /// The client for hosted APIs, or for model servers on this Mac.
    pub(crate) fn client(&self, local: bool) -> reqwest::Client {
        let (pool, idle) = if local { (&self.local, LOCAL_IDLE) } else { (&self.cloud, CLOUD_IDLE) };
        let now = (SystemTime::now(), Instant::now());
        let mut p = pool.write().unwrap();
        let wall = now.0.duration_since(p.wall).unwrap_or_default();
        if wall > now.1.duration_since(p.mono) + SLEPT {
            *p = Pool::new(idle);
        }
        (p.wall, p.mono) = now;
        p.client.clone()
    }

    /// Start over with fresh connections: after a send failed, the others in the pool are suspect.
    pub(crate) fn reset(&self, local: bool) {
        let (pool, idle) = if local { (&self.local, LOCAL_IDLE) } else { (&self.cloud, CLOUD_IDLE) };
        *pool.write().unwrap() = Pool::new(idle);
    }

    /// Send what `build` makes, again on a fresh connection when it never left this Mac.
    pub(crate) async fn send(&self, local: bool, build: impl Fn(reqwest::Client) -> reqwest::RequestBuilder) -> Result<reqwest::Response, reqwest::Error> {
        let mut tries = 0;
        loop {
            match build(self.client(local)).send().await {
                Ok(r) => return Ok(r),
                Err(e) => {
                    self.reset(local);
                    if !never_left(&e) || tries == RETRIES {
                        return Err(e);
                    }
                    tries += 1;
                    tokio::time::sleep(backoff(tries)).await;
                }
            }
        }
    }
}

/// The request didn't reach the provider: no connection was ever made for it.
pub(crate) fn never_left(e: &reqwest::Error) -> bool {
    e.is_connect()
}

/// 200 ms, 400 ms, 800 ms, each with up to half again of jitter.
fn backoff(n: u32) -> Duration {
    let base = 100u64 << n;
    let jitter = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.subsec_nanos() as u64) % (base / 2 + 1);
    Duration::from_millis(base + jitter)
}

/// The provider's own error shape for a call dino couldn't get through, marked retryable, so the
/// agent's SDK handles it like the provider being briefly unavailable. `path` says which API the
/// agent speaks: Anthropic Messages, or an OpenAI one.
pub(crate) fn unreachable(path: &str, msg: &str) -> Response<Body> {
    let body = if is_messages(path) {
        json!({"type": "error", "error": {"type": "api_error", "message": msg}})
    } else {
        json!({"error": {"message": msg, "type": "server_error", "code": "upstream_unreachable"}})
    };
    Response::builder()
        .status(StatusCode::BAD_GATEWAY)
        .header("content-type", "application/json")
        .header("x-should-retry", "true")
        .body(Body::from(body.to_string()))
        .unwrap()
}

/// Anthropic's Messages API (`v1/messages`, `v1/messages/count_tokens`).
fn is_messages(path: &str) -> bool {
    path.trim_start_matches('/').split('?').next().is_some_and(|p| p.starts_with("v1/messages"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Reads one HTTP/1 request (head and its content-length body) off `s`.
    async fn read_request(s: &mut tokio::net::TcpStream) -> bool {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let Ok(n) = s.read(&mut chunk).await else { return false };
            if n == 0 {
                return false;
            }
            buf.extend_from_slice(&chunk[..n]);
            if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&buf[..end]).to_lowercase();
                let len = head.lines().find_map(|l| l.strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0))).unwrap_or(0);
                if buf.len() >= end + 4 + len {
                    return true;
                }
            }
        }
    }

    const OK: &[u8] = b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 11\r\n\r\n{\"ok\":true}";

    fn post(url: String) -> impl Fn(reqwest::Client) -> reqwest::RequestBuilder {
        move |c| c.post(&url).body("{\"model\":\"m\"}")
    }

    /// The other side closes keep-alive connections soon after answering, as a load balancer or
    /// uvicorn does: the next request still goes through, on a fresh connection.
    #[tokio::test]
    async fn a_connection_the_other_side_closed_is_not_reused() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1/messages", listener.local_addr().unwrap());
        let conns = Arc::new(AtomicUsize::new(0));
        let seen = conns.clone();
        tokio::spawn(async move {
            loop {
                let (mut s, _) = listener.accept().await.unwrap();
                seen.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    if read_request(&mut s).await {
                        let _ = s.write_all(OK).await;
                        tokio::time::sleep(Duration::from_millis(30)).await;
                    }
                });
            }
        });
        let up = Upstream::default();
        for _ in 0..4 {
            let r = up.send(false, post(url.clone())).await.expect("no error reaches the agent");
            assert_eq!(r.status(), 200);
            assert_eq!(r.text().await.unwrap(), "{\"ok\":true}");
            tokio::time::sleep(Duration::from_millis(120)).await;
        }
        assert!(conns.load(Ordering::SeqCst) >= 2, "the closed connections were replaced");
    }

    /// Nothing listens yet (a network coming back, a server restarting): sent again until it does.
    #[tokio::test]
    async fn a_request_that_never_left_is_sent_again() {
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let url = format!("http://127.0.0.1:{port}/v1/messages");
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(350)).await;
            let listener = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
            let (mut s, _) = listener.accept().await.unwrap();
            if read_request(&mut s).await {
                let _ = s.write_all(OK).await;
            }
        });
        let r = Upstream::default().send(false, post(url)).await.expect("went through once it listened");
        assert_eq!(r.status(), 200);
    }

    /// The request reached the other side and then the connection broke: never sent twice by dino
    /// (it may have been acted on), and answered as retryable for the agent to decide.
    #[tokio::test]
    async fn a_request_that_may_have_arrived_is_not_sent_again() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1/messages", listener.local_addr().unwrap());
        let got = Arc::new(AtomicUsize::new(0));
        let seen = got.clone();
        tokio::spawn(async move {
            loop {
                let (mut s, _) = listener.accept().await.unwrap();
                if read_request(&mut s).await {
                    seen.fetch_add(1, Ordering::SeqCst);
                }
                drop(s);
            }
        });
        let e = Upstream::default().send(false, post(url)).await.expect_err("the connection broke");
        assert!(!never_left(&e));
        assert_eq!(got.load(Ordering::SeqCst), 1, "delivered once, not again");
        let resp = unreachable("v1/messages", "dino couldn't reach api.anthropic.com: the connection failed");
        assert_eq!(resp.status(), 502);
        assert_eq!(resp.headers()["x-should-retry"], "true");
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!((v["type"].as_str(), v["error"]["type"].as_str()), (Some("error"), Some("api_error")));
    }

    #[tokio::test]
    async fn errors_come_in_the_shape_the_agent_speaks() {
        let anthropic = axum::body::to_bytes(unreachable("v1/messages?beta=true", "x").into_body(), usize::MAX).await.unwrap();
        let anthropic: serde_json::Value = serde_json::from_slice(&anthropic).unwrap();
        assert_eq!(anthropic["error"]["type"], "api_error");
        for path in ["v1/responses", "v1/chat/completions", "responses"] {
            let openai = axum::body::to_bytes(unreachable(path, "x").into_body(), usize::MAX).await.unwrap();
            let openai: serde_json::Value = serde_json::from_slice(&openai).unwrap();
            assert_eq!(openai["error"]["type"], "server_error", "{path}");
            assert!(openai.get("type").is_none());
        }
    }

    #[test]
    fn backoff_grows_and_stays_short() {
        assert!(backoff(1) >= Duration::from_millis(200) && backoff(1) <= Duration::from_millis(300));
        assert!(backoff(3) >= Duration::from_millis(800) && backoff(3) <= Duration::from_millis(1200));
    }
}
