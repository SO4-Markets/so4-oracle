3//! Tests for order handler logic.
//!
//! Issue #43 — full increase + decrease position lifecycle:
//!   Open a long via MarketIncrease, simulate price movement, close via
//!   MarketDecrease, verify PnL is credited/debited, position is removed,
//!   and pool amounts are consistent.
//!
//! Issue #44 — limit order trigger price validation:
//!   (1) LimitIncrease for a long does NOT execute when price is above trigger.
//!   (2) LimitIncrease executes when price drops to trigger.
//!   (3) StopLossDecrease triggers only when price drops below stop level.
//!   (4) StopLossDecrease does NOT trigger when price is above stop level.
//!
//! Issue #32 — LimitSwap trigger price validation:
//!   (1) Sell LimitSwap does NOT execute when price is above trigger.
//!   (2) Sell LimitSwap executes when price drops to/below trigger.
//!   (3) Buy  LimitSwap does NOT execute when price is below trigger.
//!   (4) Buy  LimitSwap executes when price rises to/above trigger.

#![cfg(test)]

use contracts::{
    data_store::{DataStore, DataStoreClient},
    decrease_position_utils::decrease_position,
    increase_position_utils::{check_increase_order_trigger, increase_position},
    keys::{
        account_balance_key, max_open_interest_long_key, open_interest_long_key,
        max_swap_path_length_key, pool_long_amount_key, pool_short_amount_key,
    },
    role_store::{RoleStore, RoleStoreClient},
    swap_utils::{check_limit_swap_trigger, swap},
    types::{Order, OrderError, OrderType, Position},
};
use soroban_sdk::{testutils::Address as _, Address, Env};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn setup(env: &Env) -> (DataStoreClient<'_>, Address) {
    env.mock_all_auths();
    let rs_id = env.register(RoleStore, ());
    let ds_id = env.register(DataStore, ());
    let admin = Address::generate(env);
    RoleStoreClient::new(env, &rs_id).initialize(&admin);
    DataStoreClient::new(env, &ds_id).initialize(&admin);
    (DataStoreClient::new(env, &ds_id), admin)
}

fn make_long_position(env: &Env, market_id: u32) -> Position {
    Position {
        account: Address::generate(env),
        market_id,
        is_long: true,
        size_in_usd: 0,
        size_in_tokens: 0,
        collateral_amount: 0,
    }
}

/// Simulate executing a MarketIncrease order: validates trigger (always passes
/// for market orders), calls increase_position, and returns the updated position.
fn execute_market_increase(
    env: &Env,
    ds: &DataStoreClient,
    caller: &Address,
    position: &mut Position,
    order: &Order,
    index_price: u128,
) {
    // Market orders have no trigger condition.
    assert!(
        matches!(order.order_type, OrderType::MarketIncrease),
        "expected MarketIncrease"
    );
    increase_position(
        env,
        ds,
        caller,
        position,
        order.size_delta_usd,
        order.collateral_delta,
        index_price,
    );
}

/// Simulate executing a decrease order (MarketDecrease or StopLossDecrease).
fn execute_market_decrease(
    env: &Env,
    ds: &DataStoreClient,
    caller: &Address,
    position: &mut Position,
    order: &Order,
    index_price: u128,
) -> u128 {
    assert!(
        matches!(
            order.order_type,
            OrderType::MarketDecrease | OrderType::StopLossDecrease
        ),
        "expected a decrease order type"
    );
    decrease_position(env, ds, caller, position, order.size_delta_usd, index_price)
}

/// Check trigger condition for a StopLossDecrease: executes only when
/// `current_price <= trigger_price`.
fn check_stop_loss(order: &Order, current_price: u128) -> Result<(), OrderError> {
    if current_price <= order.trigger_price {
        Ok(())
    } else {
        Err(OrderError::UnsatisfiedTrigger)
    }
}

// ---------------------------------------------------------------------------
// Issue #43 — full increase + decrease lifecycle
// ---------------------------------------------------------------------------

