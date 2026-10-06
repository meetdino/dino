//! `/v1`: the signed-in account and settings sync for dino (bearer access tokens with audience
//! `dino`). Devices, sign-out everywhere, export and deletion are on the account page, behind a
//! browser sign-in and its CSRF checks, not open to a device's token.

pub mod account;
pub mod sync;

use axum::extract::{FromRequestParts, State};
use axum::http::request::Parts;
use axum::routing::get;
use axum::{Json, Router};
use uuid::Uuid;

use crate::AppState;
use crate::error::{Error, Result};
use crate::limits;
use crate::oauth::{self, tokens};

pub fn routes(push: bool) -> Router<AppState> {
    Router::new()
        .route("/me", get(me))
        .merge(sync::routes(push))
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
