//! Utilities for executing swap orders.
//!
//! Issue #32: implement the `LimitSwap` branch — trigger price check.
//! Issue #31: implement `swap_with_path` for `MarketSwap` — validates
//!   `min_output_amount` and updates pool accounting.

use soroban_sdk::Env;

use crate::{
    data_store::DataStoreClient,
    keys::{max_swap_path_length_key, pool_long_amount_key, pool_short_amount_key},
    types::OrderError,
};

/// Check whether the current price satisfies the trigger condition for a
/// `LimitSwap` order.
///
/// * `is_sell` – `true` for sell swaps (execute when `price ≤ trigger`),
///               `false` for buy swaps (execute when `price ≥ trigger`).
///
/// Returns `Ok(())` when the condition is met, `Err(OrderError::UnsatisfiedTrigger)`
/// otherwise.
pub fn check_limit_swap_trigger(
    trigger_price: u128,
    current_price: u128,
    is_sell: bool,
) -> Result<(), OrderError> {
    let satisfied = if is_sell {
        current_price <= trigger_price
    } else {
        current_price >= trigger_price
    };

    if satisfied {
        Ok(())
    } else {
        Err(OrderError::UnsatisfiedTrigger)
    }
}

/// Execute a swap: move `amount` from the long pool to the short pool (or
/// vice-versa for a buy swap) at `execution_price`.
///
/// This is a minimal implementation used by the order handler. Real swap
/// logic would involve token transfers; here we update the pool accounting
/// in the data store.
///
/// # Panics
/// Panics if the source pool has insufficient balance.
pub fn swap(
    env: &Env,
    ds: &DataStoreClient,
    caller: &soroban_sdk::Address,
    market_id: u32,
    amount: u128,
    is_sell: bool,
    execution_price: u128,
) -> u128 {
    let long_key = pool_long_amount_key(env, market_id);
    let short_key = pool_short_amount_key(env, market_id);

    // amount_out is the equivalent value at execution_price.
    // For simplicity: amount_out = amount (1:1 accounting in pool tokens).
    let _ = execution_price; // used by callers to satisfy the trigger check

    if is_sell {
        // Sell: reduce long pool, increase short pool.
        let long_bal = ds.get_u128(&long_key).unwrap_or(0);
        assert!(long_bal >= amount, "insufficient long pool balance");
        ds.set_u128(caller, &long_key, &(long_bal - amount));

        let short_bal = ds.get_u128(&short_key).unwrap_or(0);
        ds.set_u128(caller, &short_key, &(short_bal + amount));
    } else {
        // Buy: reduce short pool, increase long pool.
        let short_bal = ds.get_u128(&short_key).unwrap_or(0);
        assert!(short_bal >= amount, "insufficient short pool balance");
        ds.set_u128(caller, &short_key, &(short_bal - amount));

        let long_bal = ds.get_u128(&long_key).unwrap_or(0);
        ds.set_u128(caller, &long_key, &(long_bal + amount));
    }

    amount
}

/// Execute a `MarketSwap` along a multi-hop path.
///
/// Each hop in `path` is a `(market_id, is_sell)` pair. The output of one
/// hop becomes the input of the next. After all hops, the final output is
/// checked against `min_output_amount`; if it falls short the function
/// returns `Err(OrderError::InsufficientOutput)`.
///
/// On success the function returns `Ok(output_amount)` and the caller is
/// responsible for removing the order record and emitting `order_exec`.
pub fn swap_with_path(
    env: &Env,
    ds: &DataStoreClient,
    caller: &soroban_sdk::Address,
    path: &[(u32, bool)],   // (market_id, is_sell) per hop
    amount_in: u128,
    min_output_amount: u128,
    execution_price: u128,
) -> Result<u128, OrderError> {
    let max_path_length = ds
        .get_u128(&max_swap_path_length_key(env))
        .unwrap_or(u128::MAX);
    if path.len() as u128 > max_path_length {
        return Err(OrderError::SwapPathTooLong);
    }

    for i in 0..path.len() {
        for j in (i + 1)..path.len() {
            if path[i].0 == path[j].0 {
                return Err(OrderError::DuplicateMarketInPath);
            }
        }
    }

    let mut amount = amount_in;

    for &(market_id, is_sell) in path {
        amount = swap(env, ds, caller, market_id, amount, is_sell, execution_price);
    }

    if amount < min_output_amount {
        return Err(OrderError::InsufficientOutput);
    }

    Ok(amount)
}
