use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use wiremock::{MockServer, Request, ResponseTemplate};

mod common;

use common::{fixed_token, test_config_with_tokens};
use oracle::state::{AppState, CachedPrice};

/// Cache key for `test_cached_price()`/`fresh_cached_price()` — must equal the
/// matching token's `lookup_key()`, which lowercases the symbol.
const TUSDC_KEY: &str = "tusdc";

/// Token config matching `TUSDC_KEY`, for keeper-cycle tests that rely on the
/// per-token freshness filter in `execute_keeper_cycle` (#530) finding a match.
fn test_token() -> shared_config::TokenConfig {
    fixed_token("TUSDC", "")
}

fn test_cached_price() -> CachedPrice {
    CachedPrice {
        token_address: "GAUHMCMUP5FZO5675W3ISZ6E6CNYJGXBUW5WANE2JR4TGAARYCTSCBKI".to_string(),
        symbol: "TUSDC".to_string(),
        display_symbol: "USDC".to_string(),
        keeper_index: 0,
        min: 1_000_000_000_000_000_000_000_000_000_000,
        max: 1_000_000_000_000_000_000_000_000_000_000,
        median: 1_000_000_000_000_000_000_000_000_000_000,
        timestamp: 1718400000,
        ledger_seq: 12345,
        sources_used: vec!["fixed".to_string()],
        signature: "00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000".to_string(),
    }
}

/// Same as `test_cached_price()` but timestamped "now" so it survives the
/// per-token freshness filter in `execute_keeper_cycle` (#530).
fn fresh_cached_price() -> CachedPrice {
    CachedPrice {
        timestamp: oracle::current_timestamp_secs(),
        ..test_cached_price()
    }
}

/// Deterministic 64-hex transaction hash for the `nth` submission a test's mock
/// RPC answers, so `getTransaction` can be routed per submission.
fn submission_hash(nth: usize) -> String {
    format!("{nth:0>64}")
}

