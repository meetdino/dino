//! Settings sync, speaking `dino-sync`'s protocol. The server orders and relays; it never decrypts.
//!
//! - `GET /v1/sync?since=<seq>`: records accepted after `since`, oldest first, a page at a time.
//! - `POST /v1/sync`: a batch of records. Per setting, the later HLC wins; the same record twice
//!   is accepted once; stamps too far in the future are refused. Every accepted write advances
//!   the account's sequence by one (writes to one account are serialized on its head row).
//! - `GET /v1/sync/ws`: "the sequence moved" nudges. Every node LISTENs on Postgres, so a push to
//!   any node reaches devices connected to any other.
//! - `/v1/sync/approvals…`: a new device and a signed-in one run `dino_sync::approval`'s
//!   commit-then-reveal exchange through here (ask with a commitment, `claim` with a response,
//!   `reveal`, `grant`), then the sealed grant of the account key. Each message is kept as sent.
//! - `/v1/sync/recovery`: the account key wrapped with the recovery key.
//! - `POST /v1/sync/reset`: wipes the records ("Reset sync"), for when every key is lost.
//!
//! Request-level refusals answer with a `dino_sync::SyncError` as the body.

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use dino_sync::record::{PROTOCOL, Rejection, check_push};
use dino_sync::approval::{Commitment, Response as ApprovalAnswer, Reveal};
use dino_sync::{Grant, Hlc, Nudge, PullResponse, PushRequest, PushResponse, Record, RecordId, SyncError};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::AppState;
use crate::api::Authed;
use crate::error::{Error, Result};

/// Most an account may store, over all its records.
pub const MAX_ACCOUNT_BYTES: i64 = 5 * 1024 * 1024;
const PAGE: i64 = 500;
const MAX_PAGE: i64 = 1000;
const MAX_NAME: usize = 200;
const APPROVAL_TTL: chrono::Duration = chrono::Duration::minutes(10);
pub const CHANNEL: &str = "dino_sync";

