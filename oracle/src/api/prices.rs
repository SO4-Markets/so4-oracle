use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use super::{AdminAuth, ApiError};
use crate::state::{AppState, CachedPrice, FailedSubmission};

#[derive(Debug, Deserialize)]
pub struct FailedSubmissionsQuery {
    pub operation: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
}

#[derive(Debug, Serialize)]
pub struct FailuresResponse {
    pub failures: Vec<FailedSubmission>,
    pub total_count: usize,
}

pub async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
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

    match crate::keeper::check_keeper_balance(&keeper_cfg).await {
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

    Ok(Json(HealthResponse { status: "ok" }))
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
    Query(query): Query<FailedSubmissionsQuery>,
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

    // #339 — GET /health must return 200 with {"status":"ok"}, no auth required
    #[tokio::test]
    async fn health_returns_status_ok() {
        let Json(body) = health().await;
        assert_eq!(body.status, "ok");
    }
}
