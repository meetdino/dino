//! Upkeep: erase accounts 30 days after deletion, drop expired credentials, and trim the rate
//! limiters. A long-running server does it every ten minutes; a serverless one when Vercel Cron
//! calls `/internal/cron`. Each statement is idempotent, so overlapping runs are fine.

use std::time::Duration;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};

use crate::AppState;

pub fn spawn(state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        let mut n: u64 = 0;
        loop {
            tick.tick().await;
            state.limits.retain_recent();
            if n % 10 == 0 {
                if let Err(e) = cleanup(&state).await {
                    tracing::warn!(error = %e, "cleanup failed");
                }
            }
            n += 1;
        }
    })
}

pub async fn cleanup(s: &AppState) -> anyhow::Result<()> {
    let erased = sqlx::query("DELETE FROM accounts WHERE deleted_at < now() - interval '30 days'").execute(&s.db).await?.rows_affected();
    if erased > 0 {
        tracing::info!(erased, "erased deleted accounts");
        metrics::counter!("accounts_erased_total").increment(erased);
    }
    for q in [
        "DELETE FROM access_tokens WHERE expires_at < now() - interval '1 day'",
        // Rotated refresh tokens stay while their family can live, so reuse is still recognised.
        "DELETE FROM refresh_tokens WHERE expires_at < now()",
        "DELETE FROM auth_codes WHERE expires_at < now() - interval '1 day'",
        "DELETE FROM device_codes WHERE expires_at < now() - interval '1 day'",
        "DELETE FROM email_codes WHERE expires_at < now() - interval '1 day'",
        "DELETE FROM email_links WHERE expires_at < now() - interval '1 day'",
        "DELETE FROM web_sessions WHERE expires_at < now()",
        "DELETE FROM rate_counters WHERE window_start < now() - interval '1 day'",
    ] {
        sqlx::query(q).execute(&s.db).await?;
    }
    Ok(())
}

/// `GET /internal/cron`: the cleanup, for a host without a server that stays up. It wants the
/// `CRON_SECRET` as a bearer token (what Vercel Cron sends); without one configured it isn't there.
pub async fn cron(State(s): State<AppState>, headers: HeaderMap) -> StatusCode {
    let Some(secret) = &s.cfg.platform.cron_secret else { return StatusCode::NOT_FOUND };
    let sent = headers.get("authorization").and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")).unwrap_or("");
    if !bool::from(subtle::ConstantTimeEq::ct_eq(sent.as_bytes(), secret.as_bytes())) {
        return StatusCode::UNAUTHORIZED;
    }
    s.limits.retain_recent();
    match cleanup(&s).await {
        Ok(()) => StatusCode::OK,
        Err(e) => {
            tracing::warn!(error = %e, "cleanup failed");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}