/// `push`: serve `/sync/ws`. Without it the route isn't there, and `/v1/meta` tells devices to look
/// on their own.
pub fn routes(push: bool) -> Router<AppState> {
    let r = Router::new()
        .route("/meta", get(meta))
        .route("/sync", get(pull).post(push_records).layer(DefaultBodyLimit::max(8 * 1024 * 1024)));
    let r = if push { r.route("/sync/ws", get(ws)) } else { r };
    r.route("/sync/reset", post(reset))
        .route("/sync/recovery", get(get_recovery).put(put_recovery))
        .route("/sync/approvals", get(list_approvals).post(request_approval))
        .route("/sync/approvals/{id}", get(get_approval))
        .route("/sync/approvals/{id}/claim", post(claim))
        .route("/sync/approvals/{id}/reveal", post(reveal))
        .route("/sync/approvals/{id}/grant", post(grant))
        .route("/sync/approvals/{id}/deny", post(deny))
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

type Row = (String, String, i64, i64, i64, String, i32, String, bool, Value);

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
        if !r.value.starts_with("v4.local.") {
            // Values (and deletes) are sealed on the device; anything else would be stored readable.
            out.rejected.push(malformed(id, "values must be PASETO v4.local tokens"));
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
        let size = r.value.len() as i64 + id.collection.len() as i64 + id.key.len() as i64;
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

/// A nudge that isn't a `Nudge` the protocol knows yet; older clients read it as `Unknown`.
async fn notify_raw(s: &AppState, account: Uuid, kind: &str) -> Result<()> {
    sqlx::query("SELECT pg_notify($1, $2)").bind(CHANNEL).bind(format!("{account} {}", json!({"type": kind}))).execute(&s.db).await?;
    Ok(())
}

async fn reset(State(s): State<AppState>, a: Authed) -> Result<Response> {
    let mut tx = s.db.begin().await?;
    sqlx::query("DELETE FROM sync_records WHERE account_id = $1").bind(a.account_id).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM sync_keys WHERE account_id = $1").bind(a.account_id).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM sync_approvals WHERE account_id = $1").bind(a.account_id).execute(&mut *tx).await?;
    // The sequence keeps counting up, so nothing a device saw before can be mistaken for new.
    // A new generation: devices that look rather than listen see it change in their next pull.
    sqlx::query("UPDATE sync_heads SET bytes = 0, generation = generation + 1, updated_at = now() WHERE account_id = $1").bind(a.account_id).execute(&mut *tx).await?;
    notify(&mut tx, a.account_id, &Nudge::Reset).await?;
    tx.commit().await?;
    Ok(Json(json!({"reset": true})).into_response())
}

#[derive(Deserialize)]
struct Wrapped {
    wrapped: String,
}

async fn put_recovery(State(s): State<AppState>, a: Authed, Json(w): Json<Wrapped>) -> Result<Response> {
    if !w.wrapped.starts_with("v4.local.") || w.wrapped.len() > 4096 {
        return Ok(refuse(StatusCode::BAD_REQUEST, SyncError::Malformed { reason: "the wrapped key must be a PASETO v4.local token".into() }));
    }
    sqlx::query("INSERT INTO sync_keys (account_id, wrapped_recovery) VALUES ($1, $2) ON CONFLICT (account_id) DO UPDATE SET wrapped_recovery = $2, updated_at = now()")
        .bind(a.account_id)
        .bind(&w.wrapped)
        .execute(&s.db)
        .await?;
    Ok(Json(json!({"stored": true})).into_response())
}

async fn get_recovery(State(s): State<AppState>, a: Authed) -> Result<Response> {
    let row: Option<(String, DateTime<Utc>)> = sqlx::query_as("SELECT wrapped_recovery, updated_at FROM sync_keys WHERE account_id = $1").bind(a.account_id).fetch_optional(&s.db).await?;
    let (wrapped, at) = row.ok_or(Error::NotFound)?;
    Ok(Json(json!({"wrapped": wrapped, "updated_at": at})).into_response())
}

/// A message of the approval exchange, kept as the device sent it. The server only checks it's the
/// right shape and size; it can't read anything in it that matters.
fn message<T: serde::de::DeserializeOwned + serde::Serialize>(v: &Value) -> Option<Value> {
    let parsed: T = serde_json::from_value(v.clone()).ok()?;
    let back = serde_json::to_value(parsed).ok()?;
    (back.to_string().len() <= 4096).then_some(back)
}

fn bad(what: &str) -> Response {
    refuse(StatusCode::BAD_REQUEST, SyncError::Malformed { reason: format!("{what} isn't what dino-sync sends") })
}

#[derive(Deserialize)]
struct AskBody {
    commitment: Value,
}

/// 1. A device without the key asks the account's other devices, committing to its key and nonce.
async fn request_approval(State(s): State<AppState>, a: Authed, Json(b): Json<AskBody>) -> Result<Response> {
    let device = match device(&a) {
        Ok(d) => d,
        Err(r) => return Ok(r),
    };
    let Some(commitment) = message::<Commitment>(&b.commitment) else { return Ok(bad("commitment")) };
    let id = Uuid::now_v7();
    let mut tx = s.db.begin().await?;
    // One open request per device: asking again replaces the last one.
    sqlx::query("DELETE FROM sync_approvals WHERE device_id = $1 AND status IN ('pending', 'responded', 'revealed')").bind(device).execute(&mut *tx).await?;
    let expires: (DateTime<Utc>,) = sqlx::query_as("INSERT INTO sync_approvals (id, account_id, device_id, commitment, expires_at) VALUES ($1, $2, $3, $4, now() + $5) RETURNING expires_at")
        .bind(id)
        .bind(a.account_id)
        .bind(device)
        .bind(commitment)
        .bind(APPROVAL_TTL)
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    notify_raw(&s, a.account_id, "approvals").await?;
    Ok(Json(json!({"id": id, "expires_at": expires.0})).into_response())
}

type ApprovalRow = (Uuid, Uuid, String, Value, Option<Value>, Option<Value>, Option<Value>, DateTime<Utc>, DateTime<Utc>, String, String);

const APPROVAL_SELECT: &str = "SELECT p.id, p.device_id, p.status, p.commitment, p.response, p.reveal, p.grant_body, p.created_at, p.expires_at, d.name, d.os
     FROM sync_approvals p JOIN devices d ON d.id = p.device_id";

fn approval_json(r: &ApprovalRow) -> Value {
    let (id, device, status, commitment, response, reveal, grant, created_at, expires_at, name, os) = r;
    json!({
        "id": id,
        "device": {"id": device, "name": name, "os": os},
        "status": status,
        "commitment": commitment,
        "response": response,
        "reveal": reveal,
        "grant": grant,
        "created_at": created_at,
        "expires_at": expires_at,
    })
}

/// Requests from the account's other devices, for a signed-in device to answer.
async fn list_approvals(State(s): State<AppState>, a: Authed) -> Result<Response> {
    let device = match device(&a) {
        Ok(d) => d,
        Err(r) => return Ok(r),
    };
    let rows: Vec<ApprovalRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{APPROVAL_SELECT} WHERE p.account_id = $1 AND p.device_id <> $2 AND p.status IN ('pending', 'responded', 'revealed')
           AND (p.approver_device IS NULL OR p.approver_device = $2) AND p.expires_at > now() AND d.revoked_at IS NULL ORDER BY p.created_at"
    )))
    .bind(a.account_id)
    .bind(device)
    .fetch_all(&s.db)
    .await?;
    Ok(Json(json!({"approvals": rows.iter().map(approval_json).collect::<Vec<_>>()})).into_response())
}

