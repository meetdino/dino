//! Settings sync end to end: devices writing plain JSON records with `dino-sync`, the server
//! ordering and relaying them, nudges across two nodes, and older clients told to update.

mod common;

use std::time::Duration;

use common::*;
use dino_sync::{Clock, Hlc, PullResponse, PushRequest, PushResponse, Record, RecordId, Store};
use futures::{SinkExt, StreamExt};
use serde_json::{Map, Value, json};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64
}

struct Dev {
    at: String,
    id: String,
    clock: Clock,
    store: Store,
    seq: u64,
}

async fn device(s: &Server, b: &reqwest::Client, name: &str) -> Dev {
    let t = s.native_login(b, "dino", name).await;
    let id = t["device_id"].as_str().unwrap().to_owned();
    Dev { at: t["access_token"].as_str().unwrap().to_owned(), clock: Clock::new(id.clone()), id, store: Store::new(), seq: 0 }
}

fn write(d: &mut Dev, collection: &str, k: &str, value: Option<Value>, at_ms: u64) -> Record {
    let hlc = d.clock.now(at_ms);
    Record { id: RecordId::new(collection, k), hlc, schema: 1, deleted: value.is_none(), value: value.unwrap_or(Value::Null), seq: None, extra: Map::new() }
}

async fn push(s: &Server, d: &Dev, records: Vec<Record>) -> (reqwest::StatusCode, Value) {
    let req = PushRequest { device_id: d.id.clone(), records, extra: Map::new() };
    let r = app().post(s.url("/v1/sync")).bearer_auth(&d.at).header("dino-sync-version", dino_sync::record::PROTOCOL.to_string()).json(&req).send().await.unwrap();
    (r.status(), r.json().await.unwrap())
}

/// Pulls to the end, `page` at a time, merging into the device's store. Returns the pages taken.
async fn pull_all(s: &Server, d: &mut Dev, page: u32) -> usize {
    let mut pages = 0;
    loop {
        let r: PullResponse = app().get(s.url(&format!("/v1/sync?since={}&limit={page}", d.seq))).bearer_auth(&d.at).send().await.unwrap().json().await.unwrap();
        pages += 1;
        for rec in &r.records {
            let _ = d.clock.observe(&rec.hlc, now_ms());
        }
        let (_, refused) = d.store.apply_all_remote(r.records, now_ms());
        assert!(refused.is_empty());
        d.seq = r.seq;
        if !r.more {
            return pages;
        }
    }
}

fn value<'a>(d: &'a Dev, collection: &str, k: &str) -> Option<&'a Value> {
    d.store.get(&RecordId::new(collection, k)).filter(|r| !r.deleted).map(|r| &r.value)
}