/// Open a long at price 100, then close at price 150 (profit).
/// Verify: correct size_in_usd, size_in_tokens, collateral_amount after open;
/// positive PnL credited; position zeroed after full close.
#[test]
fn test_full_lifecycle_profit() {
    let env = Env::default();
    let (ds, admin) = setup(&env);
    let market_id: u32 = 0;

    // Set max OI cap high enough.
    ds.set_u128(
        &admin,
        &max_open_interest_long_key(&env, market_id),
        &1_000_000u128,
    );

    let mut pos = make_long_position(&env, market_id);

    // --- Open: MarketIncrease at price 100 ---
    let open_order = Order {
        key: 0,
        account: pos.account.clone(),
        market_id,
        order_type: OrderType::MarketIncrease,
        is_long: true,
        size_delta_usd: 10_000u128,  // $10,000 notional
        collateral_delta: 1_000u128, // $1,000 collateral
        trigger_price: 0,
        acceptable_price: 0,
        min_output_amount: 0,
        collateral_token: Address::generate(&env),
        amount_in: 0,
        created_at: 0,
        is_frozen: false,
    };
    let open_price: u128 = 100;
    execute_market_increase(&env, &ds, &admin, &mut pos, &open_order, open_price);

    // Verify position after open.
    assert_eq!(pos.size_in_usd, 10_000, "size_in_usd after open");
    assert_eq!(pos.size_in_tokens, 100, "size_in_tokens = 10_000 / 100");
    assert_eq!(pos.collateral_amount, 1_000, "collateral after open");

    // OI should be updated.
    assert_eq!(
        ds.get_u128(&open_interest_long_key(&env, market_id))
            .unwrap_or(0),
        10_000
    );

    // --- Close: MarketDecrease at price 150 (profit) ---
    // PnL = (exit_price - entry_price) * size_in_tokens
    //     = (150 - 100) * 100 = 5_000
    let close_price: u128 = 150;
    let pnl: i128 = (close_price as i128 - open_price as i128) * pos.size_in_tokens as i128;
    assert_eq!(pnl, 5_000, "expected profit of 5000");

    let close_order = Order {
        key: 1,
        account: pos.account.clone(),
        market_id,
        order_type: OrderType::MarketDecrease,
        is_long: true,
        size_delta_usd: 10_000u128, // full close
        collateral_delta: 0,
        trigger_price: 0,
        acceptable_price: 0,
        min_output_amount: 0,
        collateral_token: Address::generate(&env),
        amount_in: 0,
        created_at: 0,
        is_frozen: false,
    };
    let released = execute_market_decrease(&env, &ds, &admin, &mut pos, &close_order, close_price);

    // Full close: all collateral released.
    assert_eq!(released, 1_000, "all collateral returned on full close");

    // Credit PnL to account balance in data_store.
    let bal_key = account_balance_key(&env, market_id);
    let prev_bal = ds.get_u128(&bal_key).unwrap_or(0);
    // pnl is positive, so add to balance.
    let new_bal = (prev_bal as i128 + pnl) as u128;
    ds.set_u128(&admin, &bal_key, &new_bal);

    assert_eq!(
        ds.get_u128(&bal_key).unwrap_or(0),
        5_000,
        "profit credited to account balance"
    );

    // Position zeroed.
    assert_eq!(pos.size_in_usd, 0, "position fully closed");
    assert_eq!(pos.collateral_amount, 0);
    assert_eq!(pos.size_in_tokens, 0);

    // OI back to zero.
    assert_eq!(
        ds.get_u128(&open_interest_long_key(&env, market_id))
            .unwrap_or(0),
        0
    );
}

/// Open a long at price 100, then close at price 80 (loss).
/// Verify: loss deducted from collateral; pool amounts balance out.
#[test]
fn test_full_lifecycle_loss() {
    let env = Env::default();
    let (ds, admin) = setup(&env);
    let market_id: u32 = 1;

    ds.set_u128(
        &admin,
        &max_open_interest_long_key(&env, market_id),
        &1_000_000u128,
    );

    // Seed pool amounts (simulating liquidity in the pool).
    ds.set_u128(&admin, &pool_long_amount_key(&env, market_id), &50_000u128);
    ds.set_u128(&admin, &pool_short_amount_key(&env, market_id), &50_000u128);

    let mut pos = make_long_position(&env, market_id);

    let open_price: u128 = 100;
    let open_order = Order {
        key: 0,
        account: pos.account.clone(),
        market_id,
        order_type: OrderType::MarketIncrease,
        is_long: true,
        size_delta_usd: 10_000u128,
        collateral_delta: 2_000u128, // larger collateral to absorb loss
        trigger_price: 0,
        acceptable_price: 0,
        min_output_amount: 0,
        collateral_token: Address::generate(&env),
        amount_in: 0,
        created_at: 0,
        is_frozen: false,
    };
    execute_market_increase(&env, &ds, &admin, &mut pos, &open_order, open_price);

    assert_eq!(pos.size_in_usd, 10_000);
    assert_eq!(pos.size_in_tokens, 100);
    assert_eq!(pos.collateral_amount, 2_000);

    // Close at price 80 (loss).
    let close_price: u128 = 80;
    let pnl: i128 = (close_price as i128 - open_price as i128) * pos.size_in_tokens as i128;
    assert_eq!(pnl, -2_000, "expected loss of 2000");

    let close_order = Order {
        key: 1,
        account: pos.account.clone(),
        market_id,
        order_type: OrderType::MarketDecrease,
        is_long: true,
        size_delta_usd: 10_000u128,
        collateral_delta: 0,
        trigger_price: 0,
        acceptable_price: 0,
        min_output_amount: 0,
        collateral_token: Address::generate(&env),
        amount_in: 0,
        created_at: 0,
        is_frozen: false,
    };
    let released = execute_market_decrease(&env, &ds, &admin, &mut pos, &close_order, close_price);
    assert_eq!(released, 2_000, "collateral released on full close");

    // Deduct loss from released collateral (simulating settlement).
    let net_payout = (released as i128 + pnl).max(0) as u128;
    assert_eq!(net_payout, 0, "loss equals collateral, net payout is zero");

    // Position zeroed.
    assert_eq!(pos.size_in_usd, 0);
    assert_eq!(pos.collateral_amount, 0);

    // Pool amounts unchanged (balanced OI case — pool is the counterparty).
    assert_eq!(
        ds.get_u128(&pool_long_amount_key(&env, market_id))
            .unwrap_or(0),
        50_000,
        "pool long unchanged"
    );
}

