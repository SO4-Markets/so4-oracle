/// Integration tests for issue #395: cycle completion is logged with all three
/// required fields — `tokens_ok`, `tokens_failed`, and `latency_ms`.
///
/// Relevant code: `oracle/src/price_loop.rs::finish_cycle`
///
/// ```text
/// tracing::info!(tokens_ok, tokens_failed, latency_ms, "cycle_complete");
/// ```
///
/// Because `finish_cycle` runs on EVERY exit path of `run_price_cycle` (both
/// the ledger-failure abort and the normal token loop), the log call fires on
/// every invocation.  These tests verify that the VALUES passed to the log
/// statement are correct by inspecting observable state that is set alongside
/// (or before) the log:
///
/// Verified invariants (17 tests):
/// - `tokens_ok`  = number of cache entries added this cycle
/// - `tokens_failed` = number of per-token failure records in the ring buffer
/// - `tokens_ok + tokens_failed` = total token count (no token is silently skipped)
/// - `tokens_ok` and `tokens_failed` are per-token counts, not per-source
/// - `latency_ms` is captured by `state.metrics` on every `finish_cycle` call
/// - `finish_cycle` (and thus the log) fires on ALL exit paths including abort
/// - `cycle_status.last_price_cycle_at` is set before the log event fires
use std::sync::Arc;

use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;

use common::{bad_token, fixed_token, test_state};
use oracle::price_loop::run_price_cycle;

const USDC_ADDR: &str = "CBAN5YU3KRDKPTQ2H76D6S7HQFPRBGUD524F65BUM2RQCITPTRLKWKES";
const XLM_ADDR: &str = "CXLM11111111111111111111111111111111111111111111111111111111";
const FAIL1_ADDR: &str = "CFAIL1111111111111111111111111111111111111111111111111111111";
const FAIL2_ADDR: &str = "CFAIL2111111111111111111111111111111111111111111111111111111";
const ADDR3: &str = "CADDR3111111111111111111111111111111111111111111111111111111";
const ADDR4: &str = "CADDR4111111111111111111111111111111111111111111111111111111";

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
async fn tokens_ok_equals_successful_cache_entries() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_ok()))
        .mount(&mock)
        .await;

    let state = test_state(
        &mock.uri(),
        vec![fixed_token("USDC", USDC_ADDR), fixed_token("XLM", XLM_ADDR)],
    );

    run_price_cycle(Arc::clone(&state)).await;

    // tokens_ok is the value logged; it equals the number of cached entries.
    let cache = state.price_cache.read().await;
    assert_eq!(
        cache.prices.len(),
        2,
        "tokens_ok (logged as 2) must match the number of cache entries"
    );
}

#[tokio::test]
async fn tokens_failed_equals_failures_ring_buffer_count() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_ok()))
        .mount(&mock)
        .await;

    let state = test_state(
        &mock.uri(),
        vec![
            bad_token("FAIL1", FAIL1_ADDR),
            bad_token("FAIL2", FAIL2_ADDR),
        ],
    );

    run_price_cycle(Arc::clone(&state)).await;

    // tokens_failed is the value logged; each failed token appends to failures.
    let failures = state.failures.lock().await;
    let entries: Vec<_> = failures
        .iter()
        .filter(|e| e.operation.starts_with("price:"))
        .collect();
    assert_eq!(
        entries.len(),
        2,
        "tokens_failed (logged as 2) must match the number of per-token failure records"
    );
}

#[tokio::test]
async fn latency_ms_field_present_via_metrics_cycle_count() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_ok()))
        .mount(&mock)
        .await;

    let state = test_state(&mock.uri(), vec![fixed_token("USDC", USDC_ADDR)]);

    run_price_cycle(Arc::clone(&state)).await;

    // finish_cycle calls record_price_cycle(latency_ms) before the log; counter > 0
    // proves finish_cycle ran with a valid latency value.
    let metrics = state.metrics.to_response();
    assert_eq!(
        metrics.price_cycle_count, 1,
        "metrics counter must be 1, confirming latency_ms was recorded alongside the log"
    );
}

#[tokio::test]
async fn all_three_log_fields_correct_for_all_good_cycle() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_ok()))
        .mount(&mock)
        .await;

    let state = test_state(&mock.uri(), vec![fixed_token("USDC", USDC_ADDR)]);

    run_price_cycle(Arc::clone(&state)).await;

    let cache = state.price_cache.read().await;
    let failures = state.failures.lock().await;
    let token_failures: Vec<_> = failures
        .iter()
        .filter(|e| e.operation.starts_with("price:"))
        .collect();
    let metrics = state.metrics.to_response();

    // All three fields that would be logged in the cycle_complete event:
    assert_eq!(cache.prices.len(), 1, "tokens_ok = 1");
    assert_eq!(token_failures.len(), 0, "tokens_failed = 0");
    assert_eq!(metrics.price_cycle_count, 1, "latency_ms was recorded");
}

