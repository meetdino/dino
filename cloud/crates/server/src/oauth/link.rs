//! Sign-in links by email, for `dino login --email` and the app's "Use email instead": the device
//! asks for a device code with an address (`/oauth/device_authorization` with `email`), we mail a
//! link, and opening it approves that device code for the address's account, so the device's
//! polling picks up its tokens. Like the device grant, but the mailbox is the proof.
//!
//! - The link is a random token, stored hashed, used once, and lives 15 minutes.
//! - Opening it shows a page with a button; the sign-in happens on the POST, so a mail scanner
//!   that fetches links doesn't use it up (or sign anyone in).
//! - An address gets at most 5 links an hour; the device's answer is the same whether or not an
//!   account exists for it.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use chrono::Duration;
use maud::html;

use crate::AppState;
use crate::crypto;
use crate::error::{Error, Result};
use crate::identity::{self, Verified};
use crate::limits::{self, ClientIp};
use crate::web::pages;

pub const TTL: Duration = Duration::minutes(15);
const PER_HOUR: i64 = 5;

/// Mails a sign-in link for the device code stored as `device_hash`.
pub async fn send(s: &AppState, device_hash: &[u8], device_name: &str, email: &str) -> Result<()> {
    let recent: (i64,) = sqlx::query_as("SELECT count(*) FROM email_links WHERE email = $1 AND created_at > now() - interval '1 hour'").bind(email).fetch_one(&s.db).await?;
    if recent.0 >= PER_HOUR {
        return Err(Error::RateLimited { retry_after: 3600 });
    }
    let token = crypto::token("dino_ml");
    sqlx::query("INSERT INTO email_links (hash, device_hash, email, expires_at) VALUES ($1, $2, $3, now() + $4)")
        .bind(crypto::hash(&token))
        .bind(device_hash)
        .bind(email)
        .bind(TTL)
        .execute(&s.db)
        .await?;
    let link = s.cfg.url(&format!("/login/{token}"));
    let text = format!("Sign in to dino on {device_name}:\n\n{link}\n\nThe link works once, for 15 minutes. If you didn't ask for it, ignore this email: nobody can sign in without it.");
    s.mailer.send_html(email, "Sign in to dino", &text, Some(&html_mail(&link, device_name))).await.map_err(|e| Error::Internal(e.context("sending the sign-in link")))?;
    metrics::counter!("email_links_sent_total").increment(1);
    Ok(())
}

/// The email itself: one green button, dino's colors, and the link written out for mail apps
/// that hide buttons.
fn html_mail(link: &str, device: &str) -> String {
    html! {
        div style="background:#0b0e0a;padding:40px 16px;font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',sans-serif" {
            div style="max-width:440px;margin:0 auto;background:#151a13;border:1px solid #2a3226;border-radius:12px;padding:32px" {
                p style="margin:0 0 24px;font:600 18px ui-monospace,Menlo,monospace;color:#75b340" { "dino" }
                h1 style="margin:0 0 12px;font-size:22px;color:#eef2ea" { "Sign in to dino" }
                p style="margin:0 0 24px;color:#a3ad9c;line-height:1.5" { "Click to sign in on " strong style="color:#eef2ea" { (device) } ". Your settings sync once you're in." }
                a href=(link) style="display:block;text-align:center;background:#75b340;color:#0b0e0a;font-weight:600;text-decoration:none;padding:13px 16px;border-radius:10px" { "Sign in" }
                p style="margin:24px 0 0;color:#a3ad9c;font-size:13px;line-height:1.5" { "The link works once, for 15 minutes. If you didn't ask for it, ignore this email: nobody can sign in without it." }
                p style="margin:12px 0 0;color:#6b7565;font-size:12px;word-break:break-all" { (link) }
            }
        }
    }
    .into_string()
}

type Link = (String, String, bool);

/// The link's address and device, while it can still be used.
async fn find(s: &AppState, token: &str) -> Result<Option<Link>> {
    Ok(sqlx::query_as(
        "SELECT l.email, d.device_name, l.used_at IS NOT NULL FROM email_links l JOIN device_codes d ON d.hash = l.device_hash
         WHERE l.hash = $1 AND l.expires_at > now()",
    )
    .bind(crypto::hash(token))
    .fetch_optional(&s.db)
    .await?)
}

fn gone(s: &AppState) -> Response {
    pages::message(s, "This link has expired", "Sign-in links work once, for 15 minutes. Ask dino for a new one.").into_response()
}

/// `GET /login/{token}`: what the link opens. Nothing happens until the button is pressed.
pub async fn page(State(s): State<AppState>, ClientIp(ip): ClientIp, Path(token): Path<String>) -> Result<Response> {
    limits::auth(&s, ip).await?;
    let Some((email, device, used)) = find(&s, &token).await? else { return Ok(gone(&s)) };
    if used {
        return Ok(gone(&s));
    }
    Ok(pages::layout(
        &s,
        "Sign in",
        html! {
            h1 { "Sign in to dino on " (device) "?" }
            p { "As " strong { (email) } "." }
            form method="post" action=(format!("/login/{token}")) class="stack" {
                button.primary type="submit" { "Sign in" }
            }
            p.muted { "Only continue if you just asked dino to sign in." }
        },
    )
    .into_response())
}

/// `POST /login/{token}`: uses the link up and approves the device's request.
pub async fn open(State(s): State<AppState>, ClientIp(ip): ClientIp, Path(token): Path<String>) -> Result<Response> {
    limits::auth(&s, ip).await?;
    let taken: Option<(String, Vec<u8>)> = sqlx::query_as("UPDATE email_links SET used_at = now() WHERE hash = $1 AND used_at IS NULL AND expires_at > now() RETURNING email, device_hash")
        .bind(crypto::hash(&token))
        .fetch_optional(&s.db)
        .await?;
    let Some((email, device_hash)) = taken else { return Ok(gone(&s)) };
    let account = identity::account_for(&s, &Verified { provider: "email", subject: email.clone(), email: Some(email), email_verified: true }).await?;
    let device: Option<(String,)> = sqlx::query_as("UPDATE device_codes SET status = 'approved', account_id = $2 WHERE hash = $1 AND status = 'pending' AND expires_at > now() RETURNING device_name")
        .bind(&device_hash)
        .bind(account)
        .fetch_optional(&s.db)
        .await?;
    let Some((device,)) = device else { return Ok(gone(&s)) };
    metrics::counter!("signins_total", "provider" => "email_link").increment(1);
    Ok(pages::message(&s, "You're signed in", &format!("You're signed in on {device}. You can close this tab.")).into_response())
}
