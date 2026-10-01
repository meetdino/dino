//! The server as a serverless host runs it (Vercel): no push socket and no LISTEN, sync by
//! request alone, the limits that guard sign-in and sync writes counted in Postgres so they hold
//! across instances, and cleanup only when `/internal/cron` is called with the cron secret.

mod common;

use common::*;
use dino_sync::{AccountKey, Clock, PullResponse, PushRequest, RecordId};
use serde_json::{Map, Value};

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64
}

async fn listening(s: &Server) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE datname = current_database() AND query ILIKE 'LISTEN%'").fetch_one(&s.state.db).await.unwrap()
}

#[tokio::test]
async fn meta_tells_devices_to_poll_and_there_is_no_socket() {
    let s = start_serverless().await;
    let meta: Value = app().get(s.url("/v1/meta")).send().await.unwrap().json().await.unwrap();
    assert_eq!(meta["push"], false);
    assert_eq!(meta["protocol"], dino_sync::record::PROTOCOL);
    assert_eq!(meta["poll_secs"], 60);

    let b = browser();
    s.email_signin(&b, "poll@example.com").await;
    let t = s.native_login(&b, "dino", "Mac A").await;
    let at = t["access_token"].as_str().unwrap();
    let r = app().get(s.url("/v1/sync/ws")).bearer_auth(at).header("connection", "upgrade").header("upgrade", "websocket").header("sec-websocket-version", "13").header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==").send().await.unwrap();
    assert_eq!(r.status(), 404, "no socket without push");
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(listening(&s).await, 0, "nothing LISTENs");

    // The long-running default still offers push.
    let long = start().await;
    let meta: Value = app().get(long.url("/v1/meta")).send().await.unwrap().json().await.unwrap();
    assert_eq!(meta["push"], true);
}

#[tokio::test]
async fn sync_works_by_request_alone() {
    let s = start_serverless().await;
    let b = browser();
    s.email_signin(&b, "stateless@example.com").await;
    let ta = s.native_login(&b, "dino", "Mac A").await;
    let tb = s.native_login(&b, "dino", "Mac B").await;
    let me: Value = app().get(s.url("/v1/me")).bearer_auth(ta["access_token"].as_str().unwrap()).send().await.unwrap().json().await.unwrap();
    let account = me["account_id"].as_str().unwrap();
    let key = AccountKey::generate();
    let device = ta["device_id"].as_str().unwrap().to_owned();
    let mut clock = Clock::new(device.clone());
    let rec = key.seal_record(account, RecordId::new("agents", "claude.mode"), clock.now(now_ms()), 1, Some("plan")).unwrap();
    let req = PushRequest { device_id: device, records: vec![rec], extra: Map::new() };
    let r = app().post(s.url("/v1/sync")).bearer_auth(ta["access_token"].as_str().unwrap()).header("dino-sync-version", dino_sync::record::PROTOCOL.to_string()).json(&req).send().await.unwrap();
    assert_eq!(r.status(), 200, "push");

    let pulled: PullResponse = app().get(s.url("/v1/sync?since=0")).bearer_auth(tb["access_token"].as_str().unwrap()).header("dino-sync-version", dino_sync::record::PROTOCOL.to_string()).send().await.unwrap().json().await.unwrap();
    assert_eq!(pulled.records.len(), 1);
    assert_eq!(key.open_record(account, &pulled.records[0]).unwrap().as_deref(), Some("plan"));
    let counted: i64 = sqlx::query_scalar("SELECT count(*) FROM rate_counters WHERE key LIKE 'sync:%'").fetch_one(&s.state.db).await.unwrap();
    assert_eq!(counted, 1, "sync writes counted in Postgres");

    // A reset shows as a new generation in the next pull: how a device without a socket hears of it.
    let g0 = pulled.extra["generation"].as_i64().unwrap();
    let r = app().post(s.url("/v1/sync/reset")).bearer_auth(ta["access_token"].as_str().unwrap()).header("dino-sync-version", dino_sync::record::PROTOCOL.to_string()).send().await.unwrap();
    assert_eq!(r.status(), 200, "reset");
    let after: PullResponse = app().get(s.url(&format!("/v1/sync?since={}", pulled.seq))).bearer_auth(tb["access_token"].as_str().unwrap()).header("dino-sync-version", dino_sync::record::PROTOCOL.to_string()).send().await.unwrap().json().await.unwrap();
    assert_eq!(after.extra["generation"].as_i64().unwrap(), g0 + 1, "the generation moved on");
}

#[tokio::test]
async fn sign_in_attempts_are_limited_across_instances() {
    let s = start_serverless().await;
    // Two instances on one database: the limit is shared, not per instance.
    let other = s.second_node().await;
    let mut limited = None;
    for i in 0..100 {
        let node = if i % 2 == 0 { &s } else { &other };
        let r = app().post(node.url("/oauth/revoke")).form(&[("token", "nope"), ("client_id", "dino")]).send().await.unwrap();
        if r.status() == 429 {
            limited = Some((i, r.headers().get("retry-after").and_then(|v| v.to_str().ok()).map(str::to_owned)));
            break;
        }
    }
    let (at, retry) = limited.expect("limited within 100 tries");
    assert!(at <= 81, "limited after about 80 tries in the window, not per instance ({at})");
    assert!(retry.is_some_and(|r| r.parse::<u64>().unwrap() >= 1), "says when to come back");
}

#[tokio::test]
async fn cleanup_runs_only_with_the_cron_secret() {
    let s = start_serverless().await;
    sqlx::query("INSERT INTO rate_counters (key, window_start, count) VALUES ('old', now() - interval '2 days', 1)").execute(&s.state.db).await.unwrap();
    assert_eq!(app().get(s.url("/internal/cron")).send().await.unwrap().status(), 401);
    assert_eq!(app().get(s.url("/internal/cron")).bearer_auth("wrong").send().await.unwrap().status(), 401);
    assert_eq!(app().get(s.url("/internal/cron")).bearer_auth(CRON_SECRET).send().await.unwrap().status(), 200);
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM rate_counters WHERE key = 'old'").fetch_one(&s.state.db).await.unwrap();
    assert_eq!(left, 0, "old counters cleaned up");

    // Without a secret configured the endpoint isn't there.
    let long = start().await;
    assert_eq!(app().get(long.url("/internal/cron")).bearer_auth(CRON_SECRET).send().await.unwrap().status(), 404);
}
