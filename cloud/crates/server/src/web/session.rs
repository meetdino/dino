//! Browser sessions for the server's own pages. A session is created on first visit to hold the
//! CSRF token and whatever sign-in was asked for; signing in replaces it with a new id (no session
//! fixation). Only the id's hash is stored.

use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::Response;
use chrono::{DateTime, Duration, Utc};
use serde_json::Value;
use uuid::Uuid;

use crate::AppState;
use crate::crypto;
use crate::error::Result;

pub struct Session {
    hash: Vec<u8>,
    pub account_id: Option<Uuid>,
    pub authed_at: Option<DateTime<Utc>>,
    pub data: Value,
    /// A cookie to send: the session is new or was replaced.
    set_cookie: Option<HeaderValue>,
}

fn cookie_name(state: &AppState) -> &'static str {
    // `__Host-` makes browsers refuse the cookie unless it's Secure, host-only and on `/`.
    if state.cfg.secure_cookies() { "__Host-dino_session" } else { "dino_session" }
}

fn cookie(state: &AppState, token: &str, max_age: Duration) -> HeaderValue {
    let secure = if state.cfg.secure_cookies() { "; Secure" } else { "" };
    format!("{}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}{secure}", cookie_name(state), max_age.num_seconds()).parse().expect("cookie is ascii")
}

fn from_headers(state: &AppState, headers: &HeaderMap) -> Option<String> {
    let name = cookie_name(state);
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_owned())
}

const ANONYMOUS_FOR: i64 = 1; // days
const SIGNED_IN_FOR: i64 = 30;

impl Session {
    /// The browser's session, if it has a live one.
    pub async fn load(state: &AppState, headers: &HeaderMap) -> Result<Option<Session>> {
        let Some(token) = from_headers(state, headers) else { return Ok(None) };
        let hash = crypto::hash(&token);
        let row: Option<(Option<Uuid>, Option<DateTime<Utc>>, Value)> = sqlx::query_as(
            "SELECT s.account_id, s.authed_at, s.data FROM web_sessions s
             LEFT JOIN accounts a ON a.id = s.account_id
             WHERE s.hash = $1 AND s.expires_at > now() AND (s.account_id IS NULL OR a.deleted_at IS NULL)",
        )
        .bind(&hash)
        .fetch_optional(&state.db)
        .await?;
        Ok(row.map(|(account_id, authed_at, data)| Session { hash, account_id, authed_at, data, set_cookie: None }))
    }

    /// The browser's session, starting one if it has none.
    pub async fn get(state: &AppState, headers: &HeaderMap) -> Result<Session> {
        if let Some(s) = Self::load(state, headers).await? {
            return Ok(s);
        }
        Self::create(state, None, Value::Object(Default::default())).await
    }

    async fn create(state: &AppState, account_id: Option<Uuid>, data: Value) -> Result<Session> {
        let token = crypto::token("dino_ws");
        let hash = crypto::hash(&token);
        let ttl = Duration::days(if account_id.is_some() { SIGNED_IN_FOR } else { ANONYMOUS_FOR });
        let authed_at = account_id.map(|_| Utc::now());
        sqlx::query("INSERT INTO web_sessions (hash, account_id, authed_at, data, expires_at) VALUES ($1, $2, $3, $4, now() + $5)")
            .bind(&hash)
            .bind(account_id)
            .bind(authed_at)
            .bind(&data)
            .bind(ttl)
            .execute(&state.db)
            .await?;
        Ok(Session { hash, account_id, authed_at, data, set_cookie: Some(cookie(state, &token, ttl)) })
    }

    /// Signed in as `account`: a new session id replaces this one, keeping what it carried.
    pub async fn sign_in(self, state: &AppState, account: Uuid) -> Result<Session> {
        let mut data = self.data.clone();
        if let Some(o) = data.as_object_mut() {
            o.remove("oauth_state");
            o.remove("email");
        }
        sqlx::query("DELETE FROM web_sessions WHERE hash = $1").bind(&self.hash).execute(&state.db).await?;
        Self::create(state, Some(account), data).await
    }

    pub async fn sign_out(self, state: &AppState) -> Result<HeaderValue> {
        sqlx::query("DELETE FROM web_sessions WHERE hash = $1").bind(&self.hash).execute(&state.db).await?;
        Ok(cookie(state, "", Duration::zero()))
    }

    pub fn get_str(&self, key: &str) -> Option<String> {
        self.data.get(key).and_then(Value::as_str).map(str::to_owned)
    }

    pub fn set(&mut self, key: &str, value: Value) {
        if let Some(o) = self.data.as_object_mut() {
            o.insert(key.to_owned(), value);
        }
    }

    pub fn take(&mut self, key: &str) -> Option<Value> {
        self.data.as_object_mut().and_then(|o| o.remove(key))
    }

    pub async fn save(&self, state: &AppState) -> Result<()> {
        sqlx::query("UPDATE web_sessions SET data = $2 WHERE hash = $1").bind(&self.hash).bind(&self.data).execute(&state.db).await?;
        Ok(())
    }

    /// Signed in within `minutes`: approving a device asks for a recent sign-in.
    pub fn fresh(&self, minutes: i64) -> bool {
        self.authed_at.is_some_and(|t| Utc::now() - t < Duration::minutes(minutes))
    }

    /// The token forms carry, tied to this session.
    pub fn csrf(&self, state: &AppState) -> String {
        crypto::b64url(&crypto::hmac(&state.cfg.secret, "csrf", &self.hash))
    }

    pub fn check_csrf(&self, state: &AppState, token: &str) -> bool {
        crypto::eq(self.csrf(state).as_bytes(), token.as_bytes())
    }

    /// `res` with this session's cookie, if one needs sending.
    pub fn attach(&self, mut res: Response) -> Response {
        if let Some(c) = &self.set_cookie {
            res.headers_mut().append(header::SET_COOKIE, c.clone());
        }
        res
    }
}
