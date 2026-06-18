//! SO4 Oracle — main entrypoint.
//!
//! Loads config, builds shared state, and serves the axum HTTP API.
//! Graceful shutdown on SIGTERM / SIGINT.

use std::sync::Arc;
use tokio::sync::RwLock;

#[tokio::main]
async fn main() {
    // ── Tracing ──────────────────────────────────────────────────────────────
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    // ── Config ───────────────────────────────────────────────────────────────
    dotenvy::dotenv().ok();
    let config = oracle::config::Config::from_env().unwrap_or_else(|e| {
        tracing::error!(%e, "config validation failed");
        eprintln!("FATAL: config validation failed: {e}");
        std::process::exit(1);
    });

    // ── Shared state ─────────────────────────────────────────────────────────
    let http_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("reqwest::Client::builder");
    let state: Arc<RwLock<oracle::state::AppState>> =
        Arc::new(RwLock::new(oracle::state::AppState::new(config, http_client)));

    let bind_addr = {
        let guard = state.read().await;
        guard.config.bind_addr.clone()
    };

    // ── HTTP server ──────────────────────────────────────────────────────────
    use axum::{routing::get, Router};
    let app = Router::new()
        .route("/health", get(health))
        .with_state(state);

    tracing::info!(%bind_addr, "starting axum server");
    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .unwrap_or_else(|e| {
            eprintln!("FATAL: failed to bind {bind_addr}: {e}");
            std::process::exit(1);
        });

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .unwrap_or_else(|e| {
            eprintln!("FATAL: server error: {e}");
            std::process::exit(1);
        });
}

/// `GET /health` — liveness probe.
async fn health() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({"status": "ok"}))
}

/// Wait for SIGTERM or SIGINT.
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    tracing::info!("shutdown signal received, exiting gracefully");
}
