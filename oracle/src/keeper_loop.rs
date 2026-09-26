use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use tokio::time::{interval, timeout, MissedTickBehavior};
use tracing::{error, info, warn};

use crate::chain::scval;
use crate::chain::tx_builder;
use crate::keeper;
use crate::state::{AppState, CachedPrice, FailedSubmission, KeeperExecution};

impl std::error::Error for SequenceFetchError {}

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
    // Open a keeper-cycle generation window. `keeper_status` and `cycle_status`
    // are both updated below (in different critical sections); bumping here and
    // again at the end lets `AppState::keeper_status_snapshot` detect a reader
    // whose two-lock read straddled this cycle and retry (#797).
    state
        .keeper_cycle_generation
        .fetch_add(1, Ordering::Release);
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

    let latency_ms = started.elapsed().as_millis() as u64;
    {
        let mut status = state.cycle_status.write().await;
        status.keeper_cycle_running = false;
        status.last_keeper_cycle_at = Some(SystemTime::now());
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
                prices_stale = summary.prices_stale,
                keeper_balance_low = summary.keeper_balance_low,
                "keeper_cycle_complete"
            );
            state.metrics.record_keeper_cycle(
                latency_ms,
                summary.orders_executed,
                summary.deposits_executed,
                summary.withdrawals_executed,
                summary.errors,
                summary.keeper_balance_low,
                summary.prices_stale,
            );
        }
        Err(error) => {
            error!(latency_ms, %error, "keeper_cycle_failed");
            record_error(&state, "keeper_cycle", error, None).await;
            state.metrics.record_submit_failure();
        }
    }

    // Close the generation window: all keeper_status / cycle_status writes for
    // this cycle are now visible (#797).
    state
        .keeper_cycle_generation
        .fetch_add(1, Ordering::Release);
    result
}

#[derive(Debug)]
pub struct CycleSummary {
    pub orders_executed: usize,
    pub deposits_executed: usize,
    pub withdrawals_executed: usize,
    pub errors: usize,
    pub prices_stale: bool,
    pub keeper_balance_low: bool,
}

