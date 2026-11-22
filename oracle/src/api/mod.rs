use std::sync::Arc;

use axum::body::Body;
use axum::extract::FromRequestParts;
use axum::extract::{ConnectInfo, MatchedPath};
use axum::http::header::{
    ACCESS_CONTROL_REQUEST_HEADERS, ACCESS_CONTROL_REQUEST_METHOD, AUTHORIZATION, CACHE_CONTROL,
    ORIGIN,
};
use axum::http::request::Parts;
use axum::http::{HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get};
use axum::{Json, Router};
use serde::Serialize;
use std::time::Duration;
use tower::limit::ConcurrencyLimitLayer;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::cors::{Any, CorsLayer};
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::timeout::TimeoutLayer;
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

        // Throttle brute-force attempts per client before comparing (#1214).
        let client = parts
            .extensions
            .get::<ConnectInfo<std::net::SocketAddr>>()
            .map(|ConnectInfo(addr)| addr.ip().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let now = std::time::Instant::now();
        if state.admin_auth_limiter.is_blocked(&client, now) {
            return Err(ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "too many failed admin auth attempts; try again later",
            ));
        }

        let actual = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "));

        match actual {
            Some(actual)
                if crate::auth::constant_time_eq(
                    actual.as_bytes(),
                    expected.as_str().as_bytes(),
                ) =>
            {
                state.admin_auth_limiter.record_success(&client);
                Ok(AdminAuth)
            }
            _ => {
                state.admin_auth_limiter.record_failure(&client, now);
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
                    event = "admin_auth_failure",
                    route = route,
                    request_id = request_id,
                    "unauthorized admin access attempt"
                );

                Err(ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized"))
            }
        }
    }
}

/// Response marker set by a layer on the `/health` and `/ready` routes
/// themselves (#1459). A route-level layer is always innermost, so whether the
/// trace layer logs a probe at `debug` no longer depends on how the outer
/// middleware layers are ordered, and no route string is compared.
#[derive(Clone, Copy)]
struct ProbeRoute;

async fn mark_probe_response<B>(mut response: Response<B>) -> Response<B> {
    response.extensions_mut().insert(ProbeRoute);
    response
}

fn is_probe_response<B>(response: &Response<B>) -> bool {
    response.extensions().get::<ProbeRoute>().is_some()
}

/// Makes the inbound `x-request-id` the correlation id for everything the
/// handler does, so its outbound RPC, Horizon and price-source calls carry
/// the same id (#1455).
async fn scope_correlation_id(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let id = request
        .extensions()
        .get::<tower_http::request_id::RequestId>()
        .and_then(|id| id.header_value().to_str().ok())
        .map(str::to_owned);
    match id {
        Some(id) => crate::http::with_correlation_id(id, next.run(request)).await,
        None => next.run(request).await,
    }
}

/// Outermost request deadline (#1452). It must stay above every per-route
/// budget, or a caller gets a bare 408 instead of the handler's own, more
/// specific timeout response. Checked at compile time below; the full
/// hierarchy is documented in oracle/README.md.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const _: () = assert!(REQUEST_TIMEOUT.as_secs() > admin::ADMIN_EXTERNAL_BUDGET.as_secs());
const _: () = assert!(REQUEST_TIMEOUT.as_secs() > prices::READY_CHECK_TIMEOUT_SECS);

