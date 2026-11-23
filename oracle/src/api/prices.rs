use std::sync::Arc;
use std::time::Duration;

use axum::extract::{FromRequestParts, Query, State};
use axum::http::request::Parts;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use super::{AdminAuth, ApiError};
use crate::state::{AppState, CachedPrice, FailedSubmission};

/// Attempts for each `/ready` external probe. The keeper-balance check has
/// retried since #590; the RPC-reachability check uses the same policy (#894).
const READY_BALANCE_RETRY_ATTEMPTS: u32 = 3;
/// Base delay for the exponential backoff between `/ready` probe attempts.
const READY_BALANCE_RETRY_BASE_DELAY_MS: u64 = 100;
/// Per-attempt timeout for the RPC-reachability probe. The shared HTTP client
/// defaults to 15s, but `perform_external_ready_checks` runs under
/// `READY_CHECK_TIMEOUT_SECS`; a shorter per-attempt timeout lets all
/// `READY_BALANCE_RETRY_ATTEMPTS` attempts (plus backoff) complete instead of
/// the first attempt consuming the whole readiness budget (#894).
const READY_RPC_PROBE_TIMEOUT_SECS: u64 = 4;
/// Hard cap on the entire `/ready` external-check path (RPC reachability +
/// keeper balance with retries). Must stay below both Fly's and Railway's
/// health-check timeouts so the service returns 503 promptly instead of
/// hanging until the platform probe kills it (#1042).
pub(crate) const READY_CHECK_TIMEOUT_SECS: u64 = 15;

#[derive(Debug, Deserialize)]
pub struct FailedSubmissionsQuery {
    /// Optional **prefix** filter on the failure's operation label. This is a
    /// prefix match, not equality: `?operation=execute` matches `execute_order`,
    /// `execute_deposit` and `execute_withdrawal`, while `?operation=order`
    /// matches nothing (the labels start with `execute_`).
    pub operation: Option<String>,
    /// Maximum records to return (default 100, capped at the ring buffer size).
    /// Does not affect `matched_count` or `total_count`.
    pub limit: Option<usize>,
}

// #602 — axum's built-in `Query` rejection renders as a bare text/plain body,
// which breaks the `{"error": "..."}` envelope every other endpoint returns.
// Extracting through this impl maps the rejection onto `ApiError` so a malformed
// `?limit=` value stays parseable for clients that unconditionally read JSON.
impl FromRequestParts<Arc<AppState>> for FailedSubmissionsQuery {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        Query::<Self>::from_request_parts(parts, state)
            .await
            .map(|Query(query)| query)
            .map_err(|rejection| ApiError::new(rejection.status(), rejection.body_text()))
    }
}

// #1280 — public probes must not expose internal failure counters or cycle
// timestamps to unauthenticated callers. Liveness probes only need status+version;
// counters have moved to the admin-only GET /oracle/status endpoint.
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub version: &'static str,
}

#[derive(Debug, Serialize)]
pub struct FailuresResponse {
    pub failures: Vec<FailedSubmission>,
    /// Records matching `?operation=` before `?limit=` is applied.
    pub matched_count: usize,
    /// Records in the in-process ring buffer before filtering or limiting.
    /// Paged callers can use this to tell how many records exist overall.
    pub total_count: usize,
}

pub async fn health(_state: State<Arc<AppState>>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}

