//! The OAuth 2 authorization server: authorization code with PKCE for the app, the CLI and the
//! harness (loopback redirects, RFC 8252), the device grant for machines without a browser
//! (RFC 8628), rotating refresh tokens, revocation (RFC 7009) and metadata (RFC 8414).

pub mod authorize;
pub mod clients;
pub mod device;
pub mod link;
pub mod tokens;

use axum::extract::{Form, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

use crate::AppState;
use crate::crypto;
use crate::error::{Error, Result};
use crate::limits::{self, ClientIp};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/.well-known/oauth-authorization-server", get(metadata))
        .route("/oauth/authorize", get(authorize::start))
        .route("/oauth/authorize/confirm", get(authorize::confirm))
        .route("/oauth/authorize/decide", post(authorize::decide))
        .route("/oauth/token", post(token))
        .route("/oauth/device_authorization", post(device::authorization))
        .route("/device", get(device::enter).post(device::lookup))
        .route("/device/decide", post(device::decide))
        .route("/login/{token}", get(link::page).post(link::open))
        .route("/oauth/revoke", post(revoke))
}

async fn metadata(State(s): State<AppState>) -> Json<serde_json::Value> {
    let u = |p: &str| s.cfg.url(p);
    Json(json!({
        "issuer": s.cfg.public_url.as_str().trim_end_matches('/'),
        "authorization_endpoint": u("/oauth/authorize"),
        "token_endpoint": u("/oauth/token"),
        "device_authorization_endpoint": u("/oauth/device_authorization"),
        "revocation_endpoint": u("/oauth/revoke"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token", "urn:ietf:params:oauth:grant-type:device_code"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "scopes_supported": clients::SCOPES,
        "authorization_response_iss_parameter_supported": true,
        "subject_types_supported": ["public"],
    }))
}

#[derive(Deserialize)]
pub struct TokenForm {
    grant_type: String,
    client_id: Option<String>,
    code: Option<String>,
    redirect_uri: Option<String>,
    code_verifier: Option<String>,
    refresh_token: Option<String>,
    device_code: Option<String>,
}

fn no_store(body: impl serde::Serialize) -> Response {
    let mut r = Json(body).into_response();
    r.headers_mut().insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    r.headers_mut().insert(header::PRAGMA, "no-cache".parse().unwrap());
    r
}

async fn token(State(s): State<AppState>, ClientIp(ip): ClientIp, Form(f): Form<TokenForm>) -> Result<Response> {
    let client = f.client_id.as_deref().and_then(clients::find).ok_or_else(|| Error::oauth("invalid_client", "Unknown client."))?;
    match f.grant_type.as_str() {
        "authorization_code" => {
            limits::auth(&s, ip).await?;
            let (Some(code), Some(redirect), Some(verifier)) = (f.code, f.redirect_uri, f.code_verifier) else {
                return Err(Error::oauth("invalid_request", "code, redirect_uri and code_verifier are required."));
            };
            Ok(no_store(authorize::exchange(&s, client, &code, &redirect, &verifier).await?))
        }
        "refresh_token" => {
            let rt = f.refresh_token.ok_or_else(|| Error::oauth("invalid_request", "refresh_token is required."))?;
            Ok(no_store(tokens::refresh(&s, client, &rt).await?))
        }
        "urn:ietf:params:oauth:grant-type:device_code" => {
            let dc = f.device_code.ok_or_else(|| Error::oauth("invalid_request", "device_code is required."))?;
            Ok(no_store(device::poll(&s, client, &dc).await?))
        }
        _ => Err(Error::oauth("unsupported_grant_type", "Use authorization_code, refresh_token or the device code grant.")),
    }
}

#[derive(Deserialize)]
struct RevokeForm {
    token: String,
    client_id: Option<String>,
}

/// RFC 7009: a refresh token signs its device out; an access token just stops working. Always
/// 200, whether or not the token was known.
async fn revoke(State(s): State<AppState>, ClientIp(ip): ClientIp, Form(f): Form<RevokeForm>) -> Result<StatusCode> {
    limits::auth(&s, ip).await?;
    let hash = crypto::hash(&f.token);
    let mut tx = s.db.begin().await?;
    let device: Option<(uuid::Uuid, String)> = sqlx::query_as("SELECT r.device_id, d.client_id FROM refresh_tokens r JOIN devices d ON d.id = r.device_id WHERE r.hash = $1").bind(&hash).fetch_optional(&mut *tx).await?;
    if let Some((device, client_id)) = device {
        if f.client_id.as_deref().is_none_or(|c| c == client_id) {
            tokens::revoke_device(&mut tx, device, "signed_out").await?;
        }
    } else {
        sqlx::query("DELETE FROM access_tokens WHERE hash = $1").bind(&hash).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(StatusCode::OK)
}

pub fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers.get(header::AUTHORIZATION)?.to_str().ok()?.strip_prefix("Bearer ").map(str::trim)
}
