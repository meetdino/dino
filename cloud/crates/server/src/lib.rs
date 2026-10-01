//! dino-cloud: accounts, devices, sign-in and end-to-end encrypted settings sync for dino. It never
//! proxies agent traffic and never holds a key that opens a synced value.

pub mod api;
pub mod config;
pub mod crypto;
pub mod error;
pub mod identity;
pub mod jobs;
pub mod limits;
pub mod oauth;
pub mod telemetry;
pub mod web;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::get;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

pub use config::Config;

#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    pub db: PgPool,
    /// For the identity providers, the mail API and Turnstile.
    pub http: reqwest::Client,
    pub limits: Arc<limits::Limits>,
    pub mailer: Arc<identity::mailer::Mailer>,
    /// This node's sync WebSockets.
    pub hub: Arc<api::sync::Hub>,
}

impl AppState {
    pub async fn new(cfg: Config) -> anyhow::Result<Self> {
        // Migrations take a session advisory lock (so instances starting together don't race),
        // which needs a direct connection: through a transaction-mode pooler the lock and the
        // statements can land on different backends. One short connection, then the pool.
        if let Some(direct) = &cfg.platform.migrate_url {
            let one = PgPoolOptions::new().max_connections(1).acquire_timeout(Duration::from_secs(10)).connect(direct).await?;
            sqlx::migrate!("./migrations").run(&one).await?;
            one.close().await;
        }
        let db = PgPoolOptions::new()
            .max_connections(cfg.platform.db_max_connections)
            .acquire_timeout(Duration::from_secs(3))
            .connect(&cfg.database_url)
            .await?;
        if cfg.platform.migrate_url.is_some() {
            return Self::with_migrated_pool(cfg, db);
        }
        Self::with_pool(cfg, db).await
    }

    pub async fn with_pool(cfg: Config, db: PgPool) -> anyhow::Result<Self> {
        sqlx::migrate!("./migrations").run(&db).await?;
        Self::with_migrated_pool(cfg, db)
    }

    fn with_migrated_pool(cfg: Config, db: PgPool) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .user_agent(concat!("dino-cloud/", env!("CARGO_PKG_VERSION")))
            .build()?;
        let mailer = Arc::new(identity::mailer::Mailer::new(&cfg.mail, http.clone()));
        let limits = Arc::new(limits::Limits::new(&cfg));
        Ok(AppState { cfg: Arc::new(cfg), db, http, limits, mailer, hub: Default::default() })
    }
}

pub fn router(state: AppState) -> Router {
    let app = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/readyz", get(ready))
        .merge(web::routes())
        .merge(oauth::routes())
        .route("/internal/cron", get(jobs::cron))
        .nest("/v1", api::routes(state.cfg.platform.push))
        .layer(axum::middleware::from_fn_with_state(state.clone(), limits::per_ip))
        .layer(axum::middleware::from_fn_with_state(state.clone(), web::security_headers))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(tower_http::timeout::TimeoutLayer::with_status_code(axum::http::StatusCode::REQUEST_TIMEOUT, Duration::from_secs(15)));
    telemetry::apply(app).with_state(state)
}

async fn ready(axum::extract::State(s): axum::extract::State<AppState>) -> axum::http::StatusCode {
    match sqlx::query("SELECT 1").execute(&s.db).await {
        Ok(_) => axum::http::StatusCode::OK,
        Err(_) => axum::http::StatusCode::SERVICE_UNAVAILABLE,
    }
}

/// Serve until SIGTERM or Ctrl-C, letting requests in flight finish.
pub async fn serve(state: AppState, listener: tokio::net::TcpListener) -> anyhow::Result<()> {
    let jobs = state.cfg.platform.background_jobs.then(|| jobs::spawn(state.clone()));
    let nudges = state.cfg.platform.push.then(|| api::sync::listen(state.clone()));
    let app = router(state);
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).with_graceful_shutdown(shutdown_signal()).await?;
    for task in jobs.into_iter().chain(nudges) {
        task.abort();
    }
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = term => {}
    }
    tracing::info!("shutting down");
}
