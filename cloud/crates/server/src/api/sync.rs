//! Settings sync: end-to-end encrypted records, pulled by sequence number and pushed in batches,
//! with a WebSocket that only says "something changed". The record format, clocks and crypto live
//! in the `dino-sync` crate shared with dinod; until it lands these routes answer 501, so clients
//! can tell "not yet" from "not found".

use axum::Router;
use axum::http::StatusCode;
use axum::routing::get;

use crate::AppState;
use crate::api::Authed;

pub fn routes() -> Router<AppState> {
    Router::new().route("/sync", get(not_yet).post(not_yet))
}

async fn not_yet(_: Authed) -> (StatusCode, &'static str) {
    (StatusCode::NOT_IMPLEMENTED, r#"{"error":"not_implemented","message":"Settings sync isn't available on this server yet."}"#)
}