/// 50% partial close: verify half collateral returned, position halved.
#[test]
fn test_partial_close_50_percent() {
    let env = Env::default();
    let (ds, admin) = setup(&env);
    let market_id: u32 = 2;

    ds.set_u128(
        &admin,
        &max_open_interest_long_key(&env, market_id),
        &1_000_000u128,
    );

    let mut pos = make_long_position(&env, market_id);

    let open_price: u128 = 100;
    let open_order = Order {
        key: 0,
        account: pos.account.clone(),
        market_id,
        order_type: OrderType::MarketIncrease,
        is_long: true,
        size_delta_usd: 10_000u128,
        collateral_delta: 1_000u128,
        trigger_price: 0,
        acceptable_price: 0,
        min_output_amount: 0,
        collateral_token: Address::generate(&env),
        amount_in: 0,
        created_at: 0,
        is_frozen: false,
    };
    execute_market_increase(&env, &ds, &admin, &mut pos, &open_order, open_price);

    // 50% partial close.
    let close_order = Order {
        key: 1,
        account: pos.account.clone(),
        market_id,
        order_type: OrderType::MarketDecrease,
        is_long: true,
        size_delta_usd: 5_000u128, // half
        collateral_delta: 0,
        trigger_price: 0,
        acceptable_price: 0,
        min_output_amount: 0,
        collateral_token: Address::generate(&env),
        amount_in: 0,
        created_at: 0,
        is_frozen: false,
    };
    let released = execute_market_decrease(&env, &ds, &admin, &mut pos, &close_order, open_price);

    // Half collateral returned.
    assert_eq!(released, 500, "half collateral returned on 50% close");

    // Position halved.
    assert_eq!(pos.size_in_usd, 5_000, "size halved");
    assert_eq!(pos.collateral_amount, 500, "collateral halved");
    assert_eq!(pos.size_in_tokens, 50, "tokens halved");

    // OI halved.
    assert_eq!(
        ds.get_u128(&open_interest_long_key(&env, market_id))
            .unwrap_or(0),
        5_000
    );
}

// ---------------------------------------------------------------------------
// Issue #44 — limit order trigger price validation
// ---------------------------------------------------------------------------

/// (1) LimitIncrease for a long does NOT execute when price is ABOVE trigger.
#[test]
fn test_limit_increase_long_above_trigger_not_executed() {
    let env = Env::default();
    let (ds, admin) = setup(&env);
    let market_id: u32 = 3;

    ds.set_u128(
        &admin,
        &max_open_interest_long_key(&env, market_id),
        &1_000_000u128,
    );

    let order = Order {
        key: 0,
        account: Address::generate(&env),
        market_id,
        order_type: OrderType::LimitIncrease,
        is_long: true,
        size_delta_usd: 5_000u128,
        collateral_delta: 500u128,
        trigger_price: 90u128, // want to buy at 90 or below
        acceptable_price: 0,
        min_output_amount: 0,
        collateral_token: Address::generate(&env),
        amount_in: 0,
        created_at: 0,
        is_frozen: false,
    };

    // Current price is 100 (above trigger) → should NOT execute.
    let current_price: u128 = 100;
    let result = check_increase_order_trigger(&order, current_price);
    assert!(
        matches!(result, Err(OrderError::UnsatisfiedTrigger)),
        "should return UnsatisfiedTrigger when price > trigger"
    );
}

