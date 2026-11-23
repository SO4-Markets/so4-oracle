use std::sync::Arc;

use oracle::{api, preflight, AppState, Config};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

/// Exit code for configuration errors, distinct from transient runtime
/// failures so an orchestrator restart policy can avoid retrying a
/// misconfigured deployment (railway.json `restartPolicyMaxRetries`).
const CONFIG_ERROR_EXIT_CODE: i32 = 78;

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
    // `oracle --health-check` is the Docker HEALTHCHECK (#1431), so the
    // runtime image needs no curl. It only probes the running instance.
    if std::env::args().nth(1).as_deref() == Some("--health-check") {
        std::process::exit(health_check().await);
    }
    init_tracing();

    let config = match Config::from_env() {
        Ok(config) => Arc::new(config),
        Err(errors) => {
            tracing::error!(
                "configuration failed: {} errors\n{}",
                errors.0.len(),
                errors
            );
            std::process::exit(CONFIG_ERROR_EXIT_CODE);
        }
    };

    let bind_addr = config.bind_addr;
    let state = Arc::new(AppState::new(Arc::clone(&config)));

    if std::env::var("SKIP_PREFLIGHT").as_deref() == Ok("true") {
        tracing::warn!("SKIP_PREFLIGHT=true set; skipping startup preflight checks");
    } else {
        let result = preflight::preflight(&config).await;
        *state.preflight_result.write().await = Some(result.clone());
        if result.hard_failed() {
            tracing::error!("preflight hard checks failed; exiting");
            std::process::exit(CONFIG_ERROR_EXIT_CODE);
        }
    }
    let app = api::build_router(Arc::clone(&state));

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
    let server_future = axum::serve(
        listener,
        // Peer addresses feed the per-client admin-auth limiter (#1214).
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal(shutdown_token.clone()));

    // Split the DEFAULT_SHUTDOWN_TIMEOUT_SECS budget between HTTP drain and
    // background-loop drain so the *total* worst-case shutdown stays within
    // the orchestrator's grace period (Docker 10s, Kubernetes 30s). The two
    // timeouts are sequential: the server drains first, then the loops drain
    // afterward. Without splitting, a slow HTTP drain (15s) + a slow keeper
    // cycle (15s) = 30s total, which still fits. If each stage took the full
    // DEFAULT_SHUTDOWN_TIMEOUT_SECS (30s), the process could run 60s and get
    // SIGKILLed mid-loop, defeating the bounded shutdown (#552, #807, #870).
    let drain_timeout = drain_timeout();

    // If either background task panics or returns unexpectedly while the
    // server is still running, trigger a full shutdown so an external
    // restart policy can take over rather than serving a half-dead process.
    // See [profile.release] in Cargo.toml for the panic strategy.
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
        result = tokio::time::timeout(drain_timeout, server_future) => {
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    tracing::error!(%error, "server error");
                    std::process::exit(1);
                }
                Err(_) => {
                    tracing::warn!(
                        timeout_secs = drain_timeout.as_secs(),
                        "server shutdown timed out, canceling background tasks"
                    );
                }
            }
            None
        }
    };

    tracing::info!("shutdown initiated, draining...");
    state.shutdown_token.cancel();

    let drained = drain_background_tasks(
        &mut price_loop,
        &mut keeper_loop,
        exited_early,
        drain_timeout,
    )
    .await;
    if drained {
        tracing::info!("background tasks drained");
    } else {
        tracing::warn!(
            timeout_secs = drain_timeout.as_secs(),
            "background tasks did not drain in time; aborted them and exiting"
        );
    }
}

enum BackgroundTask {
    Price,
    Keeper,
}

/// Budget for each of the two sequential shutdown stages (HTTP drain, then
/// background-loop drain): half of DEFAULT_SHUTDOWN_TIMEOUT_SECS, so the total
/// stays inside the orchestrator grace period (#552, #807, #870).
fn drain_timeout() -> std::time::Duration {
    std::time::Duration::from_secs(oracle::config::DEFAULT_SHUTDOWN_TIMEOUT_SECS / 2)
}

/// Bounded drain of whichever loops are still running, matching the
/// server-shutdown bound. A loop that already exited (`exited_early`) is not
/// awaited again. If the rest don't finish within `timeout`, both are aborted
/// so the process exits rather than blocking indefinitely (#807). Returns
/// whether they drained in time. Extracted from `main` so the sequencing is
/// testable (#1418).
async fn drain_background_tasks(
    price_loop: &mut tokio::task::JoinHandle<()>,
    keeper_loop: &mut tokio::task::JoinHandle<()>,
    exited_early: Option<BackgroundTask>,
    timeout: std::time::Duration,
) -> bool {
    let drain = async {
        match exited_early {
            Some(BackgroundTask::Price) => {
                let _ = (&mut *keeper_loop).await;
            }
            Some(BackgroundTask::Keeper) => {
                let _ = (&mut *price_loop).await;
            }
            None => {
                let _ = tokio::join!(&mut *price_loop, &mut *keeper_loop);
            }
        }
    };
    if tokio::time::timeout(timeout, drain).await.is_ok() {
        return true;
    }
    price_loop.abort();
    keeper_loop.abort();
    false
}

