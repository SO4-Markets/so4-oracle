//! Utilities for increasing (opening or adding to) a trading position.
//!
//! Issue #45: After computing the new `size_in_usd`, check it against the
//! `max_open_interest` cap stored in `data_store`. Revert with
//! [`PositionError::MaxOpenInterestExceeded`] if the total OI would exceed
//! the cap.

use soroban_sdk::{panic_with_error, Env};

use crate::{
    data_store::DataStoreClient,
    keys::{max_open_interest_long_key, max_open_interest_short_key,
           open_interest_long_key, open_interest_short_key},
    types::{Position, PositionError},
};

/// Apply a size increase to `position`, updating the data-store OI counters.
///
/// # Errors
/// Panics with [`PositionError::MaxOpenInterestExceeded`] when
/// `current_oi + size_delta_usd > max_open_interest`.
pub fn increase_position(
    env: &Env,
    ds: &DataStoreClient,
    caller: &soroban_sdk::Address,
    position: &mut Position,
    size_delta_usd: u128,
    collateral_delta: u128,
    index_price: u128,
) {
    // Determine which OI keys to use.
    let (oi_key, max_oi_key) = if position.is_long {
        (
            open_interest_long_key(env, position.market_id),
            max_open_interest_long_key(env, position.market_id),
        )
    } else {
        (
            open_interest_short_key(env, position.market_id),
            max_open_interest_short_key(env, position.market_id),
        )
    };

    let current_oi = ds.get_u128(&oi_key).unwrap_or(0);
    let max_oi = ds.get_u128(&max_oi_key).unwrap_or(u128::MAX);

    let new_oi = current_oi.saturating_add(size_delta_usd);
    if new_oi > max_oi {
        panic_with_error!(env, PositionError::MaxOpenInterestExceeded);
    }

    // Update position fields.
    position.size_in_usd = position.size_in_usd.saturating_add(size_delta_usd);
    position.collateral_amount = position.collateral_amount.saturating_add(collateral_delta);

    // size_in_tokens = size_in_usd / index_price (integer division).
    if index_price > 0 {
        position.size_in_tokens = position.size_in_usd / index_price;
    }

    // Persist updated OI.
    ds.set_u128(caller, &oi_key, &new_oi);
}
