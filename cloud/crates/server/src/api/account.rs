//! What an account holds, and the things its owner can do to it, for the API and the web page.

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::AppState;
use crate::error::{Error, Result};
use crate::oauth::tokens;

/// How long a deleted account's rows are kept before they're erased.
pub const ERASE_AFTER: Duration = Duration::days(30);

#[derive(Serialize, sqlx::FromRow)]
pub struct Device {
    pub id: Uuid,
    pub client_id: String,
    pub name: String,
    pub os: String,
    pub dino_version: String,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

pub async fn summary(s: &AppState, account: Uuid) -> Result<Value> {
    let (email, verified, created_at): (String, bool, DateTime<Utc>) = sqlx::query_as("SELECT email, email_verified, created_at FROM accounts WHERE id = $1 AND deleted_at IS NULL")
        .bind(account)
        .fetch_optional(&s.db)
        .await?
        .ok_or(Error::Unauthorized)?;
    let identities: Vec<(String, Option<String>, DateTime<Utc>)> = sqlx::query_as("SELECT provider, email, created_at FROM identities WHERE account_id = $1 ORDER BY created_at").bind(account).fetch_all(&s.db).await?;
    Ok(json!({
        "account_id": account,
        "email": email,
        "email_verified": verified,
        "created_at": created_at,
        "identities": identities.iter().map(|(p, e, at)| json!({"provider": p, "email": e, "linked_at": at})).collect::<Vec<_>>(),
    }))
}

/// Devices signed in now (revoked ones drop off the list).
pub async fn devices(s: &AppState, account: Uuid) -> Result<Vec<Device>> {
    Ok(sqlx::query_as::<_, Device>(
        "SELECT id, client_id, name, os, dino_version, created_at, last_seen_at, revoked_at FROM devices
         WHERE account_id = $1 AND revoked_at IS NULL ORDER BY last_seen_at DESC",
    )
    .bind(account)
    .fetch_all(&s.db)
    .await?)
}

pub async fn revoke_device(s: &AppState, account: Uuid, device: Uuid) -> Result<()> {
    let mut tx = s.db.begin().await?;
    let owned: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM devices WHERE id = $1 AND account_id = $2").bind(device).bind(account).fetch_optional(&mut *tx).await?;
    if owned.is_none() {
        return Err(Error::NotFound);
    }
    tokens::revoke_device(&mut tx, device, "revoked_by_owner").await?;
    tx.commit().await?;
    Ok(())
}

pub async fn signout_everywhere(s: &AppState, account: Uuid) -> Result<()> {
    let mut tx = s.db.begin().await?;
    tokens::revoke_account(&mut tx, account, "signed_out_everywhere").await?;
    tx.commit().await?;
    Ok(())
}

/// Everything is revoked now; the rows are erased after [`ERASE_AFTER`] by the cleanup job.
pub async fn delete(s: &AppState, account: Uuid) -> Result<DateTime<Utc>> {
    let mut tx = s.db.begin().await?;
    sqlx::query("UPDATE accounts SET deleted_at = coalesce(deleted_at, now()) WHERE id = $1").bind(account).execute(&mut *tx).await?;
    tokens::revoke_account(&mut tx, account, "account_deleted").await?;
    tx.commit().await?;
    metrics::counter!("accounts_deleted_total").increment(1);
    Ok(Utc::now() + ERASE_AFTER)
}

/// Everything stored about the account, as JSON. Synced records join this once sync exists.
pub async fn export(s: &AppState, account: Uuid) -> Result<Value> {
    let mut v = summary(s, account).await?;
    let all: Vec<Device> = sqlx::query_as::<_, Device>(
        "SELECT id, client_id, name, os, dino_version, created_at, last_seen_at, revoked_at FROM devices WHERE account_id = $1 ORDER BY created_at",
    )
    .bind(account)
    .fetch_all(&s.db)
    .await?;
    v["devices"] = json!(all);
    v["records"] = json!([]);
    v["exported_at"] = json!(Utc::now());
    Ok(v)
}
