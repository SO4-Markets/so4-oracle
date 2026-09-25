use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use axum::body::Body;
use axum::http::Request;
use predicates::prelude::*;
use tower::ServiceExt;
use wiremock::matchers::method;
use wiremock::{MockServer, Request as WireMockRequest, ResponseTemplate};

mod common;

use common::{fixed_token_with_price, test_config};
use oracle::api::build_router;
use oracle::config::{Config, Network, PriceFeedConfig, SecretString};
use oracle::keeper_loop::run_keeper_loop;
use oracle::price_loop::run_price_loop;
use oracle::state::{AppState, CachedPrice};

fn sample_cached_price() -> CachedPrice {
    CachedPrice {
        token_address: "addr".to_string(),
        symbol: "BTC".to_string(),
        display_symbol: "BTC".to_string(),
        keeper_index: 0,
        min: 100,
        max: 200,
        median: 150,
        timestamp: 0,
        ledger_seq: 1,
        sources_used: vec!["binance".to_string()],
        signature: "sig".to_string(),
    }
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

// #340 — GET /ready returns 200 when RPC is reachable, keeper balance is healthy,
//        price cache is populated, and price loop is recent
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

    // Populate price cache
    {
        let mut cache = state.price_cache.write().await;
        cache
            .prices
            .insert("BTC".to_string(), sample_cached_price());
    }

    // Set price cycle and keeper cycle as recent
    {
        let mut cycle = state.cycle_status.write().await;
        cycle.last_price_cycle_at = Some(SystemTime::now());
        cycle.last_keeper_cycle_at = Some(SystemTime::now());
    }

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

#[tokio::test]
async fn get_ready_retries_transient_keeper_balance_failure() {
    let rpc_mock = MockServer::start().await;
    let horizon_mock = MockServer::start().await;

    wiremock::Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&rpc_mock)
        .await;

    let balance_attempts = Arc::new(AtomicUsize::new(0));
    let balance_attempts_for_mock = Arc::clone(&balance_attempts);
    wiremock::Mock::given(method("GET"))
        .respond_with(move |_req: &WireMockRequest| {
            let attempt = balance_attempts_for_mock.fetch_add(1, Ordering::SeqCst);
            if attempt == 0 {
                ResponseTemplate::new(500).set_body_string("transient horizon error")
            } else {
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "id": "GAUHMCMUP5FZO5675W3ISZ6E6CNYJGXBUW5WANE2JR4TGAARYCTSCBKI",
                    "balances": [{"asset_type": "native", "balance": "100.0000000"}]
                }))
            }
        })
        .mount(&horizon_mock)
        .await;

    let config = test_config(&rpc_mock.uri(), &horizon_mock.uri());
    let state = Arc::new(AppState::new(config));

    {
        let mut cache = state.price_cache.write().await;
        cache
            .prices
            .insert("BTC".to_string(), sample_cached_price());
    }
    {
        let mut cycle = state.cycle_status.write().await;
        cycle.last_price_cycle_at = Some(SystemTime::now());
        cycle.last_keeper_cycle_at = Some(SystemTime::now());
    }

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
    assert_eq!(balance_attempts.load(Ordering::SeqCst), 2);
}

// #340 — GET /ready returns 503 when keeper loop is stale
#[tokio::test]
async fn get_ready_returns_503_when_keeper_loop_stale() {
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

    // Populate price cache and make price loop recent
    {
        let mut cache = state.price_cache.write().await;
        cache
            .prices
            .insert("BTC".to_string(), sample_cached_price());
    }
    {
        let mut cycle = state.cycle_status.write().await;
        cycle.last_price_cycle_at = Some(SystemTime::now());
        cycle.last_keeper_cycle_at = None;
    }

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

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(predicate::str::contains("stale").eval(json["error"].as_str().unwrap()));
}

