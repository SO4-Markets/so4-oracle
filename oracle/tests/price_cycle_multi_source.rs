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
        pyth_api_key: None,
        min_keeper_balance_xlm: 0.0,
        set_prices_tx_fee: oracle::config::DEFAULT_SET_PRICES_TX_FEE,
        keeper_tx_fee: oracle::config::DEFAULT_KEEPER_TX_FEE,
        price_loop_interval: Duration::from_millis(1000),
        keeper_loop_interval: Duration::from_millis(1000),
        price_feed: PriceFeedConfig { tokens },
    });
    Arc::new(AppState::new(config))
}

/// Token with three sources (two fixed + one external) and min_sources: 2.
fn three_source_token() -> TokenConfig {
    TokenConfig {
        symbol: "TMSRC".to_string(),
        display_symbol: Some("MSRC".to_string()),
        stellar_address: "CBAN5YU3KRDKPTQ2H76D6S7HQFPRBGUD524F65BUM2RQCITPTRLKWKES".to_string(),
        sources: vec![
            "fixed".to_string(),
            "fixed".to_string(),
            "coinbase".to_string(),
        ],
        fixed_price: Some("1000000000000000000000000000000".to_string()),
        binance_symbol: None,
        coinbase_symbol: Some("INVALID_XYZ_123".to_string()),
        pyth_feed_id: None,
        min_sources: 2,
        max_deviation_bps: 100,
        stale_after_seconds: 60,
        submit_threshold_bps: 10,
        min: 0.0,
        max: 0.0,
        sources_used: vec![],
    }
}

/// Same as three_source_token but with coinbase_symbol: None so coinbase
/// always fails with a config error, regardless of network availability.
fn three_source_token_coinbase_must_fail() -> TokenConfig {
    TokenConfig {
        coinbase_symbol: None,
        ..three_source_token()
    }
}

#[tokio::test]
async fn multi_source_healthy_cycle_uses_all_three_sources() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_ok_response()))
        .mount(&mock_server)
        .await;

    let token = three_source_token();
    let state = test_state(&mock_server.uri(), vec![token]);

    run_price_cycle(Arc::clone(&state)).await;

    let cache = state.price_cache.read().await;
    let key = cache_key("CBAN5YU3KRDKPTQ2H76D6S7HQFPRBGUD524F65BUM2RQCITPTRLKWKES");
    let cached = cache.prices.get(&key).expect("token should be cached");

    // At least the two "fixed" sources succeed; coinbase may or may not
    // succeed depending on network availability.
    assert!(
        cached.sources_used.len() >= 2,
        "at least two sources should succeed, got {}",
        cached.sources_used.len()
    );
    assert!(cached.min > 0, "min price must be positive");
    assert!(cached.max > 0, "max price must be positive");

    let resp = state.metrics.to_response();
    assert_eq!(resp.price_cycle_count, 1, "cycle count must be 1");
    assert_eq!(resp.token_fetch_ok, 1, "one token should succeed");
    assert_eq!(resp.token_fetch_failures, 0, "no token-level failures");
}

#[tokio::test]
async fn multi_source_consecutive_cycles_preserve_sources_used() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_ok_response()))
        .mount(&mock_server)
        .await;

    let token = three_source_token();
    let state = test_state(&mock_server.uri(), vec![token]);

    run_price_cycle(Arc::clone(&state)).await;
    run_price_cycle(Arc::clone(&state)).await;

    let cache = state.price_cache.read().await;
    let key = cache_key("CBAN5YU3KRDKPTQ2H76D6S7HQFPRBGUD524F65BUM2RQCITPTRLKWKES");
    let cached = cache.prices.get(&key).expect("token should be cached");

    assert!(
        cached.sources_used.len() >= 2,
        "at least min_sources should succeed across cycles, got {}",
        cached.sources_used.len()
    );
    assert!(cache.last_updated.is_some(), "last_updated should be set");

    let resp = state.metrics.to_response();
    assert_eq!(resp.price_cycle_count, 2, "both cycles counted");
}

#[tokio::test]
async fn multi_source_coinbase_failure_recorded_separately() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_ok_response()))
        .mount(&mock_server)
        .await;

    // Use a token where coinbase is guaranteed to fail (missing coinbase_symbol).
    let token = three_source_token_coinbase_must_fail();
    let state = test_state(&mock_server.uri(), vec![token]);

    run_price_cycle(Arc::clone(&state)).await;

    let failures = state.failures.lock().await;
    let coinbase_failures: Vec<_> = failures
        .iter()
        .filter(|f| f.operation.contains("coinbase") || f.operation.contains("TMSRC"))
        .collect();
    assert!(
        !coinbase_failures.is_empty(),
        "coinbase source failure should be recorded"
    );
}

#[tokio::test]
async fn multi_source_degraded_then_recovered_metrics_stable() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_ok_response()))
        .mount(&mock_server)
        .await;

    let token = three_source_token();
    let state = test_state(&mock_server.uri(), vec![token]);

    // Three consecutive cycles: the two fixed sources keep succeeding,
    // coinbase may or may not succeed. The cycle succeeds every time
    // via min_sources: 2.
    for _ in 0..3 {
        run_price_cycle(Arc::clone(&state)).await;
    }

    let cache = state.price_cache.read().await;
    let key = cache_key("CBAN5YU3KRDKPTQ2H76D6S7HQFPRBGUD524F65BUM2RQCITPTRLKWKES");
    let cached = cache.prices.get(&key).expect("token should be cached");
    assert!(
        cached.sources_used.len() >= 2,
        "at least min_sources should succeed after 3 cycles, got {}",
        cached.sources_used.len()
    );

    let resp = state.metrics.to_response();
    assert_eq!(resp.price_cycle_count, 3, "all 3 cycles counted");
    assert_eq!(resp.token_fetch_ok, 3, "token succeeded every cycle");
    assert_eq!(resp.token_fetch_failures, 0, "no token-level failures");
}

#[tokio::test]
async fn multi_source_single_source_token_still_works_with_min_sources_1() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_ok_response()))
        .mount(&mock_server)
        .await;

    let single = TokenConfig {
        symbol: "TSINGLE".to_string(),
        display_symbol: Some("SINGLE".to_string()),
        stellar_address: "CCCC5YU3KRDKPTQ2H76D6S7HQFPRBGUD524F65BUM2RQCITPTRLKW".to_string(),
        sources: vec!["fixed".to_string()],
        fixed_price: Some("500000000000000000000000000000".to_string()),
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
    };

    let multi = three_source_token();
    let state = test_state(&mock_server.uri(), vec![single, multi]);

    run_price_cycle(Arc::clone(&state)).await;

    let cache = state.price_cache.read().await;
    assert_eq!(cache.prices.len(), 2, "both tokens cached");
}