#[tokio::test]
async fn empty_price_cache_returns_error_and_no_rpc_calls() {
    let mock_server = MockServer::start().await;
    let rpc_url = mock_server.uri();

    let config = test_config_with_tokens(&rpc_url, "http://127.0.0.1:9", vec![test_token()]);
    let state = Arc::new(AppState::new(config));

    // Don't populate the price cache — leave it empty.

    let result = oracle::keeper_loop::run_keeper_cycle(state).await;

    assert!(result.is_err(), "expected Err for empty price cache");
    assert_eq!(
        result.unwrap_err(),
        "No fresh prices available in cache (cache size: 0, all stale)",
        "error must match the exact string in execute_keeper_cycle"
    );

    // Verify no RPC calls were made — the function short-circuits before any network I/O.
    let requests = mock_server.received_requests().await;
    assert!(
        requests.unwrap_or_default().is_empty(),
        "no RPC calls should be made when cache is empty"
    );
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

    let config = test_config_with_tokens(&rpc_url, "http://127.0.0.1:9", vec![test_token()]);
    let state = Arc::new(AppState::new(config));

    {
        let mut cache = state.price_cache.write().await;
        cache
            .prices
            .insert(TUSDC_KEY.to_string(), fresh_cached_price());
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

    let config = test_config_with_tokens(&rpc_url, "http://127.0.0.1:9", vec![test_token()]);
    let state = Arc::new(AppState::new(config));

    {
        let mut cache = state.price_cache.write().await;
        cache
            .prices
            .insert(TUSDC_KEY.to_string(), fresh_cached_price());
    }

    let result = oracle::keeper_loop::run_keeper_cycle(Arc::clone(&state)).await;

    // A failing pending-work lookup is logged and skipped rather than aborting
    // the cycle, so the keeper keeps running across transient RPC outages.
    let summary = result.expect("keeper cycle should survive an RPC failure");
    assert_eq!(summary.orders_executed, 0);
    assert_eq!(summary.deposits_executed, 0);
    assert_eq!(summary.withdrawals_executed, 0);
    assert_eq!(summary.errors, 0);
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

    let config = test_config_with_tokens(&rpc_url, "http://127.0.0.1:9", vec![test_token()]);
    let state = Arc::new(AppState::new(config));

    {
        let mut cache = state.price_cache.write().await;
        cache
            .prices
            .insert(TUSDC_KEY.to_string(), fresh_cached_price());
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

#[tokio::test]
async fn keeper_cycle_retries_transient_get_account_sequence_failure() {
    let mock_server = MockServer::start().await;
    let rpc_url = mock_server.uri();
    let get_account_attempts = Arc::new(AtomicUsize::new(0));
    let get_account_attempts_for_mock = Arc::clone(&get_account_attempts);

    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .respond_with(move |req: &Request| {
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
                    let attempt = get_account_attempts_for_mock.fetch_add(1, Ordering::SeqCst);
                    if attempt == 0 {
                        ResponseTemplate::new(500).set_body_string("transient rpc error")
                    } else {
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

    let config = test_config_with_tokens(&rpc_url, "http://127.0.0.1:9", vec![test_token()]);
    let state = Arc::new(AppState::new(config));

    {
        let mut cache = state.price_cache.write().await;
        cache
            .prices
            .insert(TUSDC_KEY.to_string(), fresh_cached_price());
    }

    let result = oracle::keeper_loop::run_keeper_cycle(Arc::clone(&state)).await;

    assert!(
        result.is_ok(),
        "keeper cycle should recover from transient getAccount failure: {:?}",
        result.err()
    );
    assert!(
        get_account_attempts.load(Ordering::SeqCst) >= 2,
        "getAccount should have been retried"
    );
    assert_eq!(result.unwrap().orders_executed, 1);
}

#[test]
fn test_encode_signed_price_field_order_and_types() {
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
                "keys must match encode_signed_price's documented field order"
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

#[tokio::test]
async fn keeper_cycle_freezes_order_when_budget_exceeded_and_freeze_succeeds() {
    let mock_server = MockServer::start().await;
    let rpc_url = mock_server.uri();

    // sendTransaction hands back a distinct hash per submission so getTransaction
    // can tell set_prices, execute_order and freeze_order apart.
    let submissions = Arc::new(AtomicUsize::new(0));

    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .respond_with(move |req: &Request| {
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
                    let nth = submissions.fetch_add(1, Ordering::SeqCst);
                    // 0 = set_prices, 1 = execute_order, 2 = freeze_order
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "jsonrpc": "2.0", "id": 1,
                        "result": { "status": "PENDING", "hash": submission_hash(nth) }
                    }))
                }
                "getTransaction" => {
                    let body_obj: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
                    let tx_hash = body_obj["params"]["hash"].as_str().unwrap_or("");

                    // The execute_order submission (the second one) fails with a
                    // budget overrun; that is what drives the keeper into the
                    // freeze path. set_prices and freeze_order both succeed.
                    if tx_hash == submission_hash(1) {
                        ResponseTemplate::new(200).set_body_json(serde_json::json!({
                            "jsonrpc": "2.0", "id": 1,
                            "result": {
                                "status": "FAILED",
                                "ledger": 50001,
                                "diagnosticEventsXdr": ["HostError: Error(Budget, ExceededLimit)"]
                            }
                        }))
                    } else {
                        ResponseTemplate::new(200).set_body_json(serde_json::json!({
                            "jsonrpc": "2.0", "id": 1,
                            "result": {
                                "status": "SUCCESS",
                                "ledger": 50002,
                                "diagnosticEventsXdr": []
                            }
                        }))
                    }
                }
                _ => ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "jsonrpc": "2.0", "id": 1,
                    "error": {"code": -1, "message": "unknown method"}
                })),
            }
        })
        .mount(&mock_server)
        .await;

    let config = test_config_with_tokens(&rpc_url, "http://127.0.0.1:9", vec![test_token()]);
    let state = Arc::new(AppState::new(config));

    {
        let mut cache = state.price_cache.write().await;
        cache
            .prices
            .insert(TUSDC_KEY.to_string(), fresh_cached_price());
    }

    let result = oracle::keeper_loop::run_keeper_cycle(Arc::clone(&state)).await;

    assert!(
        result.is_ok(),
        "keeper cycle should handle budget exceeded with freeze: {:?}",
        result.err()
    );
    let summary = result.unwrap();
    assert_eq!(
        summary.errors, 1,
        "should record 1 error for the budget exceeded order"
    );
}

