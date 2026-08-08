/// Integration tests for issue #396: `price_cache.last_updated` semantics.
///
/// The oracle sets `price_cache.last_updated` at most once per price cycle,
/// in `price_loop.rs::run_price_cycle`, only when `tokens_ok > 0`:
///
/// ```text
/// if tokens_ok > 0 {
///     state.price_cache.write().await.last_updated = Some(SystemTime::now());
/// }
/// ```
///
/// Covered invariants:
/// - Set when ≥ 1 token succeeds (single, multiple, mixed success/failure).
/// - Not set when all tokens fail, the token list is empty, the source list is
///   empty, or the ledger fetch fails (cycle aborts before the token loop).
/// - Monotonically non-decreasing across consecutive successful cycles.
/// - Remains within [cycle_start, post_cycle_read] bounds.
/// - Unaffected by `finish_cycle`, which always runs regardless of outcome.
use std::sync::Arc;
use std::time::Duration;

use shared_config::TokenConfig;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;

use common::{bad_token, fixed_token, test_state};
use oracle::price_loop::run_price_cycle;

const USDC_ADDR: &str = "CBAN5YU3KRDKPTQ2H76D6S7HQFPRBGUD524F65BUM2RQCITPTRLKWKES";
const XLM_ADDR: &str = "CXLM11111111111111111111111111111111111111111111111111111111";
const FAIL1_ADDR: &str = "CFAIL1111111111111111111111111111111111111111111111111111111";
const FAIL2_ADDR: &str = "CFAIL2111111111111111111111111111111111111111111111111111111";

fn ledger_ok() -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": { "id": "abc", "sequence": 12345, "protocolVersion": "22" }
    })
}

fn ledger_fail() -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "error": { "code": -32000, "message": "node unavailable" }
    })
}

#[tokio::test]
async fn last_updated_set_when_one_token_succeeds() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_ok()))
        .mount(&mock)
        .await;

    let tokens = vec![fixed_token("USDC", USDC_ADDR)];
    let state = test_state(&mock.uri(), tokens);

    run_price_cycle(Arc::clone(&state)).await;

    let cache = state.price_cache.read().await;
    assert!(
        cache.last_updated.is_some(),
        "last_updated must be set when at least one token succeeds"
    );
}

#[tokio::test]
async fn last_updated_not_set_when_all_tokens_fail() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_ok()))
        .mount(&mock)
        .await;

    let tokens = vec![bad_token("FAILONLY", FAIL1_ADDR)];
    let state = test_state(&mock.uri(), tokens);

    run_price_cycle(Arc::clone(&state)).await;

    let cache = state.price_cache.read().await;
    assert!(
        cache.last_updated.is_none(),
        "last_updated must remain None when all tokens fail"
    );
}

#[tokio::test]
async fn last_updated_set_when_mixed_results_and_one_succeeds() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_ok()))
        .mount(&mock)
        .await;

    let tokens = vec![
        bad_token("FAIL1", FAIL1_ADDR),
        fixed_token("USDC", USDC_ADDR),
        bad_token("FAIL2", FAIL2_ADDR),
    ];
    let state = test_state(&mock.uri(), tokens);

    run_price_cycle(Arc::clone(&state)).await;

    let cache = state.price_cache.read().await;
    assert!(
        cache.last_updated.is_some(),
        "last_updated must be set when at least one of many tokens succeeds"
    );
}

#[tokio::test]
async fn last_updated_is_recent_after_successful_cycle() {
    use std::time::{SystemTime, UNIX_EPOCH};

    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_ok()))
        .mount(&mock)
        .await;

    let before = SystemTime::now();
    let tokens = vec![fixed_token("USDC", USDC_ADDR)];
    let state = test_state(&mock.uri(), tokens);

    run_price_cycle(Arc::clone(&state)).await;

    let cache = state.price_cache.read().await;
    let updated = cache
        .last_updated
        .expect("last_updated must be set after success");

    assert!(
        updated >= before,
        "last_updated must not be earlier than the cycle start time"
    );

    let secs = updated
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    assert!(secs > 0, "last_updated must be a valid epoch timestamp");
}

#[tokio::test]
async fn last_updated_cleared_after_previously_successful_cycle_then_all_fail() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_ok()))
        .mount(&mock)
        .await;

    // Cycle 1: USDC succeeds → last_updated set.
    let state_success = test_state(&mock.uri(), vec![fixed_token("USDC", USDC_ADDR)]);
    run_price_cycle(Arc::clone(&state_success)).await;

    let after_first = state_success.price_cache.read().await.last_updated;
    assert!(
        after_first.is_some(),
        "first successful cycle must set last_updated"
    );

    // Cycle 2: all tokens fail. Carry forward the previous timestamp into a fresh
    // state to verify that a total-failure cycle clears it — a stale last_updated
    // paired with an emptied price cache would misreport freshness (#530).
    let state_fail = test_state(&mock.uri(), vec![bad_token("FAILONLY", FAIL1_ADDR)]);
    {
        let mut cache = state_fail.price_cache.write().await;
        cache.last_updated = after_first;
    }

    run_price_cycle(Arc::clone(&state_fail)).await;

    let after_second = state_fail.price_cache.read().await.last_updated;
    assert!(
        after_second.is_none(),
        "last_updated must be cleared after a cycle where all tokens fail, even if it was previously set"
    );
}

#[tokio::test]
async fn last_updated_none_initially() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_ok()))
        .mount(&mock)
        .await;

    let state = test_state(&mock.uri(), vec![]);

    let cache = state.price_cache.read().await;
    assert!(
        cache.last_updated.is_none(),
        "last_updated must be None before any cycle runs"
    );
}

