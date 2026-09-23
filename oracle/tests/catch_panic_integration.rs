use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use axum::Router;
use tower::ServiceExt;
use tower_http::catch_panic::CatchPanicLayer;
use wiremock::MockServer;

mod common;

use common::test_config;
use oracle::api::{build_router, handle_panic};
use oracle::state::AppState;

#[tokio::test]
async fn test_catch_panic_middleware_returns_500_json_error() {
    let router = Router::new()
        .route(
            "/panicking-endpoint",
            get(|| async -> &'static str {
                panic!("handler intentional panic for test");
            }),
        )
        .layer(CatchPanicLayer::custom(handle_panic));

    let req = Request::builder()
        .uri("/panicking-endpoint")
        .body(Body::empty())
        .unwrap();

    let res = router.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);

    let content_type = res
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(content_type.contains("application/json"));

    let body_bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let body_json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(body_json["error"], "internal_server_error");
}

#[tokio::test]
async fn test_server_survives_handler_panic_and_serves_subsequent_requests() {
    let mock_server = MockServer::start().await;
    let config = test_config(&mock_server.uri(), "http://127.0.0.1:9");
    let state = Arc::new(AppState::new(config));

    let panic_route = Router::new()
        .route(
            "/explode",
            get(|| async -> &'static str {
                panic!("critical panic inside handler");
            }),
        )
        .layer(CatchPanicLayer::custom(handle_panic));

    let app = build_router(state).merge(panic_route);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });

    let client = reqwest::Client::new();

    // 1. Send panicking request: should return HTTP 500 without crashing process
    let explode_url = format!("http://{}/explode", addr);
    let explode_res = client.get(&explode_url).send().await.unwrap();
    assert_eq!(explode_res.status(), 500);

    let explode_body: serde_json::Value = explode_res.json().await.unwrap();
    assert_eq!(explode_body["error"], "internal_server_error");

    // 2. Subsequent valid request to /health should succeed immediately with 200 OK
    let health_url = format!("http://{}/health", addr);
    let health_res = client.get(&health_url).send().await.unwrap();
    assert_eq!(health_res.status(), 200);

    let health_body: serde_json::Value = health_res.json().await.unwrap();
    assert_eq!(health_body["status"], "ok");
}
