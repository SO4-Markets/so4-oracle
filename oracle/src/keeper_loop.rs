use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use tokio::time::{interval, timeout, MissedTickBehavior};
use tracing::{error, info, warn};

use crate::chain::scval;
use crate::chain::tx_builder;
use crate::state::{
    AppState, CachedPrice, FailedSubmission, KeeperExecution, MAX_CONSECUTIVE_FREEZE_FAILURES,
};

const ACCOUNT_SEQUENCE_RETRY_ATTEMPTS: u32 = 3;
const ACCOUNT_SEQUENCE_RETRY_BASE_DELAY_MS: u64 = 100;
/// Hard cap on a single keeper cycle — closes #490.
const KEEPER_CYCLE_TIMEOUT_SECS: u64 = 50;

#[derive(Debug)]
enum SequenceFetchError {
    Network(String),
    MissingOrInvalid(String),
}

impl std::fmt::Display for SequenceFetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Network(msg) | Self::MissingOrInvalid(msg) => write!(f, "{msg}"),
        }
    }
}

impl crate::retry::Retryable for SequenceFetchError {
    fn is_retryable(&self) -> bool {
        matches!(self, Self::Network(_))
    }
}

pub async fn run_keeper_loop(state: Arc<AppState>) {
    let mut ticker = interval(state.config.keeper_loop_interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = ticker.tick() => {}
            _ = state.shutdown_token.cancelled() => {
                tracing::info!("keeper_loop shutting down");
                break;
            }
        }
        let _ = run_keeper_cycle(Arc::clone(&state)).await;
    }
}

pub async fn run_keeper_cycle(state: Arc<AppState>) -> Result<CycleSummary, String> {
    let started = Instant::now();
    {
        let mut status = state.cycle_status.write().await;
        status.keeper_cycle_running = true;
    }

    let result = timeout(
        Duration::from_secs(KEEPER_CYCLE_TIMEOUT_SECS),
        execute_keeper_cycle(Arc::clone(&state)),
    )
    .await
    .unwrap_or_else(|_| {
        Err(format!(
            "keeper cycle exceeded {KEEPER_CYCLE_TIMEOUT_SECS}s budget"
        ))
    });

    {
        let mut status = state.cycle_status.write().await;
        status.keeper_cycle_running = false;
        status.last_keeper_cycle_at = Some(SystemTime::now());
    }

    let latency_ms = started.elapsed().as_millis() as u64;
    {
        let mut status = state.cycle_status.write().await;
        status.last_keeper_cycle_latency_ms = Some(latency_ms);
    }
    match &result {
        Ok(summary) => {
            info!(
                latency_ms,
                orders = summary.orders_executed,
                deposits = summary.deposits_executed,
                withdrawals = summary.withdrawals_executed,
                errors = summary.errors,
                "keeper_cycle_complete"
            );
            state.metrics.record_keeper_cycle(
                latency_ms,
                summary.orders_executed,
                summary.deposits_executed,
                summary.withdrawals_executed,
                summary.errors,
            );
        }
        Err(error) => {
            error!(latency_ms, %error, "keeper_cycle_failed");
            record_error(&state, "keeper_cycle", error, None).await;
            state.metrics.record_submit_failure();
        }
    }

    result
}

#[derive(Debug)]
pub struct CycleSummary {
    pub orders_executed: usize,
    pub deposits_executed: usize,
    pub withdrawals_executed: usize,
    pub errors: usize,
}

