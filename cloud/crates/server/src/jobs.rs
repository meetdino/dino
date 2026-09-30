//! Background upkeep: erase accounts 30 days after deletion, drop expired credentials, and trim the
//! rate limiters. Runs on every node; each statement is idempotent, so overlapping runs are fine.

use std::time::Duration;

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
        "DELETE FROM web_sessions WHERE expires_at < now()",
        "DELETE FROM idempotency WHERE created_at < now() - interval '1 day'",
    ] {
        sqlx::query(q).execute(&s.db).await?;
    }
    Ok(())
}
