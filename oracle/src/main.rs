use std::sync::Arc;

use oracle::{api, AppState, Config};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    init_tracing();
    load_dotenv();

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

    // If either background task panics or returns unexpectedly while the
    // server is still running, trigger a full shutdown so an external
    // restart policy can take over rather than serving a half-dead process.
    tokio::select! {
        result = price_loop => {
            match result {
                Ok(()) => tracing::error!("price_loop exited unexpectedly"),
                Err(e) => tracing::error!(error = %e, "price_loop panicked"),
            }
            state.shutdown_token.cancel();
        }
        result = keeper_loop => {
            match result {
                Ok(()) => tracing::error!("keeper_loop exited unexpectedly"),
                Err(e) => tracing::error!(error = %e, "keeper_loop panicked"),
            }
            state.shutdown_token.cancel();
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
        }
    }

    tracing::info!("shutdown initiated, draining...");
    state.shutdown_token.cancel();
}

/// Load `.env`, and say so when one exists but does not parse.
///
/// `dotenvy::dotenv()` returns `Err` for two very different situations: no `.env` file at all,
/// which is the normal case for a deployment that sets real environment variables, and a `.env`
/// that exists but fails to parse — an unescaped quote, a line with no `=`, a BOM. `.ok()` threw
/// both away, so a typo in `.env` meant the file was silently ignored and the operator's only clue
/// was `Config::from_env()` complaining that some unrelated variable was missing.
///
/// A missing file stays silent. A parse failure is a `warn!`, not an `error!`: the process can
/// still start from the real environment, and exiting here would turn a stray character in an
/// optional file into an outage.
fn load_dotenv() {
    match dotenvy::dotenv() {
        Ok(_) => {}
        // `Io` is "no such file" in practice, and a deployment without a `.env` is normal.
        Err(dotenvy::Error::Io(_)) => {}
        Err(error) => {
            tracing::warn!(%error, "ignored a .env file that could not be parsed");
        }
    }
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

#[cfg(test)]
mod tests {
    /// The premise `load_dotenv` matches on: `Io` means "no file", anything else means the file was
    /// there and the parser rejected it. Asserted rather than assumed, because the whole change is
    /// that those two stop being treated the same.
    #[test]
    fn dotenvy_distinguishes_a_missing_file_from_an_unparseable_one() {
        let dir = std::env::temp_dir().join(format!("so4-dotenv-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");

        let missing = dir.join("does-not-exist.env");
        match dotenvy::from_path(&missing) {
            Err(dotenvy::Error::Io(_)) => {}
            other => panic!("a missing file should be Io, got {other:?}"),
        }

        let malformed = dir.join("malformed.env");
        std::fs::write(&malformed, "this line has no equals sign\n").expect("write");
        match dotenvy::from_path(&malformed) {
            Err(dotenvy::Error::Io(_)) => {
                panic!("a malformed file must not look like a missing one")
            }
            Err(_) => {}
            Ok(()) => panic!("a line with no '=' should not parse"),
        }

        std::fs::remove_dir_all(&dir).ok();
    }
}