pub async fn ready(State(state): State<Arc<AppState>>) -> Result<Json<HealthResponse>, ApiError> {
    // Check price cache has at least one cycle completed
    {
        let cache = state.price_cache.read().await;
        if cache.prices.is_empty() {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "no_prices_cached",
            ));
        }
    }

    // Check price loop is not stale (must have run within 3x the loop interval)
    {
        let cycle = state.cycle_status.read().await;
        let stale_threshold = state.config.price_loop_interval * 3;
        let is_stale = cycle
            .last_price_cycle_at
            .map(|last| last.elapsed().unwrap_or_default() > stale_threshold)
            .unwrap_or(true);
        if is_stale {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "price_loop_stale",
            ));
        }
    }

    // Check keeper loop is not stale (must have run within 3x the loop interval)
    {
        let cycle = state.cycle_status.read().await;
        let stale_threshold = state.config.keeper_loop_interval * 3;
        let is_stale = cycle
            .last_keeper_cycle_at
            .map(|last| last.elapsed().unwrap_or_default() > stale_threshold)
            .unwrap_or(true);
        if is_stale {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "keeper_loop_stale",
            ));
        }
    }

    // Check cached external RPC and keeper balance readiness result (3s TTL).
    // Hold a single write lock across the entire check-then-populate sequence
    // so concurrent requests during a cache miss are coalesced into one
    // outbound check rather than each independently triggering
    // perform_external_ready_checks (#1021).
    {
        let mut cache = state.ready_cache.write().await;
        if let Some(last) = cache.last_checked {
            if last.elapsed() < std::time::Duration::from_secs(3) {
                if let Some((status, msg)) = cache.last_error.clone() {
                    return Err(ApiError::new(status, msg));
                }
                return Ok(ready_response());
            }
        }

        // #1042 — cap the total worst-case duration of external checks so the
        // endpoint always returns 503 promptly instead of hanging past the
        // platform's health-check timeout (Fly: 20s, Railway: 30s).
        let check_res = match tokio::time::timeout(
            Duration::from_secs(READY_CHECK_TIMEOUT_SECS),
            perform_external_ready_checks(&state),
        )
        .await
        {
            Ok(result) => result,
            Err(_elapsed) => Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "ready_check_timeout",
            )),
        };
        cache.last_checked = Some(std::time::Instant::now());
        match check_res {
            Ok(()) => {
                cache.last_error = None;
            }
            Err(err) => {
                cache.last_error = Some((err.status, err.message.clone()));
                return Err(err);
            }
        }
    }

    Ok(ready_response())
}

/// `/ready`'s success body: the same status+version as `/health`. The
/// counters and cycle timestamps it used to build here moved to the
/// admin-only `/oracle/status` in #1280, and the leftover fields no longer
/// existed on `HealthResponse`, so `ready` did not compile (#1415).
fn ready_response() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}

/// Run a single `/ready` external probe with the shared retry policy (#590,
/// #894): up to `READY_BALANCE_RETRY_ATTEMPTS` attempts with an exponential
/// backoff between them. An error for which `is_terminal` returns `true` (for
/// example `RpcError::BalanceBelowMinimum`) is returned immediately instead of
/// being retried, preserving the keeper-balance check's fail-fast behaviour.
async fn retry_ready_probe<T, E, F, Fut>(
    mut probe: F,
    is_terminal: impl Fn(&E) -> bool,
    probe_name: &str,
) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
    E: std::fmt::Debug,
{
    let mut last_error = None;

    for attempt in 1..=READY_BALANCE_RETRY_ATTEMPTS {
        match probe().await {
            Ok(value) => return Ok(value),
            Err(error) => {
                if is_terminal(&error) {
                    return Err(error);
                }
                tracing::warn!(
                    attempt,
                    max_attempts = READY_BALANCE_RETRY_ATTEMPTS,
                    probe = probe_name,
                    error = ?error,
                    "ready probe attempt failed"
                );
                last_error = Some(error);
                if attempt < READY_BALANCE_RETRY_ATTEMPTS {
                    tokio::time::sleep(Duration::from_millis(
                        READY_BALANCE_RETRY_BASE_DELAY_MS * 2_u64.pow(attempt - 1),
                    ))
                    .await;
                }
            }
        }
    }

    Err(last_error.expect("READY_BALANCE_RETRY_ATTEMPTS is greater than zero"))
}

/// A single RPC-reachability probe against the configured Stellar RPC URL.
async fn probe_rpc_reachability(state: &AppState) -> Result<(), ApiError> {
    let response = crate::http::with_correlation(state.http.get(&state.config.stellar_rpc_url))
        .timeout(Duration::from_secs(READY_RPC_PROBE_TIMEOUT_SECS))
        .send()
        .await
        .map_err(|_| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "rpc_unreachable"))?;

    if !response.status().is_success() {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "rpc_unhealthy",
        ));
    }

    Ok(())
}

