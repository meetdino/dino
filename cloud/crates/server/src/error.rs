//! Errors as the client sees them: OAuth error objects on `/oauth`, `{error, message}` on `/v1`,
//! and a page for the browser. Internal details go to the log, never to the response.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// RFC 6749 §5.2 / RFC 8628 §3.5 error codes, for the token endpoint.
    #[error("{code}")]
    OAuth { status: StatusCode, code: &'static str, description: String },
    #[error("unauthorized")]
    Unauthorized,
    #[error("{0}")]
    BadRequest(String),
    #[error("not found")]
    NotFound,
    #[error("too many requests")]
    RateLimited { retry_after: u64 },
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    pub fn oauth(code: &'static str, description: impl Into<String>) -> Self {
        let status = match code {
            "invalid_client" => StatusCode::UNAUTHORIZED,
            "server_error" => StatusCode::INTERNAL_SERVER_ERROR,
            _ => StatusCode::BAD_REQUEST,
        };
        Error::OAuth { status, code, description: description.into() }
    }
}

impl From<sqlx::Error> for Error {
    fn from(e: sqlx::Error) -> Self {
        Error::Internal(e.into())
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let (status, body) = match &self {
            Error::OAuth { status, code, description } => (*status, json!({"error": code, "error_description": description})),
            Error::Unauthorized => (StatusCode::UNAUTHORIZED, json!({"error": "unauthorized", "message": "Sign in again."})),
            Error::BadRequest(m) => (StatusCode::BAD_REQUEST, json!({"error": "bad_request", "message": m})),
            Error::NotFound => (StatusCode::NOT_FOUND, json!({"error": "not_found"})),
            Error::RateLimited { retry_after } => {
                let mut r = (StatusCode::TOO_MANY_REQUESTS, axum::Json(json!({"error": "rate_limited", "message": "Too many requests. Try again shortly."}))).into_response();
                r.headers_mut().insert("retry-after", retry_after.to_string().parse().unwrap());
                return r;
            }
            Error::Forbidden(m) => (StatusCode::FORBIDDEN, json!({"error": "forbidden", "message": m})),
            Error::Internal(e) => {
                tracing::error!(error = %e, "internal error");
                (StatusCode::INTERNAL_SERVER_ERROR, json!({"error": "server_error"}))
            }
        };
        let mut r = (status, axum::Json(body)).into_response();
        if matches!(self, Error::OAuth { .. } | Error::Unauthorized) {
            // RFC 6749 §5.1: responses carrying credentials or their errors aren't cached.
            r.headers_mut().insert("cache-control", "no-store".parse().unwrap());
        }
        r
    }
}