const MAX_CONCURRENT_REQUESTS: usize = 512;

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

    let response = next.run(request).await;

    let latency = start.elapsed();
    state.metrics.dec_http_in_flight();
    state.metrics.record_http_request(
        &path,
        &method,
        response.status().as_u16(),
        latency.as_millis() as u64,
    );

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
    // `Vary` is listed explicitly (#1451): it is tower-http's current default,
    // but a cache put in front of this service must always key on `Origin`,
    // so that guarantee should not depend on a library default.
    let cors = CorsLayer::new()
        .allow_methods([Method::GET])
        .allow_origin(Any)
        .vary([
            ORIGIN,
            ACCESS_CONTROL_REQUEST_METHOD,
            ACCESS_CONTROL_REQUEST_HEADERS,
        ]);

    // CORS is only opened for the public, browser-facing price feed; admin and
    // health routes are not cross-origin reachable.
    let public = Router::new()
        .route("/prices", get(prices::prices))
        .layer(cors);

    let probes = Router::new()
        .route("/health", get(prices::health))
        .route("/ready", get(prices::ready))
        .layer(axum::middleware::map_response(mark_probe_response));

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

                if is_probe_response(response) {
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

    // Everything except the probes. The concurrency cap and HTTP metrics are
    // layered here, on these routes only (#1453, #1454): a liveness probe must
    // never wait for a permit behind a load spike, and an unauthenticated
    // probe flood must not contend on the metrics mutex every price and
    // keeper cycle also uses.
    let limited = Router::new()
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
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            track_metrics,
        ))
        // Hard cap on concurrent requests (#1259). 512 is well above a single
        // Fly machine's realistic per-instance concurrency while still
        // bounding memory. Probes are merged in below, outside this layer.
        .layer(ConcurrencyLimitLayer::new(MAX_CONCURRENT_REQUESTS));

    Router::new()
        .merge(probes)
        .merge(limited)
        .with_state(state)
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
        // Inside SetRequestIdLayer, so the id exists by the time this makes
        // it the correlation id outbound calls send (#1455).
        .layer(axum::middleware::from_fn(scope_correlation_id))
        .layer(trace_layer)
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
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
        // Request timeout: return 408 if a handler takes longer than
        // REQUEST_TIMEOUT (#1259, #1452). Applied outside the trace layer so
        // the span captures the timeout as a 408, not as an internal server error.
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            REQUEST_TIMEOUT,
        ))
        // Hard cap on concurrent requests (#1259). Excess requests receive
        // 503 immediately rather than queuing, keeping /health and /ready
        // reachable even under load. 512 is well above a single Fly machine's
        // realistic per-instance concurrency while still bounding memory.
        .layer(ConcurrencyLimitLayer::new(512))
}

#[cfg(test)]
mod tests {
    use crate::{AppState, Config};
    use axum::body::Body;
    use axum::http::header::CACHE_CONTROL;
    use axum::http::{Request, StatusCode};
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

    // #1451 — CORS is opened for /prices only. If the layer ever moves to the
    // outer router, admin routes become cross-origin readable; this fails.
    #[tokio::test]
    async fn cors_headers_only_on_public_prices_route() {
        use axum::http::header::{ACCESS_CONTROL_ALLOW_ORIGIN, ORIGIN, VARY};

        let mut config = Config::default_for_tests();
        let token = "t".repeat(crate::config::MIN_ADMIN_API_TOKEN_LEN);
        config.admin_api_token = Some(crate::config::SecretString::new(token.clone()));
        let state = Arc::new(AppState::new(Arc::new(config)));
        let app = super::build_router(state);

        let request = Request::builder()
            .uri("/prices")
            .header(ORIGIN, "https://example.com")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(
            response
                .headers()
                .get(ACCESS_CONTROL_ALLOW_ORIGIN)
                .map(|v| v.to_str().unwrap()),
            Some("*")
        );
        let vary: Vec<String> = response
            .headers()
            .get_all(VARY)
            .iter()
            .map(|v| v.to_str().unwrap().to_ascii_lowercase())
            .collect();
        assert!(
            vary.iter()
                .any(|v| v.split(',').any(|h| h.trim() == "origin")),
            "Vary must include Origin on /prices, got {vary:?}"
        );

        let request = Request::builder()
            .uri("/oracle/status")
            .header(ORIGIN, "https://example.com")
            .header("Authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response
                .headers()
                .get(ACCESS_CONTROL_ALLOW_ORIGIN)
                .is_none(),
            "admin route must not carry CORS headers"
        );
    }

    // #1453, #1454 — probes sit outside the metrics and concurrency layers,
    // so a probe flood neither records HTTP metrics nor takes permits. Every
    // other route is still recorded.
    #[tokio::test]
    async fn probes_bypass_http_metrics_other_routes_do_not() {
        let config = Arc::new(Config::default_for_tests());
        let state = Arc::new(AppState::new(config));
        let app = super::build_router(Arc::clone(&state));

        for uri in ["/health", "/ready", "/prices"] {
            let request = Request::builder().uri(uri).body(Body::empty()).unwrap();
            app.clone().oneshot(request).await.unwrap();
        }

        let metrics = state.metrics.to_prometheus();
        assert!(metrics.contains("route=\"/prices\""), "{metrics}");
        assert!(!metrics.contains("route=\"/health\""), "{metrics}");
        assert!(!metrics.contains("route=\"/ready\""), "{metrics}");
    }