async fn load_approval(s: &AppState, account: Uuid, id: Uuid) -> Result<ApprovalRow> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!("{APPROVAL_SELECT} WHERE p.id = $1 AND p.account_id = $2 AND p.expires_at > now()")))
        .bind(id)
        .bind(account)
        .fetch_optional(&s.db)
        .await?
        .ok_or(Error::NotFound)
}

/// The asking device follows its request here (and, when granted, takes the sealed key).
async fn get_approval(State(s): State<AppState>, a: Authed, Path(id): Path<Uuid>) -> Result<Response> {
    Ok(Json(approval_json(&load_approval(&s, a.account_id, id).await?)).into_response())
}

#[derive(Deserialize)]
struct RespondBody {
    response: Value,
}

/// 2. A signed-in device takes the request, answering with its public key and a fresh nonce.
async fn claim(State(s): State<AppState>, a: Authed, Path(id): Path<Uuid>, Json(b): Json<RespondBody>) -> Result<Response> {
    let device = match device(&a) {
        Ok(d) => d,
        Err(r) => return Ok(r),
    };
    let Some(response) = message::<ApprovalAnswer>(&b.response) else { return Ok(bad("response")) };
    let changed = sqlx::query("UPDATE sync_approvals SET status = 'responded', approver_device = $3, response = $4 WHERE id = $1 AND account_id = $2 AND status = 'pending' AND device_id <> $3 AND expires_at > now()")
        .bind(id)
        .bind(a.account_id)
        .bind(device)
        .bind(response)
        .execute(&s.db)
        .await?
        .rows_affected();
    if changed == 0 {
        return Err(Error::NotFound);
    }
    notify_raw(&s, a.account_id, "approvals").await?;
    Ok(Json(approval_json(&load_approval(&s, a.account_id, id).await?)).into_response())
}

#[derive(Deserialize)]
struct RevealBody {
    reveal: Value,
}

