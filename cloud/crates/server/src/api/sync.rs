//! Settings sync, speaking `dino-sync`'s protocol. Values are plain JSON (settings, never secrets:
//! API keys and tokens stay on each Mac); the server orders, stores and relays them.
//!
//! - `GET /v1/sync?since=<seq>`: records accepted after `since`, oldest first, a page at a time.
//! - `POST /v1/sync`: a batch of records. Per setting, the later HLC wins; the same record twice
//!   is accepted once; stamps too far in the future are refused. Every accepted write advances
//!   the account's sequence by one (writes to one account are serialized on its head row).
//! - `GET /v1/sync/ws`: "the sequence moved" nudges. Every node LISTENs on Postgres, so a push to
//!   any node reaches devices connected to any other.
//!
//! Request-level refusals answer with a `dino_sync::SyncError` as the body.

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{DefaultBodyLimit, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use dino_sync::record::{PROTOCOL, Rejection, check_push};
use dino_sync::{Hlc, Nudge, PullResponse, PushRequest, PushResponse, Record, RecordId, SyncError};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::AppState;
use crate::api::Authed;
use crate::error::Result;

/// Most an account may store, over all its records.
pub const MAX_ACCOUNT_BYTES: i64 = 5 * 1024 * 1024;
const PAGE: i64 = 500;
const MAX_PAGE: i64 = 1000;
const MAX_NAME: usize = 200;
pub const CHANNEL: &str = "dino_sync";

/// `push`: serve `/sync/ws`. Without it the route isn't there, and `/v1/meta` tells devices to look
/// on their own.
pub fn routes(push: bool) -> Router<AppState> {
    let r = Router::new()
        .route("/meta", get(meta))
        .route("/sync", get(pull).post(push_records).layer(DefaultBodyLimit::max(8 * 1024 * 1024)));
    if push { r.route("/sync/ws", get(ws)) } else { r }
}

fn refuse(status: StatusCode, e: SyncError) -> Response {
    (status, Json(e)).into_response()
}

/// `DINO-Sync-Version`: a client older than the protocol this server serves is told to update.
fn check_version(headers: &HeaderMap) -> Option<Response> {
    let v: u32 = headers.get("dino-sync-version")?.to_str().ok()?.parse().ok()?;
    (v < PROTOCOL).then(|| refuse(StatusCode::UPGRADE_REQUIRED, SyncError::UpgradeRequired { min: PROTOCOL }))
}

fn device(a: &Authed) -> std::result::Result<Uuid, Response> {
    a.device_id.ok_or_else(|| refuse(StatusCode::UNAUTHORIZED, SyncError::UnknownDevice))
}

#[derive(Deserialize)]
struct PullQuery {
    since: Option<u64>,
    limit: Option<i64>,
}

type Row = (String, String, i64, i64, i64, String, i32, Value, bool, Value);

fn record(r: Row) -> Record {
    let (collection, key, seq, wall, counter, dev, schema, value, deleted, extra) = r;
    Record {
        id: RecordId { collection, key },
        hlc: Hlc { wall_ms: wall as u64, counter: counter as u32, device: dev },
        schema: schema as u32,
        value,
        deleted,
        seq: Some(seq as u64),
        extra: match extra {
            Value::Object(m) => m,
            _ => Map::new(),
        },
    }
}

async fn pull(State(s): State<AppState>, a: Authed, headers: HeaderMap, Query(q): Query<PullQuery>) -> Result<Response> {
    if let Some(r) = check_version(&headers) {
        return Ok(r);
    }
    if let Err(r) = device(&a) {
        return Ok(r);
    }
    let since = q.since.unwrap_or(0);
    let limit = q.limit.unwrap_or(PAGE).clamp(1, MAX_PAGE);
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT collection, key, seq, hlc_wall, hlc_counter, hlc_device, schema, value, deleted, extra FROM sync_records
         WHERE account_id = $1 AND seq > $2 ORDER BY seq LIMIT $3",
    )
    .bind(a.account_id)
    .bind(since as i64)
    .bind(limit + 1)
    .fetch_all(&s.db)
    .await?;
    let more = rows.len() as i64 > limit;
    let records: Vec<Record> = rows.into_iter().take(limit as usize).map(record).collect();
    let seq = records.last().and_then(|r| r.seq).unwrap_or(since);
    // Which reset of this account's sync the records belong to (an extra field: older devices
    // ignore it and keep relying on the Reset nudge).
    let generation: i64 = sqlx::query_scalar("SELECT generation FROM sync_heads WHERE account_id = $1").bind(a.account_id).fetch_optional(&s.db).await?.unwrap_or(0);
    let mut extra = Map::new();
    extra.insert("generation".into(), json!(generation));
    Ok(Json(PullResponse { seq, records, more, extra }).into_response())
}

