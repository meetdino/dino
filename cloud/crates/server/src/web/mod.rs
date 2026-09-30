//! The pages people see: sign-in (GitHub, Google, emailed code), the account page, and the
//! device-approval placeholder. Forms carry a CSRF token tied to the session and are checked
//! against the Origin header too.

pub mod pages;
pub mod session;

use axum::extract::{Form, Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use maud::html;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::AppState;
use crate::api::account;
use crate::crypto;
use crate::error::{Error, Result};
use crate::identity::{self, email, upstream::Provider};
use crate::limits::{self, ClientIp};
use session::Session;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(|| async { Redirect::to("/account") }))
        .route("/static/app.css", get(css))
        .route("/signin", get(signin))
        .route("/signin/email", post(email_send))
        .route("/signin/email/code", get(email_code_page).post(email_verify))
        .route("/signin/{provider}", get(provider_start))
        .route("/signin/{provider}/callback", get(provider_callback))
        .route("/signout", post(signout))
        .route("/account", get(account_page))
        .route("/account/devices/{id}/revoke", post(account_revoke))
        .route("/account/signout-everywhere", post(account_signout_everywhere))
        .route("/account/delete", post(account_delete))
        .route("/account/export", get(account_export))
        .route("/approve", get(approve))
}

async fn css() -> Response {
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8"), (header::CACHE_CONTROL, "public, max-age=3600")], pages::CSS).into_response()
}

/// Security headers on every answer. The CSP allows only this server's stylesheet, forms that
/// post here or land on a loopback redirect (native sign-in), and Turnstile when it's on.
pub async fn security_headers(State(s): State<AppState>, req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    let turnstile = if s.cfg.turnstile.is_some() { " https://challenges.cloudflare.com" } else { "" };
    let csp = format!(
        "default-src 'none'; style-src 'self'; img-src 'self' data:; script-src 'self'{turnstile}; frame-src{f}; connect-src 'self'{turnstile}; \
         form-action 'self' http://127.0.0.1:*; frame-ancestors 'none'; base-uri 'none'",
        f = if turnstile.is_empty() { " 'none'" } else { turnstile }
    );
    let h = res.headers_mut();
    let mut set = |k: &'static str, v: &str| {
        if let Ok(v) = HeaderValue::from_str(v) {
            h.insert(k, v);
        }
    };
    set("content-security-policy", &csp);
    set("x-content-type-options", "nosniff");
    set("x-frame-options", "DENY");
    set("referrer-policy", "no-referrer");
    set("cross-origin-opener-policy", "same-origin");
    set("permissions-policy", "camera=(), microphone=(), geolocation=()");
    if s.cfg.secure_cookies() {
        set("strict-transport-security", "max-age=63072000; includeSubDomains");
    }
    res
}

/// A path on this server, for redirects after sign-in (no open redirects).
fn local(path: &str) -> Option<String> {
    (path.starts_with('/') && !path.starts_with("//") && !path.contains('\\') && !path.contains("://")).then(|| path.to_owned())
}

/// Forms must come from this server's own pages: a matching CSRF token, and an Origin (when the
/// browser sends one) that is ours.
fn check_form(s: &AppState, session: &Session, headers: &HeaderMap, token: &str) -> Result<()> {
    if let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        if origin != "null" && origin.trim_end_matches('/') != s.cfg.public_url.as_str().trim_end_matches('/') {
            return Err(Error::Forbidden("Cross-site request refused.".into()));
        }
    }
    if !session.check_csrf(s, token) {
        return Err(Error::Forbidden("The form expired. Go back and try again.".into()));
    }
    Ok(())
}

#[derive(Deserialize)]
struct SigninQuery {
    next: Option<String>,
    again: Option<String>,
}

async fn signin(State(s): State<AppState>, headers: HeaderMap, Query(q): Query<SigninQuery>) -> Result<Response> {
    let mut session = Session::get(&s, &headers).await?;
    if let Some(next) = q.next.as_deref().and_then(local) {
        session.set("after_signin", json!(next));
        session.save(&s).await?;
    }
    if session.account_id.is_some() && q.again.is_none() {
        let to = session.get_str("after_signin").unwrap_or_else(|| "/account".into());
        return Ok(session.attach(Redirect::to(&to).into_response()));
    }
    let providers: Vec<Provider> = [Provider::GitHub, Provider::Google].into_iter().filter(|p| p.config(&s).is_some()).collect();
    let page = pages::layout(&s, "Sign in", html! {
        h1 { @if q.again.is_some() { "Sign in again" } @else { "Sign in to dino" } }
        p { "Your settings follow you to every Mac, encrypted so only your devices can read them." }
        div.stack {
            @for p in &providers {
                a.btn href=(format!("/signin/{}", p.id())) { "Continue with " (p.name()) }
            }
        }
        @if !providers.is_empty() { p.or { "or" } }
        form method="post" action="/signin/email" class="stack" {
            (pages::csrf(&session.csrf(&s)))
            label for="email" { "Email" }
            input #email type="email" name="email" autocomplete="email" required;
            (pages::turnstile(&s))
            button.primary type="submit" { "Email me a code" }
        }
    });
    Ok(session.attach(page.into_response()))
}

