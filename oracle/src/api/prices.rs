use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::Serialize;

use super::AdminAuth;
use crate::state::{AppState, FailedSubmission};

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
}

#[derive(Debug, Serialize)]
pub struct PricesResponse {
    pub prices: Vec<crate::state::CachedPrice>,
}

#[derive(Debug, Serialize)]
pub struct FailuresResponse {
    pub failures: Vec<FailedSubmission>,
}

pub async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

pub async fn ready() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

pub async fn prices(State(state): State<Arc<AppState>>) -> (StatusCode, Json<PricesResponse>) {
    let cache = state.price_cache.read().await;
    let prices = cache.prices.values().cloned().collect();
    (StatusCode::OK, Json(PricesResponse { prices }))
}

pub async fn failed_submissions(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
) -> Json<FailuresResponse> {
    let failures = state.failures.lock().await.iter().rev().cloned().collect();

    Json(FailuresResponse { failures })
}
