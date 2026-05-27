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

