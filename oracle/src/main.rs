use std::sync::Arc;

use oracle::{api, AppState, Config};
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    init_tracing();
    dotenvy::dotenv().ok();

    let config = match Config::from_env() {
        Ok(config) => Arc::new(config),
        Err(error) => {
            tracing::error!(%error, "configuration failed");
            eprintln!("configuration error: {error}");
            std::process::exit(1);
        }
    };

    let bind_addr = config.bind_addr;
    let state = Arc::new(AppState::new(Arc::clone(&config)));
    let app = api::build_router(Arc::clone(&state));
    let price_loop = tokio::spawn(oracle::price_loop::run_price_loop(Arc::clone(&state)));
    let keeper_loop = tokio::spawn(oracle::keeper_loop::run_keeper_loop(Arc::clone(&state)));

    let listener = match TcpListener::bind(bind_addr).await {
        Ok(listener) => listener,
        Err(error) => {
            tracing::error!(%error, %bind_addr, "failed to bind listener");
            eprintln!("failed to bind {bind_addr}: {error}");
            std::process::exit(1);
        }
    };

    tracing::info!(
        %bind_addr,
        network = config.network.as_str(),
        "oracle server listening"
    );

    let server = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await;
    price_loop.abort();
    keeper_loop.abort();

    if let Err(error) = server {
        tracing::error!(%error, "server error");
        eprintln!("server error: {error}");
        std::process::exit(1);
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(filter)
        .with_current_span(false)
        .with_span_list(false)
        .init();
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(%error, "failed to install SIGINT handler");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => tracing::error!(%error, "failed to install SIGTERM handler"),
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!("received SIGINT"),
        _ = terminate => tracing::info!("received SIGTERM"),
    }
}