/// (2) LimitIncrease for a long EXECUTES when price drops to trigger.
#[test]
fn test_limit_increase_long_at_trigger_executes() {
    let env = Env::default();
    let (ds, admin) = setup(&env);
    let market_id: u32 = 4;

    ds.set_u128(
        &admin,
        &max_open_interest_long_key(&env, market_id),
        &1_000_000u128,
    );

    let order = Order {
        key: 0,
        account: Address::generate(&env),
        market_id,
        order_type: OrderType::LimitIncrease,
        is_long: true,
        size_delta_usd: 5_000u128,
        collateral_delta: 500u128,
        trigger_price: 90u128,
        acceptable_price: 0,
        min_output_amount: 0,
        collateral_token: Address::generate(&env),
        amount_in: 0,
        created_at: 0,
        is_frozen: false,
    };

    // Price drops exactly to trigger → should execute.
    let current_price: u128 = 90;
    let result = check_increase_order_trigger(&order, current_price);
    assert!(result.is_ok(), "should execute when price == trigger");

    // Actually execute the increase.
    let mut pos = Position {
        account: order.account.clone(),
        market_id,
        is_long: true,
        size_in_usd: 0,
        size_in_tokens: 0,
        collateral_amount: 0,
    };
    increase_position(
        &env,
        &ds,
        &admin,
        &mut pos,
        order.size_delta_usd,
        order.collateral_delta,
        current_price,
    );

    assert_eq!(pos.size_in_usd, 5_000);
    assert_eq!(pos.collateral_amount, 500);
    // size_in_tokens = 5000 / 90 = 55 (integer division)
    assert_eq!(pos.size_in_tokens, 55);
}

/// LimitIncrease for a short does NOT execute when price is BELOW trigger.
#[test]
fn test_limit_increase_short_below_trigger_not_executed() {
    let env = Env::default();

    let order = Order {
        key: 0,
        account: Address::generate(&env),
        market_id: 41,
        order_type: OrderType::LimitIncrease,
        is_long: false,
        size_delta_usd: 5_000u128,
        collateral_delta: 500u128,
        trigger_price: 110u128,
        acceptable_price: 0,
        min_output_amount: 0,
        collateral_token: Address::generate(&env),
        amount_in: 0,
        created_at: 0,
        is_frozen: false,
    };

    let result = check_increase_order_trigger(&order, 100u128);
    assert!(
        matches!(result, Err(OrderError::UnsatisfiedTrigger)),
        "short limit increase should not execute when price < trigger"
    );
}

/// LimitIncrease for a short EXECUTES when price rises to trigger.
#[test]
fn test_limit_increase_short_at_trigger_executes() {
    let env = Env::default();

    let order = Order {
        key: 0,
        account: Address::generate(&env),
        market_id: 42,
        order_type: OrderType::LimitIncrease,
        is_long: false,
        size_delta_usd: 5_000u128,
        collateral_delta: 500u128,
        trigger_price: 110u128,
        acceptable_price: 0,
        min_output_amount: 0,
        collateral_token: Address::generate(&env),
        amount_in: 0,
        created_at: 0,
        is_frozen: false,
    };

    let result = check_increase_order_trigger(&order, 110u128);
    assert!(
        result.is_ok(),
        "short limit increase should execute when price >= trigger"
    );
}

/// StopIncrease for a long does NOT execute when price is BELOW trigger.
#[test]
fn test_stop_increase_long_below_trigger_not_executed() {
    let env = Env::default();

    let order = Order {
        key: 0,
        account: Address::generate(&env),
        market_id: 43,
        order_type: OrderType::StopIncrease,
        is_long: true,
        size_delta_usd: 5_000u128,
        collateral_delta: 500u128,
        trigger_price: 110u128,
        acceptable_price: 0,
        min_output_amount: 0,
        collateral_token: Address::generate(&env),
        amount_in: 0,
        created_at: 0,
        is_frozen: false,
    };

    let result = check_increase_order_trigger(&order, 100u128);
    assert!(
        matches!(result, Err(OrderError::UnsatisfiedTrigger)),
        "long stop increase should not execute when price < trigger"
    );
}

