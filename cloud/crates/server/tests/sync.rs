//! Settings sync end to end: devices sealing values with `dino-sync`, the server ordering and
//! relaying them, nudges across two nodes, key approval, recovery, and a database dump that holds
//! no plaintext.

mod common;

use std::time::Duration;

use common::*;
use dino_sync::{AccountKey, Clock, DeviceKeys, Grant, Hlc, PullResponse, PushRequest, PushResponse, Record, RecordId, RecoveryKey, Store, approval_code};
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

async fn account(s: &Server, d: &Dev) -> String {
    let me: Value = app().get(s.url("/v1/me")).bearer_auth(&d.at).send().await.unwrap().json().await.unwrap();
    me["account_id"].as_str().unwrap().to_owned()
}

fn sealed(key: &AccountKey, account: &str, d: &mut Dev, collection: &str, k: &str, value: Option<&str>, at_ms: u64) -> Record {
    let id = RecordId::new(collection, k);
    let value = value.map(|v| key.seal(account, &id, 1, v).unwrap());
    Record { hlc: d.clock.now(at_ms), id, schema: 1, value, seq: None, extra: Map::new() }
}

async fn push(s: &Server, d: &Dev, records: Vec<Record>) -> (reqwest::StatusCode, Value) {
    let req = PushRequest { device_id: d.id.clone(), records, extra: Map::new() };
    let r = app().post(s.url("/v1/sync")).bearer_auth(&d.at).header("dino-sync-version", "1").json(&req).send().await.unwrap();
    (r.status(), r.json().await.unwrap())
}

/// Pulls to the end, `page` at a time, merging into the device's store. Returns the pages taken.
async fn pull_all(s: &Server, d: &mut Dev, page: u32) -> usize {
    let mut pages = 0;
    loop {
        let r: PullResponse = app().get(s.url(&format!("/v1/sync?since={}&limit={page}", d.seq))).bearer_auth(&d.at).send().await.unwrap().json().await.unwrap();
        pages += 1;
        for rec in &r.records {
            d.clock.observe(&rec.hlc, now_ms());
        }
        d.store.apply_all(r.records);
        d.seq = r.seq;
        if !r.more {
            return pages;
        }
    }
}

fn open(key: &AccountKey, account: &str, d: &Dev, collection: &str, k: &str) -> Option<String> {
    let id = RecordId::new(collection, k);
    let r = d.store.get(&id)?;
    r.value.as_ref().map(|v| key.open(account, &id, r.schema, v).unwrap())
}

const PLAIN: [&str; 4] = ["MODE-PLAN-7f3a", "MODE-AUTO-1b2c", "nvapi-PLAINTEXT-SECRET-123", "HOST-BUILDBOX-9c2"];

