use std::sync::Arc;
use wiremock::{MockServer, Request, ResponseTemplate};

mod common;

use common::{fixed_token, test_config_with_tokens};
use oracle::chain::tx_builder::encode_soroban_transaction_data;
use oracle::state::{AppState, CachedPrice};
use stellar_xdr::{Limits, ReadXdr, SorobanTransactionData, TransactionEnvelope, TransactionExt};

const TUSDC_KEY: &str = "tusdc";

fn test_token() -> shared_config::TokenConfig {
    fixed_token("TUSDC", "")
}

fn fresh_cached_price() -> CachedPrice {
    CachedPrice {
        token_address: "GAUHMCMUP5FZO5675W3ISZ6E6CNYJGXBUW5WANE2JR4TGAARYCTSCBKI".to_string(),
        symbol: "TUSDC".to_string(),
        display_symbol: "USDC".to_string(),
        keeper_index: 0,
        min: 1_000_000_000_000_000_000_000_000_000_000,
        max: 1_000_000_000_000_000_000_000_000_000_000,
        median: 1_000_000_000_000_000_000_000_000_000_000,
        timestamp: oracle::current_timestamp_secs(),
        ledger_seq: 12345,
        sources_used: vec!["fixed".to_string()],
        signature: "00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000".to_string(),
    }
}

#[tokio::test]
async fn test_set_prices_attaches_simulated_soroban_transaction_data() {
    let mock_server = MockServer::start().await;
    let rpc_url = mock_server.uri();

    let expected_data = SorobanTransactionData::default();
    let encoded_sim_data = encode_soroban_transaction_data(&expected_data).unwrap();

    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .respond_with(move |req: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            let method = body["method"].as_str().unwrap_or("");

            match method {
                "simulateTransaction" => {
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "result": {
                            "transactionData": encoded_sim_data,
                            "minResourceFee": "1500"
                        }
                    }))
                }
                "getAccount" => {
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 1,
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
                    // Extract transaction and verify it contains TransactionExt::V1(SorobanTransactionData)
                    let signed_xdr = body["params"]["transaction"].as_str().unwrap();
                    use base64::Engine;
                    let decoded_bytes = base64::engine::general_purpose::STANDARD
                        .decode(signed_xdr)
                        .expect("valid base64 envelope");
                    let envelope = TransactionEnvelope::from_xdr(&decoded_bytes, Limits::none())
                        .expect("valid TransactionEnvelope XDR");

                    match envelope {
                        TransactionEnvelope::Tx(v1_env) => {
                            match v1_env.tx.ext {
                                TransactionExt::V1(ref data) => {
                                    assert_eq!(data, &expected_data);
                                }
                                TransactionExt::V0 => {
                                    panic!("expected TransactionExt::V1 with soroban_data, got V0");
                                }
                            }
                        }
                        _ => panic!("expected TransactionEnvelope::Tx"),
                    }

                    ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "result": {
                            "status": "PENDING",
                            "hash": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                        }
                    }))
                }
                "getTransaction" => {
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "result": {
                            "status": "SUCCESS",
                            "ledger": 50001,
                            "diagnosticEventsXdr": []
                        }
                    }))
                }
                _ => ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "error": {"code": -1, "message": "unexpected method"}
                })),
            }
        })
        .mount(&mock_server)
        .await;

    let config = test_config_with_tokens(&rpc_url, "http://127.0.0.1:9", vec![test_token()]);
    let state = Arc::new(AppState::new(config));

    {
        let mut cache = state.price_cache.write().await;
        cache.prices.insert(TUSDC_KEY.to_string(), fresh_cached_price());
    }

    let result = oracle::keeper_loop::run_keeper_cycle(Arc::clone(&state)).await;
    assert!(result.is_ok(), "keeper cycle should succeed: {:?}", result.err());
}

#[tokio::test]
async fn test_set_prices_falls_back_to_v0_when_simulation_has_no_soroban_data() {
    let mock_server = MockServer::start().await;
    let rpc_url = mock_server.uri();

    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .respond_with(|req: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            let method = body["method"].as_str().unwrap_or("");

            match method {
                "simulateTransaction" => {
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "result": 0
                    }))
                }
                "getAccount" => {
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 1,
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
                    let signed_xdr = body["params"]["transaction"].as_str().unwrap();
                    use base64::Engine;
                    let decoded_bytes = base64::engine::general_purpose::STANDARD
                        .decode(signed_xdr)
                        .expect("valid base64 envelope");
                    let envelope = TransactionEnvelope::from_xdr(&decoded_bytes, Limits::none())
                        .expect("valid TransactionEnvelope XDR");

                    match envelope {
                        TransactionEnvelope::Tx(v1_env) => {
                            match v1_env.tx.ext {
                                TransactionExt::V0 => {
                                    // Successfully fell back to V0
                                }
                                TransactionExt::V1(_) => {
                                    panic!("expected TransactionExt::V0 when simulation lacks transactionData");
                                }
                            }
                        }
                        _ => panic!("expected TransactionEnvelope::Tx"),
                    }

                    ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "result": {
                            "status": "PENDING",
                            "hash": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                        }
                    }))
                }
                "getTransaction" => {
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "result": {
                            "status": "SUCCESS",
                            "ledger": 50001,
                            "diagnosticEventsXdr": []
                        }
                    }))
                }
                _ => ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "error": {"code": -1, "message": "unexpected method"}
                })),
            }
        })
        .mount(&mock_server)
        .await;

    let config = test_config_with_tokens(&rpc_url, "http://127.0.0.1:9", vec![test_token()]);
    let state = Arc::new(AppState::new(config));

    {
        let mut cache = state.price_cache.write().await;
        cache.prices.insert(TUSDC_KEY.to_string(), fresh_cached_price());
    }

    let result = oracle::keeper_loop::run_keeper_cycle(Arc::clone(&state)).await;
    assert!(result.is_ok(), "keeper cycle should succeed with fallback: {:?}", result.err());
}
