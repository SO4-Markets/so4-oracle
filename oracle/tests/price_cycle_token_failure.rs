/// Tests for issue #395: individual token failure must be recorded and skipped
/// while all remaining tokens continue to be processed in the same cycle.
///
/// Relevant code: oracle/src/price_loop.rs — the token loop in `run_price_cycle`
/// continues on `Err` from `build_cached_price`, increments `tokens_failed`,
/// and calls `record_error`, never aborting the iteration.
use std::sync::Arc;
use std::time::Duration;

use shared_config::TokenConfig;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

use oracle::config::{Config, Network, PriceFeedConfig, SecretString};
use oracle::price_loop::run_price_cycle;
use oracle::state::AppState;

fn ledger_ok_response() -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": { "id": "abc", "sequence": 12345, "protocolVersion": "22" }
    })
}

fn fixed_token(symbol: &str, address: &str) -> TokenConfig {
    TokenConfig {
        symbol: symbol.to_string(),
        display_symbol: Some(symbol.to_string()),
        stellar_address: address.to_string(),
        sources: vec!["fixed".to_string()],
        fixed_price: Some("1000000000000000000000000000000".to_string()),
        binance_symbol: None,
        coinbase_symbol: None,
        pyth_feed_id: None,
        min_sources: 1,
        max_deviation_bps: 100,
        stale_after_seconds: 60,
        submit_threshold_bps: 10,
        min: 0.0,
        max: 0.0,
        sources_used: vec![],
    }
}

fn bad_token(symbol: &str, address: &str) -> TokenConfig {
    TokenConfig {
        symbol: symbol.to_string(),
        display_symbol: Some(symbol.to_string()),
        stellar_address: address.to_string(),
        // Use an unsupported source so build_cached_price returns Err.
        sources: vec!["unsupported_source".to_string()],
        fixed_price: None,
        binance_symbol: None,
        coinbase_symbol: None,
        pyth_feed_id: None,
        min_sources: 1,
        max_deviation_bps: 100,
        stale_after_seconds: 60,
        submit_threshold_bps: 10,
        min: 0.0,
        max: 0.0,
        sources_used: vec![],
    }
}

// lookup_key() returns stellar_address.to_lowercase(), so use this helper in assertions.
fn cache_key(address: &str) -> String {
    address.to_lowercase()
}

fn test_state(rpc_url: &str, tokens: Vec<TokenConfig>) -> Arc<AppState> {
    let config = Arc::new(Config {
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        network: Network::Testnet,
        network_passphrase: "Test SDF Network ; September 2015".to_string(),
        stellar_rpc_url: rpc_url.to_string(),
        horizon_url: "http://localhost:0".to_string(),
        oracle_contract_id: "CORACLE".to_string(),
        role_store_contract_id: "CROLE".to_string(),
        data_store_contract_id: "CDATA".to_string(),
        order_handler_contract_id: "CORDER".to_string(),
        deposit_handler_contract_id: "CDEPOSIT".to_string(),
        withdrawal_handler_contract_id: "CWITHDRAW".to_string(),
        reader_contract_id: "CREADER".to_string(),
        keeper_private_key: SecretString::new(
            "1111111111111111111111111111111111111111111111111111111111111111".to_string(),
        ),
        keeper_secret_key: SecretString::new("SSECRET".to_string()),
        keeper_account_id: "GACCOUNT".to_string(),
        keeper_index: 0,
        admin_api_token: None,
        min_keeper_balance_xlm: 0.0,
        price_loop_interval: Duration::from_millis(1000),
        keeper_loop_interval: Duration::from_millis(1000),
        price_feed: PriceFeedConfig { tokens },
    });
    Arc::new(AppState::new(config))
}

#[tokio::test]
async fn token_failure_does_not_abort_remaining_tokens() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_ok_response()))
        .mount(&mock_server)
        .await;

    // First token uses an unsupported source — will fail.
    // Second token uses "fixed" — will succeed.
    let tokens = vec![
        bad_token(
            "FAILTOKEN",
            "CFAILADDR111111111111111111111111111111111111111111111111",
        ),
        fixed_token(
            "USDC",
            "CBAN5YU3KRDKPTQ2H76D6S7HQFPRBGUD524F65BUM2RQCITPTRLKWKES",
        ),
    ];
    let state = test_state(&mock_server.uri(), tokens);

    run_price_cycle(Arc::clone(&state)).await;

    let cache = state.price_cache.read().await;
    // lookup_key() lowercases the stellar_address — match that here.
    let usdc_key = cache_key("CBAN5YU3KRDKPTQ2H76D6S7HQFPRBGUD524F65BUM2RQCITPTRLKWKES");
    assert!(
        cache.prices.contains_key(&usdc_key),
        "USDC price should be cached after a partial failure"
    );

    let fail_key = cache_key("CFAILADDR1111111111111111111111111111111111111111111111111");
    assert!(
        !cache.prices.contains_key(&fail_key),
        "FAILTOKEN must not appear in cache"
    );
}

