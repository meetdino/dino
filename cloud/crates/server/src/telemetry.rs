//! Logs, request ids and metrics. Spans carry the method, the matched route (never the raw URI,
//! whose query can hold a code or a state) and a request id; bodies and headers aren't logged. The
//! spans are plain `tracing`, so an OpenTelemetry exporter is one more subscriber layer.

use std::net::SocketAddr;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::extract::{MatchedPath, Request};
use axum::http::{HeaderName, Response};
use axum::middleware::Next;
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::trace::TraceLayer;
use tracing::Span;
use tracing_subscriber::EnvFilter;

use crate::AppState;

static METRICS: OnceLock<PrometheusHandle> = OnceLock::new();

pub fn init(json: bool) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,sqlx=warn,tower_http=warn"));
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    let _ = if json { builder.json().flatten_event(true).try_init() } else { builder.try_init() };
    metrics_handle();
}

/// The process-wide Prometheus recorder, installed once.
pub fn metrics_handle() -> &'static PrometheusHandle {
    METRICS.get_or_init(|| {
        let recorder = PrometheusBuilder::new().set_buckets(&[0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5]).expect("static buckets").build_recorder();
        let handle = recorder.handle();
        let _ = metrics::set_global_recorder(recorder);
        handle
    })
}

/// `/metrics` on its own address, for the scraper only.
pub async fn serve_metrics(addr: SocketAddr) -> anyhow::Result<()> {
    let handle = metrics_handle();
    let app = Router::new().route("/metrics", axum::routing::get(move || async move { handle.render() }));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "metrics listening");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(())
}

const REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

/// Request ids, a span per request, and per-route metrics. Applied with `Router::layer`, so it
/// runs after routing and the matched route is known.
pub fn apply(router: Router<AppState>) -> Router<AppState> {
    router
        .layer(axum::middleware::from_fn(record))
        .layer(PropagateRequestIdLayer::new(REQUEST_ID))
        .layer(TraceLayer::new_for_http().make_span_with(make_span).on_response(on_response).on_request(()))
        .layer(SetRequestIdLayer::new(REQUEST_ID, MakeRequestUuid))
}

fn route(req: &Request) -> String {
    req.extensions().get::<MatchedPath>().map(|p| p.as_str().to_owned()).unwrap_or_else(|| "unmatched".into())
}

fn make_span(req: &Request) -> Span {
    let id = req.headers().get(&REQUEST_ID).and_then(|v| v.to_str().ok()).unwrap_or("");
    tracing::info_span!("http", method = %req.method(), route = %route(req), request_id = %id)
}

fn on_response(res: &Response<Body>, latency: Duration, span: &Span) {
    tracing::info!(parent: span, status = res.status().as_u16(), latency_ms = latency.as_millis() as u64, "response");
}

async fn record(req: Request, next: Next) -> Response<Body> {
    let route = route(&req);
    let method = req.method().to_string();
    let start = Instant::now();
    let res = next.run(req).await;
    let labels = [("route", route), ("method", method), ("status", res.status().as_u16().to_string())];
    metrics::histogram!("http_request_duration_seconds", &labels).record(start.elapsed().as_secs_f64());
    metrics::counter!("http_requests_total", &labels).increment(1);
    res
}