fn malformed(id: &RecordId, reason: &str) -> Rejection {
    Rejection { id: id.clone(), error: SyncError::Malformed { reason: reason.into() } }
}

/// `GET /v1/meta`: what this server offers, before signing in. Without push, devices look every
/// `poll_secs` (and sooner when something happens on their side).
async fn meta(State(s): State<AppState>) -> Response {
    Json(json!({"protocol": PROTOCOL, "push": s.cfg.platform.push, "poll_secs": 60})).into_response()
}

async fn push_records(State(s): State<AppState>, a: Authed, headers: HeaderMap, Json(req): Json<PushRequest>) -> Result<Response> {
    if let Some(r) = check_version(&headers) {
        return Ok(r);
    }
    let device = match device(&a) {
        Ok(d) => d,
        Err(r) => return Ok(r),
    };
    if let Err(e) = check_push(&req) {
        return Ok(refuse(StatusCode::PAYLOAD_TOO_LARGE, e));
    }
    if let Some(n) = NonZeroU32::new(req.records.len() as u32) {
        if let Err(retry) = crate::limits::sync_writes(&s, device, n).await? {
            return Ok(refuse(StatusCode::TOO_MANY_REQUESTS, SyncError::RateLimited { retry_after_s: retry }));
        }
    }
    let now_ms = Utc::now().timestamp_millis().max(0) as u64;
    let mut tx = s.db.begin().await?;
    // The head row serializes writes to one account: sequence numbers come out in commit order
    // with no gaps, so a device that pulled up to N has seen everything up to N.
    sqlx::query("INSERT INTO sync_heads (account_id) VALUES ($1) ON CONFLICT DO NOTHING").bind(a.account_id).execute(&mut *tx).await?;
    let (mut seq, mut bytes): (i64, i64) = sqlx::query_as("SELECT seq, bytes FROM sync_heads WHERE account_id = $1 FOR UPDATE").bind(a.account_id).fetch_one(&mut *tx).await?;
    let start = seq;
    let mut out = PushResponse::default();
    for r in &req.records {
        let id = &r.id;
        if id.collection.is_empty() || id.key.is_empty() || id.collection.len() > MAX_NAME || id.key.len() > MAX_NAME || r.hlc.device.len() > MAX_NAME {
            out.rejected.push(malformed(id, "collection, key and device must be 1 to 200 bytes"));
            continue;
        }
        if r.deleted != r.value.is_null() {
            out.rejected.push(malformed(id, "a delete has a null value, and only a delete"));
            continue;
        }
        if let Some(ahead_ms) = r.hlc.too_far_ahead(now_ms) {
            out.rejected.push(Rejection { id: id.clone(), error: SyncError::FutureStamp { ahead_ms } });
            continue;
        }
        let stored: Option<(i64, i64, String, i32)> = sqlx::query_as("SELECT hlc_wall, hlc_counter, hlc_device, bytes FROM sync_records WHERE account_id = $1 AND collection = $2 AND key = $3")
            .bind(a.account_id)
            .bind(&id.collection)
            .bind(&id.key)
            .fetch_optional(&mut *tx)
            .await?;
        let old_bytes = stored.as_ref().map_or(0, |s| s.3 as i64);
        if let Some((wall, counter, dev, _)) = stored {
            let theirs = Hlc { wall_ms: wall as u64, counter: counter as u32, device: dev };
            if theirs == r.hlc {
                // The same write again (a retry): already here.
                out.accepted.push(id.clone());
                continue;
            }
            if theirs > r.hlc {
                out.superseded.push(id.clone());
                continue;
            }
        }
        let size = r.value.to_string().len() as i64 + id.collection.len() as i64 + id.key.len() as i64;
        if bytes - old_bytes + size > MAX_ACCOUNT_BYTES {
            out.rejected.push(Rejection { id: id.clone(), error: SyncError::TooLarge { bytes: (bytes - old_bytes + size) as usize, max: MAX_ACCOUNT_BYTES as usize } });
            continue;
        }
        seq += 1;
        bytes += size - old_bytes;
        sqlx::query(
            "INSERT INTO sync_records (account_id, collection, key, seq, hlc_wall, hlc_counter, hlc_device, schema, value, extra, bytes, device_id, deleted)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
             ON CONFLICT (account_id, collection, key) DO UPDATE SET seq = $4, hlc_wall = $5, hlc_counter = $6, hlc_device = $7,
               schema = $8, value = $9, extra = $10, bytes = $11, device_id = $12, deleted = $13, updated_at = now()",
        )
        .bind(a.account_id)
        .bind(&id.collection)
        .bind(&id.key)
        .bind(seq)
        .bind(r.hlc.wall_ms as i64)
        .bind(r.hlc.counter as i64)
        .bind(&r.hlc.device)
        .bind(r.schema as i32)
        .bind(&r.value)
        .bind(Value::Object(r.extra.clone()))
        .bind(size as i32)
        .bind(device)
        .bind(r.deleted)
        .execute(&mut *tx)
        .await?;
        out.accepted.push(id.clone());
    }
    if seq != start {
        sqlx::query("UPDATE sync_heads SET seq = $2, bytes = $3, updated_at = now() WHERE account_id = $1").bind(a.account_id).bind(seq).bind(bytes).execute(&mut *tx).await?;
        notify(&mut tx, a.account_id, &Nudge::Advanced { seq: seq as u64 }).await?;
    }
    tx.commit().await?;
    metrics::counter!("sync_records_accepted_total").increment((seq - start) as u64);
    out.seq = seq as u64;
    Ok(Json(out).into_response())
}

