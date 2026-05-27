#![cfg(test)]

use contracts::{
    data_store::{DataStore, DataStoreClient},
    keys::{claimable_fee_amount_key, pool_long_amount_key, pool_short_amount_key},
    liquidity_handler::{LiquidityHandler, LiquidityHandlerClient},
    order_handler::{order_keeper_role, OrderHandler, OrderHandlerClient},
    role_store::{RoleStore, RoleStoreClient},
    types::{OrderType, Position},
};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::{StellarAssetClient, TokenClient},
    Address, Env, IntoVal,
};

const MARKET: u32 = 7;

struct Setup {
    env: Env,
    admin: Address,
    keeper: Address,
    user: Address,
    adl_handler: Address,
    long: Address,
    short: Address,
    ds_addr: Address,
    oh_addr: Address,
}

fn setup() -> Setup {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let keeper = Address::generate(&env);
    let user = Address::generate(&env);
    let adl_handler = Address::generate(&env);

    let rs_addr = env.register(RoleStore, ());
    let ds_addr = env.register(DataStore, ());
    let lh_addr = env.register(LiquidityHandler, ());
    let oh_addr = env.register(OrderHandler, ());

    let rs = RoleStoreClient::new(&env, &rs_addr);
    let ds = DataStoreClient::new(&env, &ds_addr);
    let lh = LiquidityHandlerClient::new(&env, &lh_addr);
    let oh = OrderHandlerClient::new(&env, &oh_addr);

    rs.initialize(&admin);
    ds.initialize(&admin);
    lh.initialize(&rs_addr, &ds_addr);
    oh.initialize(&ds_addr);
    oh.configure(&rs_addr, &lh_addr);

    rs.grant_role(&admin, &order_keeper_role(&env), &keeper);
    oh.set_adl_handler(&admin, &adl_handler);
    oh.set_order_expiry_ledgers(&admin, &2u32, &8u32);

    let long = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let short = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    lh.register_market(&admin, &MARKET, &long, &short);
    lh.set_oracle_prices(&admin, &MARKET, &2u128, &1u128);

    Setup {
        env,
        admin,
        keeper,
        user,
        adl_handler,
        long,
        short,
        ds_addr: ds_addr.clone(),
        oh_addr: oh_addr.clone(),
    }
}

fn ds<'a>(s: &'a Setup) -> DataStoreClient<'a> {
    DataStoreClient::new(&s.env, &s.ds_addr)
}

fn oh<'a>(s: &'a Setup) -> OrderHandlerClient<'a> {
    OrderHandlerClient::new(&s.env, &s.oh_addr)
}

fn mint(env: &Env, token: &Address, to: &Address, amount: i128) {
    StellarAssetClient::new(env, token).mint(to, &amount);
}

#[test]
fn test_execute_adl_partial_then_full_close() {
    let s = setup();

    mint(&s.env, &s.long, &s.oh_addr, 1_000);
    oh(&s).set_position(
        &s.admin,
        &Position {
            account: s.user.clone(),
            market_id: MARKET,
            is_long: true,
            size_in_usd: 10_000,
            size_in_tokens: 100,
            collateral_amount: 1_000,
        },
    );

    oh(&s).execute_adl(&s.adl_handler, &s.user, &MARKET, &s.long, &true, &4_000u128);

    let position = oh(&s).get_position(&s.user, &MARKET, &true).unwrap();
    assert_eq!(position.size_in_usd, 6_000);
    assert_eq!(position.collateral_amount, 600);
    assert_eq!(position.size_in_tokens, 3_000);
    assert_eq!(TokenClient::new(&s.env, &s.long).balance(&s.user), 400);

    oh(&s).execute_adl(&s.adl_handler, &s.user, &MARKET, &s.long, &true, &6_000u128);
    assert_eq!(oh(&s).get_position(&s.user, &MARKET, &true), None);
    assert_eq!(TokenClient::new(&s.env, &s.long).balance(&s.user), 1_000);
}

