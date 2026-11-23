use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Path, State};
use axum::http::header::CONTENT_TYPE;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use tracing::info;

use super::{AdminAuth, ApiError};
use crate::state::{AppState, CachedPrice, FailedSubmission};

// #1280 — failure counters and last-cycle timestamps moved here from the public
// /health probe so they are only visible to authenticated admin callers.
// Matches prices.rs's READY_BALANCE_RETRY_* constants (#1024) — the same
// underlying Horizon call should be retried the same way regardless of
// which endpoint triggers it.
const KEEPER_BALANCE_RETRY_ATTEMPTS: u32 = 3;
const KEEPER_BALANCE_RETRY_BASE_DELAY_MS: u64 = 100;

/// Upper bound on the external (Horizon) work an admin handler does, retries
/// included (#1452). Below the global request timeout, so the handler — not
/// the outer 408 layer — decides what the caller sees when upstream is slow.
pub(crate) const ADMIN_EXTERNAL_BUDGET: std::time::Duration = std::time::Duration::from_secs(20);

#[derive(Debug, Serialize)]
pub struct OracleStatusResponse {
    pub last_cycle_time: Option<u64>,
    pub keeper_balance: Option<f64>,
    pub prices: Vec<CachedPrice>,
    pub recent_errors: Vec<FailedSubmission>,
    pub preflight: Option<crate::preflight::PreflightResult>,
    pub price_cycle_count: u64,
    pub keeper_cycle_count: u64,
    pub token_fetch_failures: u64,
    pub submit_failures: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_price_cycle_secs_ago: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_keeper_cycle_secs_ago: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct BlacklistedKey {
    pub key: String,
    pub consecutive_failures: u32,
}

#[derive(Debug, Serialize)]
pub struct KeeperStatusResponse {
    pub pending_orders: usize,
    pub pending_deposits: usize,
    pub pending_withdrawals: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_cycle_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_cycle_latency_ms: Option<u64>,
    pub last_executions: Vec<crate::state::KeeperExecution>,
    /// Order keys permanently blacklisted after MAX_CONSECUTIVE_FREEZE_FAILURES
    /// freeze failures. Previously only visible via a one-time ALERT log line
    /// with no way to discover or clear it through the API (#802).
    pub blacklisted_keys: Vec<BlacklistedKey>,
}

pub async fn oracle_status(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
) -> Json<OracleStatusResponse> {
    let cycle = state.cycle_status.read().await;
    let last_cycle_time = cycle.last_price_cycle_at.and_then(system_time_secs);
    let last_price_cycle_secs_ago = cycle
        .last_price_cycle_at
        .and_then(|t| t.elapsed().ok())
        .map(|d| d.as_secs());
    let last_keeper_cycle_secs_ago = cycle
        .last_keeper_cycle_at
        .and_then(|t| t.elapsed().ok())
        .map(|d| d.as_secs());
    drop(cycle);

    let metrics = state.metrics.to_response();
    let prices = state
        .price_cache
        .read()
        .await
        .prices
        .values()
        .cloned()
        .collect();
    let preflight = state.preflight_result.read().await.clone();
    let recent_errors: Vec<_> = state
        .failures
        .lock()
        .await
        .iter()
        .rev()
        .take(20)
        .cloned()
        .collect();

    // Check keeper balance using the same pattern as keeper_balance handler (#871).
    let keeper_cfg = crate::keeper::KeeperBalanceConfig {
        horizon_url: state.config.horizon_url.clone(),
        account_id: state.config.keeper_account_id.clone(),
        min_balance_xlm: state.config.min_keeper_balance_xlm,
    };

    let lookup = crate::retry::retry_with_backoff(
        || crate::keeper::check_keeper_balance(&keeper_cfg, &state.keeper_balance_below_min),
        KEEPER_BALANCE_RETRY_ATTEMPTS,
        KEEPER_BALANCE_RETRY_BASE_DELAY_MS,
        crate::retry::MAX_BACKOFF_DELAY_MS,
    );
    // Over budget is reported like any other lookup failure: no balance.
    let keeper_balance = tokio::time::timeout(ADMIN_EXTERNAL_BUDGET, lookup)
        .await
        .ok()
        .and_then(Result::ok)
        // Spendable balance, net of the account reserve, the same figure
        // /keeper/balance reports and the minimum is checked against (#1447).
        .map(|balance| crate::keeper::stroops_to_xlm(balance.spendable_stroops));

    Json(OracleStatusResponse {
        last_cycle_time,
        keeper_balance,
        prices,
        recent_errors,
        preflight,
        price_cycle_count: metrics.price_cycle_count,
        keeper_cycle_count: metrics.keeper_cycle_count,
        token_fetch_failures: metrics.token_fetch_failures,
        submit_failures: metrics.submit_failures,
        last_price_cycle_secs_ago,
        last_keeper_cycle_secs_ago,
    })
}

pub async fn keeper_status(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
) -> Json<KeeperStatusResponse> {
    // One consistent snapshot of both state objects — never a torn pair mixing
    // pending counts from before a keeper cycle with timing from after it (#797).
    let (keeper_status, cycle_status) = state.keeper_status_snapshot().await;

    let last_cycle_at = cycle_status.last_keeper_cycle_at.and_then(system_time_secs);
    let last_cycle_latency_ms = cycle_status.last_keeper_cycle_latency_ms;

    let last_executions: Vec<_> = keeper_status
        .last_executions
        .into_iter()
        .rev()
        .take(50)
        .collect();

    let blacklisted_keys = state
        .frozen_order_blacklist
        .lock()
        .await
        .iter()
        .map(|(key, consecutive_failures)| BlacklistedKey {
            key: key.clone(),
            consecutive_failures: *consecutive_failures,
        })
        .collect();

    Json(KeeperStatusResponse {
        pending_orders: keeper_status.pending_orders,
        pending_deposits: keeper_status.pending_deposits,
        pending_withdrawals: keeper_status.pending_withdrawals,
        last_cycle_at,
        last_cycle_latency_ms,
        last_executions,
        blacklisted_keys,
    })
}

#[derive(Debug, Serialize)]
pub struct ClearBlacklistResponse {
    pub key: String,
    pub cleared: bool,
}

/// Clear a single order key from `frozen_order_blacklist`, making the
/// "manual intervention required" the blacklist log message promises
/// actually possible through the API (#802).
///
/// Also resets the key's consecutive freeze-failure count and execution-failure
/// count, so it gets a fresh `MAX_CONSECUTIVE_FREEZE_FAILURES` and
/// `MAX_CONSECUTIVE_EXECUTION_FAILURES` budget instead of being re-blacklisted
/// after a single further failure (#887).
pub async fn clear_blacklisted_key(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> Result<Json<ClearBlacklistResponse>, ApiError> {
    let removed = state
        .frozen_order_blacklist
        .lock()
        .await
        .remove(&key)
        .is_some();

    if !removed {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "key_not_blacklisted"));
    }