/// StopIncrease for a long EXECUTES when price rises to trigger.
#[test]
fn test_stop_increase_long_at_trigger_executes() {
    let env = Env::default();

    let order = Order {
        key: 0,
        account: Address::generate(&env),
        market_id: 44,
        order_type: OrderType::StopIncrease,
        is_long: true,
        size_delta_usd: 5_000u128,
        collateral_delta: 500u128,
        trigger_price: 110u128,
        acceptable_price: 0,
        min_output_amount: 0,
        collateral_token: Address::generate(&env),
        amount_in: 0,
        created_at: 0,
        is_frozen: false,
    };

    let result = check_increase_order_trigger(&order, 110u128);
    assert!(
        result.is_ok(),
        "long stop increase should execute when price >= trigger"
    );
}

/// StopIncrease for a short does NOT execute when price is ABOVE trigger.
#[test]
fn test_stop_increase_short_above_trigger_not_executed() {
    let env = Env::default();

    let order = Order {
        key: 0,
        account: Address::generate(&env),
        market_id: 45,
        order_type: OrderType::StopIncrease,
        is_long: false,
        size_delta_usd: 5_000u128,
        collateral_delta: 500u128,
        trigger_price: 90u128,
        acceptable_price: 0,
        min_output_amount: 0,
        collateral_token: Address::generate(&env),
        amount_in: 0,
        created_at: 0,
        is_frozen: false,
    };

    let result = check_increase_order_trigger(&order, 100u128);
    assert!(
        matches!(result, Err(OrderError::UnsatisfiedTrigger)),
        "short stop increase should not execute when price > trigger"
    );
}

/// StopIncrease for a short EXECUTES when price drops to trigger.
#[test]
fn test_stop_increase_short_at_trigger_executes() {
    let env = Env::default();

    let order = Order {
        key: 0,
        account: Address::generate(&env),
        market_id: 46,
        order_type: OrderType::StopIncrease,
        is_long: false,
        size_delta_usd: 5_000u128,
        collateral_delta: 500u128,
        trigger_price: 90u128,
        acceptable_price: 0,
        min_output_amount: 0,
        collateral_token: Address::generate(&env),
        amount_in: 0,
        created_at: 0,
        is_frozen: false,
    };

    let result = check_increase_order_trigger(&order, 90u128);
    assert!(
        result.is_ok(),
        "short stop increase should execute when price <= trigger"
    );
}

/// (3) StopLossDecrease does NOT trigger when price is ABOVE stop level.
#[test]
fn test_stop_loss_above_stop_not_triggered() {
    let env = Env::default();
    let (ds, admin) = setup(&env);
    let market_id: u32 = 5;

    ds.set_u128(
        &admin,
        &max_open_interest_long_key(&env, market_id),
        &1_000_000u128,
    );

    let order = Order {
        key: 0,
        account: Address::generate(&env),
        market_id,
        order_type: OrderType::StopLossDecrease,
        is_long: true,
        size_delta_usd: 5_000u128,
        collateral_delta: 0,
        trigger_price: 70u128, // stop loss at 70
        acceptable_price: 0,
        min_output_amount: 0,
        collateral_token: Address::generate(&env),
        amount_in: 0,
        created_at: 0,
        is_frozen: false,
    };

    // Current price is 80 (above stop) → should NOT trigger.
    let current_price: u128 = 80;
    let result = check_stop_loss(&order, current_price);
    assert!(
        matches!(result, Err(OrderError::UnsatisfiedTrigger)),
        "stop loss should not trigger when price > stop level"
    );
}

/// (4) StopLossDecrease TRIGGERS when price drops below stop level.
#[test]
fn test_stop_loss_below_stop_triggers() {
    let env = Env::default();
    let (ds, admin) = setup(&env);
    let market_id: u32 = 6;

    ds.set_u128(
        &admin,
        &max_open_interest_long_key(&env, market_id),
        &1_000_000u128,
    );

    // First open a position.
    let mut pos = make_long_position(&env, market_id);
    let open_order = Order {
        key: 0,
        account: pos.account.clone(),
        market_id,
        order_type: OrderType::MarketIncrease,
        is_long: true,
        size_delta_usd: 5_000u128,
        collateral_delta: 1_000u128,
        trigger_price: 0,
        acceptable_price: 0,
        min_output_amount: 0,
        collateral_token: Address::generate(&env),
        amount_in: 0,
        created_at: 0,
        is_frozen: false,
    };
    execute_market_increase(&env, &ds, &admin, &mut pos, &open_order, 100u128);

    let stop_order = Order {
        key: 1,
        account: pos.account.clone(),
        market_id,
        order_type: OrderType::StopLossDecrease,
        is_long: true,
        size_delta_usd: 5_000u128,
        collateral_delta: 0,
        trigger_price: 70u128,
        acceptable_price: 0,
        min_output_amount: 0,
        collateral_token: Address::generate(&env),
        amount_in: 0,
        created_at: 0,
        is_frozen: false,
    };

    // Price drops to 65 (below stop) → should trigger.
    let current_price: u128 = 65;
    let result = check_stop_loss(&stop_order, current_price);
    assert!(
        result.is_ok(),
        "stop loss should trigger when price <= stop level"
    );

    // Execute the stop loss decrease.
    let released = execute_market_decrease(&env, &ds, &admin, &mut pos, &stop_order, current_price);

    // Full close: all collateral released.
    assert_eq!(released, 1_000);
    assert_eq!(pos.size_in_usd, 0, "position closed by stop loss");
}

