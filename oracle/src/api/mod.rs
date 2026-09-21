use std::sync::Arc;

use axum::extract::FromRequestParts;
use axum::extract::MatchedPath;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get};
use axum::{Json, Router};
use serde::Serialize;
use std::time::Duration;
use tower_http::cors::{Any, CorsLayer};
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::trace::TraceLayer;
use tracing::Span;

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
            _ => {
                let route = parts
                    .extensions
                    .get::<MatchedPath>()
                    .map(|m| m.as_str())
                    .unwrap_or("unknown");
                state.metrics.record_http_auth_failure(route);

                let request_id = parts
                    .extensions
                    .get::<tower_http::request_id::RequestId>()
                    .and_then(|id| id.header_value().to_str().ok())
                    .unwrap_or("");

                tracing::warn!(
                    route = route,
                    request_id = request_id,
                    "unauthorized admin access attempt"
                );

                Err(ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized"))
            }
        }
    }
}

#[derive(Clone)]
struct RouteExt(String);

async fn track_metrics(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let path = if let Some(matched_path) = request.extensions().get::<MatchedPath>() {
        matched_path.as_str().to_owned()
    } else {
        "/unmatched".to_owned()
    };
    let method = request.method().as_str().to_owned();

    state.metrics.inc_http_in_flight();
    let start = std::time::Instant::now();

    let mut response = next.run(request).await;

    let latency = start.elapsed();
    state.metrics.dec_http_in_flight();
    state.metrics.record_http_request(
        &path,
        &method,
        response.status().as_u16(),
        latency.as_millis() as u64,
    );

    response.extensions_mut().insert(RouteExt(path));
    response
}