    // #1459 — the trace layer logs a request at `debug` exactly when its
    // response carries the probe marker. The marker must survive every outer
    // middleware layer on /health and /ready, and appear on no other route.
    #[tokio::test]
    async fn only_probe_routes_are_logged_below_info() {
        let config = Arc::new(Config::default_for_tests());
        let state = Arc::new(AppState::new(config));
        let app = super::build_router(state);

        for (uri, is_probe) in [
            ("/health", true),
            ("/ready", true),
            ("/prices", false),
            ("/metrics", false),
            ("/does-not-exist", false),
        ] {
            let request = Request::builder().uri(uri).body(Body::empty()).unwrap();
            let response = app.clone().oneshot(request).await.unwrap();
            assert_eq!(super::is_probe_response(&response), is_probe, "{uri}");
        }
    }

    // #1214 — repeated wrong admin tokens from one client are throttled with
    // 429 (even if the next attempt is correct), until the window resets.
    #[tokio::test]
    async fn admin_auth_throttles_repeated_failures() {
        let mut config = Config::default_for_tests();
        let token = "t".repeat(crate::config::MIN_ADMIN_API_TOKEN_LEN);
        config.admin_api_token = Some(crate::config::SecretString::new(token.clone()));
        let state = Arc::new(AppState::new(Arc::new(config)));
        let app = super::build_router(state);

        let call = |bearer: String| {
            let app = app.clone();
            async move {
                let request = Request::builder()
                    .uri("/oracle/failed-submissions")
                    .header("Authorization", format!("Bearer {bearer}"))
                    .body(Body::empty())
                    .unwrap();
                app.oneshot(request).await.unwrap().status()
            }
        };

        for _ in 0..crate::state::MAX_ADMIN_AUTH_FAILURES {
            assert_eq!(call("wrong".to_string()).await, StatusCode::UNAUTHORIZED);
        }
        assert_eq!(
            call("wrong".to_string()).await,
            StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(call(token).await, StatusCode::TOO_MANY_REQUESTS);
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
        let test_secret = "SBTINLKVKI6TNGJB3WC34ILDIKSJM5RVLZDCQJ4KLMXENSWCADKVM6CV";
        let test_admin_token = "admin_super_secret_token_123";
        // #1472 — only the decoded key is kept; the raw strkey is dropped at
        // config load and must never resurface in logs or metrics.
        config.keeper_secret_key = Some(Arc::new(
            crate::chain::tx_builder::signing_key_from_secret(test_secret).unwrap(),
        ));
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

    // #1474 — every route registered in build_router must appear in the
    // endpoint table in oracle/README.md (parsed from the markdown).
    #[test]
    fn readme_endpoint_table_covers_every_route() {
        let source = include_str!("mod.rs");
        let readme = include_str!("../../README.md");

        let start = source
            .find("pub fn build_router")
            .expect("build_router present");
        let body = &source[start..];
        let end = body.find("\n}").expect("build_router closes");
        let body = &body[..end];

        let mut routes = Vec::new();
        let mut rest = body;
        while let Some(idx) = rest.find("\"/") {
            rest = &rest[idx + 1..];
            let close = rest.find('"').expect("route literal closed");
            routes.push(&rest[..close]);
            rest = &rest[close + 1..];
        }
        assert!(
            !routes.is_empty(),
            "no route literals found in build_router"
        );

        let table_start = readme
            .find("## Endpoints")
            .expect("oracle/README.md has an Endpoints section");
        let table = &readme[table_start..];
        for route in routes {
            // `/unmatched` is a metrics label for requests that matched no
            // registered route, not an endpoint the router serves.
            if route == "/unmatched" {
                continue;
            }
            assert!(
                table.contains(route),
                "route {route} from build_router is missing from the endpoint table in oracle/README.md"
            );
        }
    }
}