// #340 — readiness becomes healthy once the spawned price and keeper loops complete
#[tokio::test]
async fn cold_start_reads_ready_after_price_and_keeper_loops() {
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

    wiremock::Mock::given(method("POST"))
        .respond_with(|req: &WireMockRequest| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
            let method_name = body["method"].as_str().unwrap_or("");

            match method_name {
                "getLatestLedger" => ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {"id": "abc", "sequence": 12345, "protocolVersion": 22}
                })),
                "simulateTransaction" => {
                    let op = body["params"]["transaction"]["operations"][0].clone();
                    let contract = op["contract_id"].as_str().unwrap_or("");
                    let method = op["method"].as_str().unwrap_or("");

                    if contract == "CC6OZUHF3LVO6PNP3V2EB36ORB3YSVYSH3LWD3RFLO4NUO3BYCXSWSYC" {
                        match method {
                            "get_order_count" => ResponseTemplate::new(200).set_body_json(serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": 1,
                                "result": 1
                            })),
                            "get_order_keys" => ResponseTemplate::new(200).set_body_json(serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": 1,
                                "result": {"vec": [{"bytes": "aabbccdd00112233aabbccdd00112233aabbccdd00112233aabbccdd00112233"}]}
                            })),
                            _ => ResponseTemplate::new(200).set_body_json(serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": 1,
                                "result": 0
                            })),
                        }
                    } else {
                        ResponseTemplate::new(200).set_body_json(serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": 1,
                            "result": 0
                        }))
                    }
                }
                "getAccount" => ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {
                        "id": "GAUHMCMUP5FZO5675W3ISZ6E6CNYJGXBUW5WANE2JR4TGAARYCTSCBKI",
                        "sequence": "100",
                        "subentries": 0,
                        "inflationDestination": "",
                        "homeDomain": "",
                        "thresholds": {"low": 1, "med": 1, "high": 1},
                        "signers": [],
                        "data": {},
                        "balances": []
                    }
                })),
                "sendTransaction" => ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {
                        "status": "PENDING",
                        "hash": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
                    }
                })),
                "getTransaction" => ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {
                        "status": "SUCCESS",
                        "ledger": 50001,
                        "diagnosticEventsXdr": []
                    }
                })),
                _ => ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "error": {"code": -1, "message": "unknown method"}
                })),
            }
        })
        .mount(&rpc_mock)
        .await;

    let config = Arc::new(Config {
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        network: Network::Testnet,
        network_passphrase: "Test SDF Network ; September 2015".to_string(),
        stellar_rpc_url: rpc_mock.uri(),
        horizon_url: horizon_mock.uri(),
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
        pyth_api_key: None,
        min_keeper_balance_xlm: 10.0,
        set_prices_tx_fee: oracle::config::DEFAULT_SET_PRICES_TX_FEE,
        keeper_tx_fee: oracle::config::DEFAULT_KEEPER_TX_FEE,
        price_loop_interval: Duration::from_millis(100),
        keeper_loop_interval: Duration::from_millis(100),
        price_feed: PriceFeedConfig {
            tokens: vec![fixed_token_with_price(
                "BTC",
                "CBAN5YU3KRDKPTQ2H76D6S7HQFPRBGUD524F65BUM2RQCITPTRLKWKES",
                "1000000000000000000000000000000",
            )],
        },
    });

    let state = Arc::new(AppState::new(config));
    let app = build_router(Arc::clone(&state));

    let price_handle = tokio::spawn(run_price_loop(Arc::clone(&state)));
    let keeper_handle = tokio::spawn(run_keeper_loop(Arc::clone(&state)));

    let ready_ok = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri("/ready")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();

            if response.status() == 200 {
                break;
            }

            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;

    state.shutdown_token.cancel();
    let _ = tokio::join!(price_handle, keeper_handle);

    assert!(ready_ok.is_ok(), "ready did not become healthy in time");

    let requests = rpc_mock.received_requests().await.unwrap_or_default();
    assert!(
        requests.iter().any(|req| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
            body["method"] == "sendTransaction"
        }),
        "keeper did not submit a transaction"
    );
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