#[tokio::test]
async fn two_devices_converge_through_the_server() {
    let s = start().await;
    let b = browser();
    s.email_signin(&b, "sync@example.com").await;
    let mut a = device(&s, &b, "Mac A").await;
    let mut bd = device(&s, &b, "Mac B").await;
    let t0 = now_ms();

    let first = write(&mut a, "agents", "claude.mode", Some(json!("plan")), t0);
    let recs = vec![first.clone(), write(&mut a, "policies", "allowed_agents", Some(json!(["claude", "codex"])), t0), write(&mut a, "ssh", "build-box", Some(json!({"folder": "~/src"})), t0)];
    let (st, r) = push(&s, &a, recs).await;
    assert_eq!(st, 200);
    let r: PushResponse = serde_json::from_value(r).unwrap();
    assert_eq!((r.seq, r.accepted.len()), (3, 3));

    // A's first write again (a retry): accepted, nothing moves.
    let (_, r) = push(&s, &a, vec![first.clone()]).await;
    assert_eq!((r["seq"].as_u64(), r["accepted"].as_array().unwrap().len()), (Some(3), 1));
    // B changes the same setting later: it wins, everywhere.
    let later = write(&mut bd, "agents", "claude.mode", Some(json!("auto")), t0 + 50);
    let (_, r) = push(&s, &bd, vec![later]).await;
    assert_eq!(r["seq"], 4);
    // An older stamp for the same setting loses.
    let stale = Record { hlc: Hlc { wall_ms: t0 - 10_000, counter: 0, device: a.id.clone() }, ..first.clone() };
    let (_, r) = push(&s, &a, vec![stale]).await;
    assert_eq!(r["superseded"][0]["key"], "claude.mode");
    // A clock an hour ahead, and a delete that carries a value: refused.
    let future = write(&mut Dev { clock: Clock::new("skewed"), ..device(&s, &b, "Skewed Mac").await }, "agents", "x", Some(json!("y")), t0 + 3_600_000);
    let odd_delete = Record { deleted: true, hlc: a.clock.now(now_ms()), ..first.clone() };
    let (_, r) = push(&s, &a, vec![future, odd_delete]).await;
    let codes: Vec<&str> = r["rejected"].as_array().unwrap().iter().map(|x| x["error"]["code"].as_str().unwrap()).collect();
    assert_eq!(codes, ["future_stamp", "malformed"]);
    assert_eq!(r["seq"], 4);

    // A client on the encrypted protocol (2) is told to update, and its push isn't taken.
    let req = PushRequest { device_id: a.id.clone(), records: vec![], extra: Map::new() };
    let old = app().post(s.url("/v1/sync")).bearer_auth(&a.at).header("dino-sync-version", "2").json(&req).send().await.unwrap();
    assert_eq!(old.status(), 426);
    assert_eq!(old.json::<Value>().await.unwrap()["code"], "upgrade_required");

    // B deletes the SSH host: a tombstone.
    let gone = write(&mut bd, "ssh", "build-box", None, now_ms());
    push(&s, &bd, vec![gone]).await;

    // Both pull, a page of two at a time, and end up with the same store.
    assert!(pull_all(&s, &mut a, 2).await >= 2, "paged");
    pull_all(&s, &mut bd, 2).await;
    assert_eq!(a.store, bd.store, "converged");
    assert_eq!(a.seq, bd.seq);
    for d in [&a, &bd] {
        assert_eq!(value(d, "agents", "claude.mode"), Some(&json!("auto")));
        assert_eq!(value(d, "policies", "allowed_agents"), Some(&json!(["claude", "codex"])));
        assert!(d.store.get(&RecordId::new("ssh", "build-box")).unwrap().is_tombstone());
    }

    // Many writes, small pages: every one arrives, in order, and `more` stops at the end.
    let many: Vec<Record> = (0..25).map(|i| write(&mut a, "repos", &format!("r{i}"), Some(json!(format!("ENV-{i}"))), now_ms())).collect();
    let (_, r) = push(&s, &a, many).await;
    let head = r["seq"].as_u64().unwrap();
    let pages = pull_all(&s, &mut bd, 7).await;
    assert_eq!((bd.seq, pages), (head, 4));
    assert_eq!(bd.store.len(), 3 + 25);

    // Unknown fields a newer client sends come back unchanged.
    let mut extra = Map::new();
    extra.insert("future_field".into(), json!({"kept": true}));
    let odd = Record { extra, ..write(&mut a, "agents", "codex.mode", Some(json!("MODE-X")), now_ms()) };
    push(&s, &a, vec![odd]).await;
    pull_all(&s, &mut bd, 100).await;
    assert_eq!(bd.store.get(&RecordId::new("agents", "codex.mode")).unwrap().extra["future_field"], json!({"kept": true}));

    // The export has every record, values as written.
    let ex: Value = b.get(s.url("/account/export")).send().await.unwrap().json().await.unwrap();
    assert_eq!(ex["records"].as_array().unwrap().len(), 3 + 25 + 1);
    assert!(ex["records"].as_array().unwrap().iter().any(|r| r["value"] == json!(["claude", "codex"])));
    assert!(ex.get("recovery_wrapped_key").is_none());

    // The account page shows the settings themselves.
    let page = b.get(s.url("/account")).send().await.unwrap().text().await.unwrap();
    assert!(page.contains("agents.claude.mode") && page.contains("auto") && page.contains("repos.r3") && page.contains("ENV-3"));
    assert!(!page.contains("ssh.build-box"), "deleted settings aren't listed");
    assert!(!page.contains("Encrypted"));
}

async fn socket(s: &Server, d: &Dev) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let mut req = s.url("/v1/sync/ws").replace("http://", "ws://").into_client_request().unwrap();
    req.headers_mut().insert("authorization", format!("Bearer {}", d.at).parse().unwrap());
    tokio_tungstenite::connect_async(req).await.unwrap().0
}

