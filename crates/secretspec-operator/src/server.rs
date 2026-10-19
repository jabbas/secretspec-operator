// SPDX-License-Identifier: Apache-2.0

//! The health and metrics HTTP server: `/healthz`, `/readyz`, `/metrics`.

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::Router;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;

use crate::metrics::Metrics;

/// Content type of the `/metrics` response.
pub const OPENMETRICS_CONTENT_TYPE: &str =
    "application/openmetrics-text; version=1.0.0; charset=utf-8";

#[derive(Clone)]
struct AppState {
    metrics: Arc<Metrics>,
    ready: Arc<AtomicBool>,
}

/// The server's routes.
pub fn router(metrics: Arc<Metrics>, ready: Arc<AtomicBool>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/metrics", get(metrics_handler))
        .with_state(AppState { metrics, ready })
}

/// Serves [`router`] on `bind` until `shutdown` resolves.
///
/// # Errors
///
/// Fails when the address cannot be bound or the server stops with an I/O
/// error.
pub async fn serve(
    bind: SocketAddr,
    metrics: Arc<Metrics>,
    ready: Arc<AtomicBool>,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|e| anyhow::anyhow!("cannot bind the HTTP server to {bind}: {e}"))?;
    tracing::info!(%bind, "HTTP server listening");
    axum::serve(listener, router(metrics, ready))
        .with_graceful_shutdown(shutdown)
        .await
        .map_err(|e| anyhow::anyhow!("HTTP server failed: {e}"))
}

async fn healthz() -> &'static str {
    "ok"
}

async fn readyz(State(state): State<AppState>) -> impl IntoResponse {
    if state.ready.load(Ordering::SeqCst) {
        (StatusCode::OK, "ok")
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "not ready")
    }
}

async fn metrics_handler(State(state): State<AppState>) -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, OPENMETRICS_CONTENT_TYPE)],
        state.metrics.encode(),
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    async fn get_raw(addr: SocketAddr, path: &str) -> String {
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let request = format!("GET {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        response
    }

    #[tokio::test]
    async fn endpoints() {
        let metrics = Arc::new(Metrics::new());
        let ready = Arc::new(AtomicBool::new(false));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(
            axum::serve(listener, router(metrics, Arc::clone(&ready)))
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .into_future(),
        );

        assert!(get_raw(addr, "/healthz").await.starts_with("HTTP/1.1 200"));
        assert!(get_raw(addr, "/readyz").await.starts_with("HTTP/1.1 503"));
        ready.store(true, Ordering::SeqCst);
        assert!(get_raw(addr, "/readyz").await.starts_with("HTTP/1.1 200"));

        let response = get_raw(addr, "/metrics").await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(
            response
                .to_ascii_lowercase()
                .contains(&format!("content-type: {OPENMETRICS_CONTENT_TYPE}")),
            "{response}"
        );
        assert!(
            response.contains("secretspec_reconcile_duration_seconds"),
            "{response}"
        );
        assert!(response.contains("secretspec_secretspecs"), "{response}");
        assert!(response.trim_end().ends_with("# EOF"), "{response}");

        stop.send(()).unwrap();
        server.await.unwrap().unwrap();
    }
}