    state.freeze_failure_counts.lock().await.remove(&key);
    state.execution_failure_counts.lock().await.remove(&key);

    info!(key = %key, "blacklisted order key cleared via admin API");
    Ok(Json(ClearBlacklistResponse { key, cleared: true }))
}

pub async fn metrics(_auth: AdminAuth, State(state): State<Arc<AppState>>) -> Response {
    let mut prometheus = state.metrics.to_prometheus(state.config.network.as_str());

    // ── Gauges from AppState not tracked by Metrics (#1267) ─────────────────

    // keeper XLM balance (last known value; 0 until the first balance check).
    let balance_stroops = state
        .keeper_balance_below_min
        .load(std::sync::atomic::Ordering::Relaxed);
    // We expose the below-min flag as a gauge (0/1) since the raw balance is
    // only known after a Horizon call. The balance_xlm gauge requires the live
    // RPC call that /keeper/balance already serves; duplicating it here would
    // block every scrape. Expose the boolean flag instead.
    prometheus.push_str("# HELP oracle_keeper_balance_below_min 1 if keeper XLM balance is below minimum threshold\n");
    prometheus.push_str("# TYPE oracle_keeper_balance_below_min gauge\n");
    prometheus.push_str(&format!(
        "oracle_keeper_balance_below_min {}\n",
        if balance_stroops { 1 } else { 0 }
    ));

    // Per-token price age in seconds (#1267).
    {
        let cache = state.price_cache.read().await;
        let now = std::time::SystemTime::now();
        prometheus.push_str(
            "# HELP oracle_price_age_seconds Seconds since each token's price was last cached\n",
        );
        prometheus.push_str("# TYPE oracle_price_age_seconds gauge\n");
        for (symbol, price) in &cache.prices {
            // CachedPrice.timestamp is a Unix timestamp in seconds set by the
            // price loop when it stores a fetched price.
            let now_secs = now
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let age = price.age_seconds(now_secs);
            prometheus.push_str(&format!(
                "oracle_price_age_seconds{{symbol=\"{}\"}} {}\n",
                symbol, age
            ));
        }
        // last_set_prices timestamp (#1267).
        prometheus.push_str("# HELP oracle_last_set_prices_timestamp_seconds Unix timestamp of the last successful set_prices submission\n");
        prometheus.push_str("# TYPE oracle_last_set_prices_timestamp_seconds gauge\n");
        let last_ts = cache
            .last_updated
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        prometheus.push_str(&format!(
            "oracle_last_set_prices_timestamp_seconds {}\n",
            last_ts
        ));
    }

    // Pending work counts from KeeperStatus (#1267).
    {
        let ks = state.keeper_status.read().await;
        prometheus.push_str("# HELP oracle_pending_keys Number of pending keys by kind\n");
        prometheus.push_str("# TYPE oracle_pending_keys gauge\n");
        prometheus.push_str(&format!(
            "oracle_pending_keys{{kind=\"orders\"}} {}\n",
            ks.pending_orders
        ));
        prometheus.push_str(&format!(
            "oracle_pending_keys{{kind=\"deposits\"}} {}\n",
            ks.pending_deposits
        ));
        prometheus.push_str(&format!(
            "oracle_pending_keys{{kind=\"withdrawals\"}} {}\n",
            ks.pending_withdrawals
        ));
    }

    // In-flight and blacklisted key counts (#1267).
    {
        let in_flight = state.in_flight_keys.lock().await.len();
        prometheus
            .push_str("# HELP oracle_in_flight_keys Keys currently being submitted on-chain\n");
        prometheus.push_str("# TYPE oracle_in_flight_keys gauge\n");
        prometheus.push_str(&format!("oracle_in_flight_keys {}\n", in_flight));

        let blacklisted = state.frozen_order_blacklist.lock().await.len();
        prometheus.push_str(
            "# HELP oracle_blacklisted_keys Keys permanently abandoned after repeated failures\n",
        );
        prometheus.push_str("# TYPE oracle_blacklisted_keys gauge\n");
        prometheus.push_str(&format!("oracle_blacklisted_keys {}\n", blacklisted));
    }

    (
        StatusCode::OK,
        [(CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8")],
        prometheus,
    )
        .into_response()
}

#[derive(Debug, Serialize)]
pub struct BalanceResponse {
    pub account_id: String,
    pub balance_stroops: i64,
    pub balance_xlm: f64,
    pub min_balance_xlm: f64,
    pub is_funded: bool,
}

pub async fn keeper_balance(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
) -> Result<Json<BalanceResponse>, ApiError> {
    let keeper_cfg = crate::keeper::KeeperBalanceConfig {
        horizon_url: state.config.horizon_url.clone(),
        account_id: state.config.keeper_account_id.clone(),
        min_balance_xlm: state.config.min_keeper_balance_xlm,
    };

    let lookup = crate::retry::retry_with_backoff(
        || crate::keeper::check_keeper_balance(&keeper_cfg, &state.keeper_balance_below_min),
        KEEPER_BALANCE_RETRY_ATTEMPTS,
        KEEPER_BALANCE_RETRY_BASE_DELAY_MS,
        crate::retry::MAX_BACKOFF_DELAY_MS,
    );
    let Ok(result) = tokio::time::timeout(ADMIN_EXTERNAL_BUDGET, lookup).await else {
        tracing::warn!(
            budget_secs = ADMIN_EXTERNAL_BUDGET.as_secs(),
            "keeper balance lookup exceeded its budget"
        );
        return Err(ApiError::new(
            StatusCode::GATEWAY_TIMEOUT,
            "upstream_timeout",
        ));
    };

    match result {
        Ok(balance) => {
            let response = crate::keeper::build_balance_response(&keeper_cfg, balance);
            Ok(Json(BalanceResponse {
                account_id: response.account_id,
                balance_stroops: response.balance_stroops,
                balance_xlm: response.balance_xlm,
                min_balance_xlm: response.min_balance_xlm,
                is_funded: !response.below_minimum,
            }))
        }
        Err(crate::stellar_rpc::RpcError::BalanceBelowMinimum {
            balance_stroops,
            balance_xlm,
            min_xlm,
        }) => Ok(Json(BalanceResponse {
            account_id: state.config.keeper_account_id.clone(),
            balance_stroops,
            balance_xlm,
            min_balance_xlm: min_xlm,
            is_funded: false,
        })),
        Err(_) => Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "keeper_balance_check_failed",
        )),
    }
}

