//! Access and refresh tokens.
//!
//! Access tokens are opaque: 256 random bits, looked up by hash on each request. That makes
//! "sign out everywhere", revoking a device and deleting an account take effect on the next
//! request, with no signing keys to manage.
//!
//! Refresh tokens rotate on every use (RFC 9700 §4.14.2), one family per device. A token that was
//! already exchanged is accepted again only within [`GRACE`], and then yields the same replacement
//! (it's derived from the old one), so a client that lost an answer isn't signed out. After that,
//! presenting it means two parties hold the family: the whole device is signed out.

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::AppState;
use crate::crypto;
use crate::error::{Error, Result};
use crate::oauth::clients::Client;

pub const ACCESS_TTL: Duration = Duration::minutes(15);
pub const REFRESH_TTL: Duration = Duration::days(30);
pub const GRACE: Duration = Duration::seconds(30);

#[derive(Serialize, Debug)]
pub struct TokenResponse {
    pub access_token: String,
    pub token_type: &'static str,
    pub expires_in: i64,
    pub refresh_token: String,
    pub scope: String,
    /// Which device this sign-in is, so the client can show it and revoke it later.
    pub device_id: Uuid,
}

pub struct DeviceInfo {
    pub name: String,
    pub os: String,
    pub dino_version: String,
}

impl DeviceInfo {
    pub fn new(name: Option<&str>, os: Option<&str>, version: Option<&str>) -> Self {
        let clip = |s: Option<&str>, n: usize, default: &str| s.map(str::trim).filter(|s| !s.is_empty()).unwrap_or(default).chars().filter(|c| !c.is_control()).take(n).collect();
        DeviceInfo { name: clip(name, 80, "Unknown device"), os: clip(os, 40, ""), dino_version: clip(version, 40, "") }
    }
}

/// A new sign-in: a device row, the first refresh token of its family and an access token.
pub async fn sign_in_device(tx: &mut Transaction<'_, Postgres>, account: Uuid, client: &Client, scope: &str, device: &DeviceInfo) -> Result<TokenResponse> {
    let device_id = Uuid::now_v7();
    sqlx::query("INSERT INTO devices (id, account_id, client_id, scope, name, os, dino_version) VALUES ($1, $2, $3, $4, $5, $6, $7)")
        .bind(device_id)
        .bind(account)
        .bind(client.id)
        .bind(scope)
        .bind(&device.name)
        .bind(&device.os)
        .bind(&device.dino_version)
        .execute(&mut **tx)
        .await?;
    let refresh = crypto::token("dino_rt");
    insert_refresh(tx, &refresh, device_id, account).await?;
    let access = insert_access(tx, account, Some(device_id), client, scope).await?;
    metrics::counter!("tokens_issued_total", "grant" => "sign_in", "client" => client.id).increment(1);
    Ok(TokenResponse { access_token: access, token_type: "Bearer", expires_in: ACCESS_TTL.num_seconds(), refresh_token: refresh, scope: scope.to_owned(), device_id })
}