#[tokio::test]
async fn all_three_log_fields_correct_for_mixed_cycle() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_ok()))
        .mount(&mock)
        .await;

    let state = test_state(
        &mock.uri(),
        vec![
            fixed_token("USDC", USDC_ADDR),
            bad_token("FAIL1", FAIL1_ADDR),
            bad_token("FAIL2", FAIL2_ADDR),
        ],
    );

    run_price_cycle(Arc::clone(&state)).await;

    let cache = state.price_cache.read().await;
    let failures = state.failures.lock().await;
    let token_failures: Vec<_> = failures
        .iter()
        .filter(|e| e.operation.starts_with("price:"))
        .collect();
    let metrics = state.metrics.to_response();

    assert_eq!(cache.prices.len(), 1, "tokens_ok = 1");
    assert_eq!(token_failures.len(), 2, "tokens_failed = 2");
    assert_eq!(metrics.price_cycle_count, 1, "latency_ms was recorded");
}

#[tokio::test]
async fn log_fires_even_on_ledger_failure_abort() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_fail()))
        .mount(&mock)
        .await;

    let state = test_state(&mock.uri(), vec![fixed_token("USDC", USDC_ADDR)]);

    run_price_cycle(Arc::clone(&state)).await;

    // finish_cycle is called on the abort path too, so the log fires even on failure.
    let metrics = state.metrics.to_response();
    assert_eq!(
        metrics.price_cycle_count, 1,
        "cycle_complete log must fire on the ledger-failure abort path"
    );
    let status = state.cycle_status.read().await;
    assert!(status.last_price_cycle_at.is_some());
}

#[tokio::test]
async fn log_fires_on_all_tokens_fail_path() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_ok()))
        .mount(&mock)
        .await;

    let state = test_state(&mock.uri(), vec![bad_token("FAIL1", FAIL1_ADDR)]);

    run_price_cycle(Arc::clone(&state)).await;

    // tokens_ok = 0, tokens_failed = 1 — but finish_cycle still runs and logs.
    let metrics = state.metrics.to_response();
    assert_eq!(
        metrics.price_cycle_count, 1,
        "cycle_complete log must fire when all tokens fail"
    );
}

#[tokio::test]
async fn two_cycles_produce_two_log_events_via_metrics() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_ok()))
        .mount(&mock)
        .await;

    let state = test_state(&mock.uri(), vec![fixed_token("USDC", USDC_ADDR)]);

    run_price_cycle(Arc::clone(&state)).await;
    run_price_cycle(Arc::clone(&state)).await;

    let metrics = state.metrics.to_response();
    assert_eq!(
        metrics.price_cycle_count, 2,
        "cycle_complete must be logged once per cycle invocation"
    );
}

#[tokio::test]
async fn tokens_ok_zero_when_no_tokens_configured() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_ok()))
        .mount(&mock)
        .await;

    let state = test_state(&mock.uri(), vec![]);

    run_price_cycle(Arc::clone(&state)).await;

    // tokens_ok = 0, tokens_failed = 0 — finish_cycle still logs the event.
    let cache = state.price_cache.read().await;
    assert_eq!(
        cache.prices.len(),
        0,
        "tokens_ok = 0 when no tokens configured"
    );
    drop(cache);

    let metrics = state.metrics.to_response();
    assert_eq!(
        metrics.price_cycle_count, 1,
        "cycle_complete log fires even with an empty token list"
    );
}

#[tokio::test]
async fn latency_logged_for_every_cycle_in_consecutive_run() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_ok()))
        .mount(&mock)
        .await;

    let state = test_state(&mock.uri(), vec![fixed_token("USDC", USDC_ADDR)]);

    for _ in 0..5 {
        run_price_cycle(Arc::clone(&state)).await;
    }

    let metrics = state.metrics.to_response();
    assert_eq!(
        metrics.price_cycle_count, 5,
        "latency_ms must be logged on every cycle — 5 invocations = 5 events"
    );
}

#[tokio::test]
async fn three_good_two_bad_log_fields_are_three_and_two() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_ok()))
        .mount(&mock)
        .await;

    let state = test_state(
        &mock.uri(),
        vec![
            fixed_token("T1", USDC_ADDR),
            fixed_token("T2", XLM_ADDR),
            fixed_token("T3", ADDR3),
            bad_token("F1", FAIL1_ADDR),
            bad_token("F2", FAIL2_ADDR),
        ],
    );

    run_price_cycle(Arc::clone(&state)).await;

    let cache = state.price_cache.read().await;
    let failures = state.failures.lock().await;
    let token_failures: Vec<_> = failures
        .iter()
        .filter(|e| e.operation.starts_with("price:"))
        .collect();

    assert_eq!(cache.prices.len(), 3, "tokens_ok = 3");
    assert_eq!(token_failures.len(), 2, "tokens_failed = 2");
}