#[tokio::test]
async fn keeper_cycle_rejects_non_hex_order_key() {
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
                            // Return a non-hex key: contains 'Z' and 'G' which are not valid hex
                            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                                "jsonrpc": "2.0", "id": 1,
                                "result": {
                                    "vec": [{"bytes": "not-valid-hex-ZGZGZGZGZG"}]
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
                "getAccount" => ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "jsonrpc": "2.0", "id": 1,
                    "result": {
                        "id": "GAUHMCMUP5FZO5675W3ISZ6E6CNYJGXBUW5WANE2JR4TGAARYCTSCBKI",
                        "sequence": "100",
                        "subentries": 0, "inflationDestination": "", "homeDomain": "",
                        "thresholds": {"low":1,"med":1,"high":1},
                        "signers": [], "data": {}, "balances": []
                    }
                })),
                "sendTransaction" => ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "jsonrpc": "2.0", "id": 1,
                    "result": {
                        "status": "PENDING",
                        "hash": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
                    }
                })),
                // set_prices runs before any order is executed, so its
                // submission has to confirm for the cycle to reach the bad key.
                "getTransaction" => ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "jsonrpc": "2.0", "id": 1,
                    "result": {
                        "status": "SUCCESS",
                        "ledger": 50001,
                        "diagnosticEventsXdr": []
                    }
                })),
                _ => ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "jsonrpc": "2.0", "id": 1,
                    "error": {"code": -1, "message": "unknown method"}
                })),
            }
        })
        .mount(&mock_server)
        .await;

    let config = test_config_with_tokens(&rpc_url, "http://127.0.0.1:9", vec![test_token()]);
    let state = Arc::new(AppState::new(config));

    {
        let mut cache = state.price_cache.write().await;
        cache
            .prices
            .insert(TUSDC_KEY.to_string(), fresh_cached_price());
    }

    let result = oracle::keeper_loop::run_keeper_cycle(Arc::clone(&state)).await;

    assert!(
        result.is_ok(),
        "keeper cycle should complete even with invalid key: {:?}",
        result.err()
    );
    let summary = result.unwrap();
    assert_eq!(
        summary.errors, 1,
        "should record 1 error for the non-hex key"
    );
    assert_eq!(summary.orders_executed, 0, "should not execute any orders");
}

// ── #516: get_account_sequence error branches ─────────────────────────────────

/// Helper: state with one cached price pointing at the mock RPC.
async fn state_with_price(rpc_url: &str) -> Arc<AppState> {
    let config = test_config_with_tokens(rpc_url, "http://127.0.0.1:9", vec![test_token()]);
    let state = Arc::new(AppState::new(config));
    // Pre-populate the price cache so the cycle reaches get_account_sequence.
    state
        .price_cache
        .write()
        .await
        .prices
        .insert(TUSDC_KEY.to_string(), fresh_cached_price());
    state
}

/// Returns a wiremock handler that answers simulateTransaction with count=0
/// (no pending work) and delegates getAccount to the supplied closure.
macro_rules! mount_with_get_account {
    ($server:expr, $get_account_response:expr) => {{
        let get_account_json = $get_account_response;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(move |req: &Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
                match body["method"].as_str().unwrap_or("") {
                    "simulateTransaction" => {
                        // Return count=0 so we reach set_prices_on_chain → get_account_sequence.
                        // But we need pending work to reach get_account_sequence, so return 1
                        // for get_order_count and a valid key for get_order_keys.
                        let op = body["params"]["transaction"]["operations"][0].clone();
                        let method_name = op["method"].as_str().unwrap_or("");
                        if method_name == "get_order_count" {
                            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                                "jsonrpc": "2.0", "id": 1, "result": 1
                            }))
                        } else if method_name == "get_order_keys" {
                            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                                "jsonrpc": "2.0", "id": 1,
                                "result": {"vec": [{"bytes": "aabbccdd00112233aabbccdd00112233aabbccdd00112233aabbccdd00112233"}]}
                            }))
                        } else {
                            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                                "jsonrpc": "2.0", "id": 1, "result": 0
                            }))
                        }
                    }
                    "getAccount" => ResponseTemplate::new(200)
                        .set_body_json(get_account_json.clone()),
                    _ => ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "jsonrpc": "2.0", "id": 1,
                        "error": {"code": -1, "message": "unexpected method"}
                    })),
                }
            })
            .mount(&$server)
            .await;
    }};
}

#[tokio::test]
async fn get_account_sequence_rpc_error_field_propagates() {
    let server = MockServer::start().await;
    mount_with_get_account!(
        server,
        serde_json::json!({
            "jsonrpc": "2.0", "id": 1,
            "error": {"code": -32000, "message": "account not found"}
        })
    );

    let state = state_with_price(&server.uri()).await;
    let result = oracle::keeper_loop::run_keeper_cycle(state).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("getAccount error"));
}

#[tokio::test]
async fn get_account_sequence_missing_sequence_field_propagates() {
    let server = MockServer::start().await;
    mount_with_get_account!(
        server,
        serde_json::json!({
            "jsonrpc": "2.0", "id": 1,
            "result": {"id": "GACCOUNT"}
            // "sequence" key intentionally absent
        })
    );

    let state = state_with_price(&server.uri()).await;
    let result = oracle::keeper_loop::run_keeper_cycle(state).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("Missing sequence"));
}