/// 3. The asking device reveals what it committed to, once, after the answer arrived.
async fn reveal(State(s): State<AppState>, a: Authed, Path(id): Path<Uuid>, Json(b): Json<RevealBody>) -> Result<Response> {
    let device = match device(&a) {
        Ok(d) => d,
        Err(r) => return Ok(r),
    };
    let Some(reveal) = message::<Reveal>(&b.reveal) else { return Ok(bad("reveal")) };
    let changed = sqlx::query("UPDATE sync_approvals SET status = 'revealed', reveal = $4 WHERE id = $1 AND account_id = $2 AND device_id = $3 AND status = 'responded' AND expires_at > now()")
        .bind(id)
        .bind(a.account_id)
        .bind(device)
        .bind(reveal)
        .execute(&s.db)
        .await?
        .rows_affected();
    if changed == 0 {
        return Err(Error::NotFound);
    }
    notify_raw(&s, a.account_id, "approvals").await?;
    Ok(Json(json!({"revealed": true})).into_response())
}

#[derive(Deserialize)]
struct GrantBody {
    grant: Grant,
}

/// 4. The answering device, after the person compared codes, hands over the account key sealed to
/// the asking one. Only the device that answered, and only from the key it answered with.
async fn grant(State(s): State<AppState>, a: Authed, Path(id): Path<Uuid>, Json(b): Json<GrantBody>) -> Result<Response> {
    let device = match device(&a) {
        Ok(d) => d,
        Err(r) => return Ok(r),
    };
    let g = &b.grant;
    if g.sealed.len() > 4096 || g.nonce.len() > 64 {
        return Ok(refuse(StatusCode::BAD_REQUEST, SyncError::Malformed { reason: "grant too large".into() }));
    }
    let changed = sqlx::query("UPDATE sync_approvals SET status = 'granted', grant_body = $4 WHERE id = $1 AND account_id = $2 AND status = 'revealed' AND approver_device = $3 AND response->>'public' = $5 AND expires_at > now()")
        .bind(id)
        .bind(a.account_id)
        .bind(device)
        .bind(serde_json::to_value(g).map_err(anyhow::Error::from)?)
        .bind(&g.from)
        .execute(&s.db)
        .await?
        .rows_affected();
    if changed == 0 {
        return Err(Error::NotFound);
    }
    notify_raw(&s, a.account_id, "approvals").await?;
    Ok(Json(json!({"granted": true})).into_response())
}

async fn deny(State(s): State<AppState>, a: Authed, Path(id): Path<Uuid>) -> Result<Response> {
    let device = match device(&a) {
        Ok(d) => d,
        Err(r) => return Ok(r),
    };
    // A device that could answer it says no, or the asking device withdraws it.
    let changed = sqlx::query("UPDATE sync_approvals SET status = 'denied' WHERE id = $1 AND account_id = $2 AND status IN ('pending', 'responded', 'revealed')
           AND (device_id = $3 OR approver_device IS NULL OR approver_device = $3)")
        .bind(id)
        .bind(a.account_id)
        .bind(device)
        .execute(&s.db)
        .await?
        .rows_affected();
    if changed == 0 {
        return Err(Error::NotFound);
    }
    notify_raw(&s, a.account_id, "approvals").await?;
    Ok(Json(json!({"denied": true})).into_response())
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

/// What's synced, per collection, for the account page: counts, sizes and when. Never values.
pub async fn summary(s: &AppState, account: Uuid) -> Result<Vec<(String, i64, i64, DateTime<Utc>)>> {
    Ok(sqlx::query_as("SELECT collection, count(*), sum(bytes)::bigint, max(updated_at) FROM sync_records WHERE account_id = $1 AND NOT deleted GROUP BY collection ORDER BY collection")
        .bind(account)
        .fetch_all(&s.db)
        .await?)
}

/// Every record, sealed as stored, for the export.
pub async fn export(s: &AppState, account: Uuid) -> Result<Vec<Record>> {
    let rows: Vec<Row> = sqlx::query_as("SELECT collection, key, seq, hlc_wall, hlc_counter, hlc_device, schema, value, deleted, extra FROM sync_records WHERE account_id = $1 ORDER BY seq")
        .bind(account)
        .fetch_all(&s.db)
        .await?;
    Ok(rows.into_iter().map(record).collect())
}