async fn provider_start(State(s): State<AppState>, headers: HeaderMap, Path(provider): Path<String>) -> Result<Response> {
    let p = Provider::parse(&provider).filter(|p| p.config(&s).is_some()).ok_or(Error::NotFound)?;
    let mut session = Session::get(&s, &headers).await?;
    let state = crypto::b64url(&crypto::random_bytes::<24>());
    let verifier = crypto::b64url(&crypto::random_bytes::<32>());
    let challenge = crypto::b64url(&{
        use sha2::Digest;
        sha2::Sha256::digest(verifier.as_bytes())
    });
    session.set("oauth_state", json!({"provider": p.id(), "state": state, "verifier": verifier}));
    session.save(&s).await?;
    let to = p.authorize_url(&s, &state, &challenge).ok_or(Error::NotFound)?;
    Ok(session.attach(Redirect::to(&to).into_response()))
}

#[derive(Deserialize)]
struct Callback {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

async fn provider_callback(State(s): State<AppState>, headers: HeaderMap, ClientIp(ip): ClientIp, Path(provider): Path<String>, Query(c): Query<Callback>) -> Result<Response> {
    limits::auth(&s, ip)?;
    let p = Provider::parse(&provider).ok_or(Error::NotFound)?;
    let Some(mut session) = Session::load(&s, &headers).await? else { return Ok(Redirect::to("/signin").into_response()) };
    let pending = session.take("oauth_state");
    session.save(&s).await?;
    let retry = |msg: &str| pages::layout(&s, "Sign-in didn't finish", html! { h1 { "Sign-in didn't finish" } p { (msg) } a.btn.primary href="/signin" { "Try again" } }).into_response();
    if c.error.is_some() {
        return Ok(retry(&format!("{} didn't sign you in.", p.name())));
    }
    let (Some(pending), Some(code), Some(state)) = (pending, c.code, c.state) else { return Ok(retry("The sign-in link expired.")) };
    let expected = pending["state"].as_str().unwrap_or_default();
    if pending["provider"].as_str() != Some(p.id()) || !crypto::eq(expected.as_bytes(), state.as_bytes()) {
        return Ok(retry("The sign-in link expired."));
    }
    let verified = p.verify(&s, &code, pending["verifier"].as_str().unwrap_or_default()).await?;
    finish_signin(&s, session, &verified).await
}

async fn finish_signin(s: &AppState, session: Session, v: &identity::Verified) -> Result<Response> {
    let account = identity::account_for(s, v).await?;
    metrics::counter!("signins_total", "provider" => v.provider).increment(1);
    let next = session.get_str("after_signin").and_then(|n| local(&n)).unwrap_or_else(|| "/account".into());
    let mut session = session.sign_in(s, account).await?;
    session.take("after_signin");
    session.save(s).await?;
    Ok(session.attach(Redirect::to(&next).into_response()))
}

#[derive(Deserialize)]
struct EmailForm {
    csrf: String,
    email: String,
    #[serde(rename = "cf-turnstile-response")]
    turnstile: Option<String>,
}

async fn email_send(State(s): State<AppState>, headers: HeaderMap, ClientIp(ip): ClientIp, Form(f): Form<EmailForm>) -> Result<Response> {
    limits::auth(&s, ip)?;
    let Some(mut session) = Session::load(&s, &headers).await? else { return Ok(Redirect::to("/signin").into_response()) };
    check_form(&s, &session, &headers, &f.csrf)?;
    let Some(address) = email::normalize(&f.email) else {
        return Ok(pages::layout(&s, "Check the address", html! { h1 { "Check the address" } p { "That doesn't look like an email address." } a.btn.primary href="/signin" { "Back" } }).into_response());
    };
    email::turnstile(&s, f.turnstile.as_deref(), ip).await?;
    email::send(&s, &address).await?;
    session.set("email", json!(address));
    session.save(&s).await?;
    Ok(Redirect::to("/signin/email/code").into_response())
}

async fn email_code_page(State(s): State<AppState>, headers: HeaderMap) -> Result<Response> {
    let Some(session) = Session::load(&s, &headers).await? else { return Ok(Redirect::to("/signin").into_response()) };
    let Some(address) = session.get_str("email") else { return Ok(Redirect::to("/signin").into_response()) };
    Ok(pages::layout(&s, "Enter the code", html! {
        h1 { "Check your email" }
        p { "We sent a six-digit code to " strong { (address) } ". It works for 10 minutes." }
        form method="post" action="/signin/email/code" class="stack" {
            (pages::csrf(&session.csrf(&s)))
            label for="code" { "Code" }
            input.codeinput #code type="text" name="code" inputmode="numeric" autocomplete="one-time-code" pattern="[0-9 ]{6,7}" required;
            button.primary type="submit" { "Sign in" }
        }
        a href="/signin" { "Use a different address" }
    })
    .into_response())
}

#[derive(Deserialize)]
struct CodeForm {
    csrf: String,
    code: String,
}

async fn email_verify(State(s): State<AppState>, headers: HeaderMap, ClientIp(ip): ClientIp, Form(f): Form<CodeForm>) -> Result<Response> {
    limits::auth(&s, ip)?;
    let Some(session) = Session::load(&s, &headers).await? else { return Ok(Redirect::to("/signin").into_response()) };
    check_form(&s, &session, &headers, &f.csrf)?;
    let Some(address) = session.get_str("email") else { return Ok(Redirect::to("/signin").into_response()) };
    match email::verify(&s, &address, &f.code).await {
        Ok(v) => finish_signin(&s, session, &v).await,
        Err(Error::Forbidden(m)) => Ok(pages::layout(&s, "Code not accepted", html! { h1 { "Code not accepted" } p { (m) } a.btn.primary href="/signin/email/code" { "Try again" } }).into_response()),
        Err(e) => Err(e),
    }
}

#[derive(Deserialize)]
struct CsrfOnly {
    csrf: String,
}

async fn signout(State(s): State<AppState>, headers: HeaderMap, Form(f): Form<CsrfOnly>) -> Result<Response> {
    let Some(session) = Session::load(&s, &headers).await? else { return Ok(Redirect::to("/signin").into_response()) };
    check_form(&s, &session, &headers, &f.csrf)?;
    let cookie = session.sign_out(&s).await?;
    let mut r = Redirect::to("/signin").into_response();
    r.headers_mut().append(header::SET_COOKIE, cookie);
    Ok(r)
}

/// The signed-in session, or a redirect to sign in first.
async fn signed_in(s: &AppState, headers: &HeaderMap, back: &str) -> Result<std::result::Result<(Session, Uuid), Response>> {
    let mut session = Session::get(s, headers).await?;
    match session.account_id {
        Some(a) => Ok(Ok((session, a))),
        None => {
            session.set("after_signin", json!(back));
            session.save(s).await?;
            Ok(Err(session.attach(Redirect::to("/signin").into_response())))
        }
    }
}

fn ago(t: chrono::DateTime<chrono::Utc>) -> String {
    let d = chrono::Utc::now() - t;
    match d.num_minutes() {
        m if m < 2 => "just now".into(),
        m if m < 60 => format!("{m} min ago"),
        m if m < 60 * 48 => format!("{} h ago", m / 60),
        m => format!("{} days ago", m / 60 / 24),
    }
}

async fn account_page(State(s): State<AppState>, headers: HeaderMap) -> Result<Response> {
    let (session, account) = match signed_in(&s, &headers, "/account").await? {
        Ok(x) => x,
        Err(r) => return Ok(r),
    };
    let summary = account::summary(&s, account).await?;
    let devices = account::devices(&s, account).await?;
    let synced = crate::api::sync::summary(&s, account).await?;
    let csrf = session.csrf(&s);
    let providers: Vec<String> = summary["identities"].as_array().into_iter().flatten().filter_map(|i| i["provider"].as_str().map(str::to_owned)).collect();
    let page = pages::layout(&s, "Account", html! {
        h1 { "Your account" }
        p { "Signed in as " strong { (summary["email"].as_str().unwrap_or("")) } @if !providers.is_empty() { " · " (providers.join(", ")) } }
        h2 { "Devices" }
        div.panel {
            @if devices.is_empty() { p.muted { "No devices are signed in." } }
            ul.devices {
                @for d in &devices {
                    li {
                        div.row {
                            strong { (d.name) }
                            form.inline method="post" action=(format!("/account/devices/{}/revoke", d.id)) {
                                (pages::csrf(&csrf))
                                button type="submit" aria-label=(format!("Sign out {}", d.name)) { "Sign out" }
                            }
                        }
                        div.muted { (d.os) @if !d.dino_version.is_empty() { " · dino " (d.dino_version) } " · last seen " (ago(d.last_seen_at)) }
                    }
                }
            }
        }
        h2 { "Synced settings" }
        div.panel {
            @if synced.is_empty() {
                p.muted { "Nothing synced yet. Turn on sync in dino to keep your settings on every Mac." }
            } @else {
                ul.devices {
                    @for (collection, count, bytes, at) in &synced {
                        li {
                            div.row { strong { (collection) } span.muted { (count) @if *count == 1 { " setting" } @else { " settings" } } }
                            div.muted { "🔒 Encrypted · " (bytes) " bytes · changed " (ago(*at)) }
                        }
                    }
                }
            }
            p.muted { "Values are encrypted on your devices. This server stores them sealed and can't read them." }
        }
        h2 { "Your data" }
        div.stack {
            a.btn href="/account/export" download="dino-account.json" { "Export everything" }
            form method="post" action="/account/signout-everywhere" {
                (pages::csrf(&csrf))
                button type="submit" { "Sign out everywhere" }
            }
            form method="post" action="/signout" {
                (pages::csrf(&csrf))
                button type="submit" { "Sign out of this browser" }
            }
        }
        h2 { "Delete account" }
        form method="post" action="/account/delete" class="stack" {
            (pages::csrf(&csrf))
            p { "Signs out every device now and erases the account 30 days later. Type your email to confirm." }
            label for="confirm" { "Email" }
            input #confirm type="email" name="confirm" required autocomplete="off";
            button.danger type="submit" { "Delete account" }
        }
    });
    Ok(page.into_response())
}

async fn account_revoke(State(s): State<AppState>, headers: HeaderMap, Path(id): Path<Uuid>, Form(f): Form<CsrfOnly>) -> Result<Response> {
    let (session, account) = match signed_in(&s, &headers, "/account").await? {
        Ok(x) => x,
        Err(r) => return Ok(r),
    };
    check_form(&s, &session, &headers, &f.csrf)?;
    account::revoke_device(&s, account, id).await?;
    Ok(Redirect::to("/account").into_response())
}

async fn account_signout_everywhere(State(s): State<AppState>, headers: HeaderMap, Form(f): Form<CsrfOnly>) -> Result<Response> {
    let (session, account) = match signed_in(&s, &headers, "/account").await? {
        Ok(x) => x,
        Err(r) => return Ok(r),
    };
    check_form(&s, &session, &headers, &f.csrf)?;
    account::signout_everywhere(&s, account).await?;
    Ok(Redirect::to("/signin").into_response())
}

#[derive(Deserialize)]
struct DeleteForm {
    csrf: String,
    confirm: String,
}

async fn account_delete(State(s): State<AppState>, headers: HeaderMap, Form(f): Form<DeleteForm>) -> Result<Response> {
    let (session, account) = match signed_in(&s, &headers, "/account").await? {
        Ok(x) => x,
        Err(r) => return Ok(r),
    };
    check_form(&s, &session, &headers, &f.csrf)?;
    let summary = account::summary(&s, account).await?;
    if !summary["email"].as_str().unwrap_or("").eq_ignore_ascii_case(f.confirm.trim()) {
        return Ok(pages::message(&s, "Not deleted", "The email didn't match your account's.").into_response());
    }
    account::delete(&s, account).await?;
    Ok(pages::message(&s, "Account deleted", "Every device is signed out. Your account is erased in 30 days.").into_response())
}

async fn account_export(State(s): State<AppState>, headers: HeaderMap) -> Result<Response> {
    let (_, account) = match signed_in(&s, &headers, "/account").await? {
        Ok(x) => x,
        Err(r) => return Ok(r),
    };
    let mut r = Json(account::export(&s, account).await?).into_response();
    r.headers_mut().insert(header::CONTENT_DISPOSITION, HeaderValue::from_static("attachment; filename=\"dino-account.json\""));
    r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(r)
}

/// Where a new Mac will be approved for encrypted sync (a signed-in device confirms a code and
/// hands over the account key). It arrives with sync.
async fn approve(State(s): State<AppState>) -> Response {
    pages::message(&s, "Approve a new Mac", "Approving a new Mac for encrypted sync arrives with settings sync. Nothing to do here yet.").into_response()
}