/// Tells every node (and so every connected device of the account) once the transaction commits.
async fn notify(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, account: Uuid, nudge: &Nudge) -> Result<()> {
    let payload = format!("{account} {}", serde_json::to_string(nudge).map_err(anyhow::Error::from)?);
    sqlx::query("SELECT pg_notify($1, $2)").bind(CHANNEL).bind(payload).execute(&mut **tx).await?;
    Ok(())
}


/// Connected devices on this node, by account. Nudges are hints (a device also pulls on wake and
/// every few minutes), so a slow socket just misses some rather than holding anything up.
#[derive(Default)]
pub struct Hub {
    conns: Mutex<HashMap<Uuid, Vec<(u64, mpsc::Sender<String>)>>>,
    next: AtomicU64,
}

impl Hub {
    fn join(&self, account: Uuid) -> (u64, mpsc::Receiver<String>) {
        let (tx, rx) = mpsc::channel(32);
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        self.conns.lock().unwrap().entry(account).or_default().push((id, tx));
        metrics::gauge!("sync_sockets").increment(1.0);
        (id, rx)
    }

    fn leave(&self, account: Uuid, id: u64) {
        let mut conns = self.conns.lock().unwrap();
        if let Some(v) = conns.get_mut(&account) {
            v.retain(|(i, _)| *i != id);
            if v.is_empty() {
                conns.remove(&account);
            }
        }
        metrics::gauge!("sync_sockets").decrement(1.0);
    }

    fn publish(&self, account: Uuid, message: &str) {
        if let Some(v) = self.conns.lock().unwrap().get(&account) {
            for (_, tx) in v {
                let _ = tx.try_send(message.to_owned());
            }
        }
    }