// #435 — GET /ready returns 503 when price cache is empty
#[tokio::test]
async fn get_ready_returns_503_when_price_cache_empty() {
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

    // Set price cycle as recent (so staleness check passes)
    {
        let mut cycle = state.cycle_status.write().await;
        cycle.last_price_cycle_at = Some(SystemTime::now());
    }

    // Leave price cache empty
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

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["error"], "no_prices_cached");
}

// #435 — GET /ready returns 503 when price loop is stale (last cycle too old)
#[tokio::test]
async fn get_ready_returns_503_when_price_loop_stale() {
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

    // Populate price cache (so empty check passes)
    {
        let mut cache = state.price_cache.write().await;
        cache
            .prices
            .insert("BTC".to_string(), sample_cached_price());
    }

    // Set last_price_cycle_at to 30 seconds ago (stale, since interval is 1s * 3 = 3s)
    {
        let mut cycle = state.cycle_status.write().await;
        cycle.last_price_cycle_at = Some(SystemTime::now() - Duration::from_secs(30));
        cycle.last_keeper_cycle_at = Some(SystemTime::now());
    }

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

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["error"], "price_loop_stale");
}

// #520 — GET /ready returns 503 with keeper_balance_low when the keeper account
//        balance is below the configured minimum while all other checks pass
#[tokio::test]
async fn get_ready_returns_503_when_keeper_balance_low() {
    let rpc_mock = MockServer::start().await;
    let horizon_mock = MockServer::start().await;

    wiremock::Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&rpc_mock)
        .await;

    // 5 XLM is below the 10 XLM minimum configured by test_config
    wiremock::Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "GAUHMCMUP5FZO5675W3ISZ6E6CNYJGXBUW5WANE2JR4TGAARYCTSCBKI",
            "balances": [{"asset_type": "native", "balance": "5.0000000"}]
        })))
        .mount(&horizon_mock)
        .await;

    let config = test_config(&rpc_mock.uri(), &horizon_mock.uri());
    let state = Arc::new(AppState::new(config));

    // Populate price cache (so empty check passes)
    {
        let mut cache = state.price_cache.write().await;
        cache
            .prices
            .insert("BTC".to_string(), sample_cached_price());
    }

    // Set price and keeper cycles as recent (so both staleness checks pass)
    {
        let mut cycle = state.cycle_status.write().await;
        cycle.last_price_cycle_at = Some(SystemTime::now());
        cycle.last_keeper_cycle_at = Some(SystemTime::now());
    }

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

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["error"], "keeper_balance_low");
}

// #520 — GET /ready returns 503 with keeper_balance_check_failed when Horizon
//        cannot answer the balance query (here: HTTP 500) while all other
//        checks pass — distinct from keeper_balance_low
#[tokio::test]
async fn get_ready_returns_503_when_keeper_balance_check_fails() {
    let rpc_mock = MockServer::start().await;
    let horizon_mock = MockServer::start().await;

    wiremock::Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&rpc_mock)
        .await;

    // Horizon responds, but with a server error the balance check cannot use
    wiremock::Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500).set_body_string("internal error"))
        .mount(&horizon_mock)
        .await;

    let config = test_config(&rpc_mock.uri(), &horizon_mock.uri());
    let state = Arc::new(AppState::new(config));

    // Populate price cache (so empty check passes)
    {
        let mut cache = state.price_cache.write().await;
        cache
            .prices
            .insert("BTC".to_string(), sample_cached_price());
    }

    // Set price and keeper cycles as recent (so both staleness checks pass)
    {
        let mut cycle = state.cycle_status.write().await;
        cycle.last_price_cycle_at = Some(SystemTime::now());
        cycle.last_keeper_cycle_at = Some(SystemTime::now());
    }

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

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["error"], "keeper_balance_check_failed");
}
