use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::header::CONTENT_TYPE;
use axum::http::request::Parts;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use tracing::info;

use super::{AdminAuth, ApiError};
use crate::state::{AppState, CachedPrice, FailedSubmission, FAILURE_RING_CAPACITY};

pub const DEFAULT_ORACLE_STATUS_ERRORS_LIMIT: usize = 20;

#[derive(Debug, Deserialize, Default)]
pub struct OracleStatusQuery {
    pub limit: Option<usize>,
}

// #602/#1030 — axum's built-in `Query` rejection renders as a bare text/plain body,
// which breaks the `{"error": "..."}` envelope every other endpoint returns.
// Extracting through this impl maps the rejection onto `ApiError` so a malformed
// `?limit=` value stays parseable for clients that unconditionally read JSON.
impl FromRequestParts<Arc<AppState>> for OracleStatusQuery {
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

#[derive(Debug, Serialize)]
pub struct OracleStatusResponse {
    pub last_cycle_time: Option<u64>,
    pub keeper_balance: Option<f64>,
    pub prices: Vec<CachedPrice>,
    pub recent_errors: Vec<FailedSubmission>,
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
    query: OracleStatusQuery,
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
    let limit = query
        .limit
        .unwrap_or(DEFAULT_ORACLE_STATUS_ERRORS_LIMIT)
        .min(FAILURE_RING_CAPACITY);
    let recent_errors = state
        .failures
        .lock()
        .await
        .iter()
        .rev()
        .take(limit)
        .cloned()
        .collect();

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
/// Also resets the key's consecutive freeze-failure count, so it gets a
/// fresh `MAX_CONSECUTIVE_FREEZE_FAILURES` budget instead of being
/// re-blacklisted after a single further failure.
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

    info!(key = %key, "blacklisted order key cleared via admin API");
    Ok(Json(ClearBlacklistResponse { key, cleared: true }))
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

    match crate::keeper::check_keeper_balance(&keeper_cfg, &state.keeper_balance_below_min).await {
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