async fn next_text(ws: &mut tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>) -> Value {
    loop {
        match tokio::time::timeout(Duration::from_secs(3), ws.next()).await.expect("a nudge within 3 s").unwrap().unwrap() {
            Message::Text(t) => return serde_json::from_str(&t).unwrap(),
            _ => continue,
        }
    }
}

#[tokio::test]
async fn nudges_reach_devices_on_another_node() {
    let s1 = start().await;
    let s2 = s1.second_node().await;
    let b = browser();
    s1.email_signin(&b, "nudge@example.com").await;
    let mut a = device(&s1, &b, "Mac A").await;
    let mut bd = device(&s1, &b, "Mac B").await;

    // B is connected to the other node.
    let mut ws = socket(&s2, &bd).await;
    assert_eq!(next_text(&mut ws).await, json!({"type": "advanced", "seq": 0}), "where the account is on connect");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(s2.state.hub.connected(), 1);

    let r = write(&mut a, "agents", "claude.mode", Some(json!("plan")), now_ms());
    push(&s1, &a, vec![r]).await;
    assert_eq!(next_text(&mut ws).await, json!({"type": "advanced", "seq": 1}));
    let r = write(&mut a, "agents", "claude.effort", Some(json!("high")), now_ms());
    push(&s1, &a, vec![r]).await;
    assert_eq!(next_text(&mut ws).await, json!({"type": "advanced", "seq": 2}));
    pull_all(&s2, &mut bd, 100).await;
    assert_eq!(value(&bd, "agents", "claude.mode"), Some(&json!("plan")));

    // A write that changes nothing sends nothing.
    let same = bd.store.get(&RecordId::new("agents", "claude.mode")).unwrap().clone();
    push(&s1, &a, vec![Record { seq: None, ..same }]).await;
    assert!(tokio::time::timeout(Duration::from_millis(500), ws.next()).await.is_err(), "no nudge for a no-op");

    // The encrypted protocol's routes are gone.
    for path in ["/v1/sync/reset", "/v1/sync/approvals", "/v1/sync/recovery"] {
        let st = app().post(s1.url(path)).bearer_auth(&a.at).send().await.unwrap().status();
        assert!(st == 404 || st == 405, "{path}: {st}");
    }

    ws.send(Message::Close(None)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(s2.state.hub.connected(), 0, "gone when the socket closes");
    // Without a dino token there's no socket.
    let mut req = s2.url("/v1/sync/ws").replace("http://", "ws://").into_client_request().unwrap();
    req.headers_mut().insert("authorization", "Bearer dino_at_nope".parse().unwrap());
    assert!(tokio_tungstenite::connect_async(req).await.is_err());
}

#[tokio::test]
async fn concurrent_pushes_get_a_gapless_sequence() {
    let s = start().await;
    let b = browser();
    s.email_signin(&b, "busy-sync@example.com").await;
    let mut devices = Vec::new();
    for i in 0..5 {
        devices.push(device(&s, &b, &format!("Mac {i}")).await);
    }
    let batches: Vec<(String, String, Vec<Record>)> = devices
        .iter_mut()
        .enumerate()
        .map(|(i, d)| {
            let recs = (0..20).map(|j| write(d, "repos", &format!("d{i}-r{j}"), Some(json!("v")), now_ms())).collect();
            (d.at.clone(), d.id.clone(), recs)
        })
        .collect();
    let url = s.url("/v1/sync");
    let results = futures::future::join_all(batches.into_iter().map(|(at, id, records)| {
        let url = url.clone();
        async move { app().post(url).bearer_auth(at).json(&PushRequest { device_id: id, records, extra: Map::new() }).send().await.unwrap().status() }
    }))
    .await;
    assert!(results.iter().all(|st| *st == 200));
    let mut reader = device(&s, &b, "Reader").await;
    let r: PullResponse = app().get(s.url("/v1/sync?since=0&limit=1000")).bearer_auth(&reader.at).send().await.unwrap().json().await.unwrap();
    let seqs: Vec<u64> = r.records.iter().map(|x| x.seq.unwrap()).collect();
    assert_eq!(seqs, (1..=100).collect::<Vec<_>>(), "every write numbered once, no gaps");
    pull_all(&s, &mut reader, 100).await;
    assert_eq!(reader.seq, 100);
}