async fn handle_method_not_allowed(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let response = next.run(request).await;

    if response.status() == StatusCode::METHOD_NOT_ALLOWED {
        let (mut parts, _) = response.into_parts();
        let err_response =
            ApiError::new(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed").into_response();
        let (err_parts, err_body) = err_response.into_parts();

        parts.headers.remove(axum::http::header::CONTENT_LENGTH);
        for (name, value) in err_parts.headers {
            if let Some(name) = name {
                parts.headers.insert(name, value);
            }
        }

        Response::from_parts(parts, err_body)
    } else {
        response
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

    let trace_layer = TraceLayer::new_for_http()
        .make_span_with(|request: &axum::http::Request<_>| {
            let matched_path = request
                .extensions()
                .get::<MatchedPath>()
                .map(|m| m.as_str())
                .unwrap_or("/unmatched");

            let request_id = request
                .extensions()
                .get::<tower_http::request_id::RequestId>()
                .and_then(|id| id.header_value().to_str().ok())
                .unwrap_or("");

            tracing::info_span!(
                "request",
                method = %request.method(),
                route = %matched_path,
                request_id = %request_id,
                status = tracing::field::Empty,
                latency_ms = tracing::field::Empty,
            )
        })
        .on_response(
            |response: &axum::http::Response<_>, latency: Duration, span: &Span| {
                let status = response.status().as_u16();
                let latency_ms = latency.as_millis() as u64;
                span.record("status", status);
                span.record("latency_ms", latency_ms);

                let is_health = response
                    .extensions()
                    .get::<RouteExt>()
                    .map(|ext| ext.0 == "/health" || ext.0 == "/ready")
                    .unwrap_or(false);

                if is_health {
                    tracing::debug!("request completed");
                } else {
                    tracing::info!("request completed");
                }
            },
        )
        .on_failure(
            |error: tower_http::classify::ServerErrorsFailureClass,
             _latency: Duration,
             _span: &Span| {
                tracing::error!(%error, "request failed");
            },
        );

    Router::new()
        .route("/health", get(prices::health))
        .route("/ready", get(prices::ready))
        .merge(public)
        .route("/oracle/status", get(admin::oracle_status))
        .route("/keeper/status", get(admin::keeper_status))
        .route("/keeper/balance", get(admin::keeper_balance))
        .route(
            "/keeper/blacklist/{key}",
            delete(admin::clear_blacklisted_key),
        )
        .route("/metrics", get(admin::metrics))
        .route(
            "/oracle/failed-submissions",
            get(prices::failed_submissions),
        )
        .with_state(state.clone())
        // Layer ordering: `.layer()` calls chained directly on a `Router` make
        // the LAST-added layer the OUTERMOST — it sees the request first. So
        // `SetRequestIdLayer` must be added *after* `trace_layer` for the ID
        // to be in the request extensions by the time `trace_layer`'s
        // `make_span_with` reads it; otherwise every span's `request_id` is
        // "" (#790). `PropagateRequestIdLayer` only needs to run after the
        // handler, so it stays innermost.
        .layer(axum::middleware::from_fn(handle_method_not_allowed))
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(trace_layer)
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .layer(axum::middleware::from_fn_with_state(state, track_metrics))
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
    use crate::{AppState, Config};
    use axum::body::Body;
    use axum::http::Request;
    use std::sync::Arc;
    use tower::ServiceExt;

    #[test]
    fn constant_time_comparison_matches_equal_values_only() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"Secret"));
        assert!(!constant_time_eq(b"secret", b"secret2"));
    }

    #[tokio::test]
    async fn test_secrets_redacted_from_logs_and_metrics() {
        let mut config = Config::default_for_tests();
        let test_secret = "SCVXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX";
        let test_admin_token = "admin_super_secret_token_123";
        config.keeper_secret_key = crate::config::SecretString::new(test_secret.to_string());
        config.admin_api_token = Some(crate::config::SecretString::new(
            test_admin_token.to_string(),
        ));

        let state = Arc::new(AppState::new(Arc::new(config)));
        let app = super::build_router(Arc::clone(&state));

        // Make both successful and failing admin requests to test that secrets don't leak in either case
        let successful_request = Request::builder()
            .uri("/oracle/status")
            .header("Authorization", format!("Bearer {}", test_admin_token))
            .body(Body::empty())
            .unwrap();

        let failing_request = Request::builder()
            .uri("/oracle/status")
            .header("Authorization", "Bearer WRONG_TOKEN")
            .body(Body::empty())
            .unwrap();

        // Send both requests
        let _ = app.clone().oneshot(successful_request).await;
        let _ = app.clone().oneshot(failing_request).await;

        let metrics_out = state.metrics.to_prometheus();

        assert!(
            !metrics_out.contains(test_secret),
            "keeper secret key found in metrics"
        );
        assert!(
            !metrics_out.contains(test_admin_token),
            "admin token found in metrics"
        );
    }

    #[tokio::test]
    async fn test_unsupported_methods_on_get_endpoints_return_json_405() {
        let config = Config::default_for_tests();
        let state = Arc::new(AppState::new(Arc::new(config)));
        let app = super::build_router(state);

        let endpoints = [
            "/health",
            "/ready",
            "/prices",
            "/metrics",
            "/oracle/status",
            "/keeper/status",
            "/keeper/balance",
            "/oracle/failed-submissions",
        ];
        let unsupported_methods = [
            axum::http::Method::POST,
            axum::http::Method::PUT,
            axum::http::Method::PATCH,
            axum::http::Method::DELETE,
        ];

        for endpoint in &endpoints {
            for method in &unsupported_methods {
                let req = Request::builder()
                    .method(method.clone())
                    .uri(*endpoint)
                    .body(Body::empty())
                    .unwrap();

                let res = app.clone().oneshot(req).await.unwrap();

                assert_eq!(
                    res.status(),
                    axum::http::StatusCode::METHOD_NOT_ALLOWED,
                    "Expected 405 for {method} {endpoint}"
                );

                let content_type = res
                    .headers()
                    .get(axum::http::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("");
                assert_eq!(
                    content_type, "application/json",
                    "Expected Content-Type: application/json for {method} {endpoint}, got: {content_type}"
                );

                let allow = res
                    .headers()
                    .get(axum::http::header::ALLOW)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("");
                assert!(
                    allow.contains("GET"),
                    "Expected allow header containing GET for {method} {endpoint}, got: {allow}"
                );

                let body = axum::body::to_bytes(res.into_body(), usize::MAX)
                    .await
                    .unwrap();
                let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(
                    json,
                    serde_json::json!({ "error": "method_not_allowed" }),
                    "Payload mismatch for {method} {endpoint}"
                );
            }
        }
    }

    #[tokio::test]
    async fn test_unsupported_methods_on_delete_endpoint_return_json_405() {
        let config = Config::default_for_tests();
        let state = Arc::new(AppState::new(Arc::new(config)));
        let app = super::build_router(state);

        let endpoint = "/keeper/blacklist/test-key-123";
        let unsupported_methods = [
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::PUT,
            axum::http::Method::PATCH,
        ];

        for method in &unsupported_methods {
            let req = Request::builder()
                .method(method.clone())
                .uri(endpoint)
                .body(Body::empty())
                .unwrap();

            let res = app.clone().oneshot(req).await.unwrap();

            assert_eq!(
                res.status(),
                axum::http::StatusCode::METHOD_NOT_ALLOWED,
                "Expected 405 for {method} {endpoint}"
            );

            let content_type = res
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            assert_eq!(
                content_type, "application/json",
                "Expected Content-Type: application/json for {method} {endpoint}, got: {content_type}"
            );

            let allow = res
                .headers()
                .get(axum::http::header::ALLOW)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            assert!(
                allow.contains("DELETE"),
                "Expected allow header containing DELETE for {method} {endpoint}, got: {allow}"
            );

            let body = axum::body::to_bytes(res.into_body(), usize::MAX)
                .await
                .unwrap();
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(
                json,
                serde_json::json!({ "error": "method_not_allowed" }),
                "Payload mismatch for {method} {endpoint}"
            );
        }
    }

    #[tokio::test]
    async fn test_protocol_headers_preserved_on_method_not_allowed() {
        let config = Config::default_for_tests();
        let state = Arc::new(AppState::new(Arc::new(config)));
        let app = super::build_router(state);

        let req = Request::builder()
            .method(axum::http::Method::POST)
            .uri("/prices")
            .header("x-request-id", "unit-test-req-id-405")
            .body(Body::empty())
            .unwrap();

        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(
            res.headers()
                .get("x-request-id")
                .and_then(|v| v.to_str().ok()),
            Some("unit-test-req-id-405")
        );
        assert_eq!(
            res.headers()
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/json")
        );
        let allow = res
            .headers()
            .get(axum::http::header::ALLOW)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(allow.contains("GET"));
    }

    #[tokio::test]
    async fn test_unmatched_routes_and_valid_routes_unaffected() {
        let config = Config::default_for_tests();
        let state = Arc::new(AppState::new(Arc::new(config)));
        let app = super::build_router(state);

        // 1. GET /health returns 200 OK
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::OK);

        // 2. Unmatched path GET /unmatched-path returns 404 NOT FOUND
        let res_404_get = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/unmatched-path")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res_404_get.status(), axum::http::StatusCode::NOT_FOUND);

        // 3. Unmatched path POST /unmatched-path returns 404 NOT FOUND
        let res_404_post = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(axum::http::Method::POST)
                    .uri("/unmatched-path")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res_404_post.status(), axum::http::StatusCode::NOT_FOUND);
    }
}
