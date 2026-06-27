use std::sync::Arc;

use axum::extract::FromRequestParts;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

use crate::state::AppState;

pub mod admin;
pub mod prices;

#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub error: String,
}

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorBody {
                error: self.message,
            }),
        )
            .into_response()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AdminAuth;

impl FromRequestParts<Arc<AppState>> for AdminAuth {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let expected = state.config.admin_api_token.as_ref().ok_or_else(|| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "ADMIN_API_TOKEN is not configured",
            )
        })?;

        let actual = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "));

        match actual {
            Some(actual) if constant_time_eq(actual.as_bytes(), expected.as_str().as_bytes()) => {
                Ok(AdminAuth)
            }
            _ => Err(ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized")),
        }
    }
}

pub fn build_router(state: Arc<AppState>) -> Router {
    let cors = CorsLayer::new()
        .allow_methods([Method::GET])
        .allow_origin(Any);

    // CORS is only opened for the public, browser-facing price feed; admin and
    // health routes are not cross-origin reachable.
    let public = Router::new()
        .route("/prices", get(prices::prices))
        .layer(cors);

    Router::new()
        .route("/health", get(prices::health))
        .route("/ready", get(prices::ready))
        .merge(public)
        .route("/oracle/status", get(admin::oracle_status))
        .route("/keeper/status", get(admin::keeper_status))
        .route("/keeper/balance", get(admin::keeper_balance))
        .route("/metrics", get(admin::metrics))
        .route(
            "/oracle/failed-submissions",
            get(prices::failed_submissions),
        )
        .with_state(state)
        .layer(TraceLayer::new_for_http())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let max_len = left.len().max(right.len());
    let mut diff = left.len() ^ right.len();

    for index in 0..max_len {
        let a = left.get(index).copied().unwrap_or(0);
        let b = right.get(index).copied().unwrap_or(0);
        diff |= (a ^ b) as usize;
    }

    diff == 0
}

#[cfg(test)]
mod tests {
    use super::constant_time_eq;

    #[test]
    fn constant_time_comparison_matches_equal_values_only() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"Secret"));
        assert!(!constant_time_eq(b"secret", b"secret2"));
    }
}
