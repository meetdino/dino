//! `/v1`: the account API and settings sync for dino (bearer access tokens with audience `dino`).

pub mod account;
pub mod idempotency;
pub mod sync;

use axum::extract::{FromRequestParts, Path, State};
use axum::http::HeaderMap;
use axum::http::request::Parts;
use axum::response::Response;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use uuid::Uuid;

use crate::AppState;
use crate::error::{Error, Result};
use crate::limits;
use crate::oauth::{self, tokens};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/me", get(me))
        .route("/devices", get(devices))
        .route("/devices/{id}", delete(revoke_device))
        .route("/signout-everywhere", post(signout_everywhere))
        .route("/account", delete(delete_account))
        .route("/export", get(export))
        .merge(sync::routes())
}

/// A request made with a live dino access token.
pub struct Authed {
    pub account_id: Uuid,
    pub device_id: Option<Uuid>,
    pub scope: String,
}

impl FromRequestParts<AppState> for Authed {
    type Rejection = Error;

    async fn from_request_parts(parts: &mut Parts, s: &AppState) -> Result<Self> {
        let token = oauth::bearer(&parts.headers).ok_or(Error::Unauthorized)?;
        let b = tokens::lookup_access(s, token).await?.ok_or(Error::Unauthorized)?;
        // Tokens minted for the harness don't open dino's account API.
        if b.aud != "dino" {
            return Err(Error::Unauthorized);
        }
        limits::account(s, b.account_id)?;
        if let (Some(d), true) = (b.device_id, b.seen_stale) {
            // At most every five minutes per device, so most requests are a single read.
            sqlx::query("UPDATE devices SET last_seen_at = now() WHERE id = $1").bind(d).execute(&s.db).await?;
        }
        Ok(Authed { account_id: b.account_id, device_id: b.device_id, scope: b.scope })
    }
}

async fn me(State(s): State<AppState>, a: Authed) -> Result<Json<serde_json::Value>> {
    let mut v = account::summary(&s, a.account_id).await?;
    v["device_id"] = serde_json::json!(a.device_id);
    v["scope"] = serde_json::json!(a.scope);
    Ok(Json(v))
}

async fn devices(State(s): State<AppState>, a: Authed) -> Result<Json<serde_json::Value>> {
    Ok(Json(serde_json::json!({ "devices": account::devices(&s, a.account_id).await? })))
}

async fn revoke_device(State(s): State<AppState>, a: Authed, headers: HeaderMap, Path(id): Path<Uuid>) -> Result<Response> {
    idempotency::run(&s, &a.account_id.to_string(), &headers, "DELETE /v1/devices", async {
        account::revoke_device(&s, a.account_id, id).await?;
        Ok(serde_json::json!({"revoked": id}))
    })
    .await
}

async fn signout_everywhere(State(s): State<AppState>, a: Authed, headers: HeaderMap) -> Result<Response> {
    idempotency::run(&s, &a.account_id.to_string(), &headers, "POST /v1/signout-everywhere", async {
        account::signout_everywhere(&s, a.account_id).await?;
        Ok(serde_json::json!({"signed_out": true}))
    })
    .await
}

async fn delete_account(State(s): State<AppState>, a: Authed, headers: HeaderMap) -> Result<Response> {
    idempotency::run(&s, &a.account_id.to_string(), &headers, "DELETE /v1/account", async {
        let at = account::delete(&s, a.account_id).await?;
        Ok(serde_json::json!({"deleted": true, "erased_after": at}))
    })
    .await
}

async fn export(State(s): State<AppState>, a: Authed) -> Result<Json<serde_json::Value>> {
    Ok(Json(account::export(&s, a.account_id).await?))
}
