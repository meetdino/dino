//! Sign in with a six-digit code sent to an address. Codes are stored as HMACs (six digits are
//! easy to brute-force from a hash), live 10 minutes, allow 5 tries, and an address gets at most 5
//! a hour. The answer never says whether an account exists.

use chrono::{DateTime, Duration, Utc};
use uuid::Uuid;

use crate::AppState;
use crate::crypto;
use crate::error::{Error, Result};
use crate::identity::Verified;

const CODE_TTL: Duration = Duration::minutes(10);
const MAX_TRIES: i32 = 5;
const PER_HOUR: i64 = 5;

/// A plausible address, lowercased; the mailbox proves the rest.
pub fn normalize(email: &str) -> Option<String> {
    let e = email.trim().to_lowercase();
    let (local, domain) = e.split_once('@')?;
    (e.len() <= 254 && !local.is_empty() && domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.') && !e.chars().any(|c| c.is_whitespace() || c.is_control()) && !domain.contains('@')).then_some(e)
}

fn mac(state: &AppState, email: &str, code: &str) -> [u8; 32] {
    crypto::hmac(&state.cfg.secret, "email-code", format!("{email}:{code}").as_bytes())
}

pub async fn send(state: &AppState, email: &str) -> Result<()> {
    let recent: (i64,) = sqlx::query_as("SELECT count(*) FROM email_codes WHERE lower(email) = $1 AND created_at > now() - interval '1 hour'").bind(email).fetch_one(&state.db).await?;
    if recent.0 >= PER_HOUR {
        return Err(Error::RateLimited { retry_after: 3600 });
    }
    let code = crypto::email_code();
    sqlx::query("INSERT INTO email_codes (id, email, code_mac, expires_at) VALUES ($1, $2, $3, now() + $4)")
        .bind(Uuid::now_v7())
        .bind(email)
        .bind(mac(state, email, &code).to_vec())
        .bind(CODE_TTL)
        .execute(&state.db)
        .await?;
    let text = format!("Your dino sign-in code is {code}\n\nIt works for 10 minutes. If you didn't ask for it, you can ignore this email: nobody can sign in without the code.");
    state.mailer.send(email, &format!("{code} is your dino sign-in code"), &text).await.map_err(|e| Error::Internal(e.context("sending the sign-in code")))?;
    metrics::counter!("email_codes_sent_total").increment(1);
    Ok(())
}

pub async fn verify(state: &AppState, email: &str, code: &str) -> Result<Verified> {
    let wrong = || Error::Forbidden("That code isn't right, or it expired. Check the latest email, or send a new code.".into());
    let code: String = code.chars().filter(char::is_ascii_digit).collect();
    let mut tx = state.db.begin().await?;
    let row: Option<(Uuid, Vec<u8>, i32, DateTime<Utc>)> = sqlx::query_as(
        "SELECT id, code_mac, attempts, expires_at FROM email_codes
         WHERE lower(email) = $1 AND used_at IS NULL ORDER BY created_at DESC LIMIT 1 FOR UPDATE",
    )
    .bind(email)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((id, stored, attempts, expires_at)) = row else { return Err(wrong()) };
    if attempts >= MAX_TRIES || expires_at <= Utc::now() {
        return Err(wrong());
    }
    let ok = code.len() == 6 && crypto::eq(&mac(state, email, &code), &stored);
    sqlx::query("UPDATE email_codes SET attempts = attempts + 1, used_at = CASE WHEN $2 THEN now() ELSE NULL END WHERE id = $1").bind(id).bind(ok).execute(&mut *tx).await?;
    tx.commit().await?;
    if !ok {
        return Err(wrong());
    }
    Ok(Verified { provider: "email", subject: email.to_owned(), email: Some(email.to_owned()), email_verified: true })
}

/// Cloudflare Turnstile, when configured: the widget's token, checked server-side.
pub async fn turnstile(state: &AppState, token: Option<&str>, ip: std::net::IpAddr) -> Result<()> {
    let Some(t) = &state.cfg.turnstile else { return Ok(()) };
    let token = token.filter(|t| !t.is_empty()).ok_or_else(|| Error::Forbidden("Complete the check before continuing.".into()))?;
    #[derive(serde::Deserialize)]
    struct Answer {
        success: bool,
    }
    let a: Answer = state
        .http
        .post(&t.verify_url)
        .form(&[("secret", t.secret.as_str()), ("response", token), ("remoteip", &ip.to_string())])
        .send()
        .await
        .map_err(|e| Error::Internal(e.into()))?
        .json()
        .await
        .map_err(|e| Error::Internal(e.into()))?;
    if a.success { Ok(()) } else { Err(Error::Forbidden("The check didn't pass. Try again.".into())) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses() {
        assert_eq!(normalize("  Ben@Example.COM ").as_deref(), Some("ben@example.com"));
        assert_eq!(normalize("no-at-sign"), None);
        assert_eq!(normalize("a@b"), None);
        assert_eq!(normalize("a b@example.com"), None);
        assert_eq!(normalize("a@@example.com"), None);
    }
}