    pub fn connected(&self) -> usize {
        self.conns.lock().unwrap().values().map(Vec::len).sum()
    }
}

/// LISTENs for nudges from every node and hands them to this node's sockets. Reconnects with
/// backoff if Postgres goes away.
pub fn listen(state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut backoff = Duration::from_millis(250);
        loop {
            match sqlx::postgres::PgListener::connect_with(&state.db).await {
                Ok(mut l) => {
                    if l.listen(CHANNEL).await.is_ok() {
                        backoff = Duration::from_millis(250);
                        while let Ok(n) = l.recv().await {
                            if let Some((account, msg)) = n.payload().split_once(' ') {
                                if let Ok(account) = account.parse() {
                                    state.hub.publish(account, msg);
                                }
                            }
                        }
                    }
                }
                Err(e) => tracing::warn!(error = %e, "sync listener can't connect"),
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_secs(10));
        }
    })
}

async fn ws(State(s): State<AppState>, a: Authed, up: WebSocketUpgrade) -> Response {
    let Some(device) = a.device_id else { return refuse(StatusCode::UNAUTHORIZED, SyncError::UnknownDevice) };
    up.on_upgrade(move |socket| serve_socket(s, a.account_id, device, socket))
}

async fn serve_socket(s: AppState, account: Uuid, device: Uuid, socket: WebSocket) {
    let (conn, mut rx) = s.hub.join(account);
    let (mut out, mut inbound) = socket.split();
    // Where the account is now, so a device that was offline knows at once whether to pull.
    let head: Option<(i64,)> = sqlx::query_as("SELECT seq FROM sync_heads WHERE account_id = $1").bind(account).fetch_optional(&s.db).await.unwrap_or(None);
    let hello = serde_json::to_string(&Nudge::Advanced { seq: head.map_or(0, |h| h.0 as u64) }).unwrap_or_default();
    let mut alive = out.send(Message::Text(hello.into())).await.is_ok();
    let mut ping = tokio::time::interval(Duration::from_secs(30));
    let mut check = tokio::time::interval(Duration::from_secs(60));
    ping.tick().await;
    check.tick().await;
    while alive {
        tokio::select! {
            m = rx.recv() => match m {
                Some(m) => alive = out.send(Message::Text(m.into())).await.is_ok(),
                None => alive = false,
            },
            m = inbound.next() => alive = matches!(m, Some(Ok(_))) && !matches!(m, Some(Ok(Message::Close(_)))),
            _ = ping.tick() => alive = out.send(Message::Ping(Vec::new().into())).await.is_ok(),
            _ = check.tick() => {
                // A device signed out or an account deleted loses its socket within a minute.
                let ok: Option<(Uuid,)> = sqlx::query_as("SELECT d.id FROM devices d JOIN accounts a ON a.id = d.account_id WHERE d.id = $1 AND d.revoked_at IS NULL AND a.deleted_at IS NULL")
                    .bind(device).fetch_optional(&s.db).await.unwrap_or(None);
                if ok.is_none() {
                    let _ = out.send(Message::Close(None)).await;
                    alive = false;
                }
            }
        }
    }
    s.hub.leave(account, conn);
}

/// Every synced setting, for the account page: where it lives, its value and when it changed.
pub async fn settings(s: &AppState, account: Uuid) -> Result<Vec<(String, String, Value, DateTime<Utc>)>> {
    Ok(sqlx::query_as("SELECT collection, key, value, updated_at FROM sync_records WHERE account_id = $1 AND NOT deleted ORDER BY collection, key")
        .bind(account)
        .fetch_all(&s.db)
        .await?)
}

/// Every record, for the export.
pub async fn export(s: &AppState, account: Uuid) -> Result<Vec<Record>> {
    let rows: Vec<Row> = sqlx::query_as("SELECT collection, key, seq, hlc_wall, hlc_counter, hlc_device, schema, value, deleted, extra FROM sync_records WHERE account_id = $1 ORDER BY seq")
        .bind(account)
        .fetch_all(&s.db)
        .await?;
    Ok(rows.into_iter().map(record).collect())
}
