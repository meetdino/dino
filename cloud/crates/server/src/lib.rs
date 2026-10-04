//! dino-cloud: accounts, devices, sign-in (GitHub, Google, a link or code by email) and settings
//! sync for dino. It never proxies agent traffic, and secrets (API keys, tokens) never reach it.

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
    /// Connects and migrates, waiting out a database that isn't reachable yet (Neon waking up,
    /// Postgres restarting) rather than giving up: the server never exits over one.
    pub async fn new(cfg: Config) -> anyhow::Result<Self> {
        // Migrations take a session advisory lock (so instances starting together don't race),
        // which needs a direct connection: through a transaction-mode pooler the lock and the
        // statements can land on different backends. One short connection, then the pool.
        let target = cfg.platform.migrate_url.clone().unwrap_or_else(|| cfg.database_url.clone());
        retrying("migrating the database", || async {
            let one = PgPoolOptions::new().max_connections(1).acquire_timeout(DB_ACQUIRE).connect(&target).await?;
            let r = sqlx::migrate!("./migrations").run(&one).await;
            one.close().await;
            r.map_err(anyhow::Error::from)
        })
        .await?;
        // Connections are made when needed, so a database that goes away later only fails the
        // requests made meanwhile (503, try again), never the server.
        let db = PgPoolOptions::new().max_connections(cfg.platform.db_max_connections).acquire_timeout(DB_ACQUIRE).connect_lazy(&cfg.database_url)?;
        Self::with_migrated_pool(cfg, db)
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

/// How long a request waits for a database connection: long enough for Neon to wake a suspended
/// compute (a few seconds, occasionally ten), short of the 15-second request timeout.
const DB_ACQUIRE: Duration = Duration::from_secs(12);

/// `f` again until it works, waiting longer each time (up to 30 seconds), as long as what fails is
/// the database being out of reach. Any other failure (a migration that doesn't apply) is
/// returned.
async fn retrying<F, Fut>(what: &str, f: F) -> anyhow::Result<()>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<()>>,
{
    let mut wait = Duration::from_millis(500);
    loop {
        match f().await {
            Ok(()) => return Ok(()),
            Err(e) if unreachable(&e) => {
                tracing::warn!(error = %e, retry_in_ms = wait.as_millis() as u64, "{what}: database not reachable yet");
                tokio::time::sleep(wait).await;
                wait = (wait * 2).min(Duration::from_secs(30));
            }
            Err(e) => return Err(e),
        }
    }
}

fn unreachable(e: &anyhow::Error) -> bool {
    if let Some(e) = e.downcast_ref::<sqlx::Error>() {
        return error::transient(e);
    }
    if let Some(sqlx::migrate::MigrateError::Execute(e)) = e.downcast_ref::<sqlx::migrate::MigrateError>() {
        return error::transient(e);
    }
    false
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
