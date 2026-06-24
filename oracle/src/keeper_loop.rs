use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use tokio::time::{interval, MissedTickBehavior};
use tracing::{error, info, warn};

use crate::state::{AppState, CachedPrice, FailedSubmission, KeeperExecution};

const KEEPER_TX_FEE: u32 = 2_000_000;
const POLL_INTERVAL_MS: u64 = 3000;
const MAX_POLL_ATTEMPTS: u32 = 20;

pub async fn run_keeper_loop(state: Arc<AppState>) {
    let mut ticker = interval(state.config.keeper_loop_interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        ticker.tick().await;
        run_keeper_cycle(Arc::clone(&state)).await;
    }
}

pub async fn run_keeper_cycle(state: Arc<AppState>) {
    let started = Instant::now();
    {
        let mut status = state.cycle_status.write().await;
        status.keeper_cycle_running = true;
    }

    let result = execute_keeper_cycle(Arc::clone(&state)).await;

    {
        let mut status = state.cycle_status.write().await;
        status.keeper_cycle_running = false;
        status.last_keeper_cycle_at = Some(SystemTime::now());
    }

    let latency_ms = started.elapsed().as_millis() as u64;
    match result {
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
            record_error(&state, "keeper_cycle", error).await;
            state.metrics.record_submit_failure();
        }
    }
}

struct CycleSummary {
    orders_executed: usize,
    deposits_executed: usize,
    withdrawals_executed: usize,
    errors: usize,
}