#[tokio::test]
async fn get_account_sequence_non_numeric_sequence_propagates() {
    let server = MockServer::start().await;
    mount_with_get_account!(
        server,
        serde_json::json!({
            "jsonrpc": "2.0", "id": 1,
            "result": {"id": "GACCOUNT", "sequence": "not-a-number"}
        })
    );

    let state = state_with_price(&server.uri()).await;
    let result = oracle::keeper_loop::run_keeper_cycle(state).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("failed to parse sequence"));
}

// ── #516: simulate_contract_call error branches ───────────────────────────────

/// Mount a mock that returns the given response for every simulateTransaction
/// call (used to test the three failure shapes of simulate_contract_call).
macro_rules! mount_simulate_error {
    ($server:expr, $simulate_response:expr) => {{
        let sim_json = $simulate_response;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(move |req: &Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
                match body["method"].as_str().unwrap_or("") {
                    "simulateTransaction" => {
                        ResponseTemplate::new(200).set_body_json(sim_json.clone())
                    }
                    _ => ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "jsonrpc": "2.0", "id": 1,
                        "error": {"code": -1, "message": "unexpected method"}
                    })),
                }
            })
            .mount(&$server)
            .await;
    }};
}

#[tokio::test]
async fn simulate_contract_call_rpc_error_field_is_contained() {
    let server = MockServer::start().await;
    mount_simulate_error!(
        server,
        serde_json::json!({
            "jsonrpc": "2.0", "id": 1,
            "error": {"code": -32600, "message": "contract execution failed"}
        })
    );

    let state = state_with_price(&server.uri()).await;
    let result = oracle::keeper_loop::run_keeper_cycle(state).await;
    // The simulation error is warned about and the affected work type skipped;
    // it must not take the whole cycle down.
    let summary = result.expect("simulation errors must not abort the cycle");
    assert_eq!(summary.orders_executed, 0);
    assert_eq!(summary.errors, 0);
}

#[tokio::test]
async fn simulate_contract_call_missing_result_field_is_contained() {
    let server = MockServer::start().await;
    // A 200 response whose body has neither "result" nor "error".
    mount_simulate_error!(
        server,
        serde_json::json!({
            "jsonrpc": "2.0", "id": 1
            // "result" key intentionally absent
        })
    );

    let state = state_with_price(&server.uri()).await;
    let result = oracle::keeper_loop::run_keeper_cycle(state).await;
    let summary = result.expect("a malformed simulate response must not abort the cycle");
    assert_eq!(summary.orders_executed, 0);
    assert_eq!(summary.errors, 0);
}

// ── #803: any repeated execute_order failure (not just budget-exceeded) is
//         eventually abandoned so it can't burn a keeper fee every cycle ─────

/// hex key returned by get_order_keys in the tests below.
const REPEAT_FAIL_KEY: &str =
    "aabbccdd00112233aabbccdd00112233aabbccdd00112233aabbccdd00112233";

/// Parse the `nth` submission index back out of a `submission_hash(nth)`.
fn submission_index(hash: &str) -> usize {
    hash.trim_start_matches('0').parse().unwrap_or(0)
}

