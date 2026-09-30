//! The device grant (RFC 8628), for `dino login --device` on a machine without a browser. Device
//! flow is the classic phishing lure (someone sends you their code), so, following RFC 8628 §5.4
//! and draft-ietf-oauth-cross-device-security:
//! - the consent page names the app and the device, shows the code large, and says where the
//!   request came from, warning when that isn't where you are;
//! - approving needs a sign-in from the last 10 minutes;
//! - codes are short-lived (10 minutes), single-use, from an alphabet without look-alikes, and
//!   code entry is rate limited per address.

use axum::extract::{Form, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect, Response};
use chrono::{DateTime, Duration, Utc};
use maud::html;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::AppState;
use crate::crypto;
use crate::error::{Error, Result};
use crate::limits::{self, ClientIp};
use crate::oauth::clients::{self, Client, Kind};
use crate::oauth::tokens::{self, DeviceInfo, TokenResponse};
use crate::web::pages;
use crate::web::session::Session;

const CODE_TTL: Duration = Duration::minutes(10);
const INTERVAL: i32 = 5;
const FRESH_MINUTES: i64 = 10;

#[derive(Deserialize)]
pub struct AuthorizationForm {
    client_id: String,
    scope: Option<String>,
    device_name: Option<String>,
    device_os: Option<String>,
    dino_version: Option<String>,
}

pub async fn authorization(State(s): State<AppState>, ClientIp(ip): ClientIp, Form(f): Form<AuthorizationForm>) -> Result<Response> {
    limits::auth(&s, ip)?;
    let client = clients::find(&f.client_id).filter(|c| c.kind == Kind::Native).ok_or_else(|| Error::oauth("invalid_client", "Unknown client."))?;
    let scope = clients::scope(f.scope.as_deref()).ok_or_else(|| Error::oauth("invalid_scope", "Unknown scope."))?;
    let device = DeviceInfo::new(f.device_name.as_deref(), f.device_os.as_deref(), f.dino_version.as_deref());
    let device_code = crypto::token("dino_dc");
    // A clash between live user codes is unlikely (20^8); try again if it happens.
    let mut user_code = crypto::user_code();
    for _ in 0..5 {
        let inserted = sqlx::query(
            "INSERT INTO device_codes (hash, user_code, client_id, scope, device_name, device_os, dino_version, requested_ip, expires_at, interval_s)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, now() + $9, $10) ON CONFLICT (user_code) DO NOTHING",
        )
        .bind(crypto::hash(&device_code))
        .bind(&user_code)
        .bind(client.id)
        .bind(&scope)
        .bind(&device.name)
        .bind(&device.os)
        .bind(&device.dino_version)
        .bind(ip.to_string())
        .bind(CODE_TTL)
        .bind(INTERVAL)
        .execute(&s.db)
        .await?
        .rows_affected();
        if inserted == 1 {
            let verification = s.cfg.url("/device");
            return Ok(axum::Json(json!({
                "device_code": device_code,
                "user_code": user_code,
                "verification_uri": verification,
                "verification_uri_complete": format!("{verification}?user_code={user_code}"),
                "expires_in": CODE_TTL.num_seconds(),
                "interval": INTERVAL,
            }))
            .into_response());
        }
        user_code = crypto::user_code();
    }
    Err(Error::Internal(anyhow::anyhow!("no free user code")))
}