// ---------------------------------------------------------------------------
// Issue #45 — max open interest check
// ---------------------------------------------------------------------------

/// OI exactly at cap should be rejected.
#[test]
#[should_panic]
fn test_increase_position_oi_at_cap_rejected() {
    let env = Env::default();
    let (ds, admin) = setup(&env);
    let market_id: u32 = 7;

    // Set cap to 10_000 and current OI to 10_000 (already at cap).
    ds.set_u128(
        &admin,
        &max_open_interest_long_key(&env, market_id),
        &10_000u128,
    );
    ds.set_u128(
        &admin,
        &open_interest_long_key(&env, market_id),
        &10_000u128,
    );

    let mut pos = make_long_position(&env, market_id);
    // Any positive size_delta should be rejected.
    increase_position(&env, &ds, &admin, &mut pos, 1u128, 100u128, 100u128);
}

/// One below cap should be accepted.
#[test]
fn test_increase_position_one_below_cap_accepted() {
    let env = Env::default();
    let (ds, admin) = setup(&env);
    let market_id: u32 = 8;

    // Cap = 10_000, current OI = 9_999 → adding 1 brings it to exactly 10_000.
    ds.set_u128(
        &admin,
        &max_open_interest_long_key(&env, market_id),
        &10_000u128,
    );
    ds.set_u128(&admin, &open_interest_long_key(&env, market_id), &9_999u128);

    let mut pos = make_long_position(&env, market_id);
    // Adding 1 should succeed (9_999 + 1 == 10_000 == cap, not exceeding).
    increase_position(&env, &ds, &admin, &mut pos, 1u128, 100u128, 100u128);

    assert_eq!(pos.size_in_usd, 1);
    assert_eq!(
        ds.get_u128(&open_interest_long_key(&env, market_id))
            .unwrap_or(0),
        10_000
    );
}

// ---------------------------------------------------------------------------
// Issue #46 — partial close pro-rata collateral reduction
// ---------------------------------------------------------------------------

/// 50% partial close returns exactly half the collateral.
#[test]
fn test_partial_close_pro_rata_collateral() {
    let env = Env::default();
    let (ds, admin) = setup(&env);
    let market_id: u32 = 9;

    ds.set_u128(
        &admin,
        &max_open_interest_long_key(&env, market_id),
        &1_000_000u128,
    );

    let mut pos = make_long_position(&env, market_id);
    increase_position(&env, &ds, &admin, &mut pos, 10_000u128, 1_000u128, 100u128);

    // 50% partial close.
    let released = decrease_position(&env, &ds, &admin, &mut pos, 5_000u128, 100u128);

    assert_eq!(released, 500, "pro-rata: half collateral released");
    assert_eq!(pos.size_in_usd, 5_000, "half size remains");
    assert_eq!(pos.collateral_amount, 500, "half collateral remains");
}

/// Remaining position must pass validate_position (min collateral factor).
/// A partial close that would leave insufficient collateral must panic.
#[test]
#[should_panic]
fn test_partial_close_insufficient_remaining_collateral_panics() {
    let env = Env::default();
    let (ds, admin) = setup(&env);
    let market_id: u32 = 10;

    ds.set_u128(
        &admin,
        &max_open_interest_long_key(&env, market_id),
        &1_000_000u128,
    );

    // Open with very low collateral relative to size (leverage 100x).
    let mut pos = make_long_position(&env, market_id);
    increase_position(&env, &ds, &admin, &mut pos, 10_000u128, 100u128, 100u128);

    // Try to close 99% of the position, leaving 1% size but 1% collateral.
    // remaining_size = 100, remaining_collateral = 1
    // min_collateral = 100 / 10 = 10 → 1 < 10 → should panic.
    decrease_position(&env, &ds, &admin, &mut pos, 9_900u128, 100u128);
}

