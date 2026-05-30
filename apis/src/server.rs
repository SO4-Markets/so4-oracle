use crate::cache::Cache;
use crate::config::lookup_token;
use crate::history::{HistoryStore, Interval};
use crate::state::{AppState, MarketSummary, Reader, ReaderError};
use axum::{
    extract::{Path, Query},
    http::{header, HeaderValue, Method, StatusCode},
    response::{Html, IntoResponse},
    routing::get,
    Extension, Json, Router,
};
use futures::future::join_all;
use serde::{Deserialize, Serialize};
use std::env;
use std::sync::Arc;
use std::time::Duration;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::trace::TraceLayer;
use tracing::{info, warn};
use utoipa::OpenApi;

/// OpenAPI 3 document (issue #108).
///
/// utoipa derives the spec from `#[utoipa::path]` annotations on the
/// handler functions plus the `components.schemas` listed here. The
/// resulting JSON is served from `GET /openapi.json` and the Swagger UI
/// is mounted under `GET /docs` by `mount_openapi_routes` below.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "so4-oracle APIs",
        version = env!("CARGO_PKG_VERSION"),
        description = "Read-only API surface exposed by the SO4 oracle worker."
    ),
    paths(health, get_price, get_price_history),
    components(schemas(HealthDoc, PriceResp))
)]
pub struct ApiDoc;

/// Schema-only struct so `/health` shows up in the OpenAPI spec — the
/// handler returns `serde_json::Value` directly, which utoipa cannot
/// derive a schema from.
#[derive(Serialize, utoipa::ToSchema)]
#[allow(dead_code)]
struct HealthDoc {
    status: String,
}

/// Construct the full `Router` the production server runs, wired with
/// CORS (#105), tracing (#106), the OpenAPI/Swagger surface (#108), and
/// the shared `AppState` extension. Exposed so integration tests can
/// exercise the same router without binding a TCP listener.
pub fn build_app(state: AppState) -> Router {
    let app = Router::new()
        .route("/health", get(health))
        .route("/prices/:token", get(get_price))
        .route("/prices/:token/history", get(get_price_history))
        .route("/markets", get(get_markets))
        .route("/markets/:market_id", get(get_market))
        .route("/positions/:account", get(get_positions));

    // OpenAPI (issue #108): mount `/openapi.json` and the Swagger UI at
    // `/docs`. Done before applying layers so the static-asset routes
    // inherit the same trace and CORS configuration.
    let app = mount_openapi_routes(app);

    // CORS (issue #105), tracing middleware (issue #106), and shared
    // state are layered after the routes so they apply to every endpoint
    // including the OpenAPI surface.
    app.layer(build_cors_layer())
        .layer(TraceLayer::new_for_http())
        .layer(Extension(Arc::new(state)))
}

pub async fn run() -> Result<(), anyhow::Error> {
    let cache = Cache::new();
    let reader = Arc::new(crate::client::RpcClient::from_env()) as Arc<dyn Reader + Send + Sync>;
    let history = HistoryStore::new();

    // Background task: record a price tick every 60 seconds for all known tokens.
    let history_bg = history.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            interval.tick().await;
            let ts = chrono::Utc::now().timestamp() as u64;
            // Iterate over all tokens in the static config and record mid-price.
            if let Some(tokens) = crate::config::all_tokens() {
                for entry in tokens {
                    let mid = (entry.min + entry.max) / 2.0;
                    history_bg.record(&entry.token, ts, mid);
                }
            }
        }
    });

    let state = AppState { cache, reader, history };
    let app = build_app(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await?;
    info!("listening on {}", listener.local_addr()?);

    // Graceful shutdown (issue #107): on SIGTERM/SIGINT we stop
    // accepting new connections and let in-flight requests finish.
    // The hard cap is 30 seconds — past that, a watchdog task forces
    // the process to exit so deployment orchestrators (Kubernetes,
    // Docker, systemd) don't have to send SIGKILL themselves.
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            shutdown_signal().await;
            // Once the signal fires, arm the force-exit watchdog. The
            // watchdog is spawned (not awaited) so the returned future
            // resolves immediately and tells axum to start draining.
            tokio::spawn(async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                warn!("shutdown: 30s drain timeout exceeded, forcing exit");
                std::process::exit(0);
            });
        })
        .await?;
    info!("server stopped cleanly");
    Ok(())
}