/// The client polling `/oauth/token` with its device code.
pub async fn poll(s: &AppState, client: &Client, device_code: &str) -> Result<TokenResponse> {
    let hash = crypto::hash(device_code);
    let mut tx = s.db.begin().await?;
    let row: Option<(String, String, String, String, String, String, DateTime<Utc>, i32, Option<DateTime<Utc>>, Option<Uuid>)> = sqlx::query_as(
        "SELECT client_id, scope, device_name, device_os, dino_version, status, expires_at, interval_s, last_poll_at, account_id
         FROM device_codes WHERE hash = $1 FOR UPDATE",
    )
    .bind(&hash)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((client_id, scope, name, os, version, status, expires_at, interval, last_poll, account)) = row else {
        return Err(Error::oauth("invalid_grant", "Unknown device code."));
    };
    if client_id != client.id {
        return Err(Error::oauth("invalid_grant", "This code belongs to another app."));
    }
    if expires_at <= Utc::now() {
        return Err(Error::oauth("expired_token", "The code expired. Start again."));
    }
    // RFC 8628 §3.5: polling faster than the interval earns slow_down and a longer interval.
    if last_poll.is_some_and(|t| Utc::now() - t < Duration::seconds(interval as i64)) {
        sqlx::query("UPDATE device_codes SET interval_s = interval_s + 5, last_poll_at = now() WHERE hash = $1").bind(&hash).execute(&mut *tx).await?;
        tx.commit().await?;
        return Err(Error::oauth("slow_down", "Polling too fast."));
    }
    sqlx::query("UPDATE device_codes SET last_poll_at = now() WHERE hash = $1").bind(&hash).execute(&mut *tx).await?;
    let result = match (status.as_str(), account) {
        ("pending", _) => Err(Error::oauth("authorization_pending", "Waiting for approval.")),
        ("denied", _) => Err(Error::oauth("access_denied", "Sign-in was denied.")),
        ("approved", Some(account)) => {
            sqlx::query("UPDATE device_codes SET status = 'used' WHERE hash = $1").bind(&hash).execute(&mut *tx).await?;
            Ok(tokens::sign_in_device(&mut tx, account, client, &scope, &DeviceInfo::new(Some(&name), Some(&os), Some(&version))).await?)
        }
        _ => Err(Error::oauth("invalid_grant", "This code was already used.")),
    };
    tx.commit().await?;
    result
}

#[derive(Deserialize)]
pub struct EnterQuery {
    user_code: Option<String>,
}

/// The page a person opens on their phone or laptop to approve a device.
pub async fn enter(State(s): State<AppState>, headers: HeaderMap, Query(q): Query<EnterQuery>) -> Result<Response> {
    let mut session = Session::get(&s, &headers).await?;
    if session.account_id.is_none() {
        let back = match q.user_code.as_deref().and_then(crypto::normalize_user_code) {
            Some(c) => format!("/device?user_code={c}"),
            None => "/device".into(),
        };
        session.set("after_signin", json!(back));
        session.save(&s).await?;
        return Ok(session.attach(Redirect::to("/signin").into_response()));
    }
    let prefill = q.user_code.as_deref().and_then(crypto::normalize_user_code).unwrap_or_default();
    let page = pages::layout(&s, "Connect a device", html! {
        h1 { "Connect a device" }
        p { "Enter the code shown by " strong { "dino" } " on the device you're signing in." }
        form method="post" action="/device" class="stack" {
            (pages::csrf(&session.csrf(&s)))
            label for="user_code" { "Code" }
            input.codeinput #user_code type="text" name="user_code" value=(prefill) autocomplete="off" autocapitalize="characters" spellcheck="false" required placeholder="XXXX-XXXX";
            button.primary type="submit" { "Continue" }
        }
        p.muted { "Never enter a code someone else sent you." }
    });
    Ok(session.attach(page.into_response()))
}

#[derive(Deserialize)]
pub struct CodeForm {
    csrf: String,
    user_code: String,
}

type Pending = (String, String, String, String, String, DateTime<Utc>);

async fn find_pending(s: &AppState, code: &str) -> Result<Option<Pending>> {
    Ok(sqlx::query_as("SELECT client_id, device_name, device_os, dino_version, requested_ip, created_at FROM device_codes WHERE user_code = $1 AND status = 'pending' AND expires_at > now()")
        .bind(code)
        .fetch_optional(&s.db)
        .await?)
}

