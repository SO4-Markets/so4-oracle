use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::http::header::CONTENT_TYPE;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

use super::AdminAuth;
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
    Json(KeeperStatusResponse {
        pending_orders: keeper_status.pending_orders,
        pending_deposits: keeper_status.pending_deposits,
        pending_withdrawals: keeper_status.pending_withdrawals,
        last_executions: keeper_status.last_executions,
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

fn system_time_secs(value: SystemTime) -> Option<u64> {
    value
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs())
}
