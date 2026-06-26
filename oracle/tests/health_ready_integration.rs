use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::Request;
use tower::ServiceExt;
use wiremock::matchers::method;
use wiremock::{MockServer, ResponseTemplate};

use oracle::api::build_router;
use oracle::config::{Config, Network, PriceFeedConfig, SecretString};
use oracle::state::AppState;

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

// #339 — GET /health returns 200 with {"status":"ok"}, no auth required
#[tokio::test]
async fn get_health_returns_200_with_status_ok() {
    let config = test_config("http://127.0.0.1:9", "http://127.0.0.1:9");
    let state = Arc::new(AppState::new(config));
    let app = build_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), 200);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "ok");
}

// #340 — GET /ready returns 200 when RPC is reachable and keeper balance is healthy
#[tokio::test]
async fn get_ready_returns_200_when_healthy() {
    let rpc_mock = MockServer::start().await;
    let horizon_mock = MockServer::start().await;

    wiremock::Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&rpc_mock)
        .await;

    wiremock::Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "GAUHMCMUP5FZO5675W3ISZ6E6CNYJGXBUW5WANE2JR4TGAARYCTSCBKI",
            "balances": [{"asset_type": "native", "balance": "100.0000000"}]
        })))
        .mount(&horizon_mock)
        .await;

    let config = test_config(&rpc_mock.uri(), &horizon_mock.uri());
    let state = Arc::new(AppState::new(config));
    let app = build_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/ready")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), 200);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "ok");
}

// #340 — GET /ready returns 503 when RPC is unreachable
#[tokio::test]
async fn get_ready_returns_503_when_rpc_down() {
    // Port 9 is the discard protocol — connections are refused immediately
    let config = test_config("http://127.0.0.1:9", "http://127.0.0.1:9");
    let state = Arc::new(AppState::new(config));
    let app = build_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/ready")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), 503);
}
