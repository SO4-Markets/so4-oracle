//! Utilities for decreasing (partially or fully closing) a trading position.
//!
//! Issue #46: When `size_delta_usd < position.size_in_usd` (partial close),
//! the released collateral is proportional:
//!   `released_collateral = collateral × (size_delta / size_in_usd)`
//!
//! The remaining position must still satisfy the minimum collateral factor
//! after the close.

use soroban_sdk::{panic_with_error, Env};

use crate::{
    data_store::DataStoreClient,
    keys::{open_interest_long_key, open_interest_short_key},
    types::{Position, PositionError},
};

/// Minimum collateral factor denominator. A factor of 1/10 means the
/// remaining collateral must be at least 10% of the remaining size.
pub const MIN_COLLATERAL_FACTOR_DENOM: u128 = 10;

/// Decrease `position` by `size_delta_usd`.
///
/// Returns the amount of collateral released to the caller.
///
/// # Behaviour
/// - **Full close** (`size_delta_usd >= position.size_in_usd`): all collateral
///   is released and the position is zeroed out.
/// - **Partial close**: released collateral is pro-rata; the remaining
///   position is validated against the minimum collateral factor.
///
/// # Errors
/// Panics with [`PositionError::InsufficientCollateral`] when the remaining
/// collateral would fall below `remaining_size / MIN_COLLATERAL_FACTOR_DENOM`.
pub fn decrease_position(
    env: &Env,
    ds: &DataStoreClient,
    caller: &soroban_sdk::Address,
    position: &mut Position,
    size_delta_usd: u128,
    index_price: u128,
) -> u128 {
    if size_delta_usd == 0 || position.size_in_usd == 0 {
        return 0;
    }

    let is_full_close = size_delta_usd >= position.size_in_usd;

    let released_collateral = if is_full_close {
        // Full close: release everything.
        position.collateral_amount
    } else {
        // Pro-rata: released = collateral × (size_delta / size_in_usd).
        position.collateral_amount * size_delta_usd / position.size_in_usd
    };

    let actual_delta = if is_full_close {
        position.size_in_usd
    } else {
        size_delta_usd
    };

    let remaining_size = position.size_in_usd.saturating_sub(actual_delta);
    let remaining_collateral = position.collateral_amount.saturating_sub(released_collateral);

    // Validate remaining position (only for partial closes).
    if !is_full_close && remaining_size > 0 {
        let min_collateral = remaining_size / MIN_COLLATERAL_FACTOR_DENOM;
        if remaining_collateral < min_collateral {
            panic_with_error!(env, PositionError::InsufficientCollateral);
        }
    }

    // Update position.
    position.size_in_usd = remaining_size;
    position.collateral_amount = remaining_collateral;
    if index_price > 0 && remaining_size > 0 {
        position.size_in_tokens = remaining_size / index_price;
    } else {
        position.size_in_tokens = 0;
    }

    // Update OI in data_store.
    let oi_key = if position.is_long {
        open_interest_long_key(env, position.market_id)
    } else {
        open_interest_short_key(env, position.market_id)
    };
    let current_oi = ds.get_u128(&oi_key).unwrap_or(0);
    ds.set_u128(caller, &oi_key, &current_oi.saturating_sub(actual_delta));

    released_collateral
}