async fn execute_keeper_cycle(state: Arc<AppState>) -> Result<CycleSummary, String> {
    let prices = state.price_cache.read().await.prices.clone();
    if prices.is_empty() {
        return Err("No prices available in cache".to_string());
    }

    let order_keys = get_pending_keys(&state, "get_order_count", "get_order_keys").await?;
    let deposit_keys = get_pending_keys(&state, "get_deposit_count", "get_deposit_keys").await?;
    let withdrawal_keys =
        get_pending_keys(&state, "get_withdrawal_count", "get_withdrawal_keys").await?;

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
        "found_pending_work"
    );

    let _ = set_prices_on_chain(&state, &prices).await?;
    tokio::time::sleep(Duration::from_millis(5000)).await;

    let mut summary = CycleSummary {
        orders_executed: 0,
        deposits_executed: 0,
        withdrawals_executed: 0,
        errors: 0,
    };

    for order_key in &order_keys {
        match execute_order(&state, order_key).await {
            Ok(tx_hash) => {
                summary.orders_executed += 1;
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
            Err(error) => {
                summary.errors += 1;
                warn!(key = %order_key, %error, "order_execution_failed");

                let mut freeze_error_msg = None;
                if error.contains("Budget, ExceededLimit") {
                    match freeze_order(&state, order_key).await {
                        Ok(_) => info!(key = %order_key, "order_frozen_budget_exceeded"),
                        Err(freeze_error) => {
                            error!(key = %order_key, %freeze_error, "freeze_order_failed");
                            freeze_error_msg = Some(freeze_error.clone());
                            record_error(
                                &state,
                                format!("freeze_order:{}", order_key),
                                freeze_error,
                            )
                            .await;
                        }
                    }
                }

                record_error(&state, format!("execute_order:{}", order_key), error.clone())
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
        match execute_deposit(&state, deposit_key).await {
            Ok(tx_hash) => {
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
            Err(error) => {
                summary.errors += 1;
                warn!(key = %deposit_key, %error, "deposit_execution_failed");
                record_error(&state, format!("execute_deposit:{}", deposit_key), error.clone())
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
        match execute_withdrawal(&state, withdrawal_key).await {
            Ok(tx_hash) => {
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
            Err(error) => {
                summary.errors += 1;
                warn!(key = %withdrawal_key, %error, "withdrawal_execution_failed");
                record_error(
                    &state,
                    format!("execute_withdrawal:{}", withdrawal_key),
                    error.clone(),
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

    parse_bytes_vec_from_result(&keys_result)
}

async fn set_prices_on_chain(
    state: &Arc<AppState>,
    prices: &BTreeMap<String, CachedPrice>,
) -> Result<String, String> {
    let prices_vec: Vec<&CachedPrice> = prices.values().collect();
    let scval_arg = build_prices_scval(&prices_vec)?;

    submit_contract_transaction(
        state,
        &state.config.oracle_contract_id,
        "set_prices",
        &[&state.config.keeper_account_id, &scval_arg],
        1_000_000,
    )
    .await
}

async fn execute_order(state: &Arc<AppState>, key: &str) -> Result<String, String> {
    submit_handler_transaction(state, &state.config.order_handler_contract_id, "execute_order", key).await
}

async fn execute_deposit(state: &Arc<AppState>, key: &str) -> Result<String, String> {
    submit_handler_transaction(state, &state.config.deposit_handler_contract_id, "execute_deposit", key).await
}

async fn execute_withdrawal(state: &Arc<AppState>, key: &str) -> Result<String, String> {
    submit_handler_transaction(state, &state.config.withdrawal_handler_contract_id, "execute_withdrawal", key).await
}

async fn freeze_order(state: &Arc<AppState>, key: &str) -> Result<String, String> {
    submit_handler_transaction(state, &state.config.order_handler_contract_id, "freeze_order", key).await
}

async fn submit_handler_transaction(
    state: &Arc<AppState>,
    contract_id: &str,
    method: &str,
    key: &str,
) -> Result<String, String> {
    let key_hex = hex::decode(key).map_err(|e| format!("invalid key hex: {e}"))?;
    let key_scval = format!("Bytes({})", hex::encode(&key_hex));

    submit_contract_transaction(
        state,
        contract_id,
        method,
        &[&state.config.keeper_account_id, &key_scval],
        KEEPER_TX_FEE,
    )
    .await
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

    let response_json: serde_json::Value = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse RPC response: {e}"))?;

    if let Some(error) = response_json.get("error") {
        return Err(format!("Simulation error: {error}"));
    }

    let result = response_json
        .get("result")
        .ok_or_else(|| "Missing result in simulation response".to_string())?;

    Ok(result.to_string())
}

async fn submit_contract_transaction(
    state: &Arc<AppState>,
    contract_id: &str,
    method: &str,
    args: &[&str],
    fee: u32,
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
        "method": "sendTransaction",
        "params": {
            "transaction": {
                "source_account": state.config.keeper_account_id,
                "fee": fee.to_string(),
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

    let response_json: serde_json::Value = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse RPC response: {e}"))?;

    if let Some(error) = response_json.get("error") {
        return Err(format!("Submit error: {error}"));
    }

    let hash = response_json
        .get("result")
        .and_then(|r| r.get("hash"))
        .and_then(|h| h.as_str())
        .ok_or_else(|| "Missing hash in submit response".to_string())?;

    let hash = hash.to_string();
    info!(hash = %hash, "transaction_submitted");

    poll_transaction(state, &hash).await?;

    Ok(hash)
}

async fn poll_transaction(state: &Arc<AppState>, hash: &str) -> Result<(), String> {
    let rpc_url = &state.config.stellar_rpc_url;

    for attempt in 1..=MAX_POLL_ATTEMPTS {
        tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;

        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getTransaction",
            "params": {
                "hash": hash
            }
        });

        let response = state
            .http
            .post(rpc_url)
            .json(&payload)
            .send()
            .await
            .map_err(|e| format!("RPC request failed: {e}"))?;

        let response_json: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse RPC response: {e}"))?;

        let status = response_json
            .get("result")
            .and_then(|r| r.get("status"))
            .and_then(|s| s.as_str())
            .unwrap_or("UNKNOWN");

        match status {
            "SUCCESS" => {
                info!(hash = %hash, attempt, "transaction_confirmed");
                return Ok(());
            }
            "FAILED" => {
                let meta = response_json
                    .get("result")
                    .and_then(|r| r.get("resultMetaXdr"))
                    .and_then(|m| m.as_str())
                    .unwrap_or("n/a");
                return Err(format!(
                    "Transaction FAILED on-chain. hash={} meta={}",
                    &hash[..8.min(hash.len())],
                    &meta[..100.min(meta.len())]
                ));
            }
            _ => {
                info!(hash = %hash, attempt, max = MAX_POLL_ATTEMPTS, "transaction_pending");
            }
        }
    }

    Err(format!(
        "Transaction timed out after {}s: {}",
        (MAX_POLL_ATTEMPTS as u64 * POLL_INTERVAL_MS) / 1000,
        &hash[..8.min(hash.len())]
    ))
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
        .or_else(|| Some(&value))
        .ok_or_else(|| format!("expected vector, got: {value}"))?;

    match vec {
        serde_json::Value::Array(arr) => {
            let mut keys = Vec::new();
            for item in arr {
                if let Some(bytes) = item.get("bytes").or_else(|| item.get("Bytes")) {
                    if let Some(hex_str) = bytes.as_str() {
                        keys.push(hex_str.to_string());
                    }
                }
            }
            Ok(keys)
        }
        _ => Err(format!("expected array, got: {vec}")),
    }
}

use std::collections::BTreeMap;

fn build_prices_scval(prices: &[&CachedPrice]) -> Result<String, String> {
    let scval_entries: Vec<serde_json::Value> = prices
        .iter()
        .map(|_price| {
            serde_json::json!({
                "key": "prices",
                "val": {
                    "vec": prices.iter().map(|p| build_signed_price_scval(p)).collect::<Vec<_>>()
                }
            })
        })
        .collect();

    Ok(serde_json::to_string(&scval_entries).map_err(|e| format!("failed to serialize prices: {e}"))?)
}

fn build_signed_price_scval(price: &CachedPrice) -> serde_json::Value {
    let sig_bytes = hex::decode(&price.signature).unwrap_or_default();

    serde_json::json!({
        "map": [
            {"key": "keeper_index", "val": {"u32": 0}},
            {"key": "ledger_seq", "val": {"u32": price.ledger_seq}},
            {"key": "max_price", "val": {"i128": price.max}},
            {"key": "min_price", "val": {"i128": price.min}},
            {"key": "signature", "val": {"bytes": sig_bytes}},
            {"key": "timestamp", "val": {"u64": price.timestamp}},
            {"key": "token", "val": {"address": price.token_address}}
        ]
    })
}

async fn record_error(state: &Arc<AppState>, operation: impl Into<String>, error: impl Into<String>) {
    state.failures.lock().await.push(FailedSubmission {
        at: SystemTime::now(),
        operation: operation.into(),
        error: error.into(),
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
    // Keep only the last 100 executions
    if keeper_status.last_executions.len() > 100 {
        keeper_status.last_executions.remove(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Network, PriceFeedConfig, SecretString};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::time::Duration;

    fn test_state() -> Arc<AppState> {
        let config = Config {
            bind_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            network: Network::Testnet,
            network_passphrase: "Test SDF Network ; September 2015".to_string(),
            stellar_rpc_url: "http://localhost:0".to_string(),
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
            price_loop_interval: Duration::from_millis(1),
            keeper_loop_interval: Duration::from_millis(1),
            price_feed: PriceFeedConfig { tokens: vec![] },
        };
        Arc::new(AppState::new(Arc::new(config)))
    }

    #[test]
    fn test_parse_u32_from_result() {
        assert_eq!(parse_u32_from_result("42").unwrap(), 42);
        assert_eq!(parse_u32_from_result(r#"{"u32": 42}"#).unwrap(), 42);
    }

    #[test]
    fn test_build_signed_price_scval() {
        let price = CachedPrice {
            token_address: "CBAN5YU3KRDKPTQ2H76D6S7HQFPRBGUD524F65BUM2RQCITPTRLKWKES".to_string(),
            symbol: "TUSDC".to_string(),
            display_symbol: "USDC".to_string(),
            min: 1_000_000_000_000_000_000_000_000_000_000,
            max: 1_000_000_000_000_000_000_000_000_000_000,
            median: 1_000_000_000_000_000_000_000_000_000_000,
            timestamp: 1718400000,
            ledger_seq: 12345,
            sources_used: vec!["fixed".to_string()],
            signature: "00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000".to_string(),
        };

        let scval = build_signed_price_scval(&price);
        assert!(scval.get("map").is_some());
    }
}