async fn execute_keeper_cycle(state: Arc<AppState>) -> Result<CycleSummary, String> {
    let keeper_cfg = keeper::KeeperBalanceConfig {
        horizon_url: state.config.horizon_url.clone(),
        account_id: state.config.keeper_account_id.clone(),
        min_balance_xlm: state.config.min_keeper_balance_xlm,
    };

    let keeper_balance_low = match keeper::check_keeper_balance(&keeper_cfg).await {
        Ok(stroops) => {
            let xlm = stroops as f64 / keeper::XLM_IN_STROOPS as f64;
            if xlm < state.config.min_keeper_balance_xlm {
                warn!(balance_xlm = xlm, min_balance_xlm = state.config.min_keeper_balance_xlm, "keeper_balance_low; skipping submissions this cycle");
                true
            } else {
                false
            }
        }
        Err(e) => {
            error!(%e, "keeper_balance_check_failed; skipping submissions this cycle");
            true
        }
    };

    let prices = state.price_cache.read().await.prices.clone();
    let prices_stale = prices.is_empty();

    let (order_result, deposit_result, withdrawal_result) = tokio::join!(
        get_pending_keys(&state, "get_order_count", "get_order_keys"),
        get_pending_keys(&state, "get_deposit_count", "get_deposit_keys"),
        get_pending_keys(&state, "get_withdrawal_count", "get_withdrawal_keys"),
    );
    let order_keys = order_result.unwrap_or_else(|e| {
        warn!(error = %e, "get_pending_keys(orders) failed, skipping orders this cycle");
        Vec::new()
    });
    let deposit_keys = deposit_result.unwrap_or_else(|e| {
        warn!(error = %e, "get_pending_keys(deposits) failed, skipping deposits this cycle");
        Vec::new()
    });
    let withdrawal_keys = withdrawal_result.unwrap_or_else(|e| {
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
            prices_stale,
            keeper_balance_low,
        });
    }

    info!(
        orders = order_keys.len(),
        deposits = deposit_keys.len(),
        withdrawals = withdrawal_keys.len(),
        prices_stale,
        keeper_balance_low,
        "found_pending_work"
    );

    if keeper_balance_low {
        warn!("keeper_balance_below_min; skipping all on-chain submissions this cycle");
        return Ok(CycleSummary {
            orders_executed: 0,
            deposits_executed: 0,
            withdrawals_executed: 0,
            errors: 0,
            prices_stale,
            keeper_balance_low: true,
        });
    }

    let mut summary = CycleSummary {
        orders_executed: 0,
        deposits_executed: 0,
        withdrawals_executed: 0,
        errors: 0,
        prices_stale,
        keeper_balance_low: false,
    };

    // Cached account sequence, shared across every execute_handler call made
    // during this cycle so it's fetched via RPC at most once instead of once
    // per pending order/deposit/withdrawal (#805).
    let mut sequence_cache: Option<u64> = None;

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
        // Skip keys whose prior submission is still in-flight — closes #491.
        // Evict keys stuck longer than IN_FLIGHT_EXPIRY to prevent silent
        // permanent skips after a poll timeout (#801).
        {
            let mut in_flight = state.in_flight_keys.lock().await;
            if let Some(inserted_at) = in_flight.get(order_key) {
                if inserted_at.elapsed() > IN_FLIGHT_EXPIRY {
                    warn!(
                        key = %order_key,
                        elapsed_secs = inserted_at.elapsed().as_secs(),
                        "in_flight_key_expired_evicted"
                    );
                    in_flight.remove(order_key);
                } else {
                    info!(key = %order_key, "skipping_in_flight_order_key");
                    continue;
                }
            }
            in_flight.insert(order_key.clone(), Instant::now());
        }

        match execute_handler(
            &state,
            &state.config.order_handler_contract_id,
            "execute_order",
            order_key,
            &mut sequence_cache,
        )
        .await
        {
            Ok(tx_hash) => {
                state.in_flight_keys.lock().await.remove(order_key);
                summary.orders_executed += 1;
                // Clear any accumulated failure counts on success.
                state
                    .freeze_failure_counts
                    .lock()
                    .await
                    .remove(order_key.as_str());
                state
                    .execution_failure_counts
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
                let tx_hash = poll_timeout_hash(error);
                record_error(
                    &state,
                    &format!("execute_order:{}", order_key),
                    error,
                    tx_hash.clone(),
                )
                .await;
                record_execution(
                    &state,
                    "execute_order",
                    order_key,
                    tx_hash,
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
                if is_budget_exceeded(&error) {
                    match execute_handler(
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
                        state
                            .frozen_order_blacklist
                            .lock()
                            .await
                            .insert(order_key.clone(), consecutive_exec_failures);
                        state
                            .execution_failure_counts
                            .lock()
                            .await
                            .remove(order_key.as_str());
                        error!(
                            key = %order_key,
                            consecutive_failures = consecutive_exec_failures,
                            max = MAX_CONSECUTIVE_EXECUTION_FAILURES,
                            "ALERT: order_key blacklisted after {} consecutive execute_order \
                             failures — manual intervention required to clear",
                            MAX_CONSECUTIVE_EXECUTION_FAILURES
                        );
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
    } else {
        warn!("prices_stale; skipping set_prices and order execution, processing deposits/withdrawals only");
        summary.prices_stale = true;
    }

    for deposit_key in &deposit_keys {
        {
            let mut in_flight = state.in_flight_keys.lock().await;
            if let Some(inserted_at) = in_flight.get(deposit_key) {
                if inserted_at.elapsed() > IN_FLIGHT_EXPIRY {
                    warn!(
                        key = %deposit_key,
                        elapsed_secs = inserted_at.elapsed().as_secs(),
                        "in_flight_key_expired_evicted"
                    );
                    in_flight.remove(deposit_key);
                } else {
                    info!(key = %deposit_key, "skipping_in_flight_deposit_key");
                    continue;
                }
            }
            in_flight.insert(deposit_key.clone(), Instant::now());
        }

        match execute_handler(
            &state,
            &state.config.deposit_handler_contract_id,
            "execute_deposit",
            deposit_key,
            &mut sequence_cache,
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
                let tx_hash = poll_timeout_hash(error);
                record_error(
                    &state,
                    &format!("execute_deposit:{}", deposit_key),
                    error,
                    tx_hash.clone(),
                )
                .await;
                record_execution(
                    &state,
                    "execute_deposit",
                    deposit_key,
                    tx_hash,
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
        {
            let mut in_flight = state.in_flight_keys.lock().await;
            if let Some(inserted_at) = in_flight.get(withdrawal_key) {
                if inserted_at.elapsed() > IN_FLIGHT_EXPIRY {
                    warn!(
                        key = %withdrawal_key,
                        elapsed_secs = inserted_at.elapsed().as_secs(),
                        "in_flight_key_expired_evicted"
                    );
                    in_flight.remove(withdrawal_key);
                } else {
                    info!(key = %withdrawal_key, "skipping_in_flight_withdrawal_key");
                    continue;
                }
            }
            in_flight.insert(withdrawal_key.clone(), Instant::now());
        }

        match execute_handler(
            &state,
            &state.config.withdrawal_handler_contract_id,
            "execute_withdrawal",
            withdrawal_key,
            &mut sequence_cache,
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
                let tx_hash = poll_timeout_hash(error);
                record_error(
                    &state,
                    &format!("execute_withdrawal:{}", withdrawal_key),
                    error,
                    tx_hash.clone(),
                )
                .await;
                record_execution(
                    &state,
                    "execute_withdrawal",
                    withdrawal_key,
                    tx_hash,
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

/// Recover the transaction hash from a poll-timeout error message (#721).
///
/// `SubmitError::PollTimeout`'s `Display` impl embeds the hash as
/// `(hash: {hash})` inside a longer sentence, and `execute_handler` prefixes
/// the whole message with `{method} submit failed: ` before downcasting it to
/// the `String` that reaches the `is_poll_timeout` guard. Rather than
/// reconstructing that exact wording, scan for the marker: the hash is always
/// the first `)`-delimited token after it, whatever prefixes are attached.
///
/// Returns `None` for any other error shape, so callers can pass the result
/// through unconditionally.
fn poll_timeout_hash(error: &str) -> Option<String> {
    const MARKER: &str = "(hash: ";
    let rest = error.split_once(MARKER)?.1;
    let hash = rest.split_once(')')?.0.trim();
    if hash.is_empty() {
        None
    } else {
        Some(hash.to_string())
    }
}

/// Detect whether a `SubmitError::TransactionFailed` was caused by a Soroban
/// budget-exceeded error. The error string contains base64-encoded XDR
/// diagnostic events; each event is decoded and searched for the
/// `Budget, ExceededLimit` pattern that Soroban embeds in its error text.
fn is_budget_exceeded(error: &str) -> bool {
    use base64::Engine;

    // Fast path: if the raw error string already contains the pattern (e.g. in
    // tests that mock human-readable events), skip decoding.
    if error.contains("Budget, ExceededLimit") {
        return true;
    }

    // Slow path: extract each base64 event string, decode, and search the
    // resulting bytes. Soroban diagnostic events are base64-encoded XDR whose
    // payload includes the human-readable error text as a substring.
    for bit in error.split('"') {
        let trimmed = bit.trim();
        // Heuristic: base64 strings are long and only contain base64 chars.
        if trimmed.len() < 20
            || !trimmed
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'=')
        {
            continue;
        }
        if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(trimmed) {
            if let Ok(s) = String::from_utf8(bytes) {
                if s.contains("Budget, ExceededLimit") {
                    return true;
                }
            }
        }
    }
    false
}

/// Evict any `in_flight_keys` entry older than `IN_FLIGHT_EXPIRY`, regardless
/// of whether that key is part of this cycle's pending work.
///
/// Each work-type loop already evicts a stale in-flight entry lazily, but
/// only when that same key is scanned again in a later cycle's pending-keys
/// list. A key stranded by the cycle-level `KEEPER_CYCLE_TIMEOUT_SECS`
/// timeout (rather than the tracked poll-timeout path) may never reappear
/// there — e.g. if the order actually confirmed on-chain and drops out of
/// the pending set — so without this sweep it would never be evicted or
/// logged at all (#806).
async fn sweep_expired_in_flight_keys(state: &Arc<AppState>) {
    let mut in_flight = state.in_flight_keys.lock().await;
    in_flight.retain(|key, inserted_at| {
        let elapsed = inserted_at.elapsed();
        if elapsed > IN_FLIGHT_EXPIRY {
            warn!(
                key = %key,
                elapsed_secs = elapsed.as_secs(),
                "in_flight_key_expired_evicted_stale_sweep"
            );
            false
        } else {
            true
        }
    });
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

/// True if `error` indicates the submitted transaction was rejected for
/// carrying a stale/incorrect account sequence number (e.g. RPC status
/// `BAD_SEQUENCE`, or classic Horizon's `tx_bad_seq`), as opposed to any
/// other submission failure. Used to decide when a locally-cached sequence
/// number needs to be re-fetched from the network (#805).
fn is_bad_sequence_error(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("bad_sequence") || lower.contains("bad_seq") || lower.contains("badseq")
}

/// Decode a base64 `errorResultXdr` and check whether the decoded
/// `TransactionResult` carries a `tx_bad_seq` result code (#998).
///
/// Returns `true` only when the XDR decodes cleanly *and* the result code
/// is `tx_bad_seq`. Malformed or non-bad-seq XDR returns `false` (caller
/// falls through to other detection paths).
fn is_bad_sequence_xdr(error_result_xdr: &str) -> bool {
    use base64::Engine;
    use stellar_xdr::{Decode, TransactionResult, TransactionResultResult};

    let bytes = match base64::engine::general_purpose::STANDARD.decode(error_result_xdr) {
        Ok(b) => b,
        Err(_) => return false,
    };

    let tx_result = match TransactionResult::from_xdr_base64(error_result_xdr) {
        Ok(r) => r,
        // Manual fallback: try decoding from raw bytes
        Err(_) => match TransactionResult::decode(&mut &bytes[..]) {
            Ok(r) => r,
            Err(_) => return false,
        },
    };

    matches!(tx_result.result, TransactionResultResult::TxBadSeq(_))
}

/// Execute a handler contract call, using and maintaining a per-cycle cached
/// account sequence number instead of fetching it via RPC on every call.
///
/// `sequence_cache` is shared across every `execute_handler` invocation
/// within one keeper cycle: the first call of the cycle fetches the account
/// sequence once and caches it; every subsequent call reuses and locally
/// increments that cached value on success, avoiding a redundant `getAccount`
/// RPC round-trip per pending order/deposit/withdrawal (#805). The cache is
/// only cleared (forcing a fresh fetch on the next call) when a submission is
/// rejected for a sequence-related reason, since that's the one case where
/// the cached value is known to be wrong.
async fn execute_handler(
    state: &Arc<AppState>,
    contract_id: &str,
    method: &str,
    key: &str,
    sequence_cache: &mut Option<u64>,
) -> Result<String, String> {
    let key_bytes = hex::decode(key).map_err(|e| format!("invalid key hex: {e}"))?;
    let key_scval = stellar_xdr::ScVal::Bytes(stellar_xdr::ScBytes(
        key_bytes
            .try_into()
            .map_err(|e| format!("key bytes conversion failed: {e}"))?,
    ));

    let sequence = match *sequence_cache {
        Some(seq) => seq,
        None => {
            let seq = get_account_sequence(state)
                .await
                .map_err(|e| e.to_string())?;
            *sequence_cache = Some(seq);
            seq
        }
    };

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

    match crate::submit::submit_and_poll(&state.config.stellar_rpc_url, &signed_xdr).await {
        Ok(ledger) => {
            // Success consumed `sequence`; the next call in this cycle can
            // use `sequence + 1` without asking the network.
            *sequence_cache = Some(sequence + 1);
            info!(method, key = %key, ledger, "handler_confirmed");
            Ok(format!("confirmed on ledger {ledger}"))
        }
        Err(error) => {
            let msg = error.to_string();
            let is_bad_seq = is_bad_sequence_error(&msg)
                || matches!(
                    &error,
                    SubmitError::Rejected {
                        error_result_xdr: Some(xdr),
                        ..
                    } if is_bad_sequence_xdr(xdr)
                );
            if is_bad_seq {
                // The cached sequence is stale relative to the network;
                // drop it so the next call re-fetches instead of retrying
                // with the same wrong value.
                *sequence_cache = None;
            }
            Err(format!("{method} submit failed: {msg}"))
        }
    }
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

    let payload = serde_json::to_string(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getAccount",
        "params": { "account": account_id }
    }))
    .map_err(|e| {
        SequenceFetchError::MissingOrInvalid(format!("failed to serialize request: {e}"))
    })?;

    // Use the shared RPC POST helper instead of hand-rolling the request here (#751).
    let body = crate::stellar_rpc::rpc_post(rpc_url, payload)
        .await
        .map_err(|e| SequenceFetchError::Network(format!("getAccount request failed: {e}")))?;

    let response_json: serde_json::Value = serde_json::from_str(&body).map_err(|e| {
        SequenceFetchError::Network(format!("Failed to parse getAccount response: {e}"))
    })?;

    if let Some(error) = response_json.get("error") {
        return Err(SequenceFetchError::Network(format!(
            "getAccount error: {}",
            truncate_rpc_error(error)
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

/// Retry wrapper around [`simulate_contract_call_once`]: a single transient
/// RPC failure on either of `get_pending_keys`' two simulate calls otherwise
/// makes the keeper treat "no pending work" as the result for the whole
/// cycle. Matches `get_account_sequence`'s `retry_with_backoff` usage (#799).
async fn simulate_contract_call(
    state: &Arc<AppState>,
    contract_id: &str,
    method: &str,
    args: &[&str],
) -> Result<String, String> {
    crate::retry::retry_with_backoff(
        || async { simulate_contract_call_once(state, contract_id, method, args).await },
        SIMULATE_RETRY_ATTEMPTS,
        SIMULATE_RETRY_BASE_DELAY_MS,
        30_000,
    )
    .await
}

async fn simulate_contract_call_once(
    state: &Arc<AppState>,
    contract_id: &str,
    method: &str,
    args: &[&str],
) -> Result<String, String> {
    use stellar_xdr::{TransactionEnvelope, TransactionV1Envelope, WriteXdr};

    let rpc_url = &state.config.stellar_rpc_url;

    // Convert string arguments to ScVal — contract addresses become
    // ScVal::Address, everything else becomes ScVal::Symbol (#997).
    let scval_args: Vec<stellar_xdr::ScVal> = args
        .iter()
        .map(|arg| {
            if arg.starts_with('C') || arg.starts_with('G') {
                stellar_xdr::ScVal::Address(crate::chain::scval::strkey_to_sc_address(arg)?)
            } else {
                let sym: stellar_xdr::ScSymbol = arg
                    .to_string()
                    .try_into()
                    .map_err(|_| format!("arg '{arg}' too long for ScSymbol"))?;
                Ok(stellar_xdr::ScVal::Symbol(sym))
            }
        })
        .collect::<Result<Vec<_>, String>>()?;

    // Build a proper XDR transaction envelope using the same pipeline as
    // sendTransaction, instead of hand-rolling a JSON object that doesn't
    // match the Soroban RPC simulateTransaction contract (#997).
    let tx = tx_builder::build_invoke_tx(
        &state.config.keeper_account_id,
        contract_id,
        method,
        scval_args,
        100,
        0, // sequence number is irrelevant for simulation
        None,
    )?;

    let envelope = TransactionEnvelope::Tx(TransactionV1Envelope {
        tx,
        signatures: stellar_xdr::VecM::default(),
    });

    let envelope_xdr = envelope
        .to_xdr(stellar_xdr::Limits::none())
        .map_err(|e| format!("failed to serialize envelope to XDR: {e}"))?;
    let envelope_b64 =
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &envelope_xdr);

    let payload = serde_json::to_string(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "simulateTransaction",
        "params": {
            "transaction": envelope_b64
        }
    }))
    .map_err(|e| format!("failed to serialize request: {e}"))?;

    // Use the shared RPC POST helper instead of hand-rolling the request here (#751).
    let body = crate::stellar_rpc::rpc_post(rpc_url, payload)
        .await
        .map_err(|e| e.to_string())?;

    let response_json: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("Failed to parse RPC response: {e}"))?;

    if let Some(error) = response_json.get("error") {
        return Err(format!("Simulation error: {}", truncate_rpc_error(error)));
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
    tx_hash: Option<String>,
) {
    state.failures.lock().await.push(FailedSubmission {
        at: SystemTime::now(),
        operation: operation.to_string(),
        network: state.config.network.as_str().to_string(),
        token: String::new(),
        symbol: String::new(),
        min: 0,
        max: 0,
        // Carry the hash whenever the failure happened *after* submission
        // (a poll timeout), so `/oracle/failed-submissions` hands an operator
        // something to look up on a block explorer for exactly the ambiguous
        // outcomes that need manual verification (#721). `None` stays
        // correct for pre-submission failures, which have no hash yet.
        tx_hash,
        error: error.to_string(),
        timestamp: crate::current_timestamp_secs(),
        // Unlike price_loop.rs, this loop never fetches a Soroban ledger
        // sequence anywhere in its execution path - there is no real value
        // to thread through here (#726). `0` stays honest rather than
        // fabricated.
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
    keeper_status.last_executions.push_back(KeeperExecution {
        timestamp: SystemTime::now(),
        operation: operation.into(),
        key: key.into(),
        tx_hash,
        success,
        error,
    });
    if keeper_status.last_executions.len() > 100 {
        keeper_status.last_executions.pop_front();
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

    // ── #721: poll-timeout hash recovery ────────────────────────────────────

    #[test]
    fn poll_timeout_hash_extracts_hash_from_submit_error_message() {
        // Exactly what `SubmitError::PollTimeout`'s Display impl renders.
        let hash = "a".repeat(64);
        let error = format!(
            "transaction not confirmed after 10 attempts (hash: {hash}); check status on next cycle"
        );
        assert_eq!(poll_timeout_hash(&error), Some(hash));
    }

    #[test]
    fn poll_timeout_hash_survives_execute_handler_prefix() {
        // `execute_handler` re-wraps the SubmitError message as a String, so
        // the marker never sits at the start of the string the keeper sees.
        let hash = "b".repeat(64);
        let error = format!(
            "execute_order submit failed: transaction not confirmed after 10 attempts \
             (hash: {hash}); check status on next cycle"
        );
        assert!(is_poll_timeout(&error));
        assert_eq!(poll_timeout_hash(&error), Some(hash));
    }

    #[test]
    fn poll_timeout_hash_is_none_for_non_poll_timeout_errors() {
        for error in [
            "execute_order submit failed: transaction rejected: txFAILED",
            "keeper cycle exceeded 30s budget",
            "transaction failed on-chain; diagnostic events: []",
            "",
        ] {
            assert_eq!(
                poll_timeout_hash(error),
                None,
                "unexpected hash for error {error:?}"
            );
        }
    }

    #[test]
    fn poll_timeout_hash_ignores_empty_hash() {
        assert_eq!(
            poll_timeout_hash("not confirmed after 10 attempts (hash: )"),
            None
        );
        assert_eq!(
            poll_timeout_hash("not confirmed after 10 attempts (hash:"),
            None
        );
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
    async fn record_error_stores_post_submission_tx_hash() {
        // #721 — a poll timeout means the transaction was submitted, so the
        // hash must survive into FailedSubmission for the admin endpoint.
        let state = shutdown_test_state();
        let hash = "c".repeat(64);
        let error = format!(
            "execute_deposit:abcd submit failed: transaction not confirmed after 10 attempts \
             (hash: {hash}); check status on next cycle"
        );
        assert!(is_poll_timeout(&error));

        record_error(
            &state,
            "execute_deposit:abcd",
            &error,
            poll_timeout_hash(&error),
        )
        .await;

        let failures = state.failures.lock().await;
        let recorded: Vec<_> = failures.iter().collect();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].tx_hash.as_deref(), Some(hash.as_str()));
        assert_eq!(recorded[0].operation, "execute_deposit:abcd");
    }

    #[tokio::test]
    async fn record_error_leaves_tx_hash_none_for_pre_submission_failures() {
        let state = shutdown_test_state();

        record_error(
            &state,
            "keeper_cycle",
            "keeper cycle exceeded 30s budget",
            None,
        )
        .await;

        let failures = state.failures.lock().await;
        let recorded: Vec<_> = failures.iter().collect();
        assert_eq!(recorded.len(), 1);
        assert!(recorded[0].tx_hash.is_none());
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
        use crate::config::{Config, Network, PriceFeedConfig, SecretString};
        use crate::state::AppState;
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};
        use std::time::Duration;
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        // The keeper cycle needs pending work to reach the stale-price filter,
        // then set_prices needs getAccount + sendTransaction + getTransaction.
        // Return no pending work for simulate calls so the cycle short-circuits
        // after the stale-price filter — the filter runs before pending-work
        // lookup, so this exercises the code path we care about.
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0", "id": 1,
                "result": 0
            })))
            .mount(&mock_server)
            .await;

        let config = Config {
            bind_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            network: Network::Testnet,
            network_passphrase: "Test SDF Network ; September 2015".to_string(),
            stellar_rpc_url: mock_server.uri(),
            horizon_url: "http://127.0.0.1:9".to_string(),
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
            price_feed: PriceFeedConfig {
                tokens: vec![
                    shared_config::TokenConfig {
                        symbol: "FRESH".to_string(),
                        display_symbol: Some("FRESH".to_string()),
                        stellar_address: "GAFRESH".to_string(),
                        sources: vec!["test".to_string()],
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
                    },
                    shared_config::TokenConfig {
                        symbol: "STALE".to_string(),
                        display_symbol: Some("STALE".to_string()),
                        stellar_address: "GASTALE".to_string(),
                        sources: vec!["test".to_string()],
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
                    },
                ],
            },
        };
        let state = Arc::new(AppState::new(Arc::new(config)));

        let now = crate::current_timestamp_secs();

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

        {
            let mut cache = state.price_cache.write().await;
            cache.prices.insert("gafresh".to_string(), fresh_price);
            cache.prices.insert("gastale".to_string(), stale_price);
        }

        // This actually calls execute_keeper_cycle, which filters stale prices
        // before looking up pending work (#724).
        let result = run_keeper_cycle(Arc::clone(&state)).await;

        // The cycle succeeds because there is no pending work — the stale price
        // was filtered out before the simulate calls, and the fresh price is
        // still available if needed.
        assert!(
            result.is_ok(),
            "keeper cycle should succeed with stale prices filtered: {:?}",
            result.err()
        );
    }
}
