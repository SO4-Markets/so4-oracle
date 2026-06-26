use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::Serialize;

use super::{AdminAuth, ApiError};
use crate::state::{AppState, CachedPrice, FailedSubmission};

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
}

#[derive(Debug, Serialize)]
pub struct FailuresResponse {
    pub failures: Vec<FailedSubmission>,
}

pub async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

pub async fn ready(State(state): State<Arc<AppState>>) -> Result<Json<HealthResponse>, ApiError> {
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
        Ok(stroops) => {
            let xlm = stroops as f64 / crate::keeper::XLM_IN_STROOPS as f64;
            if xlm < state.config.min_keeper_balance_xlm {
                return Err(ApiError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "keeper_balance_low",
                ));
            }
        }
        Err(_) => {
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
    State(state): State<Arc<AppState>>,
) -> Json<FailuresResponse> {
    let failures = state.failures.lock().await.iter().rev().cloned().collect();

    Json(FailuresResponse { failures })
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
