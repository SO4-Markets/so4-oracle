use std::sync::Arc;

use oracle::{api, AppState, Config};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    init_tracing();
    dotenvy::dotenv().ok();

    let config = match Config::from_env() {
        Ok(config) => Arc::new(config),
        Err(errors) => {
            for error in &errors.0 {
                tracing::error!(%error, "configuration failed");
            }
            std::process::exit(1);
        }
    };

    let bind_addr = config.bind_addr;
    let state = Arc::new(AppState::new(Arc::clone(&config)));
    let app = api::build_router(Arc::clone(&state));

    #[allow(unused_mut)]
    let listener = match TcpListener::bind(bind_addr).await {
        Ok(listener) => listener,
        Err(error) => {
            tracing::error!(%error, %bind_addr, "failed to bind listener");
            std::process::exit(1);
        }
    };

    let price_loop = tokio::spawn(oracle::price_loop::run_price_loop(Arc::clone(&state)));
    let keeper_loop = tokio::spawn(oracle::keeper_loop::run_keeper_loop(Arc::clone(&state)));

    tracing::info!(
        %bind_addr,
        network = config.network.as_str(),
        "oracle server listening"
    );

    let shutdown_token = state.shutdown_token.clone();
    let server_future =
        axum::serve(listener, app).with_graceful_shutdown(shutdown_signal(shutdown_token.clone()));

    let server_result =
        tokio::time::timeout(std::time::Duration::from_secs(30), server_future).await;

    match server_result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            tracing::error!(%error, "server error");
            std::process::exit(1);
        }
        Err(_) => {
            tracing::warn!("server shutdown timed out after 30s, canceling background tasks");
        }
    }

    tracing::info!("shutdown initiated, draining...");
    state.shutdown_token.cancel();

    let _ = tokio::join!(price_loop, keeper_loop);
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

async fn shutdown_signal(token: CancellationToken) {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(%error, "failed to install SIGINT handler");
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                tracing::error!(%error, "failed to install SIGTERM handler");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!("received SIGINT"),
        _ = terminate => tracing::info!("received SIGTERM"),
    }

    token.cancel();
}
