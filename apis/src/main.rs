mod server;
use axum::{
    extract::State,
    http::{header, Request, StatusCode},
    middleware::{self, Next},
    response::Response,
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::env;
use std::sync::Arc;
use tokio::sync::RwLock;

// ─── Oracle Status (Issue #110) ─────────────────────────────────────────────

/// Oracle status returned by `GET /oracle/status`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OracleStatus {
    /// Unix timestamp (seconds) of the last successful price update.
    pub last_price_update: Option<u64>,
    /// Keeper account address on Stellar.
    pub keeper_address: Option<String>,
    /// Keeper XLM balance.
    pub balance_xlm: Option<f64>,
    /// Number of tokens currently tracked.
    pub tokens_tracked: u32,
    /// Latency of the last submission in milliseconds.
    pub last_submission_latency_ms: Option<u64>,
    /// Errors from the last N cycles.
    pub recent_errors: Vec<String>,
}

impl Default for OracleStatus {
    fn default() -> Self {
        Self {
            last_price_update: None,
            keeper_address: None,
            balance_xlm: None,
            tokens_tracked: 0,
            last_submission_latency_ms: None,
            recent_errors: Vec::new(),
        }
    }
}

/// Shared application state.
#[derive(Clone)]
pub struct AppState {
    pub oracle_status: Arc<RwLock<OracleStatus>>,
}

// ─── Admin Auth Middleware (Issue #109) ──────────────────────────────────────

/// Constant-time comparison to prevent timing attacks.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut result = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        result |= x ^ y;
    }
    result == 0
}

/// Middleware that enforces `Authorization: Bearer <API_KEY>` on `/admin` routes.
async fn admin_auth(
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let path = request.uri().path().to_owned();

    // Only protect /admin routes
    if path.starts_with("/admin") {
        let api_key = env::var("API_KEY").unwrap_or_default();

        if api_key.is_empty() {
            return Response::builder()
                .status(StatusCode::UNAUTHORIZED)
                .header(header::WWW_AUTHENTICATE, "Bearer")
                .body(axum::body::Body::from(r#"{"error":"API key not configured"}"#))
                .unwrap();
        }

        let auth_header = request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok());

        let valid = match auth_header {
            Some(val) => {
                if let Some(token) = val.strip_prefix("Bearer ") {
                    constant_time_eq(token.as_bytes(), api_key.as_bytes())
                } else {
                    false
                }
            }
            None => false,
        };

        if !valid {
            return Response::builder()
                .status(StatusCode::UNAUTHORIZED)
                .header(header::WWW_AUTHENTICATE, "Bearer")
                .body(axum::body::Body::from(r#"{"error":"invalid or missing API key"}"#))
                .unwrap();
        }
    }

    next.run(request).await
}

// ─── Handlers ───────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct Health {
    status: &'static str,
}

async fn health() -> Json<Health> {
    Json(Health { status: "ok" })
}

/// `GET /oracle/status` — returns the current oracle health status.
///
/// Returns 503 if the oracle has not submitted in over 5 minutes.
async fn oracle_status(State(state): State<AppState>) -> Result<Json<OracleStatus>, StatusCode> {
    let status = state.oracle_status.read().await;

    // Return 503 if no submission in the last 5 minutes (300 seconds)
    if let Some(last_update) = status.last_price_update {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if now.saturating_sub(last_update) > 300 {
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        }
    } else {
        // No submission ever recorded
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }

    Ok(Json(status.clone()))
}

/// `POST /oracle/status` — update the oracle status (called by the oracle worker).
async fn update_oracle_status(
    State(state): State<AppState>,
    Json(payload): Json<OracleStatus>,
) -> Json<OracleStatus> {
    let mut status = state.oracle_status.write().await;
    *status = payload.clone();
    Json(payload)
}

// ─── App ────────────────────────────────────────────────────────────────────

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/oracle/status", get(oracle_status).post(update_oracle_status))
        .route("/admin/hello", get(|| async { "admin ok" }))
        .layer(middleware::from_fn(admin_auth))
        .with_state(state)
}

#[tokio::main]
async fn main() {
    server::run().await.unwrap();
    let state = AppState {
        oracle_status: Arc::new(RwLock::new(OracleStatus::default())),
    };

    let app = app(state);
    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    println!("listening on {}", listener.local_addr().unwrap());
    axum::serve(listener, app).await.unwrap();
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn test_state() -> AppState {
        AppState {
            oracle_status: Arc::new(RwLock::new(OracleStatus::default())),
        }
    }

    fn test_state_with_status(status: OracleStatus) -> AppState {
        AppState {
            oracle_status: Arc::new(RwLock::new(status)),
        }
    }

    #[tokio::test]
    async fn health_endpoint_returns_ok() {
        let state = test_state();
        let app = app(state);

        let req = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn public_routes_unaffected_by_auth() {
        let state = test_state();
        let app = app(state);

        let req = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn admin_route_without_key_returns_401() {
        env::set_var("API_KEY", "test-secret-key");
        let state = test_state();
        let app = app(state);

        let req = Request::builder()
            .uri("/admin/hello")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            resp.headers().get(header::WWW_AUTHENTICATE).unwrap(),
            "Bearer"
        );
    }

    #[tokio::test]
    async fn admin_route_with_invalid_key_returns_401() {
        env::set_var("API_KEY", "test-secret-key");
        let state = test_state();
        let app = app(state);

        let req = Request::builder()
            .uri("/admin/hello")
            .header(header::AUTHORIZATION, "Bearer wrong-key")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn admin_route_with_valid_key_passes() {
        env::set_var("API_KEY", "test-secret-key");
        let state = test_state();
        let app = app(state);

        let req = Request::builder()
            .uri("/admin/hello")
            .header(header::AUTHORIZATION, "Bearer test-secret-key")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn oracle_status_returns_503_when_no_submission() {
        let state = test_state();
        let app = app(state);

        let req = Request::builder()
            .uri("/oracle/status")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn oracle_status_returns_503_when_stale() {
        // 10 minutes ago — stale
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let status = OracleStatus {
            last_price_update: Some(now - 600),
            keeper_address: Some("GABC".to_string()),
            balance_xlm: Some(100.0),
            tokens_tracked: 5,
            last_submission_latency_ms: Some(150),
            recent_errors: vec![],
        };
        let state = test_state_with_status(status);
        let app = app(state);

        let req = Request::builder()
            .uri("/oracle/status")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn oracle_status_returns_200_when_fresh() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let status = OracleStatus {
            last_price_update: Some(now),
            keeper_address: Some("GABC123".to_string()),
            balance_xlm: Some(50.5),
            tokens_tracked: 3,
            last_submission_latency_ms: Some(200),
            recent_errors: vec!["timeout on binance".to_string()],
        };
        let state = test_state_with_status(status);
        let app = app(state);

        let req = Request::builder()
            .uri("/oracle/status")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: OracleStatus = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed.keeper_address, Some("GABC123".to_string()));
        assert_eq!(parsed.tokens_tracked, 3);
        assert_eq!(parsed.last_submission_latency_ms, Some(200));
    }

    #[tokio::test]
    async fn constant_time_eq_works() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(!constant_time_eq(b"", b"a"));
    }
}
