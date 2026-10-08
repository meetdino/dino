//! Authorization code with PKCE. A native client opens the browser here with a loopback
//! redirect; after sign-in the person confirms on a page naming the device (a local program can't
//! quietly borrow a signed-in browser), and the code goes back to the loopback listener.
//!
//! With `provider=github` it's one click, like VS Code's "Sign in with GitHub": straight to
//! GitHub's authorize page, and back to the app once GitHub says who it is, without our sign-in
//! or confirm pages. GitHub's own page (or its remembered consent for dino) stands in for the
//! confirmation.

use axum::extract::{Form, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect, Response};
use chrono::{DateTime, Duration, Utc};
use maud::html;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::AppState;
use crate::crypto;
use crate::error::{Error, Result};
use crate::identity::upstream::Provider;
use crate::oauth::clients::{self, Client};
use crate::oauth::tokens::{self, DeviceInfo, TokenResponse};
use crate::web::pages;
use crate::web::session::Session;

const CODE_TTL: Duration = Duration::seconds(60);

#[derive(Deserialize, Serialize, Clone)]
pub struct Params {
    response_type: Option<String>,
    client_id: Option<String>,
    redirect_uri: Option<String>,
    state: Option<String>,
    code_challenge: Option<String>,
    code_challenge_method: Option<String>,
    scope: Option<String>,
    device_name: Option<String>,
    device_os: Option<String>,
    dino_version: Option<String>,
    /// `github`: sign in with GitHub, without dino's own sign-in and confirm pages.
    provider: Option<String>,
}

/// Back to the client with an error, once its redirect is known to be its own.
fn error_redirect(s: &AppState, redirect: &str, state: Option<&str>, error: &str, description: &str) -> Response {
    let mut u = url::Url::parse(redirect).expect("checked");
    {
        let mut q = u.query_pairs_mut();
        q.append_pair("error", error).append_pair("error_description", description);
        if let Some(st) = state {
            q.append_pair("state", st);
        }
        q.append_pair("iss", s.cfg.public_url.as_str().trim_end_matches('/'));
    }
    Redirect::to(u.as_str()).into_response()
}

pub async fn start(State(s): State<AppState>, headers: HeaderMap, Query(p): Query<Params>) -> Result<Response> {
    // An unknown client or a redirect it didn't register gets a page, never a redirect (RFC 6749 §4.1.2.1).
    let Some(client) = p.client_id.as_deref().and_then(clients::find) else {
        return Ok(pages::message(&s, "Unknown app", "This sign-in link isn't from an app dino knows.").into_response());
    };
    let Some(redirect) = p.redirect_uri.clone().filter(|r| client.allows_redirect(r)) else {
        return Ok(pages::message(&s, "Sign-in link not valid", "The app asked to return somewhere it isn't allowed to.").into_response());
    };
    let st = p.state.as_deref();
    if p.response_type.as_deref() != Some("code") {
        return Ok(error_redirect(&s, &redirect, st, "unsupported_response_type", "Only response_type=code is supported."));
    }
    if p.code_challenge_method.as_deref() != Some("S256") || p.code_challenge.as_deref().is_none_or(|c| c.len() != 43) {
        return Ok(error_redirect(&s, &redirect, st, "invalid_request", "PKCE with S256 is required."));
    }
    if clients::scope(p.scope.as_deref()).is_none() {
        return Ok(error_redirect(&s, &redirect, st, "invalid_scope", "Unknown scope."));
    }
    let mut session = Session::get(&s, &headers).await?;
    session.set("authorize", serde_json::to_value(&p).map_err(anyhow::Error::from)?);
    let one_click = p.provider.as_deref().and_then(Provider::parse).filter(|pr| pr.config(&s).is_some());
    if let Some(pr) = one_click {
        session.set("after_signin", json!("/oauth/authorize/confirm"));
        session.set("one_click", json!(true));
        session.save(&s).await?;
        return Ok(session.attach(Redirect::to(&format!("/signin/{}", pr.id())).into_response()));
    }
    session.take("one_click");
    let to = if session.account_id.is_some() {
        "/oauth/authorize/confirm"
    } else {
        session.set("after_signin", json!("/oauth/authorize/confirm"));
        "/signin"
    };
    session.save(&s).await?;
    Ok(session.attach(Redirect::to(to).into_response()))
}

