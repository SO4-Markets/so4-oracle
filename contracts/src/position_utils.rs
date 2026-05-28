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
pub fn is_liquidatable(
    pos: &PositionProps,
    current_price: u128,
    maintenance_margin_factor: u128,
    funding_factor: u128,
    borrowing_factor: u128,
) -> bool {
    if !pos.is_open {
        return false;
    }

    let pnl = calculate_pnl(pos, current_price);
    
    // Remaining Collateral = Collateral + PnL - fees
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

    let fee_amount = calculate_total_fees(pos.quantity, funding_factor, borrowing_factor);
    let collateral_after_fees = remaining_collateral.saturating_sub(fee_amount);

    // Maintenance Margin = Notional * Factor
    let maintenance_margin = pos.quantity * maintenance_margin_factor / PRECISION;

    collateral_after_fees < maintenance_margin
}

fn calculate_total_fees(quantity: u128, funding_factor: u128, borrowing_factor: u128) -> u128 {
    let funding_fee = quantity.saturating_mul(funding_factor) / PRECISION;
    let borrowing_fee = quantity.saturating_mul(borrowing_factor) / PRECISION;
    funding_fee.saturating_add(borrowing_fee)
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