async fn execute_keeper_cycle(state: Arc<AppState>) -> Result<CycleSummary, String> {
    // Get fresh (non-stale) prices from cache
    let now = crate::current_timestamp_secs();
    let fresh_prices = {
        let cache = state.price_cache.read().await;
        let tokens = &state.config.price_feed.tokens;

        cache
            .prices
            .iter()
            .filter_map(|(key, price)| {
                tokens
                    .iter()
                    .find(|t| t.lookup_key() == *key)
                    .and_then(|token| {
                        if price.is_stale(token.stale_after_seconds, now) {
                            tracing::debug!(
                                symbol = %token.symbol,
                                token = %token.stellar_address,
                                cached_timestamp = price.timestamp,
                                stale_after_seconds = token.stale_after_seconds,
                                age_seconds = now.saturating_sub(price.timestamp),
                                "skipping stale price in keeper cycle"
                            );
                            None
                        } else {
                            Some((key.clone(), price.clone()))
                        }
                    })
            })
            .collect::<std::collections::BTreeMap<_, _>>()
    };

    if fresh_prices.is_empty() {
        let cache = state.price_cache.read().await;
        return Err(format!(
            "No fresh prices available in cache (cache size: {}, all stale)",
            cache.prices.len()
        ));
    }

    let order_keys = get_pending_keys(&state, "get_order_count", "get_order_keys")
        .await
        .unwrap_or_else(|e| {
            warn!(error = %e, "get_pending_keys(orders) failed, skipping orders this cycle");
            Vec::new()
        });
    let deposit_keys = get_pending_keys(&state, "get_deposit_count", "get_deposit_keys")
        .await
        .unwrap_or_else(|e| {
            warn!(error = %e, "get_pending_keys(deposits) failed, skipping deposits this cycle");
            Vec::new()
        });
    let withdrawal_keys =
        get_pending_keys(&state, "get_withdrawal_count", "get_withdrawal_keys")
            .await
            .unwrap_or_else(|e| {
                warn!(error = %e, "get_pending_keys(withdrawals) failed, skipping withdrawals this cycle");
                Vec::new()
            });

    {
        let mut keeper_status = state.keeper_status.write().await;
        keeper_status.pending_orders = order_keys.len();
        keeper_status.pending_deposits = deposit_keys.len();
        keeper_status.pending_withdrawals = withdrawal_keys.len();
    }

    if order_keys.is_empty() && deposit_keys.is_empty() && withdrawal_keys.is_empty() {
        info!("no_pending_work");
        return Ok(CycleSummary {
            orders_executed: 0,
            deposits_executed: 0,
            withdrawals_executed: 0,
            errors: 0,
        });
    }

    info!(
        orders = order_keys.len(),
        deposits = deposit_keys.len(),
        withdrawals = withdrawal_keys.len(),
        fresh_prices = fresh_prices.len(),
        "found_pending_work"
    );

    // Submit prices on-chain - only fresh prices are included
    let tx_hash = set_prices_on_chain(&state, &fresh_prices).await?;
    info!(hash = %tx_hash, "set_prices_confirmed");
    tokio::time::sleep(Duration::from_millis(5000)).await;

    let mut summary = CycleSummary {
        orders_executed: 0,
        deposits_executed: 0,
        withdrawals_executed: 0,
        errors: 0,
    };

    for order_key in &order_keys {
        // Skip permanently blacklisted orders — retrying burns fee attempts (#498).
        {
            let blacklist = state.frozen_order_blacklist.lock().await;
            if blacklist.contains_key(order_key.as_str()) {
                error!(
                    key = %order_key,
                    "order_key permanently blacklisted after repeated freeze failures; \
                     manual intervention required to clear"
                );
                summary.errors += 1;
                continue;
            }
        }
        // Skip keys whose prior submission is still in-flight — closes #491
        if state.in_flight_keys.lock().await.contains(order_key) {
            info!(key = %order_key, "skipping_in_flight_order_key");
            continue;
        }
        state.in_flight_keys.lock().await.insert(order_key.clone());

        match execute_handler(
            &state,
            &state.config.order_handler_contract_id,
            "execute_order",
            order_key,
        )
        .await
        {
            Ok(tx_hash) => {
                state.in_flight_keys.lock().await.remove(order_key);
                summary.orders_executed += 1;
                // Clear any accumulated freeze-failure count on success.
                state
                    .freeze_failure_counts
                    .lock()
                    .await
                    .remove(order_key.as_str());
                record_execution(
                    &state,
                    "execute_order",
                    order_key,
                    Some(tx_hash),
                    true,
                    None,
                )
                .await;
            }
            Err(ref error) if is_poll_timeout(error) => {
                // Tx may still confirm on-chain; retain in-flight to prevent re-submission.
                warn!(key = %order_key, %error, "order_poll_timeout_key_remains_in_flight");
                summary.errors += 1;
                record_error(&state, &format!("execute_order:{}", order_key), error, None).await;
                record_execution(
                    &state,
                    "execute_order",
                    order_key,
                    None,
                    false,
                    Some(error.clone()),
                )
                .await;
            }
            Err(error) => {
                state.in_flight_keys.lock().await.remove(order_key);
                summary.errors += 1;
                warn!(key = %order_key, %error, "order_execution_failed");

                let mut freeze_error_msg = None;
                if error.contains("Budget, ExceededLimit") {
                    match execute_handler(
                        &state,
                        &state.config.order_handler_contract_id,
                        "freeze_order",
                        order_key,
                    )
                    .await
                    {
                        Ok(_) => {
                            info!(key = %order_key, "order_frozen_budget_exceeded");
                            state
                                .freeze_failure_counts
                                .lock()
                                .await
                                .remove(order_key.as_str());
                        }
                        Err(freeze_error) => {
                            let consecutive = {
                                let mut counts = state.freeze_failure_counts.lock().await;
                                let count = counts.entry(order_key.clone()).or_insert(0);
                                *count += 1;
                                *count
                            };

                            error!(
                                key = %order_key,
                                %freeze_error,
                                consecutive_failures = consecutive,
                                max = MAX_CONSECUTIVE_FREEZE_FAILURES,
                                "freeze_order_failed"
                            );
                            freeze_error_msg = Some(freeze_error.clone());
                            record_error(&state, "freeze_order", &freeze_error, None).await;

                            if consecutive >= MAX_CONSECUTIVE_FREEZE_FAILURES {
                                // Permanently blacklist this key and stop
                                // burning fee attempts on it (#498).
                                state
                                    .frozen_order_blacklist
                                    .lock()
                                    .await
                                    .insert(order_key.clone(), consecutive);
                                error!(
                                    key = %order_key,
                                    consecutive_failures = consecutive,
                                    "ALERT: order_key blacklisted after {} consecutive \
                                     freeze failures — manual intervention required",
                                    MAX_CONSECUTIVE_FREEZE_FAILURES
                                );
                            }
                        }
                    }
                }

                record_error(
                    &state,
                    &format!("execute_order:{}", order_key),
                    &error,
                    None,
                )
                .await;
                record_execution(
                    &state,
                    "execute_order",
                    order_key,
                    None,
                    false,
                    Some(format!("{}{}", error, freeze_error_msg.unwrap_or_default())),
                )
                .await;
            }
        }
    }

    for deposit_key in &deposit_keys {
        if state.in_flight_keys.lock().await.contains(deposit_key) {
            info!(key = %deposit_key, "skipping_in_flight_deposit_key");
            continue;
        }
        state
            .in_flight_keys
            .lock()
            .await
            .insert(deposit_key.clone());

        match execute_handler(
            &state,
            &state.config.deposit_handler_contract_id,
            "execute_deposit",
            deposit_key,
        )
        .await
        {
            Ok(tx_hash) => {
                state.in_flight_keys.lock().await.remove(deposit_key);
                summary.deposits_executed += 1;
                record_execution(
                    &state,
                    "execute_deposit",
                    deposit_key,
                    Some(tx_hash),
                    true,
                    None,
                )
                .await;
            }
            Err(ref error) if is_poll_timeout(error) => {
                warn!(key = %deposit_key, %error, "deposit_poll_timeout_key_remains_in_flight");
                summary.errors += 1;
                record_error(
                    &state,
                    &format!("execute_deposit:{}", deposit_key),
                    error,
                    None,
                )
                .await;
                record_execution(
                    &state,
                    "execute_deposit",
                    deposit_key,
                    None,
                    false,
                    Some(error.clone()),
                )
                .await;
            }
            Err(error) => {
                state.in_flight_keys.lock().await.remove(deposit_key);
                summary.errors += 1;
                warn!(key = %deposit_key, %error, "deposit_execution_failed");
                record_error(
                    &state,
                    &format!("execute_deposit:{}", deposit_key),
                    &error,
                    None,
                )
                .await;
                record_execution(
                    &state,
                    "execute_deposit",
                    deposit_key,
                    None,
                    false,
                    Some(error),
                )
                .await;
            }
        }
    }

    for withdrawal_key in &withdrawal_keys {
        if state.in_flight_keys.lock().await.contains(withdrawal_key) {
            info!(key = %withdrawal_key, "skipping_in_flight_withdrawal_key");
            continue;
        }
        state
            .in_flight_keys
            .lock()
            .await
            .insert(withdrawal_key.clone());

        match execute_handler(
            &state,
            &state.config.withdrawal_handler_contract_id,
            "execute_withdrawal",
            withdrawal_key,
        )
        .await
        {
            Ok(tx_hash) => {
                state.in_flight_keys.lock().await.remove(withdrawal_key);
                summary.withdrawals_executed += 1;
                record_execution(
                    &state,
                    "execute_withdrawal",
                    withdrawal_key,
                    Some(tx_hash),
                    true,
                    None,
                )
                .await;
            }
            Err(ref error) if is_poll_timeout(error) => {
                warn!(key = %withdrawal_key, %error, "withdrawal_poll_timeout_key_remains_in_flight");
                summary.errors += 1;
                record_error(
                    &state,
                    &format!("execute_withdrawal:{}", withdrawal_key),
                    error,
                    None,
                )
                .await;
                record_execution(
                    &state,
                    "execute_withdrawal",
                    withdrawal_key,
                    None,
                    false,
                    Some(error.clone()),
                )
                .await;
            }
            Err(error) => {
                state.in_flight_keys.lock().await.remove(withdrawal_key);
                summary.errors += 1;
                warn!(key = %withdrawal_key, %error, "withdrawal_execution_failed");
                record_error(
                    &state,
                    &format!("execute_withdrawal:{}", withdrawal_key),
                    &error,
                    None,
                )
                .await;
                record_execution(
                    &state,
                    "execute_withdrawal",
                    withdrawal_key,
                    None,
                    false,
                    Some(error),
                )
                .await;
            }
        }
    }

    Ok(summary)
}

