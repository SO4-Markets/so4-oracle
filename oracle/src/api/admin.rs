use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::http::header::CONTENT_TYPE;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

use super::{AdminAuth, ApiError};
use crate::state::{AppState, CachedPrice, FailedSubmission};

#[derive(Debug, Serialize)]
pub struct OracleStatusResponse {
    pub last_cycle_time: Option<u64>,
    pub keeper_balance: Option<f64>,
    pub prices: Vec<CachedPrice>,
    pub recent_errors: Vec<FailedSubmission>,
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
}

pub async fn oracle_status(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
) -> Json<OracleStatusResponse> {
    let last_cycle_time = state
        .cycle_status
        .read()
        .await
        .last_price_cycle_at
        .and_then(system_time_secs);
    let prices = state
        .price_cache
        .read()
        .await
        .prices
        .values()
        .cloned()
        .collect();
    let recent_errors = state.failures.lock().await.iter().rev().cloned().collect();

    Json(OracleStatusResponse {
        last_cycle_time,
        keeper_balance: None,
        prices,
        recent_errors,
    })
}

pub async fn keeper_status(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
) -> Json<KeeperStatusResponse> {
    let keeper_status = state.keeper_status.read().await.clone();
    let cycle_status = state.cycle_status.read().await.clone();

    let last_cycle_at = cycle_status
        .last_keeper_cycle_at
        .and_then(system_time_secs);
    let last_cycle_latency_ms = cycle_status.last_keeper_cycle_latency_ms;

    let last_executions: Vec<_> = keeper_status
        .last_executions
        .into_iter()
        .rev()
        .take(50)
        .collect();

    Json(KeeperStatusResponse {
        pending_orders: keeper_status.pending_orders,
        pending_deposits: keeper_status.pending_deposits,
        pending_withdrawals: keeper_status.pending_withdrawals,
        last_cycle_at,
        last_cycle_latency_ms,
        last_executions,
    })
}

pub async fn metrics(_auth: AdminAuth, State(state): State<Arc<AppState>>) -> Response {
    let prometheus = state.metrics.to_prometheus();
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

    match crate::keeper::check_keeper_balance(&keeper_cfg).await {
        Ok(stroops) => {
            let response = crate::keeper::build_balance_response(&keeper_cfg, stroops);
            Ok(Json(BalanceResponse {
                account_id: response.account_id,
                balance_stroops: response.balance_stroops,
                balance_xlm: response.balance_xlm,
                min_balance_xlm: response.min_balance_xlm,
                is_funded: !response.below_minimum,
            }))
        }
        Err(crate::stellar_rpc::RpcError::BalanceBelowMinimum {
            balance_xlm,
            ..
        }) => {
            let stroops = (balance_xlm * crate::keeper::XLM_IN_STROOPS as f64) as i64;
            Ok(Json(BalanceResponse {
                account_id: state.config.keeper_account_id.clone(),
                balance_stroops: stroops,
                balance_xlm,
                min_balance_xlm: state.config.min_keeper_balance_xlm,
                is_funded: false,
            }))
        }
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