pub async fn lookup(State(s): State<AppState>, headers: HeaderMap, ClientIp(ip): ClientIp, Form(f): Form<CodeForm>) -> Result<Response> {
    limits::auth(&s, ip)?;
    let Some(session) = Session::load(&s, &headers).await? else { return Ok(Redirect::to("/device").into_response()) };
    if !session.check_csrf(&s, &f.csrf) {
        return Err(Error::Forbidden("The form expired. Go back and try again.".into()));
    }
    if session.account_id.is_none() {
        return Ok(Redirect::to("/device").into_response());
    }
    let not_found = || pages::message(&s, "Code not found", "Check the code on the device. Codes expire after 10 minutes.").into_response();
    let Some(code) = crypto::normalize_user_code(&f.user_code) else { return Ok(not_found()) };
    let Some((client_id, name, os, version, requested_ip, created_at)) = find_pending(&s, &code).await? else { return Ok(not_found()) };
    let client = clients::find(&client_id).expect("stored client");
    let elsewhere = requested_ip != ip.to_string();
    let minutes = (Utc::now() - created_at).num_minutes();
    let fresh = session.fresh(FRESH_MINUTES);
    let page = pages::layout(&s, "Approve device", html! {
        h1 { "Sign in " (client.name) " on this device?" }
        (pages::user_code(&code))
        p { "Check this matches the code on the device." }
        div.panel {
            div.row { span { "Device" } strong { (name) } }
            @if !os.is_empty() { div.row { span.muted { "System" } span.muted { (os) } } }
            @if !version.is_empty() { div.row { span.muted { "dino" } span.muted { (version) } } }
            div.row { span.muted { "Requested" } span.muted { @if minutes < 1 { "just now" } @else { (minutes) " min ago" } } }
        }
        @if elsewhere {
            div.panel.warn role="alert" {
                p { strong { "This request came from a different network than yours" } " (" (requested_ip) "). Approve only if the device is yours and in front of you." }
            }
        }
        @if fresh {
            form method="post" action="/device/decide" class="stack" {
                (pages::csrf(&session.csrf(&s)))
                input type="hidden" name="user_code" value=(code);
                button.primary type="submit" name="decision" value="approve" { "Approve" }
                button type="submit" name="decision" value="deny" { "Deny" }
            }
        } @else {
            p { "For your security, sign in again before approving." }
            a.btn.primary href=(format!("/signin?again=1&next=/device?user_code={code}")) { "Sign in again" }
        }
    });
    Ok(page.into_response())
}

#[derive(Deserialize)]
pub struct DecideForm {
    csrf: String,
    user_code: String,
    decision: String,
}

pub async fn decide(State(s): State<AppState>, headers: HeaderMap, ClientIp(ip): ClientIp, Form(f): Form<DecideForm>) -> Result<Response> {
    limits::auth(&s, ip)?;
    let Some(session) = Session::load(&s, &headers).await? else { return Err(Error::Forbidden("No session.".into())) };
    if !session.check_csrf(&s, &f.csrf) {
        return Err(Error::Forbidden("The form expired. Go back and try again.".into()));
    }
    let Some(account) = session.account_id else { return Ok(Redirect::to("/device").into_response()) };
    if !session.fresh(FRESH_MINUTES) {
        return Err(Error::Forbidden("Sign in again before approving.".into()));
    }
    let code = crypto::normalize_user_code(&f.user_code).ok_or(Error::NotFound)?;
    let approve = f.decision == "approve";
    let changed = sqlx::query("UPDATE device_codes SET status = $2, account_id = $3 WHERE user_code = $1 AND status = 'pending' AND expires_at > now()")
        .bind(&code)
        .bind(if approve { "approved" } else { "denied" })
        .bind(account)
        .execute(&s.db)
        .await?
        .rows_affected();
    if changed == 0 {
        return Ok(pages::message(&s, "Code not found", "It expired or was already used. Start again on the device.").into_response());
    }
    Ok(if approve {
        pages::message(&s, "Device connected", "You can go back to the device. It signs in by itself.")
    } else {
        pages::message(&s, "Denied", "The device wasn't signed in.")
    }
    .into_response())
}