fn is_poll_timeout(error: &str) -> bool {
    error.contains("not confirmed after")
}

async fn get_pending_keys(
    state: &Arc<AppState>,
    count_method: &str,
    keys_method: &str,
) -> Result<Vec<String>, String> {
    let count_result = simulate_contract_call(
        state,
        &state.config.reader_contract_id,
        count_method,
        &[&state.config.data_store_contract_id],
    )
    .await?;

    let count = parse_u32_from_result(&count_result)?;
    if count == 0 {
        return Ok(Vec::new());
    }

    let keys_result = simulate_contract_call(
        state,
        &state.config.reader_contract_id,
        keys_method,
        &[
            &state.config.data_store_contract_id,
            "0",
            &count.to_string(),
        ],
    )
    .await?;

    let keys = parse_bytes_vec_from_result(&keys_result)?;

    // Cross-check parsed length against reported count to detect RPC/ABI drift
    if keys.len() != count as usize {
        tracing::warn!(
            expected = count,
            actual = keys.len(),
            method = keys_method,
            "parsed key count mismatch — RPC/ABI drift or malformed response"
        );
        return Err(format!(
            "expected {count} keys from {keys_method}, got {}",
            keys.len()
        ));
    }

    Ok(keys)
}

async fn set_prices_on_chain(
    state: &Arc<AppState>,
    prices: &BTreeMap<String, CachedPrice>,
) -> Result<String, String> {
    let prices_vec: Vec<&CachedPrice> = prices.values().collect();
    let prices_scval = scval::encode_prices_vec(&prices_vec)?;

    let sequence = get_account_sequence(state)
        .await
        .map_err(|e| e.to_string())?;

    let tx = tx_builder::build_invoke_tx(
        &state.config.keeper_account_id,
        &state.config.oracle_contract_id,
        "set_prices",
        vec![prices_scval],
        state.config.set_prices_tx_fee,
        sequence,
        None,
    )?;

    let signed_xdr = tx_builder::sign_transaction(
        &tx,
        state.config.keeper_secret_key.as_str(),
        &state.config.network_passphrase,
    )?;

    let ledger = crate::submit::submit_and_poll(&state.config.stellar_rpc_url, &signed_xdr)
        .await
        .map_err(|e| format!("set_prices submit failed: {e}"))?;

    info!(ledger, "set_prices confirmed on ledger");
    Ok(format!("confirmed on ledger {ledger}"))
}

