//! Who someone is: GitHub, Google or an emailed code. Each identity belongs to one account; a new
//! identity joins the account that already has the same verified address, otherwise it starts one.

pub mod email;
pub mod mailer;
pub mod upstream;

use uuid::Uuid;

use crate::AppState;
use crate::error::{Error, Result};

pub struct Verified {
    pub provider: &'static str,
    /// The provider's stable id for the person (their address, for email).
    pub subject: String,
    pub email: Option<String>,
    pub email_verified: bool,
}

/// The account `v` signs in to, creating or linking as needed.
pub async fn account_for(state: &AppState, v: &Verified) -> Result<Uuid> {
    let mut tx = state.db.begin().await?;
    let existing: Option<(Uuid, Option<chrono::DateTime<chrono::Utc>>)> =
        sqlx::query_as("SELECT a.id, a.deleted_at FROM identities i JOIN accounts a ON a.id = i.account_id WHERE i.provider = $1 AND i.subject = $2")
            .bind(v.provider)
            .bind(&v.subject)
            .fetch_optional(&mut *tx)
            .await?;
    if let Some((id, deleted)) = existing {
        if deleted.is_some() {
            return Err(Error::Forbidden("This account is being deleted.".into()));
        }
        return Ok(id);
    }
    let email = v.email.clone().unwrap_or_default();
    // Only a verified address joins an existing account, and only one that was verified too.
    let linked: Option<(Uuid,)> = if v.email_verified && !email.is_empty() {
        sqlx::query_as("SELECT id FROM accounts WHERE lower(email) = lower($1) AND email_verified AND deleted_at IS NULL ORDER BY created_at LIMIT 1")
            .bind(&email)
            .fetch_optional(&mut *tx)
            .await?
    } else {
        None
    };
    let account = match linked {
        Some((id,)) => id,
        None => {
            let id = Uuid::now_v7();
            sqlx::query("INSERT INTO accounts (id, email, email_verified) VALUES ($1, $2, $3)").bind(id).bind(&email).bind(v.email_verified).execute(&mut *tx).await?;
            metrics::counter!("accounts_created_total", "provider" => v.provider).increment(1);
            id
        }
    };
    sqlx::query("INSERT INTO identities (id, account_id, provider, subject, email) VALUES ($1, $2, $3, $4, $5)")
        .bind(Uuid::now_v7())
        .bind(account)
        .bind(v.provider)
        .bind(&v.subject)
        .bind(v.email.as_deref())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(account)
}
