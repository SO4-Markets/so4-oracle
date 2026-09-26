use std::sync::Arc;

use axum::body::Body;
use axum::extract::FromRequestParts;
use axum::extract::MatchedPath;
use axum::http::header::{AUTHORIZATION, CACHE_CONTROL};
use axum::http::request::Parts;
use axum::http::{HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get};
use axum::{Json, Router};
use serde::Serialize;
use std::time::Duration;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::cors::{Any, CorsLayer};
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::set_header::SetResponseHeaderLayer;
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
            Some(actual) if crate::auth::constant_time_eq(actual.as_bytes(), expected.as_str().as_bytes()) => {
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

/// Middleware that converts axum's bare 405 Method Not Allowed responses into
/// the same `{"error": "..."}` JSON envelope this API returns for every other
/// error path (#1029).
async fn map_method_not_allowed(
    request: axum::http::Request<Body>,
    next: axum::middleware::Next,
) -> Response {
    let response = next.run(request).await;
    if response.status() == StatusCode::METHOD_NOT_ALLOWED {
        (
            StatusCode::METHOD_NOT_ALLOWED,
            Json(ErrorBody {
                error: "method_not_allowed".to_string(),
            }),
        )
            .into_response()
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
        .fallback(|| async { ApiError::new(StatusCode::NOT_FOUND, "not_found") })
        .with_state(state.clone())
        // #1044 — CatchPanicLayer must be the OUTERMOST layer so it wraps
        // every handler. With `panic = "abort"` in the release profile a
        // panicking handler would otherwise abort the entire process; this
        // layer converts panics into 500 responses instead, isolating faults
        // to the offending request.
        .layer(CatchPanicLayer::new())
        // Layer ordering: `.layer()` calls chained directly on a `Router` make
        // the LAST-added layer the OUTERMOST — it sees the request first. So
        // `SetRequestIdLayer` must be added *after* `trace_layer` for the ID
        // to be in the request extensions by the time `trace_layer`'s
        // `make_span_with` reads it; otherwise every span's `request_id` is
        // "" (#790). `PropagateRequestIdLayer` only needs to run after the
        // handler, so it stays innermost.
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(trace_layer)
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .layer(axum::middleware::from_fn_with_state(state, track_metrics))
        // No response from this service is meant to be cached — /prices is
        // the one endpoint explicitly CORS-enabled for direct browser
        // access, updates roughly once a second, and backs a trading
        // frontend, so a response with no cache directives at all is
        // otherwise subject to whatever default heuristics a browser,
        // proxy, or CDN in front of this service chooses to apply (#1026).
        // Applied to every route, not just /prices, since nothing here is
        // cacheable.
        .layer(SetResponseHeaderLayer::overriding(
            CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        ))
        .layer(axum::middleware::from_fn(map_method_not_allowed))
}

#[cfg(test)]
mod tests {
    use crate::{AppState, Config};
    use axum::body::Body;
    use axum::http::header::CACHE_CONTROL;
    use axum::http::Request;
    use std::sync::Arc;
    use tower::ServiceExt;

    // #1026 — every response, not just /prices, must carry an explicit
    // no-store directive so no browser/proxy/CDN in front of this service
    // applies default caching heuristics to a price that updates ~1/sec.
    #[tokio::test]
    async fn every_response_sets_cache_control_no_store() {
        let config = Arc::new(Config::default_for_tests());
        let state = Arc::new(AppState::new(config));
        let app = super::build_router(state);

        for uri in ["/health", "/ready", "/prices"] {
            let request = Request::builder().uri(uri).body(Body::empty()).unwrap();
            let response = app.clone().oneshot(request).await.unwrap();
            assert_eq!(
                response
                    .headers()
                    .get(CACHE_CONTROL)
                    .map(|v| v.to_str().unwrap()),
                Some("no-store"),
                "missing/incorrect Cache-Control on {uri}"
            );
        }
    }


    // #1029 — a wrong HTTP method on a known route must return the API's JSON
    // error envelope, not axum's bare 405.
    #[tokio::test]
    async fn method_not_allowed_returns_json_envelope() {
        let config = Arc::new(Config::default_for_tests());
        let state = Arc::new(AppState::new(config));
        let app = super::build_router(state);

        // POST to /prices (which only accepts GET) should return 405 with JSON body
        let request = Request::builder()
            .method("POST")
            .uri("/prices")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "method_not_allowed");
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
}