async fn perform_external_ready_checks(state: &AppState) -> Result<(), ApiError> {
    // Check RPC reachability. This runs before the keeper-balance check, so a
    // single dropped connection or timeout used to short-circuit the whole
    // readiness probe; retry it with the same policy as the balance check
    // (#590, #894).
    retry_ready_probe(
        || probe_rpc_reachability(state),
        |_| false,
        "rpc reachability",
    )
    .await?;

    // Check keeper balance
    let keeper_cfg = crate::keeper::KeeperBalanceConfig {
        horizon_url: state.config.horizon_url.clone(),
        account_id: state.config.keeper_account_id.clone(),
        min_balance_xlm: state.config.min_keeper_balance_xlm,
    };

    match retry_ready_probe(
        || crate::keeper::check_keeper_balance(&keeper_cfg, &state.keeper_balance_below_min),
        |error: &crate::stellar_rpc::RpcError| {
            matches!(
                error,
                crate::stellar_rpc::RpcError::BalanceBelowMinimum { .. }
            )
        },
        "keeper balance",
    )
    .await
    {
        Ok(_) => {}
        Err(crate::stellar_rpc::RpcError::BalanceBelowMinimum { .. }) => {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "keeper_balance_low",
            ));
        }
        Err(error) => {
            tracing::warn!(error = %error, "keeper balance check failed");
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "keeper_balance_check_failed",
            ));
        }
    }
    Ok(())
}

pub async fn prices(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<CachedPrice>>, ApiError> {
    let cache = state.price_cache.read().await;
    if cache.prices.is_empty() {
        return Err(ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "no_prices"));
    }
    Ok(Json(cache.prices.values().cloned().collect()))
}