#[tokio::test]
async fn failed_token_error_is_recorded_in_failures() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_ok_response()))
        .mount(&mock_server)
        .await;

    let tokens = vec![
        bad_token(
            "BADTOK",
            "CBADADDR11111111111111111111111111111111111111111111111111",
        ),
        fixed_token(
            "USDC",
            "CBAN5YU3KRDKPTQ2H76D6S7HQFPRBGUD524F65BUM2RQCITPTRLKWKES",
        ),
    ];
    let state = test_state(&mock_server.uri(), tokens);

    run_price_cycle(Arc::clone(&state)).await;

    let failures = state.failures.lock().await;
    let entries: Vec<_> = failures.iter().collect();
    assert!(
        !entries.is_empty(),
        "a failure entry should be recorded for the bad token"
    );
    assert!(
        entries
            .iter()
            .any(|f| f.operation.contains("BADTOK") || f.operation.contains("price:")),
        "failure entry should reference the failed token"
    );
}

#[tokio::test]
async fn multiple_bad_tokens_each_recorded_as_separate_failure() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_ok_response()))
        .mount(&mock_server)
        .await;

    let tokens = vec![
        bad_token(
            "BAD_X",
            "CBADX111111111111111111111111111111111111111111111111111111",
        ),
        bad_token(
            "BAD_Y",
            "CBADY111111111111111111111111111111111111111111111111111111",
        ),
    ];
    let state = test_state(&mock_server.uri(), tokens);

    run_price_cycle(Arc::clone(&state)).await;

    let failures = state.failures.lock().await;
    let entries: Vec<_> = failures.iter().collect();
    assert!(
        entries.len() >= 2,
        "each failed token must produce its own failure record; got {}",
        entries.len()
    );
}

#[tokio::test]
async fn single_bad_token_alone_leaves_cache_empty() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_ok_response()))
        .mount(&mock_server)
        .await;

    let tokens = vec![bad_token(
        "ONLY_BAD",
        "CBADONLY111111111111111111111111111111111111111111111111111",
    )];
    let state = test_state(&mock_server.uri(), tokens);

    run_price_cycle(Arc::clone(&state)).await;

    let cache = state.price_cache.read().await;
    assert!(
        cache.prices.is_empty(),
        "cache must be empty when the only token fails"
    );
}

#[tokio::test]
async fn failed_token_does_not_overwrite_previous_good_cache_entry() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_ok_response()))
        .mount(&mock_server)
        .await;

    // First cycle: USDC succeeds.
    let tokens = vec![fixed_token(
        "USDC",
        "CBAN5YU3KRDKPTQ2H76D6S7HQFPRBGUD524F65BUM2RQCITPTRLKWKES",
    )];
    let state = test_state(&mock_server.uri(), tokens);
    run_price_cycle(Arc::clone(&state)).await;

    let usdc_key = cache_key("CBAN5YU3KRDKPTQ2H76D6S7HQFPRBGUD524F65BUM2RQCITPTRLKWKES");
    let first_price = state
        .price_cache
        .read()
        .await
        .prices
        .get(&usdc_key)
        .cloned();
    assert!(
        first_price.is_some(),
        "USDC must be cached after first cycle"
    );
}

#[tokio::test]
async fn cycle_status_not_stuck_running_after_partial_failure() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_ok_response()))
        .mount(&mock_server)
        .await;

    let tokens = vec![
        bad_token(
            "FAILME",
            "CFAILME11111111111111111111111111111111111111111111111111111",
        ),
        fixed_token(
            "USDC",
            "CBAN5YU3KRDKPTQ2H76D6S7HQFPRBGUD524F65BUM2RQCITPTRLKWKES",
        ),
    ];
    let state = test_state(&mock_server.uri(), tokens);

    run_price_cycle(Arc::clone(&state)).await;

    let status = state.cycle_status.read().await;
    assert!(
        !status.price_cycle_running,
        "price_cycle_running must be false after cycle completes, even with partial failure"
    );
    assert!(
        status.last_price_cycle_at.is_some(),
        "last_price_cycle_at must be recorded after cycle completes"
    );
}

#[tokio::test]
async fn all_good_tokens_processed_when_one_fails() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_ok_response()))
        .mount(&mock_server)
        .await;

    let tokens = vec![
        bad_token(
            "TOKEN_A",
            "CADDRA11111111111111111111111111111111111111111111111111111",
        ),
        fixed_token(
            "TOKEN_B",
            "CBAN5YU3KRDKPTQ2H76D6S7HQFPRBGUD524F65BUM2RQCITPTRLKWKES",
        ),
        fixed_token(
            "TOKEN_C",
            "CADDRC11111111111111111111111111111111111111111111111111111",
        ),
    ];
    let state = test_state(&mock_server.uri(), tokens);

    run_price_cycle(Arc::clone(&state)).await;

    let cache = state.price_cache.read().await;
    assert_eq!(
        cache.prices.len(),
        2,
        "both good tokens must be cached; only the bad one omitted"
    );
}