/// Mount the OpenAPI JSON endpoint and a minimal Swagger UI under `/docs`.
///
/// Rather than pull in the heavy `utoipa-swagger-ui` crate (which had
/// transitive `arbitrary` version conflicts against the Stellar SDK in
/// this workspace), `/docs` is a tiny HTML page that loads the Swagger UI
/// bundle from the official CDN and points it at our own
/// `/openapi.json`. Same UX, zero extra dependencies.
fn mount_openapi_routes(app: Router) -> Router {
    app.route("/openapi.json", get(serve_openapi))
        .route("/docs", get(serve_swagger_ui))
}

async fn serve_openapi() -> impl IntoResponse {
    let body = ApiDoc::openapi()
        .to_json()
        .unwrap_or_else(|_| "{}".to_string());
    (
        [(header::CONTENT_TYPE, "application/json")],
        body,
    )
}

async fn serve_swagger_ui() -> Html<&'static str> {
    Html(SWAGGER_UI_HTML)
}

const SWAGGER_UI_HTML: &str = r##"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>so4-oracle APIs</title>
    <link rel="stylesheet" href="https://unpkg.com/swagger-ui-dist@5/swagger-ui.css" />
  </head>
  <body>
    <div id="swagger-ui"></div>
    <script src="https://unpkg.com/swagger-ui-dist@5/swagger-ui-bundle.js" crossorigin></script>
    <script>
      window.ui = SwaggerUIBundle({
        url: "/openapi.json",
        dom_id: "#swagger-ui",
        presets: [SwaggerUIBundle.presets.apis],
        layout: "BaseLayout",
      });
    </script>
  </body>
</html>
"##;

/// CORS configuration (issue #105).
///
/// In development (`APP_ENV != "production"` and no explicit
/// `CORS_ALLOWED_ORIGINS`) we fall back to a permissive any-origin
/// policy so local frontends just work. In production we require
/// `CORS_ALLOWED_ORIGINS` (comma-separated) and reject the request if
/// none parse cleanly — the layer simply omits the
/// `Access-Control-Allow-Origin` header for non-matching origins,
/// which browsers translate into a CORS error.
fn build_cors_layer() -> CorsLayer {
    let allowed = env::var("CORS_ALLOWED_ORIGINS").unwrap_or_default();
    let is_production =
        env::var("APP_ENV").unwrap_or_default().eq_ignore_ascii_case("production");

    let methods = [
        Method::GET,
        Method::POST,
        Method::PUT,
        Method::PATCH,
        Method::DELETE,
        Method::OPTIONS,
    ];
    let headers = [
        header::AUTHORIZATION,
        header::ACCEPT,
        header::CONTENT_TYPE,
    ];

    let base = CorsLayer::new()
        .allow_methods(methods)
        .allow_headers(headers)
        .max_age(Duration::from_secs(3600));

    if allowed.trim().is_empty() {
        if is_production {
            warn!(
                "CORS_ALLOWED_ORIGINS is empty in production — no origin will be allowed. Set CORS_ALLOWED_ORIGINS=https://frontend.example.com,…"
            );
            return base.allow_origin(AllowOrigin::list(Vec::new()));
        }
        info!("CORS: dev mode — allowing any origin (set CORS_ALLOWED_ORIGINS to restrict)");
        return base.allow_origin(tower_http::cors::Any);
    }

    let origins: Vec<HeaderValue> = allowed
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse::<HeaderValue>().ok())
        .collect();

    info!(
        count = origins.len(),
        "CORS: allowing {} origin(s) from CORS_ALLOWED_ORIGINS",
        origins.len()
    );
    base.allow_origin(AllowOrigin::list(origins))
}