/// `LOG_FORMAT=pretty` switches to human-readable output for local
/// development (#1457). Anything else, including unset, keeps JSON, which is
/// what the deployed log pipelines parse.
fn use_pretty_logs(log_format: Option<&str>) -> bool {
    log_format.is_some_and(|value| value.trim().eq_ignore_ascii_case("pretty"))
}

fn init_tracing() {
    let (filter, invalid_rust_log) = match std::env::var("RUST_LOG") {
        Ok(raw) => match EnvFilter::try_new(&raw) {
            Ok(filter) => (filter, None),
            Err(error) => (EnvFilter::new("info"), Some((raw, error))),
        },
        Err(_) => (EnvFilter::new("info"), None),
    };

    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    if use_pretty_logs(std::env::var("LOG_FORMAT").ok().as_deref()) {
        builder.pretty().init();
    } else {
        builder
            .json()
            .with_current_span(true)
            .with_span_list(true)
            .init();
    }

    if let Some((raw, error)) = invalid_rust_log {
        tracing::warn!(
            %error,
            invalid_rust_log = %raw,
            "failed to parse RUST_LOG environment variable; falling back to 'info'"
        );
    }
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

/// URL the `--health-check` probe calls: `/health` on loopback at the
/// `BIND_ADDR` port, falling back to the default port when unset or invalid.
fn health_check_url(bind_addr: Option<&str>) -> String {
    let port = bind_addr
        .unwrap_or(oracle::config::DEFAULT_BIND_ADDR)
        .parse::<std::net::SocketAddr>()
        .map_or(8080, |addr| addr.port());
    format!("http://127.0.0.1:{port}/health")
}

/// Exit code for `--health-check`: 0 when `/health` answers 2xx, 1 otherwise.
async fn health_check() -> i32 {
    let url = health_check_url(std::env::var("BIND_ADDR").ok().as_deref());
    match oracle::http::client().get(&url).send().await {
        Ok(response) if response.status().is_success() => 0,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::health_check_url;
    use super::{drain_background_tasks, drain_timeout, use_pretty_logs, BackgroundTask};
    use super::{
        drain_background_tasks, drain_timeout, health_check_url, use_pretty_logs, BackgroundTask,
    };

    #[test]
    fn health_check_targets_loopback_on_the_bind_port() {
        assert_eq!(health_check_url(None), "http://127.0.0.1:8080/health");
        assert_eq!(
            health_check_url(Some("0.0.0.0:9090")),
            "http://127.0.0.1:9090/health"
        );
        assert_eq!(
            health_check_url(Some("not-an-addr")),
            "http://127.0.0.1:8080/health"
        );
    }

    #[test]
    fn log_format_defaults_to_json() {
        assert!(!use_pretty_logs(None));
        assert!(!use_pretty_logs(Some("")));
        assert!(!use_pretty_logs(Some("json")));
        assert!(!use_pretty_logs(Some("something-else")));
    }

    #[test]
    fn log_format_pretty_is_case_insensitive() {
        assert!(use_pretty_logs(Some("pretty")));
        assert!(use_pretty_logs(Some("PRETTY")));
        assert!(use_pretty_logs(Some(" Pretty ")));
    }

    // #1418 — the two sequential stages together must fit the total budget.
    #[test]
    fn shutdown_stages_fit_the_total_budget() {
        let secs = oracle::config::DEFAULT_SHUTDOWN_TIMEOUT_SECS;
        let total = std::time::Duration::from_secs(secs);
        assert!(drain_timeout() * 2 <= total);
        assert!(!drain_timeout().is_zero());
    }

    #[tokio::test(start_paused = true)]
    async fn drain_returns_true_when_loops_finish() {
        let mut p = tokio::spawn(async {});
        let mut k = tokio::spawn(async {});
        let b = std::time::Duration::from_secs(15);
        assert!(drain_background_tasks(&mut p, &mut k, None, b).await);
    }

    #[tokio::test(start_paused = true)]
    async fn drain_aborts_loops_that_overrun_the_budget() {
        let mut p = tokio::spawn(std::future::pending::<()>());
        let mut k = tokio::spawn(async {});
        let b = std::time::Duration::from_secs(15);
        let started = tokio::time::Instant::now();

        assert!(!drain_background_tasks(&mut p, &mut k, None, b).await);
        assert!(started.elapsed() >= b);
        assert!(p.await.unwrap_err().is_cancelled());
    }

    // A loop that already exited is not awaited again: only the other one is.
    #[tokio::test(start_paused = true)]
    async fn drain_skips_the_loop_that_exited_early() {
        let mut p = tokio::spawn(std::future::pending::<()>());
        let mut k = tokio::spawn(async {});
        let b = std::time::Duration::from_secs(15);
        let early = Some(BackgroundTask::Price);

        assert!(drain_background_tasks(&mut p, &mut k, early, b).await);
        p.abort();
    }
}
