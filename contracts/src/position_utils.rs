use crate::types::PositionProps;
use crate::libs::math::checked_sub_u128;

/// Precision denominator for margin factors and price calculations.
pub const PRECISION: u128 = 1_000_000;

/// Returns whether a position is liquidatable based on current oracle price.
///
/// A position is liquidatable if:
/// `RemainingCollateral < MaintenanceMargin`
/// where `RemainingCollateral = Collateral + PnL`
/// and `MaintenanceMargin = Notional * MaintenanceMarginFactor`
///
/// Callers that perform liquidation checks (e.g. `PositionHandler::is_liquidatable`)
/// must supply the **worst-case** price (`maximize = true`) — the lower price for
/// longs and the higher price for shorts — so the check is conservative.
pub fn is_liquidatable(
    pos: &PositionProps,
    current_price: u128,
    maintenance_margin_factor: u128,
) -> bool {
    if !pos.is_open {
        return false;
    }

    let pnl = calculate_pnl(pos, current_price);
    
    // Remaining Collateral = Collateral + PnL
    // If PnL is negative and exceeds collateral, remaining is 0.
    let remaining_collateral = if pnl >= 0 {
        pos.collateral_amount + (pnl as u128)
    } else {
        let abs_pnl = pnl.unsigned_abs();
        if abs_pnl >= pos.collateral_amount {
            0
        } else {
            checked_sub_u128(pos.collateral_amount, abs_pnl)
        }
    };

    // Maintenance Margin = Notional * Factor
    let maintenance_margin = pos.quantity * maintenance_margin_factor / PRECISION;

    remaining_collateral < maintenance_margin
}

/// Returns whether the pool's total PnL exposure has reached `max_pnl_factor`.
///
/// ADL is triggered when: `total_pnl / pool_value >= max_pnl_factor / PRECISION`
/// i.e. `total_pnl * PRECISION >= pool_value * max_pnl_factor`
///
/// Returns `false` when `pool_value` is zero to avoid division by zero.
pub fn is_adl_triggered(total_pnl: i128, pool_value: u128, max_pnl_factor: u128) -> bool {
    if pool_value == 0 || total_pnl <= 0 {
        return false;
    }
    let pnl = total_pnl as u128;
    pnl * PRECISION >= pool_value * max_pnl_factor
}

/// Calculates PnL for a position.
/// PnL = Notional * (CurrentPrice / AveragePrice - 1) for Long
/// PnL = Notional * (1 - CurrentPrice / AveragePrice) for Short
pub fn calculate_pnl(pos: &PositionProps, current_price: u128) -> i128 {
    if pos.average_price == 0 {
        return 0;
    }

    // PnL = Notional * (CurrentPrice - EntryPrice) / EntryPrice
    // To maintain precision: (Notional * (CurrentPrice - EntryPrice)) / EntryPrice
    
    if pos.is_long {
        if current_price >= pos.average_price {
            let diff = checked_sub_u128(current_price, pos.average_price);
            (pos.quantity * diff / pos.average_price) as i128
        } else {
            let diff = checked_sub_u128(pos.average_price, current_price);
            -((pos.quantity * diff / pos.average_price) as i128)
        }
    } else {
        if current_price <= pos.average_price {
            let diff = checked_sub_u128(pos.average_price, current_price);
            (pos.quantity * diff / pos.average_price) as i128
        } else {
            let diff = checked_sub_u128(current_price, pos.average_price);
            -((pos.quantity * diff / pos.average_price) as i128)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::PositionProps;
    use soroban_sdk::{testutils::Address as _, BytesN, Env};

    fn borderline_long(env: &Env) -> PositionProps {
        // quantity=1000, collateral=50, entry=100, margin_factor=10% (100_000/1_000_000)
        // maintenance_margin = 1000 * 100_000 / 1_000_000 = 100
        //
        // maximize=true  (long_price=90):
        //   PnL = 1000*(90-100)/100 = -100 → remaining = max(50-100,0) = 0 < 100 → liquidatable
        //
        // maximize=false (short_price=110):
        //   PnL = 1000*(110-100)/100 = +100 → remaining = 50+100 = 150 >= 100 → NOT liquidatable
        PositionProps {
            position_key: BytesN::from_array(env, &[1u8; 32]),
            account: soroban_sdk::Address::generate(env),
            market_id: 0,
            quantity: 1_000,
            collateral_amount: 50,
            average_price: 100,
            is_long: true,
            is_open: true,
            referral_code: BytesN::from_array(env, &[0u8; 32]),
        }
    }

    #[test]
    fn test_maximize_true_flags_borderline_long_as_liquidatable() {
        let env = Env::default();
        let pos = borderline_long(&env);
        let margin_factor = 100_000; // 10 %
        // maximize = true: worst-case price for a long is the lower price.
        assert!(is_liquidatable(&pos, 90, margin_factor));
    }

    #[test]
    fn test_maximize_false_does_not_flag_borderline_long() {
        let env = Env::default();
        let pos = borderline_long(&env);
        let margin_factor = 100_000;
        // maximize = false: optimistic price for a long is the higher price.
        // The same position that is liquidatable at 90 is healthy at 110.
        assert!(!is_liquidatable(&pos, 110, margin_factor));
    }
}