async fn execute_handler(
    state: &Arc<AppState>,
    contract_id: &str,
    method: &str,
    key: &str,
) -> Result<String, String> {
    let key_bytes = hex::decode(key).map_err(|e| format!("invalid key hex: {e}"))?;
    let key_scval = stellar_xdr::ScVal::Bytes(stellar_xdr::ScBytes(
        key_bytes
            .try_into()
            .map_err(|e| format!("key bytes conversion failed: {e}"))?,
    ));

    let sequence = get_account_sequence(state)
        .await
        .map_err(|e| e.to_string())?;

    let tx = tx_builder::build_invoke_tx(
        &state.config.keeper_account_id,
        contract_id,
        method,
        vec![
            stellar_xdr::ScVal::Address(crate::chain::scval::strkey_to_sc_address(
                &state.config.keeper_account_id,
            )?),
            key_scval,
        ],
        state.config.keeper_tx_fee,
        sequence,
        None,
    )?;

    let signed_xdr = tx_builder::sign_transaction(
        &tx,
        state.config.keeper_secret_key.as_str(),
        &state.config.network_passphrase,
    )?;

    let ledger = crate::submit::submit_and_poll(&state.config.stellar_rpc_url, &signed_xdr)
        .await
        .map_err(|e| format!("{method} submit failed: {e}"))?;

    info!(method, key = %key, ledger, "handler_confirmed");
    Ok(format!("confirmed on ledger {ledger}"))
}

