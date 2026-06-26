/// Tests for issue #395: individual token failure must be recorded and skipped
/// while all remaining tokens continue to be processed.
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
        bad_token("FAILTOKEN", "CFAILADDR111111111111111111111111111111111111111111111111"),
        fixed_token("USDC", "CBAN5YU3KRDKPTQ2H76D6S7HQFPRBGUD524F65BUM2RQCITPTRLKWKES"),
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
        bad_token("BADTOK", "CBADADDR11111111111111111111111111111111111111111111111111"),
        fixed_token("USDC", "CBAN5YU3KRDKPTQ2H76D6S7HQFPRBGUD524F65BUM2RQCITPTRLKWKES"),
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
async fn all_good_tokens_processed_when_one_fails() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_ok_response()))
        .mount(&mock_server)
        .await;

    let tokens = vec![
        bad_token("TOKEN_A", "CADDRA11111111111111111111111111111111111111111111111111111"),
        fixed_token("TOKEN_B", "CBAN5YU3KRDKPTQ2H76D6S7HQFPRBGUD524F65BUM2RQCITPTRLKWKES"),
        fixed_token("TOKEN_C", "CADDRC11111111111111111111111111111111111111111111111111111"),
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
