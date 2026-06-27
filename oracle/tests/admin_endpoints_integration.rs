use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;
use wiremock::matchers::method;
use wiremock::{MockServer, ResponseTemplate};

use oracle::api::build_router;
use oracle::config::{Config, Network, PriceFeedConfig, SecretString};
use oracle::state::{AppState, FailedSubmission};
use std::time::SystemTime;

fn test_config(rpc_url: &str, horizon_url: &str) -> Arc<Config> {
    Arc::new(Config {
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        network: Network::Testnet,
        network_passphrase: "Test SDF Network ; September 2015".to_string(),
        stellar_rpc_url: rpc_url.to_string(),
        horizon_url: horizon_url.to_string(),
        oracle_contract_id: "CBEMTV23SIJJBIST3V5HTMWHR4MHYGHNBIG4M26U4LGUJTWZXTFSVQEY".to_string(),
        role_store_contract_id: "CBSUAIAMIFFS4AXQYZ7KR7FNO7IMKAPS5WF4DXANVXDTPKH2F7YUIN6Q"
            .to_string(),
        data_store_contract_id: "CCZ3VKBEDLNBO2JM3EXL3SNBDJOV5BTN52FVQPER7F6D5GCE53PITQ3J"
            .to_string(),
        order_handler_contract_id: "CC35OFZVWUTAZPV3B6UKSDVAVORZEWUUMOMTHO33H4YR4C5FKPEFODKY"
            .to_string(),
        deposit_handler_contract_id: "CDWOFIP4YQJGMCYAOWLSRBAWN2OTJUG2I5WOFC32O2TX2SRU56RWBE5C"
            .to_string(),
        withdrawal_handler_contract_id: "CCA5HRHMG6E6BVYRICSLZ5CK5KNPAAKXQ7XWDM34WWVGNHWHA26GRVVE"
            .to_string(),
        reader_contract_id: "CC6OZUHF3LVO6PNP3V2EB36ORB3YSVYSH3LWD3RFLO4NUO3BYCXSWSYC".to_string(),
        keeper_private_key: SecretString::new(
            "1111111111111111111111111111111111111111111111111111111111111111".to_string(),
        ),
        keeper_secret_key: SecretString::new(
            "1111111111111111111111111111111111111111111111111111111111111111".to_string(),
        ),
        keeper_account_id: "GAUHMCMUP5FZO5675W3ISZ6E6CNYJGXBUW5WANE2JR4TGAARYCTSCBKI".to_string(),
        keeper_index: 0,
        admin_api_token: Some(SecretString::new("test-admin-token".to_string())),
        min_keeper_balance_xlm: 10.0,
        price_loop_interval: Duration::from_millis(1000),
        keeper_loop_interval: Duration::from_millis(1500),
        price_feed: PriceFeedConfig { tokens: vec![] },
    })
}

fn auth_header() -> String {
    "Bearer test-admin-token".to_string()
}

// ── #431 — GET /keeper/status ────────────────────────────────────────────────

#[tokio::test]
async fn keeper_status_returns_401_without_token() {
    let config = test_config("http://127.0.0.1:9", "http://127.0.0.1:9");
    let state = Arc::new(AppState::new(config));
    let app = build_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/keeper/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn keeper_status_returns_expected_fields() {
    let config = test_config("http://127.0.0.1:9", "http://127.0.0.1:9");
    let state = Arc::new(AppState::new(config));
    let app = build_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/keeper/status")
                .header("Authorization", auth_header())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json.get("pending_orders").is_some());
    assert!(json.get("pending_deposits").is_some());
    assert!(json.get("pending_withdrawals").is_some());
    assert!(json.get("last_executions").is_some());
    assert!(json["last_executions"].as_array().unwrap().len() <= 50);
}

// ── #432 — GET /oracle/failed-submissions ────────────────────────────────────

#[tokio::test]
async fn failed_submissions_returns_401_without_token() {
    let config = test_config("http://127.0.0.1:9", "http://127.0.0.1:9");
    let state = Arc::new(AppState::new(config));
    let app = build_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/oracle/failed-submissions")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn failed_submissions_returns_total_count() {
    let config = test_config("http://127.0.0.1:9", "http://127.0.0.1:9");
    let state = Arc::new(AppState::new(config));

    {
        let mut failures = state.failures.lock().await;
        failures.push(FailedSubmission {
            at: SystemTime::now(),
            operation: "execute_order".to_string(),
            network: "testnet".to_string(),
            token: "CADDR".to_string(),
            symbol: "BTC".to_string(),
            min: 0,
            max: 0,
            tx_hash: None,
            error: "Budget ExceededLimit".to_string(),
            timestamp: 0,
            ledger_seq: 0,
        });
    }

    let app = build_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/oracle/failed-submissions")
                .header("Authorization", auth_header())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json.get("failures").is_some());
    assert!(json.get("total_count").is_some());
    assert_eq!(json["total_count"], 1);
}

