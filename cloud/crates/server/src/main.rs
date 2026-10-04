use dino_cloud::{AppState, Config, telemetry};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cfg = Config::from_env()?;
    telemetry::init(cfg.json_logs);
    if let Some(addr) = cfg.metrics_bind {
        telemetry::serve_metrics(addr).await?;
    }
    let listener = tokio::net::TcpListener::bind(cfg.bind).await?;
    tracing::info!(addr = %cfg.bind, public = %cfg.public_url, "dino-cloud listening");
    let state = AppState::new(cfg).await?;
    dino_cloud::serve(state, listener).await
}