/// Returns the in-process failure ring buffer, newest first.
///
/// `?operation=` is a **prefix** match on the record's operation label (see
/// [`FailedSubmissionsQuery::operation`]); it is deliberately not an equality
/// filter so one query can collect every `execute_*` failure. `?limit=` caps
/// only the returned page. The response reports both `matched_count` (after
/// filtering, before limiting) and `total_count` (the whole buffer) so paginated
/// callers can tell how many records exist overall.
pub async fn failed_submissions(
    _auth: AdminAuth,
    query: FailedSubmissionsQuery,
    State(state): State<Arc<AppState>>,
) -> Json<FailuresResponse> {
    let all_failures = state.failures.lock().await;
    let total_count = all_failures.len();
    let failures_iter = all_failures.iter().rev();

    let filtered: Vec<FailedSubmission> = match &query.operation {
        Some(op) => failures_iter
            .filter(|f| f.operation().starts_with(op.as_str()))
            .cloned()
            .collect(),
        None => failures_iter.cloned().collect(),
    };

    let matched_count = filtered.len();
    let limit = query.limit.unwrap_or(100).min(256);
    let failures: Vec<_> = filtered.into_iter().take(limit).collect();

    Json(FailuresResponse {
        failures,
        matched_count,
        total_count,
    })
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;
    use std::time::Instant;

    use super::*;
    use crate::{AppState, Config};

    fn test_state() -> Arc<AppState> {
        let config = Arc::new(Config::default_for_tests());
        Arc::new(AppState::new(config))
    }

    // #339 — GET /health must return 200 with {"status":"ok"}, no auth required
    // #1280 — public probes must not expose internal failure counters
    #[tokio::test]
    async fn health_returns_status_ok_and_version_only() {
        let state = test_state();
        let Json(body) = health(State(state)).await;
        assert_eq!(body.status, "ok");
        assert!(!body.version.is_empty());
        // Counters must NOT be present on the public response (they moved to /oracle/status).
        let json = serde_json::to_value(&body).unwrap();
        assert!(json.get("price_cycle_count").is_none());
        assert!(json.get("keeper_cycle_count").is_none());
        assert!(json.get("token_fetch_failures").is_none());
        assert!(json.get("submit_failures").is_none());
        assert!(json.get("last_price_cycle_secs_ago").is_none());
        assert!(json.get("last_keeper_cycle_secs_ago").is_none());
    }

    /// #894 - the shared readiness retry helper recovers from a transient error.
    #[tokio::test(start_paused = true)]
    async fn retry_ready_probe_recovers_from_transient_failure() {
        let calls = Rc::new(Cell::new(0u32));
        let counter = Rc::clone(&calls);

        let result: Result<(), &'static str> = retry_ready_probe(
            || {
                let counter = Rc::clone(&counter);
                async move {
                    let call = counter.get() + 1;
                    counter.set(call);
                    if call < READY_BALANCE_RETRY_ATTEMPTS {
                        Err("transient")
                    } else {
                        Ok(())
                    }
                }
            },
            |_| false,
            "test",
        )
        .await;

        assert_eq!(result, Ok(()));
        assert_eq!(calls.get(), READY_BALANCE_RETRY_ATTEMPTS);
    }

    /// #894 - terminal errors are returned without consuming retries.
    #[tokio::test(start_paused = true)]
    async fn retry_ready_probe_returns_terminal_error_immediately() {
        let calls = Rc::new(Cell::new(0u32));
        let counter = Rc::clone(&calls);

        let result: Result<(), &'static str> = retry_ready_probe(
            || {
                let counter = Rc::clone(&counter);
                async move {
                    counter.set(counter.get() + 1);
                    Err("terminal")
                }
            },
            |_| true,
            "test",
        )
        .await;

        assert_eq!(result, Err("terminal"));
        assert_eq!(calls.get(), 1);
    }

    /// #894 - a probe that keeps failing is retried a bounded number of times.
    #[tokio::test(start_paused = true)]
    async fn retry_ready_probe_exhausts_attempts() {
        let calls = Rc::new(Cell::new(0u32));
        let counter = Rc::clone(&calls);

        let result: Result<(), &'static str> = retry_ready_probe(
            || {
                let counter = Rc::clone(&counter);
                async move {
                    counter.set(counter.get() + 1);
                    Err("always fails")
                }
            },
            |_| false,
            "test",
        )
        .await;

        assert_eq!(result, Err("always fails"));
        assert_eq!(calls.get(), READY_BALANCE_RETRY_ATTEMPTS);
    }

    // ── #1415: ready, prices and failed_submissions ──────────────────────

    /// State whose RPC and Horizon URLs refuse connections immediately, so a
    /// test fails fast (rather than hanging) if a cached path makes a call.
    fn offline_state() -> Arc<AppState> {
        let mut config = Config::default_for_tests();
        config.stellar_rpc_url = "http://127.0.0.1:9".to_string();
        config.horizon_url = "http://127.0.0.1:9".to_string();
        Arc::new(AppState::new(Arc::new(config)))
    }

    fn cached_price(symbol: &str) -> CachedPrice {
        CachedPrice {
            token_address: format!("C{symbol}"),
            symbol: symbol.to_string(),
            display_symbol: symbol.to_string(),
            keeper_index: 0,
            min: 1,
            max: 2,
            median: 1,
            timestamp: 0,
            ledger_seq: 1,
            sources_used: vec!["fixed".to_string()],
            signature: String::new(),
        }
    }

    async fn add_price(state: &AppState, symbol: &str) {
        let mut cache = state.price_cache.write().await;
        cache
            .prices
            .insert(symbol.to_string(), cached_price(symbol));
    }

    async fn mark_cycles(state: &AppState, price: bool, keeper: bool) {
        let mut cycle = state.cycle_status.write().await;
        let now = std::time::SystemTime::now();
        cycle.last_price_cycle_at = price.then_some(now);
        cycle.last_keeper_cycle_at = keeper.then_some(now);
    }

    async fn ready_error(state: Arc<AppState>) -> ApiError {
        ready(State(state)).await.expect_err("ready should fail")
    }

    #[tokio::test]
    async fn ready_without_cached_prices_is_unavailable() {
        let error = ready_error(offline_state()).await;
        assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(error.message, "no_prices_cached");
    }

    #[tokio::test]
    async fn ready_with_a_stale_price_loop_is_unavailable() {
        let state = offline_state();
        add_price(&state, "BTC").await;
        mark_cycles(&state, false, true).await;

        assert_eq!(ready_error(state).await.message, "price_loop_stale");
    }

    #[tokio::test]
    async fn ready_with_a_stale_keeper_loop_is_unavailable() {
        let state = offline_state();
        add_price(&state, "BTC").await;
        mark_cycles(&state, true, false).await;

        assert_eq!(ready_error(state).await.message, "keeper_loop_stale");
    }

    // A fresh cached success is served without any outbound call: the RPC
    // and Horizon URLs refuse connections, so a real check would fail.
    #[tokio::test]
    async fn ready_serves_a_fresh_cached_success_without_outbound_calls() {
        let state = offline_state();
        add_price(&state, "BTC").await;
        mark_cycles(&state, true, true).await;
        state.ready_cache.write().await.last_checked = Some(Instant::now());

        let Json(body) = ready(State(state)).await.expect("cached success");
        assert_eq!(body.status, "ok");
    }

    #[tokio::test]
    async fn ready_serves_a_fresh_cached_failure() {
        let state = offline_state();
        add_price(&state, "BTC").await;
        mark_cycles(&state, true, true).await;
        {
            let mut cache = state.ready_cache.write().await;
            cache.last_checked = Some(Instant::now());
            let error = (StatusCode::SERVICE_UNAVAILABLE, "keeper_balance_low".into());
            cache.last_error = Some(error);
        }

        assert_eq!(ready_error(state).await.message, "keeper_balance_low");
    }

    // An expired cache entry re-runs the external checks and records the
    // new outcome for the next request.
    #[tokio::test]
    async fn ready_rechecks_after_the_cache_expires_and_records_the_result() {
        let state = offline_state();
        add_price(&state, "BTC").await;
        mark_cycles(&state, true, true).await;
        let expired = Instant::now().checked_sub(Duration::from_secs(10));
        state.ready_cache.write().await.last_checked = expired;

        let error = ready_error(Arc::clone(&state)).await;

        assert_eq!(error.message, "rpc_unreachable");
        let cache = state.ready_cache.read().await;
        let recorded = cache.last_error.clone().map(|(_, message)| message);
        assert_eq!(recorded.as_deref(), Some("rpc_unreachable"));
        assert!(cache.last_checked > expired);
    }

    #[tokio::test]
    async fn prices_is_unavailable_until_the_first_cycle() {
        let error = prices(State(offline_state()))
            .await
            .expect_err("empty cache");
        assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(error.message, "no_prices");
    }

    #[tokio::test]
    async fn prices_returns_every_cached_price() {
        let state = offline_state();
        add_price(&state, "BTC").await;
        add_price(&state, "ETH").await;

        let Json(body) = prices(State(state)).await.expect("cached prices");
        let mut symbols: Vec<_> = body.iter().map(|p| p.symbol.as_str()).collect();
        symbols.sort_unstable();
        assert_eq!(symbols, ["BTC", "ETH"]);
    }

    fn failure(operation: &str) -> FailedSubmission {
        FailedSubmission::Price {
            at: std::time::SystemTime::now(),
            operation: operation.to_string(),
            network: "testnet".to_string(),
            token: String::new(),
            symbol: String::new(),
            source: None,
            min: 0,
            max: 0,
            tx_hash: None,
            error: "boom".to_string(),
            timestamp: 0,
            ledger_seq: 0,
        }
    }

    async fn query_failures(
        state: Arc<AppState>,
        operation: Option<&str>,
        limit: Option<usize>,
    ) -> FailuresResponse {
        let query = FailedSubmissionsQuery {
            operation: operation.map(str::to_string),
            limit,
        };
        let Json(body) = failed_submissions(AdminAuth, query, State(state)).await;
        body
    }

    #[tokio::test]
    async fn failed_submissions_filters_by_operation_prefix_newest_first() {
        let state = offline_state();
        {
            let mut failures = state.failures.lock().await;
            failures.push(failure("source:BTC:binance"));
            failures.push(failure("execute_order"));
            failures.push(failure("source:ETH:pyth"));
        }

        let body = query_failures(state, Some("source:"), None).await;

        assert_eq!(body.matched_count, 2);
        assert_eq!(body.total_count, 3);
        let operations: Vec<_> = body.failures.iter().map(|f| f.operation()).collect();
        assert_eq!(operations, ["source:ETH:pyth", "source:BTC:binance"]);
    }

    #[tokio::test]
    async fn failed_submissions_limit_defaults_to_100_and_is_capped_at_256() {
        let state = offline_state();
        {
            let mut failures = state.failures.lock().await;
            for i in 0..300 {
                failures.push(failure(&format!("op-{i}")));
            }
        }

        let defaulted = query_failures(Arc::clone(&state), None, None).await;
        let capped = query_failures(Arc::clone(&state), None, Some(10_000)).await;
        let small = query_failures(state, None, Some(5)).await;

        assert_eq!(defaulted.failures.len(), 100);
        assert_eq!(capped.failures.len(), capped.matched_count.min(256));
        assert_eq!(small.failures.len(), 5);
        assert_eq!(small.total_count, defaulted.total_count);
    }
}