#[tokio::test]
async fn failed_submissions_filters_by_operation() {
    let config = test_config("http://127.0.0.1:9", "http://127.0.0.1:9");
    let state = Arc::new(AppState::new(config));

    {
        let mut failures = state.failures.lock().await;
        failures.push(FailedSubmission {
            at: SystemTime::now(),
            operation: "execute_order".to_string(),
            network: "testnet".to_string(),
            token: String::new(),
            symbol: String::new(),
            min: 0,
            max: 0,
            tx_hash: None,
            error: "err1".to_string(),
            timestamp: 0,
            ledger_seq: 0,
        });
        failures.push(FailedSubmission {
            at: SystemTime::now(),
            operation: "execute_deposit".to_string(),
            network: "testnet".to_string(),
            token: String::new(),
            symbol: String::new(),
            min: 0,
            max: 0,
            tx_hash: None,
            error: "err2".to_string(),
            timestamp: 0,
            ledger_seq: 0,
        });
    }

    let app = build_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/oracle/failed-submissions?operation=execute_order")
                .header("Authorization", auth_header())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["total_count"], 1);
    assert_eq!(json["failures"][0]["operation"], "execute_order");
}

#[tokio::test]
async fn failed_submissions_respects_limit() {
    let config = test_config("http://127.0.0.1:9", "http://127.0.0.1:9");
    let state = Arc::new(AppState::new(config));

    {
        let mut failures = state.failures.lock().await;
        for i in 0..5 {
            failures.push(FailedSubmission {
                at: SystemTime::now(),
                operation: format!("op_{i}"),
                network: "testnet".to_string(),
                token: String::new(),
                symbol: String::new(),
                min: 0,
                max: 0,
                tx_hash: None,
                error: "err".to_string(),
                timestamp: 0,
                ledger_seq: 0,
            });
        }
    }

    let app = build_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/oracle/failed-submissions?limit=2")
                .header("Authorization", auth_header())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["failures"].as_array().unwrap().len(), 2);
    assert_eq!(json["total_count"], 5);
}

// ── #433 — GET /keeper/balance ───────────────────────────────────────────────

#[tokio::test]
async fn keeper_balance_returns_401_without_token() {
    let config = test_config("http://127.0.0.1:9", "http://127.0.0.1:9");
    let state = Arc::new(AppState::new(config));
    let app = build_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/keeper/balance")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn keeper_balance_returns_503_when_horizon_unreachable() {
    let config = test_config("http://127.0.0.1:9", "http://127.0.0.1:9");
    let state = Arc::new(AppState::new(config));
    let app = build_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/keeper/balance")
                .header("Authorization", auth_header())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn keeper_balance_returns_200_when_horizon_healthy() {
    let horizon_mock = MockServer::start().await;

    wiremock::Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "GAUHMCMUP5FZO5675W3ISZ6E6CNYJGXBUW5WANE2JR4TGAARYCTSCBKI",
            "balances": [{"asset_type": "native", "balance": "100.5000000"}]
        })))
        .mount(&horizon_mock)
        .await;

    let config = test_config("http://127.0.0.1:9", &horizon_mock.uri());
    let state = Arc::new(AppState::new(config));
    let app = build_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/keeper/balance")
                .header("Authorization", auth_header())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json.get("account_id").is_some());
    assert!(json.get("balance_stroops").is_some());
    assert!(json.get("balance_xlm").is_some());
    assert!(json.get("min_balance_xlm").is_some());
    assert!(json.get("is_funded").is_some());
    assert_eq!(json["balance_xlm"], 100.5);
    assert_eq!(json["is_funded"], true);
}

#[tokio::test]
async fn keeper_balance_reports_unfunded_when_below_minimum() {
    let horizon_mock = MockServer::start().await;

    wiremock::Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "GAUHMCMUP5FZO5675W3ISZ6E6CNYJGXBUW5WANE2JR4TGAARYCTSCBKI",
            "balances": [{"asset_type": "native", "balance": "3.0000000"}]
        })))
        .mount(&horizon_mock)
        .await;

    let config = test_config("http://127.0.0.1:9", &horizon_mock.uri());
    let state = Arc::new(AppState::new(config));
    let app = build_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/keeper/balance")
                .header("Authorization", auth_header())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["balance_xlm"], 3.0);
    assert_eq!(json["is_funded"], false);
}