/// wiremock handler: one pending order whose `execute_order` submission always
/// fails on-chain with a NON-budget diagnostic; `set_prices` and `freeze_order`
/// succeed. `execute_order` is always the odd-numbered submission of a cycle
/// (set_prices is 0/even, freeze — when attempted — comes after).
fn mount_order_exec_always_fails(mock_server_uri_submissions: Arc<AtomicUsize>) -> impl Fn(&Request) -> ResponseTemplate + Send + Sync {
    move |req: &Request| {
        let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
        match body["method"].as_str().unwrap_or("") {
            "simulateTransaction" => {
                let op = body["params"]["transaction"]["operations"][0].clone();
                let contract = op["contract_id"].as_str().unwrap_or("");
                let method_name = op["method"].as_str().unwrap_or("");
                if contract == "CC6OZUHF3LVO6PNP3V2EB36ORB3YSVYSH3LWD3RFLO4NUO3BYCXSWSYC" {
                    if method_name == "get_order_count" {
                        ResponseTemplate::new(200).set_body_json(serde_json::json!({
                            "jsonrpc": "2.0", "id": 1, "result": 1
                        }))
                    } else if method_name == "get_order_keys" {
                        ResponseTemplate::new(200).set_body_json(serde_json::json!({
                            "jsonrpc": "2.0", "id": 1,
                            "result": { "vec": [{"bytes": REPEAT_FAIL_KEY}] }
                        }))
                    } else {
                        ResponseTemplate::new(200).set_body_json(serde_json::json!({
                            "jsonrpc": "2.0", "id": 1, "result": 0
                        }))
                    }
                } else {
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "jsonrpc": "2.0", "id": 1, "result": 0
                    }))
                }
            }
            "getAccount" => ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0", "id": 1,
                "result": {
                    "id": "GAUHMCMUP5FZO5675W3ISZ6E6CNYJGXBUW5WANE2JR4TGAARYCTSCBKI",
                    "sequence": "100",
                    "subentries": 0, "inflationDestination": "", "homeDomain": "",
                    "thresholds": {"low":1,"med":1,"high":1},
                    "signers": [], "data": {}, "balances": []
                }
            })),
            "sendTransaction" => {
                let nth = mock_server_uri_submissions.fetch_add(1, Ordering::SeqCst);
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "jsonrpc": "2.0", "id": 1,
                    "result": { "status": "PENDING", "hash": submission_hash(nth) }
                }))
            }
            "getTransaction" => {
                let body_obj: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
                let tx_hash = body_obj["params"]["hash"].as_str().unwrap_or("");
                // Odd submission of each cycle == execute_order → deterministic
                // non-budget on-chain failure. Everything else succeeds.
                if submission_index(tx_hash) % 2 == 1 {
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "jsonrpc": "2.0", "id": 1,
                        "result": {
                            "status": "FAILED",
                            "ledger": 50001,
                            "diagnosticEventsXdr": ["HostError: Error(Contract, #7) order rejected"]
                        }
                    }))
                } else {
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "jsonrpc": "2.0", "id": 1,
                        "result": { "status": "SUCCESS", "ledger": 50002, "diagnosticEventsXdr": [] }
                    }))
                }
            }
            _ => ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0", "id": 1,
                "error": {"code": -1, "message": "unknown method"}
            })),
        }
    }
}

#[tokio::test]
async fn keeper_cycle_blacklists_order_after_repeated_non_budget_execution_failures() {
    let mock_server = MockServer::start().await;
    let submissions = Arc::new(AtomicUsize::new(0));

    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .respond_with(mount_order_exec_always_fails(Arc::clone(&submissions)))
        .mount(&mock_server)
        .await;

    let config = test_config_with_tokens(&mock_server.uri(), "http://127.0.0.1:9", vec![test_token()]);
    let state = Arc::new(AppState::new(config));
    state
        .price_cache
        .write()
        .await
        .prices
        .insert(TUSDC_KEY.to_string(), fresh_cached_price());

    // One short of the abandon threshold — this cycle's failure crosses it.
    state.execution_failure_counts.lock().await.insert(
        REPEAT_FAIL_KEY.to_string(),
        oracle::state::MAX_CONSECUTIVE_EXECUTION_FAILURES - 1,
    );

    let summary = oracle::keeper_loop::run_keeper_cycle(Arc::clone(&state))
        .await
        .expect("cycle must not abort on a failing order");
    assert_eq!(summary.errors, 1);
    assert_eq!(summary.orders_executed, 0);

    assert!(
        state
            .frozen_order_blacklist
            .lock()
            .await
            .contains_key(REPEAT_FAIL_KEY),
        "a persistently-failing non-budget order must be blacklisted, not retried forever"
    );
    assert!(
        !state
            .execution_failure_counts
            .lock()
            .await
            .contains_key(REPEAT_FAIL_KEY),
        "the failure tally is cleared once the key is abandoned"
    );
}

#[tokio::test]
async fn keeper_cycle_does_not_blacklist_order_on_a_single_non_budget_failure() {
    let mock_server = MockServer::start().await;
    let submissions = Arc::new(AtomicUsize::new(0));

    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .respond_with(mount_order_exec_always_fails(Arc::clone(&submissions)))
        .mount(&mock_server)
        .await;

    let config = test_config_with_tokens(&mock_server.uri(), "http://127.0.0.1:9", vec![test_token()]);
    let state = Arc::new(AppState::new(config));
    state
        .price_cache
        .write()
        .await
        .prices
        .insert(TUSDC_KEY.to_string(), fresh_cached_price());

    let summary = oracle::keeper_loop::run_keeper_cycle(Arc::clone(&state))
        .await
        .expect("cycle must not abort on a failing order");
    assert_eq!(summary.errors, 1);

    assert!(
        !state
            .frozen_order_blacklist
            .lock()
            .await
            .contains_key(REPEAT_FAIL_KEY),
        "one failure is not enough to abandon an order"
    );
    assert_eq!(
        state
            .execution_failure_counts
            .lock()
            .await
            .get(REPEAT_FAIL_KEY)
            .copied(),
        Some(1),
        "the consecutive-failure counter advances by one"
    );
}