/// Future that resolves on the first SIGTERM (Unix) or Ctrl-C
/// (cross-platform). Used by `axum::serve(...).with_graceful_shutdown(...)`
/// to start tearing the server down. See issue #107.
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            warn!("failed to install Ctrl-C handler: {err}");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sigterm) => {
                sigterm.recv().await;
            }
            Err(err) => warn!("failed to install SIGTERM handler: {err}"),
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => info!("shutdown: Ctrl-C received, draining in-flight requests"),
        _ = terminate => info!("shutdown: SIGTERM received, draining in-flight requests"),
    }
}

#[utoipa::path(
    get,
    path = "/health",
    responses(
        (status = 200, description = "Service is up", body = HealthDoc)
    )
)]
async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({"status":"ok"}))
}

// ── GET /prices/:token/history ───────────────────────────────────────────────

#[derive(Deserialize)]
struct HistoryQuery {
    /// Candle interval: "1m", "5m", or "1h" (default: "1m").
    interval: Option<String>,
    /// Unix timestamp (seconds) — start of range (default: now − 24 h).
    from: Option<u64>,
    /// Unix timestamp (seconds) — end of range (default: now).
    to: Option<u64>,
}

#[utoipa::path(
    get,
    path = "/prices/{token}/history",
    params(
        ("token" = String, Path, description = "Token symbol (case-insensitive)"),
        ("interval" = Option<String>, Query, description = "Candle interval: 1m, 5m, 1h"),
        ("from" = Option<u64>, Query, description = "Unix timestamp range start (default: now − 24h)"),
        ("to" = Option<u64>, Query, description = "Unix timestamp range end (default: now)"),
    ),
    responses(
        (status = 200, description = "OHLCV candles for the token"),
        (status = 400, description = "Invalid interval or `from` > `to`"),
        (status = 404, description = "No history recorded for this token"),
    )
)]
pub async fn get_price_history(
    Path(token): Path<String>,
    Query(params): Query<HistoryQuery>,
    Extension(state): Extension<Arc<AppState>>,
) -> impl IntoResponse {
    let interval_str = params.interval.as_deref().unwrap_or("1m");
    let interval = match Interval::from_str(interval_str) {
        Some(i) => i,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "invalid interval — use 1m, 5m, or 1h"})),
            )
                .into_response();
        }
    };

    let now = chrono::Utc::now().timestamp() as u64;
    let to = params.to.unwrap_or(now);
    let from = params.from.unwrap_or_else(|| to.saturating_sub(86_400));

    if from > to {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "`from` must be <= `to`"})),
        )
            .into_response();
    }

    match state.history.query(&token, from, to, interval) {
        Some(candles) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "token": token.to_lowercase(),
                "interval": interval_str,
                "from": from,
                "to": to,
                "candles": candles,
            })),
        )
            .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "no history for token"})),
        )
            .into_response(),
    }
}

#[derive(Serialize, utoipa::ToSchema)]
struct PriceResp {
    token: String,
    symbol: String,
    min: f64,
    max: f64,
    timestamp: i64,
    sources_used: Vec<String>,
}

#[utoipa::path(
    get,
    path = "/prices/{token}",
    params(
        ("token" = String, Path, description = "Token symbol (case-insensitive)")
    ),
    responses(
        (status = 200, description = "Latest min/max price for the token", body = PriceResp),
        (status = 404, description = "Token not in the configured feed"),
    )
)]
pub async fn get_price(
    Path(token): Path<String>,
    Extension(_state): Extension<Arc<AppState>>,
) -> impl IntoResponse {
    let key = token.to_lowercase();
    if let Some(entry) = lookup_token(&key) {
        let resp = PriceResp {
            token: entry.token.clone(),
            symbol: entry.symbol,
            min: entry.min,
            max: entry.max,
            timestamp: chrono::Utc::now().timestamp(),
            sources_used: entry.sources_used.clone(),
        };
        return (StatusCode::OK, Json(resp)).into_response();
    }
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({"error":"token not found in feed"})),
    )
        .into_response()
}