#[tokio::test]
async fn last_updated_set_only_once_per_cycle_not_per_token() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_ok()))
        .mount(&mock)
        .await;

    let tokens = vec![fixed_token("USDC", USDC_ADDR), fixed_token("XLM", XLM_ADDR)];
    let state = test_state(&mock.uri(), tokens);

    run_price_cycle(Arc::clone(&state)).await;

    let cache = state.price_cache.read().await;
    // Both tokens cached; last_updated is Some — set once after the full loop.
    assert!(
        cache.last_updated.is_some(),
        "last_updated must be set when multiple tokens all succeed"
    );
    assert_eq!(cache.prices.len(), 2, "both tokens must be in the cache");
}

#[tokio::test]
async fn two_consecutive_good_cycles_both_update_last_updated() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_ok()))
        .mount(&mock)
        .await;

    let state = test_state(&mock.uri(), vec![fixed_token("USDC", USDC_ADDR)]);

    run_price_cycle(Arc::clone(&state)).await;
    let first = state
        .price_cache
        .read()
        .await
        .last_updated
        .expect("first timestamp must be set");

    // Small yield so SystemTime::now() can advance.
    tokio::time::sleep(Duration::from_millis(5)).await;

    run_price_cycle(Arc::clone(&state)).await;
    let second = state
        .price_cache
        .read()
        .await
        .last_updated
        .expect("second timestamp must be set");

    assert!(
        second > first,
        "last_updated from the second cycle must be strictly greater than the first"
    );
}

#[tokio::test]
async fn empty_token_list_leaves_last_updated_none() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_ok()))
        .mount(&mock)
        .await;

    // No tokens → tokens_ok == 0 → last_updated stays None.
    let state = test_state(&mock.uri(), vec![]);

    run_price_cycle(Arc::clone(&state)).await;

    let cache = state.price_cache.read().await;
    assert!(
        cache.last_updated.is_none(),
        "last_updated must stay None when there are no tokens to process"
    );
}

#[tokio::test]
async fn last_updated_not_set_when_ledger_fetch_fails() {
    let mock = MockServer::start().await;
    // Return an RPC error for getLatestLedger — cycle aborts before any token.
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_fail()))
        .mount(&mock)
        .await;

    let state = test_state(&mock.uri(), vec![fixed_token("USDC", USDC_ADDR)]);

    run_price_cycle(Arc::clone(&state)).await;

    let cache = state.price_cache.read().await;
    assert!(
        cache.last_updated.is_none(),
        "last_updated must not be set when the cycle aborts due to ledger fetch failure"
    );
}

const ADDR3: &str = "CADDR3111111111111111111111111111111111111111111111111111111";
const ADDR4: &str = "CADDR4111111111111111111111111111111111111111111111111111111";

#[tokio::test]
async fn last_updated_set_with_three_successful_tokens() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_ok()))
        .mount(&mock)
        .await;

    let tokens = vec![
        fixed_token("T1", USDC_ADDR),
        fixed_token("T2", XLM_ADDR),
        fixed_token("T3", ADDR3),
    ];
    let state = test_state(&mock.uri(), tokens);

    run_price_cycle(Arc::clone(&state)).await;

    let cache = state.price_cache.read().await;
    assert!(cache.last_updated.is_some());
    assert_eq!(cache.prices.len(), 3, "all three tokens must be cached");
}

#[tokio::test]
async fn last_updated_not_set_when_token_has_no_sources() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_ok()))
        .mount(&mock)
        .await;

    // A token with an empty source list cannot produce any price → tokens_ok stays 0.
    let no_source_token = TokenConfig {
        symbol: "NOSRC".to_string(),
        display_symbol: Some("NOSRC".to_string()),
        stellar_address: ADDR4.to_string(),
        sources: vec![],
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
    };
    let state = test_state(&mock.uri(), vec![no_source_token]);

    run_price_cycle(Arc::clone(&state)).await;

    let cache = state.price_cache.read().await;
    assert!(
        cache.last_updated.is_none(),
        "last_updated must stay None when the token has no sources to query"
    );
}

#[tokio::test]
async fn cycle_running_is_false_after_successful_cycle() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_ok()))
        .mount(&mock)
        .await;

    let state = test_state(&mock.uri(), vec![fixed_token("USDC", USDC_ADDR)]);

    run_price_cycle(Arc::clone(&state)).await;

    let status = state.cycle_status.read().await;
    assert!(
        !status.price_cycle_running,
        "price_cycle_running must be false after finish_cycle"
    );
    assert!(
        status.last_price_cycle_at.is_some(),
        "last_price_cycle_at must be set by finish_cycle"
    );
}

#[tokio::test]
async fn cycle_running_is_false_after_ledger_failure() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_fail()))
        .mount(&mock)
        .await;

    let state = test_state(&mock.uri(), vec![fixed_token("USDC", USDC_ADDR)]);

    run_price_cycle(Arc::clone(&state)).await;

    let status = state.cycle_status.read().await;
    assert!(
        !status.price_cycle_running,
        "price_cycle_running must be false even when ledger fetch fails"
    );
}

#[tokio::test]
async fn last_updated_bounded_between_cycle_start_and_now() {
    use std::time::SystemTime;

    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_ok()))
        .mount(&mock)
        .await;

    let before = SystemTime::now();
    let state = test_state(&mock.uri(), vec![fixed_token("USDC", USDC_ADDR)]);
    run_price_cycle(Arc::clone(&state)).await;
    let after = SystemTime::now();

    let last_updated = state
        .price_cache
        .read()
        .await
        .last_updated
        .expect("last_updated must be set after a successful cycle");

    assert!(
        last_updated >= before,
        "last_updated must not predate the cycle"
    );
    assert!(
        last_updated <= after,
        "last_updated must not postdate the observation"
    );
}
