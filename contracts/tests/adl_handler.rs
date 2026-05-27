//! Tests for ADL trigger boundary — issue #63.
//!
//! Verifies `is_adl_triggered` fires exactly at `maxPnlFactor`, not one unit
//! below, and fires one unit above.

#![cfg(test)]

use contracts::position_utils::is_adl_triggered;

// max_pnl_factor = 50% expressed in PRECISION units = 500_000
const MAX_PNL_FACTOR: u128 = 500_000;
// pool_value = 1_000_000 units
const POOL_VALUE: u128 = 1_000_000;
// Exact boundary: total_pnl / pool_value == 0.5
// => total_pnl = pool_value * max_pnl_factor / PRECISION = 500_000
const BOUNDARY_PNL: i128 = 500_000;

#[test]
fn test_adl_triggered_at_exact_boundary() {
    assert!(
        is_adl_triggered(BOUNDARY_PNL, POOL_VALUE, MAX_PNL_FACTOR),
        "ADL should trigger when total_pnl / pool_value == maxPnlFactor exactly"
    );
}

#[test]
fn test_adl_not_triggered_one_unit_below_boundary() {
    assert!(
        !is_adl_triggered(BOUNDARY_PNL - 1, POOL_VALUE, MAX_PNL_FACTOR),
        "ADL must NOT trigger when total_pnl is one unit below the boundary"
    );
}

#[test]
fn test_adl_triggered_one_unit_above_boundary() {
    assert!(
        is_adl_triggered(BOUNDARY_PNL + 1, POOL_VALUE, MAX_PNL_FACTOR),
        "ADL should trigger when total_pnl is one unit above the boundary"
    );
}

#[test]
fn test_adl_accounts_for_both_long_and_short_pnl() {
    // Net PnL = long_pnl - short_pnl; only triggers if net is positive and >= factor.
    let long_pnl: i128 = 700_000;
    let short_pnl: i128 = 300_000; // shorts are losing = negative contribution
    let net_pnl = long_pnl - short_pnl; // 400 < 500 boundary
    assert!(
        !is_adl_triggered(net_pnl, POOL_VALUE, MAX_PNL_FACTOR),
        "Net PnL below boundary should not trigger ADL"
    );

    let net_pnl_above = long_pnl - 100; // 600 > 500 boundary
    assert!(
        is_adl_triggered(net_pnl_above, POOL_VALUE, MAX_PNL_FACTOR),
        "Net PnL above boundary should trigger ADL"
    );
}
