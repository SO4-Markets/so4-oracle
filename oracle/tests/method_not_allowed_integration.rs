use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use tower::ServiceExt;
use wiremock::MockServer;

mod common;
use common::test_config;
use oracle::api::build_router;
use oracle::state::AppState;

#[tokio::test]
async fn test_unsupported_methods_on_get_endpoints_return_json_405() {
    let mock_server = MockServer::start().await;
    let config = test_config(&mock_server.uri(), "http://127.0.0.1:9");
    let state = Arc::new(AppState::new(config));
    let app = build_router(state);

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
    let unsupported_methods = [Method::POST, Method::PUT, Method::PATCH, Method::DELETE];

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
                StatusCode::METHOD_NOT_ALLOWED,
                "Expected 405 for {} {}",
                method,
                endpoint
            );

            let content_type = res
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            assert_eq!(
                content_type, "application/json",
                "Expected Content-Type: application/json for {} {}, got: {}",
                method, endpoint, content_type
            );

            let allow = res
                .headers()
                .get(axum::http::header::ALLOW)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            assert!(
                allow.contains("GET"),
                "Expected allow header with GET for {} {}, got: {}",
                method,
                endpoint,
                allow
            );

            let body = axum::body::to_bytes(res.into_body(), usize::MAX)
                .await
                .unwrap();
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(
                json,
                serde_json::json!({ "error": "method_not_allowed" }),
                "Payload mismatch for {} {}",
                method,
                endpoint
            );
        }
    }
}

#[tokio::test]
async fn test_unsupported_methods_on_delete_endpoint_return_json_405() {
    let mock_server = MockServer::start().await;
    let config = test_config(&mock_server.uri(), "http://127.0.0.1:9");
    let state = Arc::new(AppState::new(config));
    let app = build_router(state);

    let endpoint = "/keeper/blacklist/test-key-123";
    let unsupported_methods = [Method::GET, Method::POST, Method::PUT, Method::PATCH];

    for method in &unsupported_methods {
        let req = Request::builder()
            .method(method.clone())
            .uri(endpoint)
            .body(Body::empty())
            .unwrap();

        let res = app.clone().oneshot(req).await.unwrap();

        assert_eq!(
            res.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "Expected 405 for {} {}",
            method,
            endpoint
        );

        let content_type = res
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert_eq!(
            content_type, "application/json",
            "Expected Content-Type: application/json for {} {}, got: {}",
            method, endpoint, content_type
        );

        let allow = res
            .headers()
            .get(axum::http::header::ALLOW)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            allow.contains("DELETE"),
            "Expected allow header with DELETE for {} {}, got: {}",
            method,
            endpoint,
            allow
        );

        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "error": "method_not_allowed" }),
            "Payload mismatch for {} {}",
            method,
            endpoint
        );
    }
}

#[tokio::test]
async fn test_protocol_headers_preserved_on_method_not_allowed() {
    let mock_server = MockServer::start().await;
    let config = test_config(&mock_server.uri(), "http://127.0.0.1:9");
    let state = Arc::new(AppState::new(config));
    let app = build_router(state);

    // Test x-request-id propagation on 405
    let req = Request::builder()
        .method(Method::POST)
        .uri("/prices")
        .header("x-request-id", "custom-request-id-405")
        .body(Body::empty())
        .unwrap();

    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(
        res.headers()
            .get("x-request-id")
            .and_then(|v| v.to_str().ok()),
        Some("custom-request-id-405")
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
async fn test_legitimate_endpoints_and_unmatched_routes_unaffected() {
    let mock_server = MockServer::start().await;
    let config = test_config(&mock_server.uri(), "http://127.0.0.1:9");
    let state = Arc::new(AppState::new(config));
    let app = build_router(state);

    // 1. GET /health works (200 OK)
    let res_health = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res_health.status(), StatusCode::OK);

    // 2. Unmatched path GET /unmatched-path returns 404 NOT FOUND (not 405)
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
    assert_eq!(res_404_get.status(), StatusCode::NOT_FOUND);

    // 3. Unmatched path POST /unmatched-path returns 404 NOT FOUND (not 405)
    let res_404_post = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/unmatched-path")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res_404_post.status(), StatusCode::NOT_FOUND);

    // 4. DELETE /keeper/blacklist/{key} is recognized as a valid registered route:
    // Without admin token it returns 401 UNAUTHORIZED, proving the handler/auth ran and wasn't rejected as 405.
    let res_delete_auth = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri("/keeper/blacklist/test-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res_delete_auth.status(), StatusCode::UNAUTHORIZED);
}