#[test]
fn test_cancel_expired_order_uses_separate_market_and_limit_expiry() {
    let s = setup();

    mint(&s.env, &s.long, &s.user, 1_000);
    let market_order = oh(&s).create_order(
        &s.user,
        &MARKET,
        &OrderType::MarketIncrease,
        &s.long,
        &true,
        &1_000u128,
        &500u128,
        &0u128,
        &0u128,
        &0u128,
    );
    let limit_order = oh(&s).create_order(
        &s.user,
        &MARKET,
        &OrderType::LimitIncrease,
        &s.long,
        &true,
        &1_000u128,
        &200u128,
        &90u128,
        &91u128,
        &0u128,
    );

    s.env.ledger().with_mut(|li| {
        li.sequence_number += 3;
    });

    oh(&s).cancel_expired_order(&s.keeper, &market_order);
    assert_eq!(oh(&s).get_order(&market_order), None);
    assert_eq!(TokenClient::new(&s.env, &s.long).balance(&s.user), 800);

    let err = s.env.try_invoke_contract::<(), soroban_sdk::Error>(
        &s.oh_addr,
        &soroban_sdk::Symbol::new(&s.env, "cancel_expired_order"),
        soroban_sdk::vec![
            &s.env,
            s.keeper.clone().into_val(&s.env),
            limit_order.into_val(&s.env),
        ],
    );
    assert!(err.is_err(), "limit order should still be active");

    s.env.ledger().with_mut(|li| {
        li.sequence_number += 6;
    });
    oh(&s).cancel_expired_order(&s.keeper, &limit_order);
    assert_eq!(oh(&s).get_order(&limit_order), None);
    assert_eq!(TokenClient::new(&s.env, &s.long).balance(&s.user), 1_000);
}

#[test]
fn test_update_order_rejects_market_orders_and_unfreezes_limit_orders() {
    let s = setup();
    mint(&s.env, &s.long, &s.user, 1_000);
    mint(&s.env, &s.short, &s.oh_addr, 1_000);

    let market_swap = oh(&s).create_market_swap(&s.user, &MARKET, &s.long, &100u128, &0u128);
    let market_increase = oh(&s).create_order(
        &s.user,
        &MARKET,
        &OrderType::MarketIncrease,
        &s.long,
        &true,
        &1_000u128,
        &100u128,
        &0u128,
        &0u128,
        &0u128,
    );
    let market_decrease = oh(&s).create_order(
        &s.user,
        &MARKET,
        &OrderType::MarketDecrease,
        &s.long,
        &true,
        &1_000u128,
        &0u128,
        &0u128,
        &0u128,
        &0u128,
    );

    for key in [market_swap, market_increase, market_decrease] {
        let err = s.env.try_invoke_contract::<(), soroban_sdk::Error>(
            &s.oh_addr,
            &soroban_sdk::Symbol::new(&s.env, "update_order"),
            soroban_sdk::vec![
                &s.env,
                s.user.clone().into_val(&s.env),
                key.into_val(&s.env),
                80u128.into_val(&s.env),
                81u128.into_val(&s.env),
                500u128.into_val(&s.env),
                5u128.into_val(&s.env),
            ],
        );
        assert!(err.is_err(), "market order should be immutable");
    }

    let limit_order = oh(&s).create_order(
        &s.user,
        &MARKET,
        &OrderType::LimitIncrease,
        &s.long,
        &true,
        &1_000u128,
        &100u128,
        &90u128,
        &91u128,
        &1u128,
    );
    oh(&s).set_order_frozen(&s.admin, &limit_order, &true);
    oh(&s).update_order(&s.user, &limit_order, &85u128, &86u128, &750u128, &2u128);

    let updated = oh(&s).get_order(&limit_order).unwrap();
    assert_eq!(updated.trigger_price, 85);
    assert_eq!(updated.acceptable_price, 86);
    assert_eq!(updated.size_delta_usd, 750);
    assert_eq!(updated.min_output_amount, 2);
    assert!(!updated.is_frozen);
}

#[test]
fn test_market_swap_round_trip_applies_fee_and_price_impact() {
    let s = setup();
    let long_token = TokenClient::new(&s.env, &s.long);
    let short_token = TokenClient::new(&s.env, &s.short);

    oh(&s).set_swap_fee_factor(&s.admin, &10_000u128);
    oh(&s).set_price_impact_factor(&s.admin, &20_000u128);

    mint(&s.env, &s.long, &s.user, 1_000);
    mint(&s.env, &s.short, &s.oh_addr, 10_000);
    ds(&s).set_u128(&s.admin, &pool_long_amount_key(&s.env, MARKET), &10_000u128);
    ds(&s).set_u128(
        &s.admin,
        &pool_short_amount_key(&s.env, MARKET),
        &10_000u128,
    );

    let order_key = oh(&s).create_market_swap(&s.user, &MARKET, &s.long, &100u128, &190u128);
    oh(&s).execute_order(&s.keeper, &order_key);

    let gross_output = 100u128 * 2u128 / 1u128;
    let expected_price_impact = gross_output * 20_000u128 / 1_000_000u128;
    let expected_fee = gross_output * 10_000u128 / 1_000_000u128;
    let expected_output = gross_output - expected_price_impact - expected_fee;

    assert_eq!(long_token.balance(&s.user), 900);
    assert_eq!(short_token.balance(&s.user), expected_output as i128);
    assert_eq!(
        ds(&s)
            .get_u128(&claimable_fee_amount_key(&s.env, MARKET))
            .unwrap_or(0),
        expected_fee
    );
    assert_eq!(oh(&s).get_order(&order_key), None);
}