async fn get_account_sequence(state: &Arc<AppState>) -> Result<u64, SequenceFetchError> {
    crate::retry::retry_with_backoff(
        || async { get_account_sequence_once(state).await },
        ACCOUNT_SEQUENCE_RETRY_ATTEMPTS,
        ACCOUNT_SEQUENCE_RETRY_BASE_DELAY_MS,
        30_000,
    )
    .await
}

async fn get_account_sequence_once(state: &Arc<AppState>) -> Result<u64, SequenceFetchError> {
    let rpc_url = &state.config.stellar_rpc_url;
    let account_id = &state.config.keeper_account_id;

    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getAccount",
        "params": { "account": account_id }
    });

    let response = state
        .http
        .post(rpc_url)
        .json(&payload)
        .send()
        .await
        .map_err(|e| SequenceFetchError::Network(format!("getAccount request failed: {e}")))?;

    let status = response.status();
    let body = response.text().await.map_err(|e| {
        SequenceFetchError::Network(format!("Failed to read getAccount response: {e}"))
    })?;

    if !status.is_success() {
        return Err(SequenceFetchError::Network(format!(
            "getAccount returned HTTP {}",
            status.as_u16()
        )));
    }

    let response_json: serde_json::Value = serde_json::from_str(&body).map_err(|e| {
        SequenceFetchError::Network(format!("Failed to parse getAccount response: {e}"))
    })?;

    if let Some(error) = response_json.get("error") {
        return Err(SequenceFetchError::Network(format!(
            "getAccount error: {error}"
        )));
    }

    let seq_str = response_json
        .get("result")
        .and_then(|r| r.get("sequence"))
        .and_then(|s| s.as_str())
        .ok_or_else(|| {
            SequenceFetchError::MissingOrInvalid(
                "Missing sequence in getAccount response".to_string(),
            )
        })?;

    seq_str.parse::<u64>().map(|seq| seq + 1).map_err(|e| {
        SequenceFetchError::MissingOrInvalid(format!("failed to parse sequence '{seq_str}': {e}"))
    })
}