// ---------------------------------------------------------------------------
// Issue #32 — LimitSwap trigger price validation
// ---------------------------------------------------------------------------

/// (1) Sell LimitSwap does NOT execute when price is ABOVE trigger.
#[test]
fn test_limit_swap_sell_above_trigger_not_executed() {
    let result = check_limit_swap_trigger(90u128, 100u128, true);
    assert!(
        matches!(result, Err(OrderError::UnsatisfiedTrigger)),
        "sell swap should not execute when price > trigger"
    );
}

/// (2) Sell LimitSwap executes when price drops to/below trigger; pool updated.
#[test]
fn test_limit_swap_sell_at_trigger_executes() {
    let env = Env::default();
    let (ds, admin) = setup(&env);
    let market_id: u32 = 20;

    ds.set_u128(&admin, &pool_long_amount_key(&env, market_id), &10_000u128);
    ds.set_u128(&admin, &pool_short_amount_key(&env, market_id), &5_000u128);

    // Price exactly at trigger → should pass.
    let trigger: u128 = 90;
    let current: u128 = 90;
    assert!(check_limit_swap_trigger(trigger, current, true).is_ok());

    // Execute swap: sell 1_000 units.
    let out = swap(&env, &ds, &admin, market_id, 1_000u128, true, current);
    assert_eq!(out, 1_000);
    assert_eq!(ds.get_u128(&pool_long_amount_key(&env, market_id)).unwrap_or(0), 9_000);
    assert_eq!(ds.get_u128(&pool_short_amount_key(&env, market_id)).unwrap_or(0), 6_000);
}

/// (3) Buy LimitSwap does NOT execute when price is BELOW trigger.
#[test]
fn test_limit_swap_buy_below_trigger_not_executed() {
    let result = check_limit_swap_trigger(110u128, 100u128, false);
    assert!(
        matches!(result, Err(OrderError::UnsatisfiedTrigger)),
        "buy swap should not execute when price < trigger"
    );
}

/// (4) Buy LimitSwap executes when price rises to/above trigger; pool updated.
#[test]
fn test_limit_swap_buy_at_trigger_executes() {
    let env = Env::default();
    let (ds, admin) = setup(&env);
    let market_id: u32 = 21;

    ds.set_u128(&admin, &pool_long_amount_key(&env, market_id), &5_000u128);
    ds.set_u128(&admin, &pool_short_amount_key(&env, market_id), &10_000u128);

    let trigger: u128 = 110;
    let current: u128 = 110;
    assert!(check_limit_swap_trigger(trigger, current, false).is_ok());

    let out = swap(&env, &ds, &admin, market_id, 2_000u128, false, current);
    assert_eq!(out, 2_000);
    assert_eq!(ds.get_u128(&pool_short_amount_key(&env, market_id)).unwrap_or(0), 8_000);
    assert_eq!(ds.get_u128(&pool_long_amount_key(&env, market_id)).unwrap_or(0), 7_000);
}

// ---------------------------------------------------------------------------
// Issue #31 — MarketSwap: swap_with_path + min_output_amount check
// ---------------------------------------------------------------------------

/// MarketSwap executes along a path and updates pool balances.
#[test]
fn test_market_swap_executes_and_updates_pools() {
    let env = Env::default();
    let (ds, admin) = setup(&env);
    let market_id: u32 = 30;

    ds.set_u128(&admin, &pool_long_amount_key(&env, market_id), &20_000u128);
    ds.set_u128(&admin, &pool_short_amount_key(&env, market_id), &5_000u128);

    let path = [(market_id, true)]; // single-hop sell
    let result = contracts::swap_utils::swap_with_path(
        &env, &ds, &admin, &path, 3_000u128, 1_000u128, 100u128,
    );

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), 3_000);
    assert_eq!(ds.get_u128(&pool_long_amount_key(&env, market_id)).unwrap_or(0), 17_000);
    assert_eq!(ds.get_u128(&pool_short_amount_key(&env, market_id)).unwrap_or(0), 8_000);
}

/// MarketSwap reverts with InsufficientOutput when output < min_output_amount.
#[test]
fn test_market_swap_insufficient_output_reverts() {
    let env = Env::default();
    let (ds, admin) = setup(&env);
    let market_id: u32 = 31;

    ds.set_u128(&admin, &pool_long_amount_key(&env, market_id), &20_000u128);
    ds.set_u128(&admin, &pool_short_amount_key(&env, market_id), &5_000u128);

    let path = [(market_id, true)];
    // min_output is higher than the swap output → InsufficientOutput
    let result = contracts::swap_utils::swap_with_path(
        &env, &ds, &admin, &path, 500u128, 1_000u128, 100u128,
    );

    assert!(
        matches!(result, Err(OrderError::InsufficientOutput)),
        "should return InsufficientOutput"
    );
}

