use std::sync::Arc;
use std::time::Duration;

use axum::extract::{FromRequestParts, Query, State};
use axum::http::request::Parts;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use super::{AdminAuth, ApiError};
use crate::state::{AppState, CachedPrice, FailedSubmission};

const READY_BALANCE_RETRY_ATTEMPTS: u32 = 3;
const READY_BALANCE_RETRY_BASE_DELAY_MS: u64 = 100;

#[derive(Debug, Deserialize)]
pub struct FailedSubmissionsQuery {
    pub operation: Option<String>,
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

// #497 — extended health response exposes last-cycle timestamps and failure
// counts so staleness or failure streaks are detectable without grepping logs.
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_price_cycle_secs_ago: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_keeper_cycle_secs_ago: Option<u64>,
    pub price_cycle_count: u64,
    pub keeper_cycle_count: u64,
    pub token_fetch_failures: u64,
    pub submit_failures: u64,
}

#[derive(Debug, Serialize)]
pub struct FailuresResponse {
    pub failures: Vec<FailedSubmission>,
    pub total_count: usize,
}

pub async fn health(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    let cycle = state.cycle_status.read().await;
    let metrics = state.metrics.to_response();

    let last_price_cycle_secs_ago = cycle
        .last_price_cycle_at
        .and_then(|t| t.elapsed().ok())
        .map(|d| d.as_secs());

    let last_keeper_cycle_secs_ago = cycle
        .last_keeper_cycle_at
        .and_then(|t| t.elapsed().ok())
        .map(|d| d.as_secs());

    Json(HealthResponse {
        status: "ok",
        last_price_cycle_secs_ago,
        last_keeper_cycle_secs_ago,
        price_cycle_count: metrics.price_cycle_count,
        keeper_cycle_count: metrics.keeper_cycle_count,
        token_fetch_failures: metrics.token_fetch_failures,
        submit_failures: metrics.submit_failures,
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

    // Check cached external RPC and keeper balance readiness result (3s TTL)
    let metrics = state.metrics.to_response();
    {
        let cache = state.ready_cache.read().await;
        if let Some(last) = cache.last_checked {
            if last.elapsed() < std::time::Duration::from_secs(3) {
                if let Some((status, msg)) = cache.last_error.clone() {
                    return Err(ApiError::new(status, msg));
                }
                return Ok(Json(HealthResponse {
                    status: "ok",
                    last_price_cycle_secs_ago: None,
                    last_keeper_cycle_secs_ago: None,
                    price_cycle_count: metrics.price_cycle_count,
                    keeper_cycle_count: metrics.keeper_cycle_count,
                    token_fetch_failures: metrics.token_fetch_failures,
                    submit_failures: metrics.submit_failures,
                }));
            }
        }
    }

    let check_res = perform_external_ready_checks(&state).await;
    {
        let mut cache = state.ready_cache.write().await;
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

    let metrics = state.metrics.to_response();
    Ok(Json(HealthResponse {
        status: "ok",
        last_price_cycle_secs_ago: None,
        last_keeper_cycle_secs_ago: None,
        price_cycle_count: metrics.price_cycle_count,
        keeper_cycle_count: metrics.keeper_cycle_count,
        token_fetch_failures: metrics.token_fetch_failures,
        submit_failures: metrics.submit_failures,
    }))
}

async fn perform_external_ready_checks(state: &AppState) -> Result<(), ApiError> {
    // Check RPC reachability
    let rpc_url = &state.config.stellar_rpc_url;
    let response = state
        .http
        .get(rpc_url)
        .send()
        .await
        .map_err(|_| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "rpc_unreachable"))?;

    if !response.status().is_success() {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "rpc_unhealthy",
        ));
    }

    // Check keeper balance
    let keeper_cfg = crate::keeper::KeeperBalanceConfig {
        horizon_url: state.config.horizon_url.clone(),
        account_id: state.config.keeper_account_id.clone(),
        min_balance_xlm: state.config.min_keeper_balance_xlm,
    };

    match check_keeper_balance_for_ready(&keeper_cfg).await {
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

async fn check_keeper_balance_for_ready(
    keeper_cfg: &crate::keeper::KeeperBalanceConfig,
) -> Result<i64, crate::stellar_rpc::RpcError> {
    let mut last_error = None;

    for attempt in 1..=READY_BALANCE_RETRY_ATTEMPTS {
        match crate::keeper::check_keeper_balance(keeper_cfg).await {
            Ok(stroops) => return Ok(stroops),
            Err(error @ crate::stellar_rpc::RpcError::BalanceBelowMinimum { .. }) => {
                return Err(error);
            }
            Err(error) => {
                tracing::warn!(
                    attempt,
                    max_attempts = READY_BALANCE_RETRY_ATTEMPTS,
                    error = %error,
                    "ready keeper balance attempt failed"
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

pub async fn prices(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<CachedPrice>>, ApiError> {
    let cache = state.price_cache.read().await;
    if cache.prices.is_empty() {
        return Err(ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "no_prices"));
    }
    Ok(Json(cache.prices.values().cloned().collect()))
}

pub async fn failed_submissions(
    _auth: AdminAuth,
    query: FailedSubmissionsQuery,
    State(state): State<Arc<AppState>>,
) -> Json<FailuresResponse> {
    let all_failures = state.failures.lock().await;
    let failures_iter = all_failures.iter().rev();

    let filtered: Vec<FailedSubmission> = match &query.operation {
        Some(op) => failures_iter
            .filter(|f| f.operation.starts_with(op.as_str()))
            .cloned()
            .collect(),
        None => failures_iter.cloned().collect(),
    };

    let total_count = filtered.len();
    let limit = query.limit.unwrap_or(100).min(256);
    let failures: Vec<_> = filtered.into_iter().take(limit).collect();

    Json(FailuresResponse {
        failures,
        total_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AppState, Config};

    fn test_state() -> Arc<AppState> {
        let config = Arc::new(Config::default_for_tests());
        Arc::new(AppState::new(config))
    }

    // #339 — GET /health must return 200 with {"status":"ok"}, no auth required
    // #497 — health now also surfaces cycle timestamps and failure counters
    #[tokio::test]
    async fn health_returns_status_ok() {
        let state = test_state();
        let Json(body) = health(State(state)).await;
        assert_eq!(body.status, "ok");
        // No cycles have run yet — counts are zero and timestamps are absent
        assert_eq!(body.price_cycle_count, 0);
        assert_eq!(body.keeper_cycle_count, 0);
        assert!(body.last_price_cycle_secs_ago.is_none());
        assert!(body.last_keeper_cycle_secs_ago.is_none());
    }
}