async fn simulate_contract_call(
    state: &Arc<AppState>,
    contract_id: &str,
    method: &str,
    args: &[&str],
) -> Result<String, String> {
    let rpc_url = &state.config.stellar_rpc_url;
    let passphrase = &state.config.network_passphrase;

    let args_json: Vec<serde_json::Value> = args
        .iter()
        .map(|arg| serde_json::Value::String(arg.to_string()))
        .collect();

    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "simulateTransaction",
        "params": {
            "transaction": {
                "source_account": state.config.keeper_account_id,
                "fee": "100",
                "network_passphrase": passphrase,
                "operations": [{
                    "type": "invoke",
                    "contract_id": contract_id,
                    "method": method,
                    "args": args_json
                }]
            }
        }
    });

    let response = state
        .http
        .post(rpc_url)
        .json(&payload)
        .send()
        .await
        .map_err(|e| format!("RPC request failed: {e}"))?;

    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| format!("Failed to read RPC response: {e}"))?;

    if !status.is_success() {
        return Err(format!("RPC request returned HTTP {}", status.as_u16()));
    }

    let response_json: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("Failed to parse RPC response: {e}"))?;

    if let Some(error) = response_json.get("error") {
        return Err(format!("Simulation error: {error}"));
    }

    let result = response_json
        .get("result")
        .ok_or_else(|| "Missing result in simulation response".to_string())?;

    Ok(result.to_string())
}

fn parse_u32_from_result(result: &str) -> Result<u32, String> {
    let value: serde_json::Value =
        serde_json::from_str(result).map_err(|e| format!("failed to parse result: {e}"))?;

    value
        .as_u64()
        .and_then(|v| u32::try_from(v).ok())
        .or_else(|| {
            value
                .get("u32")
                .and_then(|v| v.as_u64())
                .and_then(|v| u32::try_from(v).ok())
        })
        .ok_or_else(|| format!("expected u32 value, got: {value}"))
}

fn parse_bytes_vec_from_result(result: &str) -> Result<Vec<String>, String> {
    let value: serde_json::Value =
        serde_json::from_str(result).map_err(|e| format!("failed to parse result: {e}"))?;

    let vec = value
        .get("vec")
        .or_else(|| value.get("Vec"))
        .or(Some(&value))
        .ok_or_else(|| format!("expected vector, got: {value}"))?;

    match vec {
        serde_json::Value::Array(arr) => {
            let mut keys = Vec::new();
            for item in arr {
                let bytes = item
                    .get("bytes")
                    .or_else(|| item.get("Bytes"))
                    .ok_or_else(|| format!("vector entry missing 'bytes' field: {item}"))?;
                let hex_str = bytes
                    .as_str()
                    .ok_or_else(|| format!("'bytes' field is not a string: {item}"))?;
                keys.push(hex_str.to_string());
            }
            Ok(keys)
        }
        _ => Err(format!("expected array, got: {vec}")),
    }
}

async fn record_error(
    state: &Arc<AppState>,
    operation: &str,
    error: &str,
    _tx_hash: Option<String>,
) {
    state.failures.lock().await.push(FailedSubmission {
        at: SystemTime::now(),
        operation: operation.to_string(),
        network: state.config.network.as_str().to_string(),
        token: String::new(),
        symbol: String::new(),
        min: 0,
        max: 0,
        tx_hash: None,
        error: error.to_string(),
        timestamp: 0,
        ledger_seq: 0,
    });
}