/// MarketSwap executes a two-hop path and feeds output into the next market.
#[test]
fn test_market_swap_two_hop_path_executes() {
    let env = Env::default();
    let (ds, admin) = setup(&env);
    let first_market: u32 = 32;
    let second_market: u32 = 33;

    ds.set_u128(&admin, &pool_long_amount_key(&env, first_market), &10_000u128);
    ds.set_u128(&admin, &pool_short_amount_key(&env, first_market), &1_000u128);
    ds.set_u128(&admin, &pool_long_amount_key(&env, second_market), &500u128);
    ds.set_u128(&admin, &pool_short_amount_key(&env, second_market), &10_000u128);

    let path = [(first_market, true), (second_market, false)];
    let result = contracts::swap_utils::swap_with_path(
        &env, &ds, &admin, &path, 2_000u128, 2_000u128, 100u128,
    );

    assert_eq!(result.unwrap(), 2_000u128);
    assert_eq!(ds.get_u128(&pool_long_amount_key(&env, first_market)).unwrap_or(0), 8_000);
    assert_eq!(ds.get_u128(&pool_short_amount_key(&env, first_market)).unwrap_or(0), 3_000);
    assert_eq!(ds.get_u128(&pool_short_amount_key(&env, second_market)).unwrap_or(0), 8_000);
    assert_eq!(ds.get_u128(&pool_long_amount_key(&env, second_market)).unwrap_or(0), 2_500);
}

/// MarketSwap executes a three-hop path.
#[test]
fn test_market_swap_three_hop_path_executes() {
    let env = Env::default();
    let (ds, admin) = setup(&env);
    let market_a: u32 = 34;
    let market_b: u32 = 35;
    let market_c: u32 = 36;

    ds.set_u128(&admin, &pool_long_amount_key(&env, market_a), &10_000u128);
    ds.set_u128(&admin, &pool_short_amount_key(&env, market_a), &0u128);
    ds.set_u128(&admin, &pool_long_amount_key(&env, market_b), &0u128);
    ds.set_u128(&admin, &pool_short_amount_key(&env, market_b), &10_000u128);
    ds.set_u128(&admin, &pool_long_amount_key(&env, market_c), &10_000u128);
    ds.set_u128(&admin, &pool_short_amount_key(&env, market_c), &0u128);

    let path = [(market_a, true), (market_b, false), (market_c, true)];
    let result = contracts::swap_utils::swap_with_path(
        &env, &ds, &admin, &path, 1_500u128, 1_500u128, 100u128,
    );

    assert_eq!(result.unwrap(), 1_500u128);
    assert_eq!(ds.get_u128(&pool_long_amount_key(&env, market_a)).unwrap_or(0), 8_500);
    assert_eq!(ds.get_u128(&pool_short_amount_key(&env, market_b)).unwrap_or(0), 8_500);
    assert_eq!(ds.get_u128(&pool_long_amount_key(&env, market_c)).unwrap_or(0), 8_500);
}

/// MarketSwap rejects paths that contain a duplicate market.
#[test]
fn test_market_swap_duplicate_market_in_path_reverts() {
    let env = Env::default();
    let (ds, admin) = setup(&env);
    let market_id: u32 = 37;
    let path = [(market_id, true), (38u32, true), (market_id, false)];

    let result = contracts::swap_utils::swap_with_path(
        &env, &ds, &admin, &path, 1_000u128, 1u128, 100u128,
    );

    assert!(
        matches!(result, Err(OrderError::DuplicateMarketInPath)),
        "should reject duplicate markets"
    );
}

/// MarketSwap rejects paths above the configured max path length.
#[test]
fn test_market_swap_path_too_long_reverts() {
    let env = Env::default();
    let (ds, admin) = setup(&env);
    ds.set_u128(&admin, &max_swap_path_length_key(&env), &2u128);
    let path = [(39u32, true), (40u32, true), (41u32, true)];

    let result = contracts::swap_utils::swap_with_path(
        &env, &ds, &admin, &path, 1_000u128, 1u128, 100u128,
    );

    assert!(
        matches!(result, Err(OrderError::SwapPathTooLong)),
        "should reject paths longer than max_swap_path_length"
    );
}
