use std::sync::Arc;

use oracle::{api, AppState, Config};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    // A missing .env is the normal case for a production deployment that sets
    // env vars directly — stay quiet for it. A .env that *exists but fails to
    // parse* is an operator mistake that would otherwise surface only as a
    // confusing downstream "required env var X not set" (#809).
    if let Err(error) = dotenvy::dotenv() {
        if error.not_found() {
            tracing::debug!("no .env file found; using process environment only");
        } else {
            tracing::warn!(%error, "failed to load .env file; using process environment only");
        }
    }
    init_tracing();

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

    let mut price_loop = tokio::spawn(oracle::price_loop::run_price_loop(Arc::clone(&state)));
    let mut keeper_loop = tokio::spawn(oracle::keeper_loop::run_keeper_loop(Arc::clone(&state)));

    tracing::info!(
        %bind_addr,
        network = config.network.as_str(),
        "oracle server listening"
    );

    let shutdown_token = state.shutdown_token.clone();
    let server_future =
        axum::serve(listener, app).with_graceful_shutdown(shutdown_signal(shutdown_token.clone()));

    // How long to wait for the background loops to finish their in-progress
    // cycle once shutdown has been signalled. A keeper cycle can legitimately
    // run for KEEPER_CYCLE_TIMEOUT_SECS (50s) and a price cycle longer, so
    // without a bound `tokio::join!` below can block well past the
    // orchestrator's SIGTERM grace period (Docker 10s, Kubernetes 30s) — the
    // process then gets SIGKILLed mid-loop, which is exactly what the bounded
    // server shutdown above exists to prevent (#552, #807).
    let drain_timeout = std::time::Duration::from_secs(30);

    // If either background task panics or returns unexpectedly while the
    // server is still running, trigger a full shutdown so an external
    // restart policy can take over rather than serving a half-dead process.
    let exited_early = tokio::select! {
        result = &mut price_loop => {
            match result {
                Ok(()) => tracing::error!("price_loop exited unexpectedly"),
                Err(e) => tracing::error!(error = %e, "price_loop panicked"),
            }
            state.shutdown_token.cancel();
            Some(BackgroundTask::Price)
        }
        result = &mut keeper_loop => {
            match result {
                Ok(()) => tracing::error!("keeper_loop exited unexpectedly"),
                Err(e) => tracing::error!(error = %e, "keeper_loop panicked"),
            }
            state.shutdown_token.cancel();
            Some(BackgroundTask::Keeper)
        }
        result = tokio::time::timeout(std::time::Duration::from_secs(30), server_future) => {
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    tracing::error!(%error, "server error");
                    std::process::exit(1);
                }
                Err(_) => {
                    tracing::warn!("server shutdown timed out after 30s, canceling background tasks");
                }
            }
            None
        }
    };

    tracing::info!("shutdown initiated, draining...");
    state.shutdown_token.cancel();

    // Bounded drain of whichever loops are still running, matching the
    // server-shutdown bound above. If they don't finish in time, abort them
    // and let the process exit rather than blocking indefinitely (#807).
    let drain = async {
        match exited_early {
            Some(BackgroundTask::Price) => {
                let _ = (&mut keeper_loop).await;
            }
            Some(BackgroundTask::Keeper) => {
                let _ = (&mut price_loop).await;
            }
            None => {
                let _ = tokio::join!(&mut price_loop, &mut keeper_loop);
            }
        }
    };
    if tokio::time::timeout(drain_timeout, drain).await.is_err() {
        tracing::warn!(
            timeout_secs = drain_timeout.as_secs(),
            "background tasks did not drain in time; aborting and exiting"
        );
        price_loop.abort();
        keeper_loop.abort();
    } else {
        tracing::info!("background tasks drained");
    }
}

enum BackgroundTask {
    Price,
    Keeper,
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(filter)
        .with_current_span(true)
        .with_span_list(true)
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