fn pending(session: &Session) -> Option<(Params, &'static Client)> {
    let p: Params = serde_json::from_value(session.data.get("authorize")?.clone()).ok()?;
    let client = clients::find(p.client_id.as_deref()?)?;
    Some((p, client))
}

pub async fn confirm(State(s): State<AppState>, headers: HeaderMap) -> Result<Response> {
    let Some(session) = Session::load(&s, &headers).await? else { return Ok(Redirect::to("/signin").into_response()) };
    if session.account_id.is_none() {
        return Ok(Redirect::to("/signin").into_response());
    }
    let Some((p, client)) = pending(&session) else {
        return Ok(pages::message(&s, "Nothing to confirm", "Start signing in from dino again.").into_response());
    };
    let device = DeviceInfo::new(p.device_name.as_deref(), p.device_os.as_deref(), p.dino_version.as_deref());
    let email: (String,) = sqlx::query_as("SELECT email FROM accounts WHERE id = $1").bind(session.account_id).fetch_one(&s.db).await?;
    let page = pages::layout(
        &s,
        "Continue",
        html! {
            h1 { "Sign in to " (client.name) "?" }
            p { "As " strong { (email.0) } "." }
            div.panel {
                div.row { span { "Device" } strong { (device.name) } }
                @if !device.os.is_empty() { div.row { span.muted { "System" } span.muted { (device.os) } } }
                @if !device.dino_version.is_empty() { div.row { span.muted { "dino" } span.muted { (device.dino_version) } } }
            }
            p.muted { "Only continue if you just asked " (client.name) " to sign in on this device." }
            form method="post" action="/oauth/authorize/decide" class="stack" {
                (pages::csrf(&session.csrf(&s)))
                button.primary type="submit" name="decision" value="allow" { "Continue" }
                button type="submit" name="decision" value="deny" { "Cancel" }
            }
        },
    );
    Ok(page.into_response())
}

#[derive(Deserialize)]
pub struct Decision {
    csrf: String,
    decision: String,
}

pub async fn decide(State(s): State<AppState>, headers: HeaderMap, Form(d): Form<Decision>) -> Result<Response> {
    let Some(mut session) = Session::load(&s, &headers).await? else { return Err(Error::Forbidden("No session.".into())) };
    if !session.check_csrf(&s, &d.csrf) {
        return Err(Error::Forbidden("The form expired. Go back and try again.".into()));
    }
    let Some(account) = session.account_id else { return Ok(Redirect::to("/signin").into_response()) };
    let Some((p, client)) = pending(&session) else {
        return Ok(pages::message(&s, "Nothing to confirm", "Start signing in from dino again.").into_response());
    };
    session.take("authorize");
    session.save(&s).await?;
    let redirect = p.redirect_uri.clone().expect("checked at start");
    if !client.allows_redirect(&redirect) {
        return Err(Error::BadRequest("redirect".into()));
    }
    if d.decision != "allow" {
        return Ok(error_redirect(&s, &redirect, p.state.as_deref(), "access_denied", "Cancelled."));
    }
    issue(&s, account, &p, client).await
}

/// After a one-click sign-in (`provider`): the code goes straight back to the app, which started
/// this sign-in a moment ago in the same browser session.
pub async fn finish_one_click(s: &AppState, session: &mut Session, account: Uuid) -> Result<Option<Response>> {
    if session.take("one_click").is_none() {
        return Ok(None);
    }
    let Some((p, client)) = pending(session) else { return Ok(None) };
    session.take("authorize");
    if !p.redirect_uri.as_deref().is_some_and(|r| client.allows_redirect(r)) {
        return Err(Error::BadRequest("redirect".into()));
    }
    Ok(Some(issue(s, account, &p, client).await?))
}