#[tokio::test]
async fn two_devices_converge_through_the_server_which_sees_no_plaintext() {
    let s = start().await;
    let b = browser();
    s.email_signin(&b, "sync@example.com").await;
    let mut a = device(&s, &b, "Mac A").await;
    let mut bd = device(&s, &b, "Mac B").await;
    let acct = account(&s, &a).await;
    let key = AccountKey::generate();
    let t0 = now_ms();

    let first = sealed(&key, &acct, &mut a, "agents", "claude.mode", Some(PLAIN[0]), t0);
    let recs = vec![first.clone(), sealed(&key, &acct, &mut a, "keys", "NVIDIA_API_KEY", Some(PLAIN[2]), t0), sealed(&key, &acct, &mut a, "ssh", "build-box", Some(PLAIN[3]), t0)];
    let (st, r) = push(&s, &a, recs).await;
    assert_eq!(st, 200);
    let r: PushResponse = serde_json::from_value(r).unwrap();
    assert_eq!((r.seq, r.accepted.len()), (3, 3));

    // A's first write again (a retry): accepted, nothing moves.
    let (_, r) = push(&s, &a, vec![first.clone()]).await;
    assert_eq!((r["seq"].as_u64(), r["accepted"].as_array().unwrap().len()), (Some(3), 1));
    // B changes the same setting later: it wins, everywhere.
    let later = sealed(&key, &acct, &mut bd, "agents", "claude.mode", Some(PLAIN[1]), t0 + 50);
    let (_, r) = push(&s, &bd, vec![later]).await;
    assert_eq!(r["seq"], 4);
    // An older stamp for the same setting loses.
    let stale = Record { hlc: Hlc { wall_ms: t0 - 10_000, counter: 0, device: a.id.clone() }, ..first.clone() };
    let (_, r) = push(&s, &a, vec![stale]).await;
    assert_eq!(r["superseded"][0]["key"], "claude.mode");
    // A clock an hour ahead, and a value that isn't sealed: refused.
    let future = sealed(&key, &acct, &mut Dev { clock: Clock::new("skewed"), ..device(&s, &b, "Skewed Mac").await }, "agents", "x", Some("y"), t0 + 3_600_000);
    let plain = Record { value: Some("not sealed".into()), hlc: a.clock.now(now_ms()), ..first.clone() };
    let (_, r) = push(&s, &a, vec![future, plain]).await;
    let codes: Vec<&str> = r["rejected"].as_array().unwrap().iter().map(|x| x["error"]["code"].as_str().unwrap()).collect();
    assert_eq!(codes, ["future_stamp", "malformed"]);
    assert_eq!(r["seq"], 4);

    // B deletes the SSH host: a tombstone.
    let gone = sealed(&key, &acct, &mut bd, "ssh", "build-box", None, now_ms());
    push(&s, &bd, vec![gone]).await;

    // Both pull, a page of two at a time, and end up with the same store.
    assert!(pull_all(&s, &mut a, 2).await >= 2, "paged");
    pull_all(&s, &mut bd, 2).await;
    assert_eq!(a.store, bd.store, "converged");
    assert_eq!(a.seq, bd.seq);
    for d in [&a, &bd] {
        assert_eq!(open(&key, &acct, d, "agents", "claude.mode").as_deref(), Some(PLAIN[1]));
        assert_eq!(open(&key, &acct, d, "keys", "NVIDIA_API_KEY").as_deref(), Some(PLAIN[2]));
        assert!(d.store.get(&RecordId::new("ssh", "build-box")).unwrap().is_tombstone());
    }

    // Many writes, small pages: every one arrives, in order, and `more` stops at the end.
    let many: Vec<Record> = (0..25).map(|i| sealed(&key, &acct, &mut a, "repos", &format!("r{i}"), Some(&format!("ENV-{i}")), now_ms())).collect();
    let (_, r) = push(&s, &a, many).await;
    let head = r["seq"].as_u64().unwrap();
    let pages = pull_all(&s, &mut bd, 7).await;
    assert_eq!((bd.seq, pages), (head, 4));
    assert_eq!(bd.store.len(), 3 + 25);

    // Unknown fields a newer client sends come back unchanged.
    let mut extra = Map::new();
    extra.insert("future_field".into(), json!({"kept": true}));
    let odd = Record { extra, ..sealed(&key, &acct, &mut a, "agents", "codex.mode", Some("MODE-X"), now_ms()) };
    push(&s, &a, vec![odd]).await;
    pull_all(&s, &mut bd, 100).await;
    assert_eq!(bd.store.get(&RecordId::new("agents", "codex.mode")).unwrap().extra["future_field"], json!({"kept": true}));

    // The export has the records, still sealed.
    let ex: Value = app().get(s.url("/v1/export")).bearer_auth(&a.at).send().await.unwrap().json().await.unwrap();
    assert_eq!(ex["records"].as_array().unwrap().len(), 3 + 25 + 1);
    assert!(ex["records"][0]["value"].as_str().unwrap().starts_with("v4.local."));

    // What the database holds: no value, no key.
    let dump = s.dump().await;
    assert!(dump.contains("claude.mode"), "the dump is real");
    for p in PLAIN.iter().chain(&["ENV-3", "MODE-X"]) {
        assert!(!dump.contains(p), "plaintext {p} in the database");
    }
    use base64::Engine;
    assert!(!dump.contains(&base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(key.bytes())));
    assert!(!dump.contains(&base64::engine::general_purpose::STANDARD.encode(key.bytes())));

    // The account page shows counts, not values.
    let page = b.get(s.url("/account")).send().await.unwrap().text().await.unwrap();
    assert!(page.contains("Encrypted") && page.contains("repos") && !page.contains(PLAIN[2]));
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
    let acct = account(&s1, &a).await;
    let key = AccountKey::generate();

    // B is connected to the other node.
    let mut ws = socket(&s2, &bd).await;
    assert_eq!(next_text(&mut ws).await, json!({"type": "advanced", "seq": 0}), "where the account is on connect");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(s2.state.hub.connected(), 1);

    let r = sealed(&key, &acct, &mut a, "agents", "claude.mode", Some(PLAIN[0]), now_ms());
    push(&s1, &a, vec![r]).await;
    assert_eq!(next_text(&mut ws).await, json!({"type": "advanced", "seq": 1}));
    let r = sealed(&key, &acct, &mut a, "agents", "claude.effort", Some("E"), now_ms());
    push(&s1, &a, vec![r]).await;
    assert_eq!(next_text(&mut ws).await, json!({"type": "advanced", "seq": 2}));
    pull_all(&s2, &mut bd, 100).await;
    assert_eq!(open(&key, &acct, &bd, "agents", "claude.mode").as_deref(), Some(PLAIN[0]));

    // A write that changes nothing sends nothing.
    let same = bd.store.get(&RecordId::new("agents", "claude.mode")).unwrap().clone();
    push(&s1, &a, vec![Record { seq: None, ..same }]).await;
    assert!(tokio::time::timeout(Duration::from_millis(500), ws.next()).await.is_err(), "no nudge for a no-op");

    // Reset: every device hears it and starts over.
    app().post(s1.url("/v1/sync/reset")).bearer_auth(&a.at).send().await.unwrap();
    assert_eq!(next_text(&mut ws).await, json!({"type": "reset"}));
    let r: PullResponse = app().get(s2.url("/v1/sync?since=0")).bearer_auth(&bd.at).send().await.unwrap().json().await.unwrap();
    assert!(r.records.is_empty());
    let r = sealed(&key, &acct, &mut a, "agents", "claude.mode", Some(PLAIN[1]), now_ms());
    let (_, p) = push(&s1, &a, vec![r]).await;
    assert_eq!(p["seq"], 3, "the sequence keeps counting after a reset");

    ws.send(Message::Close(None)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(s2.state.hub.connected(), 0, "gone when the socket closes");
    // Without a dino token there's no socket.
    let mut req = s2.url("/v1/sync/ws").replace("http://", "ws://").into_client_request().unwrap();
    req.headers_mut().insert("authorization", "Bearer dino_at_nope".parse().unwrap());
    assert!(tokio_tungstenite::connect_async(req).await.is_err());
    let _ = &mut a;
}

#[tokio::test]
async fn a_new_device_gets_the_key_by_approval_or_recovery_key() {
    let s = start().await;
    let b = browser();
    s.email_signin(&b, "approve@example.com").await;
    let mut a = device(&s, &b, "Mac A").await;
    let acct = account(&s, &a).await;
    let key = AccountKey::generate();
    let r = sealed(&key, &acct, &mut a, "keys", "NVIDIA_API_KEY", Some(PLAIN[2]), now_ms());
    push(&s, &a, vec![r]).await;

    // C asks, A claims: both screens show the same code before anything moves.
    let mut c = device(&s, &b, "Mac C").await;
    let c_keys = DeviceKeys::generate();
    let asked: Value = app().post(s.url("/v1/sync/approvals")).bearer_auth(&c.at).json(&json!({"public_key": c_keys.public()})).send().await.unwrap().json().await.unwrap();
    let id = asked["id"].as_str().unwrap().to_owned();
    let mine: Value = app().get(s.url("/v1/sync/approvals")).bearer_auth(&c.at).send().await.unwrap().json().await.unwrap();
    assert!(mine["approvals"].as_array().unwrap().is_empty(), "a device doesn't see its own request");
    let list: Value = app().get(s.url("/v1/sync/approvals")).bearer_auth(&a.at).send().await.unwrap().json().await.unwrap();
    let req = &list["approvals"][0];
    assert_eq!((req["device"]["name"].as_str(), req["public_key"].as_str()), (Some("Mac C"), Some(c_keys.public().as_str())));
    assert_eq!(app().post(s.url(&format!("/v1/sync/approvals/{id}/claim"))).bearer_auth(&c.at).json(&json!({"public_key": c_keys.public()})).send().await.unwrap().status(), 404, "can't approve itself");
    let a_keys = DeviceKeys::generate();
    app().post(s.url(&format!("/v1/sync/approvals/{id}/claim"))).bearer_auth(&a.at).json(&json!({"public_key": a_keys.public()})).send().await.unwrap();
    let seen: Value = app().get(s.url(&format!("/v1/sync/approvals/{id}"))).bearer_auth(&c.at).send().await.unwrap().json().await.unwrap();
    assert_eq!(seen["status"], "claimed");
    let approver = seen["approver_key"].as_str().unwrap().to_owned();
    assert_eq!(approval_code(&acct, &c_keys.public(), &approver), approval_code(&acct, &c_keys.public(), &a_keys.public()), "same code on both screens");

    // Only the claiming device can grant, and only from the key it claimed with.
    let g = a_keys.grant(&c_keys.public(), &acct, &key, &mut rand_core::OsRng).unwrap();
    let other = device(&s, &b, "Mac D").await;
    assert_eq!(app().post(s.url(&format!("/v1/sync/approvals/{id}/grant"))).bearer_auth(&other.at).json(&json!({"grant": g})).send().await.unwrap().status(), 404);
    let r = app().post(s.url(&format!("/v1/sync/approvals/{id}/grant"))).bearer_auth(&a.at).json(&json!({"grant": g})).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let done: Value = app().get(s.url(&format!("/v1/sync/approvals/{id}"))).bearer_auth(&c.at).send().await.unwrap().json().await.unwrap();
    assert_eq!(done["status"], "granted");
    let grant: Grant = serde_json::from_value(done["grant"].clone()).unwrap();
    let got = c_keys.accept(&grant, &approver, &acct).unwrap();
    assert_eq!(got.bytes(), key.bytes());
    pull_all(&s, &mut c, 100).await;
    assert_eq!(open(&got, &acct, &c, "keys", "NVIDIA_API_KEY").as_deref(), Some(PLAIN[2]));

    // Deny.
    let d_keys = DeviceKeys::generate();
    let asked: Value = app().post(s.url("/v1/sync/approvals")).bearer_auth(&other.at).json(&json!({"public_key": d_keys.public()})).send().await.unwrap().json().await.unwrap();
    let did = asked["id"].as_str().unwrap();
    app().post(s.url(&format!("/v1/sync/approvals/{did}/deny"))).bearer_auth(&a.at).send().await.unwrap();
    let denied: Value = app().get(s.url(&format!("/v1/sync/approvals/{did}"))).bearer_auth(&other.at).send().await.unwrap().json().await.unwrap();
    assert_eq!(denied["status"], "denied");
    assert_eq!(app().post(s.url("/v1/sync/approvals")).bearer_auth(&other.at).json(&json!({"public_key": "short"})).send().await.unwrap().status(), 400);

    // Recovery key: the account key wrapped by A, unwrapped by a device with the typed key.
    let rk = RecoveryKey::generate();
    let shown = rk.display();
    let wrapped = rk.wrap(&acct, &key).unwrap();
    assert_eq!(app().put(s.url("/v1/sync/recovery")).bearer_auth(&a.at).json(&json!({"wrapped": wrapped})).send().await.unwrap().status(), 200);
    assert_eq!(app().put(s.url("/v1/sync/recovery")).bearer_auth(&a.at).json(&json!({"wrapped": "plaintext"})).send().await.unwrap().status(), 400);
    let mut e = device(&s, &b, "Mac E").await;
    let back: Value = app().get(s.url("/v1/sync/recovery")).bearer_auth(&e.at).send().await.unwrap().json().await.unwrap();
    let typed = RecoveryKey::parse(&shown.to_lowercase()).unwrap();
    let got = typed.unwrap(&acct, back["wrapped"].as_str().unwrap()).unwrap();
    pull_all(&s, &mut e, 100).await;
    assert_eq!(open(&got, &acct, &e, "keys", "NVIDIA_API_KEY").as_deref(), Some(PLAIN[2]));

    // Neither the grant nor the wrapped key gave the server anything it can read.
    let dump = s.dump().await;
    use base64::Engine;
    for leak in [base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(key.bytes()), base64::engine::general_purpose::STANDARD.encode(key.bytes()), shown.clone(), shown.replace('-', ""), PLAIN[2].to_owned()] {
        assert!(!dump.contains(&leak), "the database holds something readable");
    }
    assert!(dump.contains(&c_keys.public()), "public keys are relayed, as designed");
}

#[tokio::test]
async fn concurrent_pushes_get_a_gapless_sequence() {
    let s = start().await;
    let b = browser();
    s.email_signin(&b, "busy-sync@example.com").await;
    let key = AccountKey::generate();
    let mut devices = Vec::new();
    for i in 0..5 {
        devices.push(device(&s, &b, &format!("Mac {i}")).await);
    }
    let acct = account(&s, &devices[0]).await;
    let batches: Vec<(String, String, Vec<Record>)> = devices
        .iter_mut()
        .enumerate()
        .map(|(i, d)| {
            let recs = (0..20).map(|j| sealed(&key, &acct, d, "repos", &format!("d{i}-r{j}"), Some("v"), now_ms())).collect();
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