fn system_time_secs(value: SystemTime) -> Option<u64> {
    value
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // #1024 — GET /keeper/balance now retries a transient Horizon failure
    // the same way /ready's equivalent check does, instead of surfacing it
    // as a failure on the first blip.
    #[tokio::test]
    async fn keeper_balance_retries_transient_horizon_failure_then_succeeds() {
        let server = MockServer::start().await;

        use std::sync::atomic::{AtomicUsize, Ordering};
        let request_count = std::sync::Arc::new(AtomicUsize::new(0));
        let counter = request_count.clone();

        Mock::given(method("GET"))
            .respond_with(move |_: &wiremock::Request| {
                if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                    ResponseTemplate::new(503)
                } else {
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "id": "GKEEPER",
                        "balances": [{"asset_type": "native", "balance": "20.0000000"}]
                    }))
                }
            })
            .mount(&server)
            .await;

        let mut config = Config::default_for_tests();
        config.horizon_url = server.uri();
        config.keeper_account_id = "GKEEPER".to_string();
        config.min_keeper_balance_xlm = 10.0;
        let state = std::sync::Arc::new(AppState::new(std::sync::Arc::new(config)));

        let result = keeper_balance(AdminAuth, State(state)).await;

        let Json(body) = result.expect("expected Ok after retry");
        assert!(body.is_funded);
        assert_eq!(body.balance_stroops, 200_000_000);
        assert_eq!(request_count.load(Ordering::SeqCst), 2);
    }

    // ── #1414: handler-level coverage ────────────────────────────────────

    /// State whose Horizon URL refuses connections immediately, so handlers
    /// that check the keeper balance fail fast instead of reaching a network.
    fn offline_state() -> Arc<AppState> {
        let mut config = Config::default_for_tests();
        config.horizon_url = "http://127.0.0.1:9".to_string();
        Arc::new(AppState::new(Arc::new(config)))
    }

    fn failure(operation: String) -> FailedSubmission {
        FailedSubmission::Price {
            at: SystemTime::now(),
            operation,
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

    fn execution(key: String) -> crate::state::KeeperExecution {
        crate::state::KeeperExecution {
            timestamp: SystemTime::now(),
            operation: "execute_order".to_string(),
            key,
            tx_hash: None,
            success: true,
            error: None,
        }
    }

    #[tokio::test]
    async fn oracle_status_caps_recent_errors_newest_first_and_reports_counters() {
        let state = offline_state();
        {
            let mut failures = state.failures.lock().await;
            for i in 0..25 {
                failures.push(failure(format!("op-{i}")));
            }
        }
        state.metrics.record_price_cycle(10, 1, 0);
        state.metrics.record_submit_failure();

        let Json(body) = oracle_status(AdminAuth, State(Arc::clone(&state))).await;

        assert_eq!(body.recent_errors.len(), 20);
        assert_eq!(body.recent_errors[0].operation(), "op-24");
        assert_eq!(body.recent_errors[19].operation(), "op-5");
        assert_eq!(body.price_cycle_count, 1);
        assert_eq!(body.submit_failures, 1);
        assert_eq!(body.keeper_balance, None, "Horizon is unreachable");
    }

    #[tokio::test]
    async fn keeper_status_caps_executions_and_lists_blacklisted_keys() {
        let state = offline_state();
        {
            let mut status = state.keeper_status.write().await;
            for i in 0..60 {
                let entry = execution(format!("k-{i}"));
                status.last_executions.push_back(entry);
            }
        }
        state
            .frozen_order_blacklist
            .lock()
            .await
            .insert("stuck".to_string(), 3);

        let Json(body) = keeper_status(AdminAuth, State(Arc::clone(&state))).await;

        assert_eq!(body.last_executions.len(), 50);
        assert_eq!(body.last_executions[0].key, "k-59");
        assert_eq!(body.blacklisted_keys.len(), 1);
        assert_eq!(body.blacklisted_keys[0].key, "stuck");
        assert_eq!(body.blacklisted_keys[0].consecutive_failures, 3);
    }

    #[tokio::test]
    async fn clear_blacklisted_key_404s_for_an_unknown_key() {
        let state = offline_state();

        let path = Path("nope".to_string());
        let result = clear_blacklisted_key(AdminAuth, State(state), path).await;

        let error = result.expect_err("unknown key must not be cleared");
        assert_eq!(error.status, StatusCode::NOT_FOUND);
        assert_eq!(error.message, "key_not_blacklisted");
    }

    // #887 — clearing must reset all three per-key maps, or the key is
    // re-blacklisted after a single further failure.
    #[tokio::test]
    async fn clear_blacklisted_key_clears_all_three_maps() {
        let state = offline_state();
        let key = "order-1".to_string();
        {
            let mut blacklist = state.frozen_order_blacklist.lock().await;
            blacklist.insert(key.clone(), 5);
            let mut freezes = state.freeze_failure_counts.lock().await;
            freezes.insert(key.clone(), 5);
            let mut executions = state.execution_failure_counts.lock().await;
            executions.insert(key.clone(), 2);
        }

        let handler_state = State(Arc::clone(&state));
        let path = Path(key.clone());
        let result = clear_blacklisted_key(AdminAuth, handler_state, path).await;
        let Json(body) = result.expect("blacklisted key clears");

        assert!(body.cleared);
        assert_eq!(body.key, key);
        let blacklist = state.frozen_order_blacklist.lock().await;
        let freezes = state.freeze_failure_counts.lock().await;
        let executions = state.execution_failure_counts.lock().await;
        assert!(!blacklist.contains_key(&key));
        assert!(!freezes.contains_key(&key));
        assert!(!executions.contains_key(&key));
    }

    // #1267 — the gauges appended after `to_prometheus` are part of the
    // endpoint's contract, as is the Prometheus text content type.
    #[tokio::test]
    async fn metrics_sets_content_type_and_appends_state_gauges() {
        let state = offline_state();

        let response = metrics(AdminAuth, State(state)).await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(CONTENT_TYPE).unwrap(),
            "text/plain; version=0.0.4; charset=utf-8"
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        for gauge in [
            "oracle_keeper_balance_below_min 0",
            "oracle_last_set_prices_timestamp_seconds 0",
            "oracle_pending_keys{kind=\"orders\"} 0",
            "oracle_in_flight_keys 0",
            "oracle_blacklisted_keys 0",
        ] {
            assert!(body.contains(gauge), "missing {gauge:?} in:\n{body}");
        }
    }
}
