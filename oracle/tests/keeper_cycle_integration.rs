use std::sync::Arc;
use std::time::Duration;

use wiremock::{MockServer, Request, ResponseTemplate};

use oracle::config::{Config, Network, PriceFeedConfig, SecretString};
use oracle::state::{AppState, CachedPrice};

fn test_config(rpc_url: &str) -> Arc<Config> {
    Arc::new(Config {
        bind_addr: "127.0.0.1:8080".parse().unwrap(),
        network: Network::Testnet,
        network_passphrase: "Test SDF Network ; September 2015".to_string(),
        stellar_rpc_url: rpc_url.to_string(),
        horizon_url: "https://horizon-testnet.stellar.org".to_string(),
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
    }
}

#[tokio::test]
async fn mock_rpc_empty_cycle_skips_submission() {
    let mock_server = MockServer::start().await;
    let rpc_url = mock_server.uri();

    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .respond_with(|req: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            let method = body["method"].as_str().unwrap_or("");

            match method {
                "simulateTransaction" => {
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "jsonrpc": "2.0", "id": 1,
                        "result": 0
                    }))
                }
                _ => ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "jsonrpc": "2.0", "id": 1,
                    "error": {"code": -1, "message": "unexpected method"}
                })),
            }
        })
        .mount(&mock_server)
        .await;

    let config = test_config(&rpc_url);
    let state = Arc::new(AppState::new(config));

    {
        let mut cache = state.price_cache.write().await;
        cache
            .prices
            .insert("TUSDC".to_string(), test_cached_price());
    }

    let result = oracle::keeper_loop::run_keeper_cycle(Arc::clone(&state)).await;

    assert!(
        result.is_ok(),
        "keeper cycle should succeed with no pending work: {:?}",
        result.err()
    );
    let summary = result.unwrap();
    assert_eq!(summary.orders_executed, 0);
    assert_eq!(summary.deposits_executed, 0);
    assert_eq!(summary.withdrawals_executed, 0);
    assert_eq!(summary.errors, 0);
}

#[tokio::test]
async fn mock_rpc_rpc_failure_continues() {
    let mock_server = MockServer::start().await;
    let rpc_url = mock_server.uri();

    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .respond_with(ResponseTemplate::new(500).set_body_string("internal error"))
        .mount(&mock_server)
        .await;

    let config = test_config(&rpc_url);
    let state = Arc::new(AppState::new(config));

    {
        let mut cache = state.price_cache.write().await;
        cache
            .prices
            .insert("TUSDC".to_string(), test_cached_price());
    }

    let result = oracle::keeper_loop::run_keeper_cycle(Arc::clone(&state)).await;

    assert!(result.is_err(), "keeper cycle should fail on RPC error");
    let err = result.err().unwrap();
    assert!(
        err.contains("RPC")
            || err.contains("request failed")
            || err.contains("parse")
            || err.contains("status")
    );
}

#[tokio::test]
async fn mock_rpc_full_keeper_cycle_with_pending_orders() {
    let mock_server = MockServer::start().await;
    let rpc_url = mock_server.uri();

    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .respond_with(|req: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            let method = body["method"].as_str().unwrap_or("");

            match method {
                "simulateTransaction" => {
                    let op = body["params"]["transaction"]["operations"][0].clone();
                    let contract = op["contract_id"].as_str().unwrap_or("");
                    let method_name = op["method"].as_str().unwrap_or("");

                    if contract == "CC6OZUHF3LVO6PNP3V2EB36ORB3YSVYSH3LWD3RFLO4NUO3BYCXSWSYC" {
                        if method_name == "get_order_count" {
                            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                                "jsonrpc": "2.0", "id": 1,
                                "result": 1
                            }))
                        } else if method_name == "get_order_keys" {
                            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                                "jsonrpc": "2.0", "id": 1,
                                "result": {
                                    "vec": [{"bytes": "aabbccdd00112233aabbccdd00112233aabbccdd00112233aabbccdd00112233"}]
                                }
                            }))
                        } else {
                            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                                "jsonrpc": "2.0", "id": 1,
                                "result": 0
                            }))
                        }
                    } else {
                        ResponseTemplate::new(200).set_body_json(serde_json::json!({
                            "jsonrpc": "2.0", "id": 1,
                            "result": 0
                        }))
                    }
                }
                "getAccount" => {
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "jsonrpc": "2.0", "id": 1,
                        "result": {
                            "id": "GAUHMCMUP5FZO5675W3ISZ6E6CNYJGXBUW5WANE2JR4TGAARYCTSCBKI",
                            "sequence": "100",
                            "subentries": 0, "inflationDestination": "", "homeDomain": "",
                            "thresholds": {"low":1,"med":1,"high":1},
                            "signers": [], "data": {}, "balances": []
                        }
                    }))
                }
                "sendTransaction" => {
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "jsonrpc": "2.0", "id": 1,
                        "result": {
                            "status": "PENDING",
                            "hash": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
                        }
                    }))
                }
                "getTransaction" => {
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "jsonrpc": "2.0", "id": 1,
                        "result": {
                            "status": "SUCCESS",
                            "ledger": 50001,
                            "diagnosticEventsXdr": []
                        }
                    }))
                }
                _ => ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "jsonrpc": "2.0", "id": 1,
                    "error": {"code": -1, "message": "unknown method"}
                })),
            }
        })
        .mount(&mock_server)
        .await;

    let config = test_config(&rpc_url);
    let state = Arc::new(AppState::new(config));

    {
        let mut cache = state.price_cache.write().await;
        cache
            .prices
            .insert("TUSDC".to_string(), test_cached_price());
    }

    let result = oracle::keeper_loop::run_keeper_cycle(Arc::clone(&state)).await;

    assert!(
        result.is_ok(),
        "keeper cycle should succeed: {:?}",
        result.err()
    );
    let summary = result.unwrap();
    assert_eq!(summary.orders_executed, 1);
}

#[test]
fn test_scval_encoding_matches_ts_keeper_pattern() {
    use oracle::chain::scval::encode_signed_price;

    let price = test_cached_price();
    let scval = encode_signed_price(&price).unwrap();

    match scval {
        stellar_xdr::ScVal::Map(Some(map)) => {
            let entries: Vec<_> = map.0.iter().collect();
            assert_eq!(entries.len(), 7, "SignedPrice map must have 7 fields");

            let keys: Vec<String> = entries
                .iter()
                .map(|e| match &e.key {
                    stellar_xdr::ScVal::Symbol(s) => {
                        String::from_utf8_lossy(s.as_ref()).to_string()
                    }
                    _ => panic!("expected symbol key"),
                })
                .collect();

            let expected = vec![
                "keeper_index",
                "ledger_seq",
                "max_price",
                "min_price",
                "signature",
                "timestamp",
                "token",
            ];
            assert_eq!(
                keys, expected,
                "keys must match TS keeper buildSignedPriceScVal order"
            );

            assert!(matches!(entries[0].val, stellar_xdr::ScVal::U32(0)));
            assert!(matches!(entries[1].val, stellar_xdr::ScVal::U32(12345)));
            assert!(matches!(entries[2].val, stellar_xdr::ScVal::I128(_)));
            assert!(matches!(entries[3].val, stellar_xdr::ScVal::I128(_)));
            assert!(matches!(entries[4].val, stellar_xdr::ScVal::Bytes(_)));
            assert!(matches!(
                entries[5].val,
                stellar_xdr::ScVal::U64(1718400000)
            ));
            assert!(matches!(entries[6].val, stellar_xdr::ScVal::Address(_)));
        }
        _ => panic!("expected ScVal::Map with 7 entries"),
    }
}
