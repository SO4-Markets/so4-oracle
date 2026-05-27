//! Tests for referral rebates during order execution (issue #68).

#![cfg(test)]

use contracts::{
    data_store::{DataStore, DataStoreClient},
    keys::{claimable_referral_amount_key, position_fee_factor_key},
    liquidity_handler::{LiquidityHandler, LiquidityHandlerClient},
    order_handler::{order_keeper_role, OrderHandler, OrderHandlerClient},
    referral_storage::{ReferralStorage, ReferralStorageClient},
    role_store::{RoleStore, RoleStoreClient},
    types::{OrderType, Position, TierConfig},
};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::{StellarAssetClient, TokenClient},
    Address, BytesN, Env,
};

const MARKET: u32 = 7;

fn make_code(env: &Env, seed: u8) -> BytesN<32> {
    BytesN::from_array(env, &[seed; 32])
}

struct Setup {
    env: Env,
    admin: Address,
    keeper: Address,
    user: Address,
    referrer: Address,
    long: Address,
    ds_addr: Address,
    lh_addr: Address,
    oh_addr: Address,
    ref_addr: Address,
}

fn setup() -> Setup {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let keeper = Address::generate(&env);
    let user = Address::generate(&env);
    let referrer = Address::generate(&env);

    let rs_addr = env.register(RoleStore, ());
    let ds_addr = env.register(DataStore, ());
    let lh_addr = env.register(LiquidityHandler, ());
    let oh_addr = env.register(OrderHandler, ());
    let ref_addr = env.register(ReferralStorage, ());

    RoleStoreClient::new(&env, &rs_addr).initialize(&admin);
    DataStoreClient::new(&env, &ds_addr).initialize(&admin);
    LiquidityHandlerClient::new(&env, &lh_addr).initialize(&rs_addr, &ds_addr);
    ReferralStorageClient::new(&env, &ref_addr).initialize(&rs_addr);

    let oh = OrderHandlerClient::new(&env, &oh_addr);
    oh.initialize(&ds_addr);
    oh.configure(&rs_addr, &lh_addr);
    oh.set_referral_storage(&admin, &ref_addr);

    RoleStoreClient::new(&env, &rs_addr).grant_role(&admin, &order_keeper_role(&env), &keeper);

    let long = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let short = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    LiquidityHandlerClient::new(&env, &lh_addr).register_market(&admin, &MARKET, &long, &short);
    LiquidityHandlerClient::new(&env, &lh_addr).set_oracle_prices(&admin, &MARKET, &100, &100);

    Setup {
        env,
        admin,
        keeper,
        user,
        referrer,
        long,
        ds_addr,
        lh_addr,
        oh_addr,
        ref_addr,
    }
}

fn ds(s: &Setup) -> DataStoreClient<'_> {
    DataStoreClient::new(&s.env, &s.ds_addr)
}

fn oh(s: &Setup) -> OrderHandlerClient<'_> {
    OrderHandlerClient::new(&s.env, &s.oh_addr)
}

fn lh(s: &Setup) -> LiquidityHandlerClient<'_> {
    LiquidityHandlerClient::new(&s.env, &s.lh_addr)
}

fn refs(s: &Setup) -> ReferralStorageClient<'_> {
    ReferralStorageClient::new(&s.env, &s.ref_addr)
}

fn mint(env: &Env, token: &Address, to: &Address, amount: i128) {
    StellarAssetClient::new(env, token).mint(to, &amount);
}

#[test]
fn test_trade_with_referral_applies_discount_and_rebate() {
    let s = setup();
    let code = make_code(&s.env, 9);

    refs(&s).register_code(&s.referrer, &code);
    refs(&s).set_tier(
        &s.admin,
        &s.referrer,
        &TierConfig {
            rebate_bps: 2_000,
            discount_bps: 1_000,
        },
    );

    ds(&s).set_u128(
        &s.admin,
        &position_fee_factor_key(&s.env, MARKET),
        &10_000,
    );

    mint(&s.env, &s.long, &s.oh_addr, 2_000);
    oh(&s).set_position(
        &s.admin,
        &Position {
            account: s.user.clone(),
            market_id: MARKET,
            is_long: true,
            size_in_usd: 10_000,
            size_in_tokens: 100,
            collateral_amount: 1_000,
            referral_code: code,
        },
    );

    let order_key = oh(&s).create_order(
        &s.user,
        &MARKET,
        &OrderType::LimitDecrease,
        &s.long,
        &true,
        &5_000,
        &0,
        &80,
        &0,
        &0,
    );

    lh(&s).set_oracle_prices(&s.admin, &MARKET, &100, &100);

    let claim_key = claimable_referral_amount_key(&s.env, &s.referrer, &s.long);
    let before = ds(&s).get_u128(&claim_key).unwrap_or(0);

    oh(&s).execute_order(&s.keeper, &order_key);

    let position_fee = 5_000u128 * 10_000 / 1_000_000;
    let expected_rebate = position_fee * 2_000 / 10_000;
    let expected_discount = position_fee * 1_000 / 10_000;

    let after = ds(&s).get_u128(&claim_key).unwrap_or(0);
    assert_eq!(after - before, expected_rebate);
    assert_eq!(expected_rebate, 10);
    assert_eq!(expected_discount, 5);
    assert_eq!(position_fee - expected_discount, 45);
    assert!(oh(&s).get_order(&order_key).is_none());
}