pub async fn get_markets(Extension(state): Extension<Arc<AppState>>) -> impl IntoResponse {
    // cache key
    let cache_key = "markets_list";
    if let Some(cached) = state.cache.get::<Vec<MarketSummary>>(cache_key).await {
        return (StatusCode::OK, Json(serde_json::to_value(cached).unwrap_or_default()));
    }

    let markets = match state.reader.get_markets().await {
        Ok(v) => v,
        Err(_) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({"error":"rpc failure"})),
            )
        }
    };

    let futs = markets.into_iter().map(|m| {
        let r = state.reader.clone();
        async move { r.get_market_pool_value_info(&m).await }
    });

    let results = join_all(futs).await;
    let mut out = Vec::new();
    for r in results {
        if let Ok(s) = r {
            out.push(s);
        }
    }

    let ttl = Duration::from_secs(30);
    state.cache.set(cache_key, &out, ttl).await;

    (StatusCode::OK, Json(serde_json::to_value(out).unwrap_or_default()))
}

async fn get_market(
    Path(market_id): Path<String>,
    Extension(state): Extension<Arc<AppState>>,
) -> impl IntoResponse {
    let key = format!("market_detail:{}", market_id.to_lowercase());
    if let Some(cached) = state.cache.get::<serde_json::Value>(&key).await {
        return (StatusCode::OK, Json(cached));
    }

    let detail = match state.reader.get_market_detail(&market_id).await {
        Ok(v) => v,
        Err(ReaderError::NotFound) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error":"market not found"})),
            )
        }
        Err(_) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({"error":"rpc failure"})),
            )
        }
    };

    // For top positions, assume detail contains position ids under "top_positions"
    let top_positions: Vec<String> = detail
        .get("top_positions")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    let futs = top_positions.iter().map(|p| {
        let r = state.reader.clone();
        let pid = p.clone();
        async move { r.get_position_info(&pid).await }
    });
    let pos_results = join_all(futs).await;
    let mut positions = Vec::new();
    for r in pos_results {
        if let Ok(v) = r {
            positions.push(v);
        }
    }

    let resp = serde_json::json!({
        "market": detail,
        "top_positions": positions,
    });

    let ttl = Duration::from_secs(15);
    state.cache.set(&key, &resp, ttl).await;

    (StatusCode::OK, Json(resp))
}

async fn get_positions(
    Path(account): Path<String>,
    Extension(state): Extension<Arc<AppState>>,
) -> impl IntoResponse {
    let acct = account.to_lowercase();
    // validate simple format
    if acct.len() != 56 || !(acct.starts_with('g') || acct.starts_with('G')) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error":"invalid account"})),
        );
    }

    let positions = match state.reader.get_account_positions(&acct).await {
        Ok(v) => v,
        Err(_) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({"error":"rpc failure"})),
            )
        }
    };

    if positions.is_empty() {
        return (StatusCode::OK, Json(serde_json::json!([])));
    }

    let futs = positions.iter().map(|p| {
        let r = state.reader.clone();
        let pid = p.clone();
        async move { r.get_position_info(&pid).await }
    });
    let pos_results = join_all(futs).await;
    let mut out = Vec::new();
    for r in pos_results {
        if let Ok(mut v) = r {
            // compute pnl using latest price for position's index token
            if let Some(idx) = v.get("index_token").and_then(|x| x.as_str()) {
                if let Ok(price) = state.reader.get_latest_price(idx).await {
                    if let Some(entry_price) = v.get("entry_price").and_then(|x| x.as_f64()) {
                        let size = v.get("size").and_then(|x| x.as_f64()).unwrap_or(0.0);
                        let pnl = (price - entry_price) * size;
                        v.as_object_mut().map(|m| {
                            m.insert("current_pnl".to_string(), serde_json::json!(pnl));
                        });
                    }
                }
            }
            out.push(v);
        }
    }

    (StatusCode::OK, Json(serde_json::Value::Array(out)))
}
