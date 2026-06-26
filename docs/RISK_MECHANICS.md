# Risk Mechanics — Liquidation, ADL, Insurance Fund

## 1. Liquidation

### Formula

A position is **liquidatable** when its remaining collateral falls below the maintenance margin:

```
RemainingCollateral < MaintenanceMargin

where:
  RemainingCollateral = collateral_amount + PnL
  MaintenanceMargin   = notional * maintenance_margin_factor / PRECISION
  PRECISION           = 1_000_000
```

PnL for a **long**: `notional * (current_price - entry_price) / entry_price`
PnL for a **short**: `notional * (entry_price - current_price) / entry_price`

### Worked Example

| Field | Value |
|---|---|
| `collateral_amount` | 1,000 |
| `notional` (quantity) | 10,000 |
| `entry_price` | 100 |
| `current_price` | 89 |
| `maintenance_margin_factor` | 50,000 (= 5%) |

```
PnL = 10,000 * (89 - 100) / 100 = -1,100

RemainingCollateral = 1,000 + (-1,100) = capped at 0  →  0
MaintenanceMargin   = 10,000 * 50,000 / 1,000,000     = 500

0 < 500  →  LIQUIDATABLE ✓
```

At `current_price = 91`:
```
PnL = 10,000 * (91 - 100) / 100 = -900
RemainingCollateral = 1,000 - 900 = 100
MaintenanceMargin   = 500

100 < 500  →  still LIQUIDATABLE ✓
```

At `current_price = 96`:
```
PnL = 10,000 * (96 - 100) / 100 = -400
RemainingCollateral = 1,000 - 400 = 600
MaintenanceMargin   = 500

600 >= 500  →  NOT liquidatable ✓
```

### Keeper Workflow

1. Monitor open positions via `position_handler::is_liquidatable(position_key)`.
2. If `true`, call `order_handler::execute_liquidation(caller, position_key)`.
3. The contract fully closes the position at the current oracle price.
4. Any shortfall (bad debt) is covered by the insurance fund (see §3).

---

## 2. Auto-Deleveraging (ADL)

ADL reduces the most profitable positions when the pool's aggregate winning-side PnL threatens to exceed its capacity to pay.

### Trigger Condition

```
total_pnl * PRECISION >= pool_value * max_pnl_factor
```

Equivalently: `total_pnl / pool_value >= max_pnl_factor / PRECISION`

Implemented in `position_utils::is_adl_triggered`.

### Worked Example

| Field | Value |
|---|---|
| `pool_value` | 1,000,000 |
| `max_pnl_factor` | 500,000 (= 50%) |
| Boundary PnL | 500 |

```
Boundary: 500 * 1,000,000 >= 1,000,000 * 500,000  →  500,000,000 >= 500,000,000  TRIGGERS ✓
Below:    499 * 1,000,000 >= 1,000,000 * 500,000  →  499,000,000 >= 500,000,000  NO ✓
Above:    501 * 1,000,000 >= 1,000,000 * 500,000  →  501,000,000 >= 500,000,000  TRIGGERS ✓
```

**Net PnL** accounts for both sides: `total_pnl = long_pnl - short_losses`. ADL fires only when this net value is positive and meets the threshold.

### Keeper Workflow

1. Compute `total_pnl` by summing position PnLs for the winning side.
2. Read `pool_value` from the liquidity handler.
3. Call `position_utils::is_adl_triggered(total_pnl, pool_value, max_pnl_factor)`.
4. If triggered, identify the most profitable position(s) and call `order_handler::execute_adl(caller, position_key)` for each until the ratio falls below `max_pnl_factor`.

---

## 3. Insurance Fund

The insurance fund absorbs bad debt that arises when a liquidated position's collateral is insufficient to cover its losses.

**Bad debt** = `|PnL| - collateral_amount` when the position is already underwater at liquidation time.

The fund is topped up by a portion of trading fees. When the fund is depleted, remaining bad debt is socialised across LP holders via pool value reduction.

---

## 4. Edge Cases

| Scenario | Behaviour |
|---|---|
| Simultaneous liquidation and ADL on the same position | Liquidation takes precedence; ADL is skipped for that position |
| `pool_value == 0` | `is_adl_triggered` returns `false` to avoid division by zero |
| Negative `total_pnl` | ADL never triggers — the pool is not under pressure |
| Bad debt exceeds insurance fund | Shortfall socialised across LPs; pool value decreases proportionally |
| Position closed before keeper acts | `is_liquidatable` / `execute_liquidation` both check `is_open`; no-op if already closed |