async fn insert_refresh(tx: &mut Transaction<'_, Postgres>, token: &str, device: Uuid, account: Uuid) -> Result<()> {
    sqlx::query("INSERT INTO refresh_tokens (hash, device_id, account_id, expires_at) VALUES ($1, $2, $3, now() + $4) ON CONFLICT (hash) DO NOTHING")
        .bind(crypto::hash(token))
        .bind(device)
        .bind(account)
        .bind(REFRESH_TTL)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn insert_access(tx: &mut Transaction<'_, Postgres>, account: Uuid, device: Option<Uuid>, client: &Client, scope: &str) -> Result<String> {
    let token = crypto::token("dino_at");
    sqlx::query("INSERT INTO access_tokens (hash, account_id, device_id, client_id, aud, scope, expires_at) VALUES ($1, $2, $3, $4, $5, $6, now() + $7)")
        .bind(crypto::hash(&token))
        .bind(account)
        .bind(device)
        .bind(client.id)
        .bind(client.aud)
        .bind(scope)
        .bind(ACCESS_TTL)
        .execute(&mut **tx)
        .await?;
    Ok(token)
}

/// Sign a device out: its row is marked, every token of its family goes.
pub async fn revoke_device(tx: &mut Transaction<'_, Postgres>, device: Uuid, reason: &str) -> Result<()> {
    sqlx::query("UPDATE devices SET revoked_at = coalesce(revoked_at, now()), revoke_reason = coalesce(revoke_reason, $2) WHERE id = $1")
        .bind(device)
        .bind(reason)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM refresh_tokens WHERE device_id = $1").bind(device).execute(&mut **tx).await?;
    sqlx::query("DELETE FROM access_tokens WHERE device_id = $1").bind(device).execute(&mut **tx).await?;
    Ok(())
}

/// Every device and token of an account.
pub async fn revoke_account(tx: &mut Transaction<'_, Postgres>, account: Uuid, reason: &str) -> Result<()> {
    sqlx::query("UPDATE devices SET revoked_at = now(), revoke_reason = $2 WHERE account_id = $1 AND revoked_at IS NULL")
        .bind(account)
        .bind(reason)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM refresh_tokens WHERE account_id = $1").bind(account).execute(&mut **tx).await?;
    sqlx::query("DELETE FROM access_tokens WHERE account_id = $1").bind(account).execute(&mut **tx).await?;
    sqlx::query("DELETE FROM web_sessions WHERE account_id = $1").bind(account).execute(&mut **tx).await?;
    Ok(())
}

/// `grant_type=refresh_token`.
pub async fn refresh(state: &AppState, client: &Client, presented: &str) -> Result<TokenResponse> {
    let invalid = || Error::oauth("invalid_grant", "The refresh token isn't valid. Sign in again.");
    let mut tx = state.db.begin().await?;
    // Lock the row so two refreshes of the same token can't both rotate it.
    let row: Option<(Uuid, Uuid, DateTime<Utc>, Option<DateTime<Utc>>, String, String, Option<DateTime<Utc>>, Option<DateTime<Utc>>)> = sqlx::query_as(
        "SELECT r.device_id, r.account_id, r.expires_at, r.rotated_at, d.client_id, d.scope, d.revoked_at, a.deleted_at
         FROM refresh_tokens r JOIN devices d ON d.id = r.device_id JOIN accounts a ON a.id = r.account_id
         WHERE r.hash = $1 FOR UPDATE OF r",
    )
    .bind(crypto::hash(presented))
    .fetch_optional(&mut *tx)
    .await?;
    let Some((device, account, expires_at, rotated_at, client_id, scope, revoked_at, deleted_at)) = row else { return Err(invalid()) };
    if client_id != client.id || revoked_at.is_some() || deleted_at.is_some() || expires_at <= Utc::now() {
        return Err(invalid());
    }
    let next = crypto::next_refresh(&state.cfg.secret, presented);
    match rotated_at {
        None => {
            sqlx::query("UPDATE refresh_tokens SET rotated_at = now() WHERE hash = $1").bind(crypto::hash(presented)).execute(&mut *tx).await?;
            insert_refresh(&mut tx, &next, device, account).await?;
        }
        Some(at) if Utc::now() - at <= GRACE => {
            // Asked again right after rotating: hand back the same replacement, unless that one
            // has itself moved on already.
            let child: Option<(Option<DateTime<Utc>>,)> = sqlx::query_as("SELECT rotated_at FROM refresh_tokens WHERE hash = $1").bind(crypto::hash(&next)).fetch_optional(&mut *tx).await?;
            if !matches!(child, Some((None,))) {
                return reuse(state, tx, device).await;
            }
        }
        Some(_) => return reuse(state, tx, device).await,
    }
    sqlx::query("UPDATE devices SET last_seen_at = now() WHERE id = $1").bind(device).execute(&mut *tx).await?;
    let access = insert_access(&mut tx, account, Some(device), client, &scope).await?;
    tx.commit().await?;
    metrics::counter!("tokens_issued_total", "grant" => "refresh_token", "client" => client.id).increment(1);
    Ok(TokenResponse { access_token: access, token_type: "Bearer", expires_in: ACCESS_TTL.num_seconds(), refresh_token: next, scope, device_id: device })
}

/// A spent refresh token came back: someone else has the family. Sign the device out.
async fn reuse(_state: &AppState, mut tx: Transaction<'_, Postgres>, device: Uuid) -> Result<TokenResponse> {
    revoke_device(&mut tx, device, "refresh_token_reuse").await?;
    tx.commit().await?;
    metrics::counter!("refresh_reuse_total").increment(1);
    tracing::warn!(%device, "refresh token reused: device signed out");
    Err(Error::oauth("invalid_grant", "This sign-in was used from somewhere else, so it was signed out. Sign in again."))
}

/// The account behind a bearer access token, for `/v1`.
#[derive(Clone, Debug)]
pub struct Bearer {
    pub account_id: Uuid,
    pub device_id: Option<Uuid>,
    pub client_id: String,
    pub aud: String,
    pub scope: String,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    /// The device's last-seen time is older than five minutes: worth writing now.
    pub seen_stale: bool,
}

pub async fn lookup_access(state: &AppState, token: &str) -> Result<Option<Bearer>> {
    let row: Option<(Uuid, Option<Uuid>, String, String, String, DateTime<Utc>, DateTime<Utc>, bool)> = sqlx::query_as(
        "SELECT t.account_id, t.device_id, t.client_id, t.aud, t.scope, t.expires_at, t.created_at, coalesce(d.last_seen_at < now() - interval '5 minutes', false)
         FROM access_tokens t JOIN accounts a ON a.id = t.account_id LEFT JOIN devices d ON d.id = t.device_id
         WHERE t.hash = $1 AND t.expires_at > now() AND a.deleted_at IS NULL AND d.revoked_at IS NULL",
    )
    .bind(crypto::hash(token))
    .fetch_optional(&state.db)
    .await?;
    Ok(row.map(|(account_id, device_id, client_id, aud, scope, expires_at, created_at, seen_stale)| Bearer { account_id, device_id, client_id, aud, scope, expires_at, created_at, seen_stale }))
}