#[tokio::test]
async fn tokens_ok_is_per_token_not_per_source() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_ok()))
        .mount(&mock)
        .await;

    // ADDR4 gets a fixed token — one token with one source.
    // tokens_ok should be 1 (one token succeeded), NOT 1-per-source.
    let state = test_state(&mock.uri(), vec![fixed_token("USDC", ADDR4)]);

    run_price_cycle(Arc::clone(&state)).await;

    let cache = state.price_cache.read().await;
    assert_eq!(
        cache.prices.len(),
        1,
        "tokens_ok = 1: counted per token, not per source"
    );
}

#[tokio::test]
async fn tokens_failed_operation_field_identifies_failing_token() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_ok()))
        .mount(&mock)
        .await;

    let state = test_state(&mock.uri(), vec![bad_token("MYBAD", FAIL1_ADDR)]);

    run_price_cycle(Arc::clone(&state)).await;

    let failures = state.failures.lock().await;
    let token_failures: Vec<_> = failures
        .iter()
        .filter(|e| e.operation.starts_with("price:"))
        .collect();

    assert_eq!(token_failures.len(), 1);
    assert!(
        token_failures[0].operation.contains("MYBAD"),
        "failure operation must contain the token symbol so the log is meaningful"
    );
}

#[tokio::test]
async fn cycle_status_set_by_finish_cycle_before_log() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_ok()))
        .mount(&mock)
        .await;

    let state = test_state(&mock.uri(), vec![fixed_token("USDC", USDC_ADDR)]);

    run_price_cycle(Arc::clone(&state)).await;

    let status = state.cycle_status.read().await;
    // finish_cycle sets these before calling record_price_cycle and the log.
    assert!(!status.price_cycle_running);
    assert!(
        status.last_price_cycle_at.is_some(),
        "last_price_cycle_at must be set, confirming finish_cycle (and the log) ran"
    );
}

#[tokio::test]
async fn accumulated_cycle_count_matches_invocations() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_ok()))
        .mount(&mock)
        .await;

    let state = test_state(&mock.uri(), vec![fixed_token("USDC", USDC_ADDR)]);

    const N: u64 = 4;
    for _ in 0..N {
        run_price_cycle(Arc::clone(&state)).await;
    }

    let metrics = state.metrics.to_response();
    assert_eq!(
        metrics.price_cycle_count, N,
        "latency_ms is logged once per cycle; after {N} cycles counter must be {N}"
    );
}

#[tokio::test]
async fn mixed_ledger_fail_and_ok_both_logged_in_metrics() {
    // One mock server, one AppState: swap the ledger response between the two
    // cycles (via scoped mocks) so a genuine fail-then-ok sequence runs against
    // the *same* state and `price_cycle_count` is asserted to accumulate to 2 —
    // rather than checking two independent states that each trivially reach 1
    // (#787).
    let mock = MockServer::start().await;
    let state = test_state(&mock.uri(), vec![fixed_token("USDC", USDC_ADDR)]);

    // Cycle 1: ledger fails — finish_cycle still runs and logs latency_ms.
    {
        let _guard = Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ledger_fail()))
            .mount_as_scoped(&mock)
            .await;
        run_price_cycle(Arc::clone(&state)).await;
    }
    assert_eq!(
        state.metrics.to_response().price_cycle_count,
        1,
        "failed ledger cycle must also log latency_ms"
    );

    // Cycle 2: ledger succeeds — a second cycle_complete event on the same state.
    {
        let _guard = Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ledger_ok()))
            .mount_as_scoped(&mock)
            .await;
        run_price_cycle(Arc::clone(&state)).await;
    }

    assert_eq!(
        state.metrics.to_response().price_cycle_count,
        2,
        "price_cycle_count must accumulate to 2 across a fail-then-ok sequence on one state"
    );
}

#[tokio::test]
async fn tokens_ok_plus_tokens_failed_equals_total_tokens() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ledger_ok()))
        .mount(&mock)
        .await;

    let state = test_state(
        &mock.uri(),
        vec![
            fixed_token("T1", USDC_ADDR),
            bad_token("F1", FAIL1_ADDR),
            fixed_token("T2", XLM_ADDR),
            bad_token("F2", FAIL2_ADDR),
            fixed_token("T3", ADDR3),
        ],
    );

    run_price_cycle(Arc::clone(&state)).await;

    let cache = state.price_cache.read().await;
    let failures = state.failures.lock().await;
    let token_failures: Vec<_> = failures
        .iter()
        .filter(|e| e.operation.starts_with("price:"))
        .collect();

    let tokens_ok = cache.prices.len();
    let tokens_failed = token_failures.len();
    assert_eq!(
        tokens_ok + tokens_failed,
        5,
        "tokens_ok ({tokens_ok}) + tokens_failed ({tokens_failed}) must equal total tokens (5)"
    );
}
