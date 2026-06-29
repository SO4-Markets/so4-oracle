use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use tokio::time::{interval, MissedTickBehavior};
use tracing::{error, info, warn};

use crate::chain::scval;
use crate::chain::tx_builder;
use crate::state::{AppState, CachedPrice, FailedSubmission, KeeperExecution};

const KEEPER_TX_FEE: u32 = 2_000_000;

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

    let result = execute_keeper_cycle(Arc::clone(&state)).await;

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

    let tx_hash = set_prices_on_chain(&state, &prices).await?;
    info!(hash = %tx_hash, "set_prices_confirmed");
    tokio::time::sleep(Duration::from_millis(5000)).await;

    let mut summary = CycleSummary {
        orders_executed: 0,
        deposits_executed: 0,
        withdrawals_executed: 0,
        errors: 0,
    };

    for order_key in &order_keys {
        match execute_handler(
            &state,
            &state.config.order_handler_contract_id,
            "execute_order",
            order_key,
        )
        .await
        {
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
                    match execute_handler(
                        &state,
                        &state.config.order_handler_contract_id,
                        "freeze_order",
                        order_key,
                    )
                    .await
                    {
                        Ok(_) => info!(key = %order_key, "order_frozen_budget_exceeded"),
                        Err(freeze_error) => {
                            error!(key = %order_key, %freeze_error, "freeze_order_failed");
                            freeze_error_msg = Some(freeze_error.clone());
                            record_error(&state, "freeze_order", &freeze_error, None).await;
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
        match execute_handler(
            &state,
            &state.config.deposit_handler_contract_id,
            "execute_deposit",
            deposit_key,
        )
        .await
        {
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
        match execute_handler(
            &state,
            &state.config.withdrawal_handler_contract_id,
            "execute_withdrawal",
            withdrawal_key,
        )
        .await
        {
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
    let prices_scval = scval::encode_prices_vec(&prices_vec)?;

    let sequence = get_account_sequence(state).await?;

    let tx = tx_builder::build_invoke_tx(
        &state.config.keeper_account_id,
        &state.config.oracle_contract_id,
        "set_prices",
        vec![prices_scval],
        1_000_000,
        sequence,
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

    let sequence = get_account_sequence(state).await?;

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
        KEEPER_TX_FEE,
        sequence,
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

async fn get_account_sequence(state: &Arc<AppState>) -> Result<u64, String> {
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
        .map_err(|e| format!("getAccount request failed: {e}"))?;

    let response_json: serde_json::Value = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse getAccount response: {e}"))?;

    if let Some(error) = response_json.get("error") {
        return Err(format!("getAccount error: {error}"));
    }

    let seq_str = response_json
        .get("result")
        .and_then(|r| r.get("sequence"))
        .and_then(|s| s.as_str())
        .ok_or_else(|| "Missing sequence in getAccount response".to_string())?;

    seq_str
        .parse::<u64>()
        .map_err(|e| format!("failed to parse sequence '{seq_str}': {e}"))
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
}