async fn record_execution(
    state: &Arc<AppState>,
    operation: impl Into<String>,
    key: impl Into<String>,
    tx_hash: Option<String>,
    success: bool,
    error: Option<String>,
) {
    let mut keeper_status = state.keeper_status.write().await;
    keeper_status.last_executions.push(KeeperExecution {
        timestamp: SystemTime::now(),
        operation: operation.into(),
        key: key.into(),
        tx_hash,
        success,
        error,
    });
    if keeper_status.last_executions.len() > 100 {
        keeper_status.last_executions.remove(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_u32_from_result() {
        assert_eq!(parse_u32_from_result("42").unwrap(), 42);
        assert_eq!(parse_u32_from_result(r#"{"u32": 42}"#).unwrap(), 42);
    }

    // ── #512: run_keeper_loop shutdown coverage ───────────────────────────────

    use crate::config::{Config, Network, PriceFeedConfig, SecretString};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::time::Duration;

    fn shutdown_test_state() -> Arc<AppState> {
        let config = Config {
            bind_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            network: Network::Testnet,
            network_passphrase: "Test SDF Network ; September 2015".to_string(),
            stellar_rpc_url: "not-a-valid-url".to_string(), // immediate error — no network
            horizon_url: "not-a-valid-url".to_string(),
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
            keeper_secret_key: SecretString::new(
                "SAUHMCMUP5FZO5675W3ISZ6E6CNYJGXBUW5WANE2JR4TGAARYCTSCBKI".to_string(),
            ),
            keeper_account_id: "GAUHMCMUP5FZO5675W3ISZ6E6CNYJGXBUW5WANE2JR4TGAARYCTSCBKI"
                .to_string(),
            keeper_index: 0,
            admin_api_token: None,
            pyth_api_key: None,
            min_keeper_balance_xlm: 0.0,
            set_prices_tx_fee: crate::config::DEFAULT_SET_PRICES_TX_FEE,
            keeper_tx_fee: crate::config::DEFAULT_KEEPER_TX_FEE,
            price_loop_interval: Duration::from_millis(50),
            keeper_loop_interval: Duration::from_millis(50),
            price_feed: PriceFeedConfig { tokens: vec![] },
        };
        Arc::new(AppState::new(Arc::new(config)))
    }

    #[tokio::test]
    async fn run_keeper_loop_exits_promptly_on_shutdown() {
        let state = shutdown_test_state();
        let state2 = Arc::clone(&state);

        let handle = tokio::spawn(run_keeper_loop(state2));

        // Let the loop tick at least once.
        tokio::time::sleep(Duration::from_millis(120)).await;

        state.shutdown_token.cancel();

        let completed = tokio::time::timeout(Duration::from_millis(500), handle).await;
        assert!(
            completed.is_ok(),
            "run_keeper_loop must exit within 500 ms of shutdown_token cancellation"
        );
    }

    #[tokio::test]
    async fn test_keeper_cycle_filters_stale_prices() {
        let now = crate::current_timestamp_secs();
        let stale_after = 60;

        let fresh_price = CachedPrice {
            token_address: "GAFRESH".to_string(),
            symbol: "FRESH".to_string(),
            display_symbol: "FRESH".to_string(),
            keeper_index: 0,
            min: 1000,
            max: 1000,
            median: 1000,
            timestamp: now,
            ledger_seq: 12345,
            sources_used: vec!["test".to_string()],
            signature: "sig".to_string(),
        };

        let stale_price = CachedPrice {
            token_address: "GASTALE".to_string(),
            symbol: "STALE".to_string(),
            display_symbol: "STALE".to_string(),
            keeper_index: 0,
            min: 900,
            max: 900,
            median: 900,
            timestamp: now - 100,
            ledger_seq: 12344,
            sources_used: vec!["test".to_string()],
            signature: "sig".to_string(),
        };

        let mut prices = std::collections::BTreeMap::new();
        prices.insert("GAFRESH".to_string(), fresh_price);
        prices.insert("GASTALE".to_string(), stale_price);

        let filtered_prices: Vec<_> = prices
            .iter()
            .filter(|(_, price)| !price.is_stale(stale_after, now))
            .collect();

        assert_eq!(filtered_prices.len(), 1);
        assert_eq!(filtered_prices[0].1.symbol, "FRESH");

        let stale_filtered: Vec<_> = prices
            .iter()
            .filter(|(_, price)| price.is_stale(stale_after, now))
            .collect();
        assert_eq!(stale_filtered.len(), 1);
        assert_eq!(stale_filtered[0].1.symbol, "STALE");
    }
}