/// A code for `account`, back to the client's redirect.
async fn issue(s: &AppState, account: Uuid, p: &Params, client: &'static Client) -> Result<Response> {
    let redirect = p.redirect_uri.clone().expect("checked at start");
    let code = crypto::token("dino_ac");
    let device = DeviceInfo::new(p.device_name.as_deref(), p.device_os.as_deref(), p.dino_version.as_deref());
    sqlx::query(
        "INSERT INTO auth_codes (hash, client_id, redirect_uri, code_challenge, account_id, scope, device_name, device_os, dino_version, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, now() + $10)",
    )
    .bind(crypto::hash(&code))
    .bind(client.id)
    .bind(&redirect)
    .bind(p.code_challenge.as_deref().unwrap_or_default())
    .bind(account)
    .bind(clients::scope(p.scope.as_deref()).unwrap_or_else(|| "account".into()))
    .bind(&device.name)
    .bind(&device.os)
    .bind(&device.dino_version)
    .bind(CODE_TTL)
    .execute(&s.db)
    .await?;
    let mut u = url::Url::parse(&redirect).expect("checked");
    {
        let mut q = u.query_pairs_mut();
        q.append_pair("code", &code);
        if let Some(st) = &p.state {
            q.append_pair("state", st);
        }
        // RFC 9207: says which server the code came from, against mix-up attacks.
        q.append_pair("iss", s.cfg.public_url.as_str().trim_end_matches('/'));
    }
    Ok(Redirect::to(u.as_str()).into_response())
}

/// `grant_type=authorization_code`: one use, within a minute, same client and redirect, and the
/// verifier that matches the challenge.
pub async fn exchange(s: &AppState, client: &Client, code: &str, redirect: &str, verifier: &str) -> Result<TokenResponse> {
    let invalid = || Error::oauth("invalid_grant", "The code isn't valid.");
    let mut tx = s.db.begin().await?;
    let row: Option<(String, String, String, Uuid, String, String, String, String, DateTime<Utc>, Option<DateTime<Utc>>)> = sqlx::query_as(
        "SELECT client_id, redirect_uri, code_challenge, account_id, scope, device_name, device_os, dino_version, expires_at, used_at
         FROM auth_codes WHERE hash = $1 FOR UPDATE",
    )
    .bind(crypto::hash(code))
    .fetch_optional(&mut *tx)
    .await?;
    let Some((client_id, redirect_uri, challenge, account, scope, name, os, version, expires_at, used_at)) = row else { return Err(invalid()) };
    if used_at.is_some() {
        // RFC 6749 §4.1.2: a code used twice revokes what it gave out.
        let device: (Option<Uuid>,) = sqlx::query_as("SELECT device_id FROM auth_codes WHERE hash = $1").bind(crypto::hash(code)).fetch_one(&mut *tx).await?;
        if let Some(d) = device.0 {
            tokens::revoke_device(&mut tx, d, "code_reuse").await?;
        }
        tx.commit().await?;
        return Err(invalid());
    }
    sqlx::query("UPDATE auth_codes SET used_at = now() WHERE hash = $1").bind(crypto::hash(code)).execute(&mut *tx).await?;
    if client_id != client.id || redirect_uri != redirect || expires_at <= Utc::now() || !crypto::pkce_matches(verifier, &challenge) {
        tx.commit().await?;
        return Err(invalid());
    }
    let deleted: (Option<DateTime<Utc>>,) = sqlx::query_as("SELECT deleted_at FROM accounts WHERE id = $1").bind(account).fetch_one(&mut *tx).await?;
    if deleted.0.is_some() {
        tx.commit().await?;
        return Err(invalid());
    }
    let res = tokens::sign_in_device(&mut tx, account, client, &scope, &DeviceInfo::new(Some(&name), Some(&os), Some(&version))).await?;
    sqlx::query("UPDATE auth_codes SET device_id = $2 WHERE hash = $1").bind(crypto::hash(code)).bind(res.device_id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(res)
}
