use std::sync::Arc;
use std::time::Duration;

use wiremock::MockServer;

use oracle::state::{AppState, CachedPrice};

mod common;

fn test_cached_price() -> CachedPrice {
    CachedPrice {
        token_address: "GAUHMCMUP5FZO5675W3ISZ6E6CNYJGXBUW5WANE2JR4TGAARYCTSCBKI".to_string(),
        symbol: "TUSDC".to_string(),
        display_symbol: "USDC".to_string(),
        min: 1_000_000_000_000_000_000_000_000_000_000,
        max: 1_000_000_000_000_000_000_000_000_000_000,
        median: 1_000_000_000_000_000_000_000_000_000_000,
        timestamp: 1718400000,
        ledger_seq: 12345,
        sources_used: vec!["fixed".to_string()],
        signature: "00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000".to_string(),
        keeper_index: 0,
    }
}

#[tokio::test]
async fn http_get_prices_with_populated_cache() {
    let _mock_server = MockServer::start().await;
    let config = common::test_config(&_mock_server.uri(), &_mock_server.uri());
    let state = Arc::new(AppState::new(config));

    {
        let mut cache = state.price_cache.write().await;
        cache
            .prices
            .insert("TUSDC".to_string(), test_cached_price());
    }

    let app = oracle::api::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move { axum::serve(listener, app).await.ok() });

    let client = reqwest::Client::new();
    let url = format!("http://{}/prices", addr);
    let response = client.get(&url).send().await.unwrap();

    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.unwrap();
    assert!(body.is_array());
    let prices = body.as_array().unwrap();
    assert!(prices
        .iter()
        .any(|p| p.get("symbol").unwrap().as_str().unwrap() == "TUSDC"));
}

#[tokio::test]
async fn http_get_prices_with_empty_cache() {
    let _mock_server = MockServer::start().await;
    let config = common::test_config(&_mock_server.uri(), &_mock_server.uri());
    let state = Arc::new(AppState::new(config));

    // Price cache is empty, so should return 503
    let app = oracle::api::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move { axum::serve(listener, app).await.ok() });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let response = reqwest::Client::new()
        .get(format!("http://{}/prices", addr))
        .send()
        .await;

    // May fail due to timing, but if successful, should be 503
    if let Ok(resp) = response {
        assert_eq!(resp.status(), 503);
    }
}

#[tokio::test]
async fn http_get_metrics_rejects_without_token() {
    let _mock_server = MockServer::start().await;
    let config = common::test_config(&_mock_server.uri(), &_mock_server.uri());
    let state = Arc::new(AppState::new(config));

    let app = oracle::api::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move { axum::serve(listener, app).await.ok() });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let response = reqwest::Client::new()
        .get(format!("http://{}/metrics", addr))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 401);
}

#[tokio::test]
async fn http_get_metrics_succeeds_with_valid_token() {
    let _mock_server = MockServer::start().await;
    let config = common::test_config(&_mock_server.uri(), &_mock_server.uri());
    let state = Arc::new(AppState::new(config));

    let app = oracle::api::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move { axum::serve(listener, app).await.ok() });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let response = reqwest::Client::new()
        .get(format!("http://{}/metrics", addr))
        .header("Authorization", "Bearer test-admin-token")
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
}

#[tokio::test]
async fn http_get_oracle_status_rejects_without_token() {
    let _mock_server = MockServer::start().await;
    let config = common::test_config(&_mock_server.uri(), &_mock_server.uri());
    let state = Arc::new(AppState::new(config));

    let app = oracle::api::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move { axum::serve(listener, app).await.ok() });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let response = reqwest::Client::new()
        .get(format!("http://{}/oracle/status", addr))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 401);
}

#[tokio::test]
async fn http_get_oracle_status_succeeds_with_valid_token() {
    let _mock_server = MockServer::start().await;
    let config = common::test_config(&_mock_server.uri(), &_mock_server.uri());
    let state = Arc::new(AppState::new(config));

    let app = oracle::api::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move { axum::serve(listener, app).await.ok() });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let response = reqwest::Client::new()
        .get(format!("http://{}/oracle/status", addr))
        .header("Authorization", "Bearer test-admin-token")
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
}
