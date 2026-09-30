//! `Idempotency-Key` for mutations: a retried request with the same key gets the first answer back
//! instead of running again. Answers are kept 24 hours, per account (or per address before sign-in).

use axum::Json;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::AppState;
use crate::error::{Error, Result};

pub async fn run<F>(s: &AppState, scope: &str, headers: &HeaderMap, route: &str, f: F) -> Result<Response>
where
    F: Future<Output = Result<serde_json::Value>>,
{
    let key = headers.get("idempotency-key").and_then(|v| v.to_str().ok()).map(str::trim).filter(|k| !k.is_empty());
    let Some(key) = key else { return f.await.map(|v| Json(v).into_response()) };
    if key.len() > 200 {
        return Err(Error::BadRequest("Idempotency-Key is too long.".into()));
    }
    let seen: Option<(i16, Vec<u8>)> = sqlx::query_as("SELECT status, body FROM idempotency WHERE scope = $1 AND key = $2 AND route = $3 AND created_at > now() - interval '24 hours'")
        .bind(scope)
        .bind(key)
        .bind(route)
        .fetch_optional(&s.db)
        .await?;
    if let Some((status, body)) = seen {
        let mut r = (StatusCode::from_u16(status as u16).unwrap_or(StatusCode::OK), body).into_response();
        r.headers_mut().insert("content-type", "application/json".parse().unwrap());
        r.headers_mut().insert("idempotent-replayed", "true".parse().unwrap());
        return Ok(r);
    }
    let (status, body) = match f.await {
        Ok(v) => (StatusCode::OK, serde_json::to_vec(&v).map_err(anyhow::Error::from)?),
        // Client errors are part of the answer; server errors aren't remembered, so a retry runs again.
        Err(Error::NotFound) => (StatusCode::NOT_FOUND, br#"{"error":"not_found"}"#.to_vec()),
        Err(e) => return Err(e),
    };
    sqlx::query("INSERT INTO idempotency (scope, key, route, status, body) VALUES ($1, $2, $3, $4, $5) ON CONFLICT DO NOTHING")
        .bind(scope)
        .bind(key)
        .bind(route)
        .bind(status.as_u16() as i16)
        .bind(&body)
        .execute(&s.db)
        .await?;
    let mut r = (status, body).into_response();
    r.headers_mut().insert("content-type", "application/json".parse().unwrap());
    Ok(r)
}