#[tokio::test]
async fn good_tokens_have_non_zero_price_after_partial_failure() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_ok_response()))
        .mount(&mock_server)
        .await;

    let tokens = vec![
        bad_token(
            "SKIPME",
            "CSKIPME111111111111111111111111111111111111111111111111111",
        ),
        fixed_token(
            "USDC",
            "CBAN5YU3KRDKPTQ2H76D6S7HQFPRBGUD524F65BUM2RQCITPTRLKWKES",
        ),
    ];
    let state = test_state(&mock_server.uri(), tokens);

    run_price_cycle(Arc::clone(&state)).await;

    let cache = state.price_cache.read().await;
    let usdc_key = cache_key("CBAN5YU3KRDKPTQ2H76D6S7HQFPRBGUD524F65BUM2RQCITPTRLKWKES");
    let price = cache.prices.get(&usdc_key).expect("USDC must be cached");
    assert!(price.min > 0, "cached min price must be positive");
    assert!(price.max > 0, "cached max price must be positive");
}

#[tokio::test]
async fn metrics_price_cycle_count_increments_after_partial_failure() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_ok_response()))
        .mount(&mock_server)
        .await;

    let tokens = vec![
        bad_token(
            "FAILME2",
            "CFAILME21111111111111111111111111111111111111111111111111111",
        ),
        fixed_token(
            "USDC",
            "CBAN5YU3KRDKPTQ2H76D6S7HQFPRBGUD524F65BUM2RQCITPTRLKWKES",
        ),
    ];
    let state = test_state(&mock_server.uri(), tokens);

    run_price_cycle(Arc::clone(&state)).await;

    let resp = state.metrics.to_response();
    assert_eq!(
        resp.price_cycle_count, 1,
        "price_cycle_count must be 1 after one cycle, even with a partial failure"
    );
}

#[tokio::test]
async fn two_cycles_with_partial_failure_count_both_in_metrics() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_ok_response()))
        .mount(&mock_server)
        .await;

    let tokens = vec![
        bad_token(
            "ALWAYS_BAD",
            "CBAD22222222222222222222222222222222222222222222222222222",
        ),
        fixed_token(
            "USDC",
            "CBAN5YU3KRDKPTQ2H76D6S7HQFPRBGUD524F65BUM2RQCITPTRLKWKES",
        ),
    ];
    let state = test_state(&mock_server.uri(), tokens);

    run_price_cycle(Arc::clone(&state)).await;
    run_price_cycle(Arc::clone(&state)).await;

    let resp = state.metrics.to_response();
    assert_eq!(
        resp.price_cycle_count, 2,
        "both cycles must be counted even though a token failed in each"
    );
}

#[tokio::test]
async fn failure_operation_field_contains_token_symbol() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_ok_response()))
        .mount(&mock_server)
        .await;

    let tokens = vec![bad_token(
        "XLM",
        "CXLM1111111111111111111111111111111111111111111111111111111",
    )];
    let state = test_state(&mock_server.uri(), tokens);

    run_price_cycle(Arc::clone(&state)).await;

    let failures = state.failures.lock().await;
    let entries: Vec<_> = failures.iter().collect();
    assert!(!entries.is_empty(), "at least one failure must be recorded");
    let has_symbol = entries.iter().any(|f| f.operation.contains("XLM"));
    assert!(
        has_symbol,
        "failure operation field must contain the token symbol; got: {:?}",
        entries.iter().map(|f| &f.operation).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn token_failure_count_reflected_in_metrics_after_partial_cycle() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_ok_response()))
        .mount(&mock_server)
        .await;

    // One bad token (unsupported source) and one good token (fixed source).
    let tokens = vec![
        bad_token(
            "FAIL1",
            "CFAIL111111111111111111111111111111111111111111111111111111",
        ),
        fixed_token(
            "OK1",
            "CBAN5YU3KRDKPTQ2H76D6S7HQFPRBGUD524F65BUM2RQCITPTRLKWKES",
        ),
    ];
    let state = test_state(&mock_server.uri(), tokens);

    run_price_cycle(Arc::clone(&state)).await;

    let resp = state.metrics.to_response();
    assert_eq!(
        resp.token_fetch_failures, 1,
        "exactly one token failed in this cycle"
    );
    assert_eq!(
        resp.token_fetch_ok, 1,
        "exactly one token succeeded in this cycle"
    );
}
