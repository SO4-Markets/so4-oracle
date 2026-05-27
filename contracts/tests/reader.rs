//! Tests for the Reader contract.
//!
//! Issue #60 — ADL queue prioritisation (most profitable first):
//!   `get_adl_targets` returns positions sorted by descending unrealised PnL,
//!   limited by a `count` parameter.

#![cfg(test)]

use contracts::{
    data_store::{DataStore, DataStoreClient},
    liquidity_handler::{LiquidityHandler, LiquidityHandlerClient},
    reader::{Reader, ReaderClient},
    role_store::{RoleStore, RoleStoreClient},
    types::PositionProps,
};
use soroban_sdk::{testutils::Address as _, Address, BytesN, Env};

fn make_key(env: &Env, seed: u8) -> BytesN<32> {
    BytesN::from_array(env, &[seed; 32])
}

fn setup(
    env: &Env,
) -> (
    ReaderClient<'_>,
    DataStoreClient<'_>,
    LiquidityHandlerClient<'_>,
    Address,
) {
    env.mock_all_auths();

    let rs_id = env.register(RoleStore, ());
    let rs = RoleStoreClient::new(env, &rs_id);
    let admin = Address::generate(env);
    rs.initialize(&admin);

    let ds_id = env.register(DataStore, ());
    let ds = DataStoreClient::new(env, &ds_id);
    ds.initialize(&admin);

    let lh_id = env.register(LiquidityHandler, ());
    let lh = LiquidityHandlerClient::new(env, &lh_id);
    lh.initialize(&rs_id, &ds_id);

    let reader_id = env.register(Reader, ());
    let reader = ReaderClient::new(env, &reader_id);
    reader.initialize(&ds_id, &lh_id);

    (reader, ds, lh, admin)
}

/// 5 long positions with different entry prices produce different PnLs at a
/// common current price. Verify that `get_adl_targets` returns them sorted by
/// descending PnL and respects the `count` limit.
#[test]
fn test_get_adl_targets_sorted_by_pnl_desc() {
    let env = Env::default();
    let (reader, ds, lh, admin) = setup(&env);

    let market_id: u32 = 0;
    let current_price: u128 = 150;
    lh.set_oracle_prices(&admin, &market_id, &current_price, &current_price);

    // Create 5 long positions with different entry prices.
    // PnL = quantity * (current_price - entry_price) / entry_price
    // All quantity = 10_000 for simplicity.
    let quantity: u128 = 10_000;

    // entry=100 → PnL = 10000 * 50/100 = 5000
    // entry=120 → PnL = 10000 * 30/120 = 2500
    // entry=140 → PnL = 10000 * 10/140 = 714
    // entry=160 → PnL = 10000 * (-10)/160 = -625
    // entry=200 → PnL = 10000 * (-50)/200 = -2500
    let entries: &[(u8, u128)] = &[
        (1, 100u128),  // key seed, entry_price
        (2, 200u128),
        (3, 160u128),
        (4, 120u128),
        (5, 140u128),
    ];

    for &(seed, entry_price) in entries {
        let key = make_key(&env, seed);
        let account = Address::generate(&env);
        let pos = PositionProps {
            position_key: key.clone(),
            account: account.clone(),
            market_id,
            quantity,
            collateral_amount: 1_000,
            average_price: entry_price,
            is_long: true,
            is_open: true,
        };
        ds.set_position_props(&admin, &key, &pos);
        ds.add_position_to_oi_list(&admin, &market_id, &true, &key);
    }

    // Fetch all 5 targets.
    let targets = reader.get_adl_targets(&market_id, &true, &5);
    assert_eq!(targets.len(), 5, "should return all 5 positions");

    // Verify descending PnL order.
    let pnl_0 = targets.get(0).unwrap().2;
    let pnl_1 = targets.get(1).unwrap().2;
    let pnl_2 = targets.get(2).unwrap().2;
    let pnl_3 = targets.get(3).unwrap().2;
    let pnl_4 = targets.get(4).unwrap().2;

    assert!(pnl_0 >= pnl_1, "pnl[0] >= pnl[1]");
    assert!(pnl_1 >= pnl_2, "pnl[1] >= pnl[2]");
    assert!(pnl_2 >= pnl_3, "pnl[2] >= pnl[3]");
    assert!(pnl_3 >= pnl_4, "pnl[3] >= pnl[4]");

    // Most profitable (entry=100) should be first.
    let top_key = &targets.get(0).unwrap().1;
    assert_eq!(*top_key, make_key(&env, 1), "entry=100 should be most profitable");

    // Least profitable (entry=200) should be last.
    let bottom_key = &targets.get(4).unwrap().1;
    assert_eq!(*bottom_key, make_key(&env, 2), "entry=200 should be least profitable");
}

/// The `count` parameter must limit the number of results returned.
#[test]
fn test_get_adl_targets_count_limits_results() {
    let env = Env::default();
    let (reader, ds, lh, admin) = setup(&env);

    let market_id: u32 = 1;
    let current_price: u128 = 100;
    lh.set_oracle_prices(&admin, &market_id, &current_price, &current_price);

    // Create 3 positions.
    for seed in 1u8..=3 {
        let key = make_key(&env, seed);
        let account = Address::generate(&env);
        let pos = PositionProps {
            position_key: key.clone(),
            account,
            market_id,
            quantity: 5_000,
            collateral_amount: 500,
            average_price: 90 + (seed as u128) * 5, // entry=95, 100, 105
            is_long: true,
            is_open: true,
        };
        ds.set_position_props(&admin, &key, &pos);
        ds.add_position_to_oi_list(&admin, &market_id, &true, &key);
    }

    // Request only top 2.
    let targets = reader.get_adl_targets(&market_id, &true, &2);
    assert_eq!(targets.len(), 2, "count=2 should return exactly 2 entries");

    // First should be the most profitable (entry=95, seed=1).
    assert_eq!(targets.get(0).unwrap().1, make_key(&env, 1));
    // Second should be entry=100 (seed=2), PnL=0.
    assert_eq!(targets.get(1).unwrap().1, make_key(&env, 2));
}

/// When `count` exceeds the number of positions, all positions are returned.
#[test]
fn test_get_adl_targets_count_exceeds_positions() {
    let env = Env::default();
    let (reader, ds, lh, admin) = setup(&env);

    let market_id: u32 = 2;
    let current_price: u128 = 100;
    lh.set_oracle_prices(&admin, &market_id, &current_price, &current_price);

    let key = make_key(&env, 1);
    let pos = PositionProps {
        position_key: key.clone(),
        account: Address::generate(&env),
        market_id,
        quantity: 1_000,
        collateral_amount: 100,
        average_price: 90,
        is_long: true,
        is_open: true,
    };
    ds.set_position_props(&admin, &key, &pos);
    ds.add_position_to_oi_list(&admin, &market_id, &true, &key);

    // Request 10 but only 1 exists.
    let targets = reader.get_adl_targets(&market_id, &true, &10);
    assert_eq!(targets.len(), 1, "should return only 1 position");
}

/// Empty market returns empty result.
#[test]
fn test_get_adl_targets_empty_market() {
    let env = Env::default();
    let (reader, _ds, lh, admin) = setup(&env);

    let market_id: u32 = 99;
    lh.set_oracle_prices(&admin, &market_id, &100, &100);

    let targets = reader.get_adl_targets(&market_id, &true, &5);
    assert_eq!(targets.len(), 0, "empty market should return empty");
}
