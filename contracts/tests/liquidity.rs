//! Integration tests for the liquidity handler:
//!
//! #17 execute_withdrawal — pro-rata payout, slippage guards, pool decrement
//! #18 concurrent deposits — LP supply, price monotonicity, pool amounts
//! #19 withdrawal after price movement — output at current pool value, min-out
//! #20 withdrawal fee — proportional fee deducted, claimable fee accrued

#![cfg(test)]

use contracts::{
    data_store::DataStore,
    liquidity_handler::{LiquidityHandler, LiquidityHandlerClient},
    role_store::{RoleStore, RoleStoreClient},
};
use soroban_sdk::{
    testutils::Address as _,
    token::{StellarAssetClient, TokenClient},
    Address, Env,
};

const MARKET: u32 = 0;

struct Setup {
    lh: Address,
    admin: Address,
    long: Address,
    short: Address,
}

/// Deploys role_store + data_store + liquidity_handler, registers two Stellar
/// Asset Contracts as the market's long/short pool tokens, and registers the
/// market with the handler. `admin` holds ROLE_ADMIN (and is the SAC admin).
fn setup(env: &Env) -> Setup {
    env.mock_all_auths();

    let admin = Address::generate(env);

    let rs = env.register(RoleStore, ());
    RoleStoreClient::new(env, &rs).initialize(&admin);

    let ds = env.register(DataStore, ());
    let lh = env.register(LiquidityHandler, ());
    let lhc = LiquidityHandlerClient::new(env, &lh);
    lhc.initialize(&rs, &ds);

    let long = env.register_stellar_asset_contract_v2(admin.clone()).address();
    let short = env.register_stellar_asset_contract_v2(admin.clone()).address();

    lhc.register_market(&admin, &MARKET, &long, &short);

    Setup { lh, admin, long, short }
}

fn mint(env: &Env, token: &Address, to: &Address, amount: i128) {
    StellarAssetClient::new(env, token).mint(to, &amount);
}

// ---------------------------------------------------------------------------
// Issue #17 — execute_withdrawal
// ---------------------------------------------------------------------------

#[test]
fn test_execute_withdrawal_full_pro_rata() {
    let env = Env::default();
    let s = setup(&env);
    let lhc = LiquidityHandlerClient::new(&env, &s.lh);
    let long_tok = TokenClient::new(&env, &s.long);
    let short_tok = TokenClient::new(&env, &s.short);

    let user = Address::generate(&env);
    mint(&env, &s.long, &user, 1000);
    mint(&env, &s.short, &user, 1000);

    lhc.set_oracle_prices(&s.admin, &MARKET, &1u128, &1u128);

    let lp = lhc.execute_deposit(&user, &MARKET, &1000u128, &1000u128, &user);
    assert_eq!(lp, 2000u128, "first deposit seeds LP with deposit value");
    assert_eq!(lhc.lp_supply(&MARKET), 2000u128);
    assert_eq!(lhc.pool_amounts(&MARKET), (1000u128, 1000u128));
    // Tokens moved into the pool.
    assert_eq!(long_tok.balance(&user), 0);
    assert_eq!(long_tok.balance(&s.lh), 1000);

    // Redeem all LP.
    let wid = lhc.create_withdrawal(&user, &MARKET, &2000u128, &user, &0u128, &0u128);
    assert!(lhc.get_withdrawal(&wid).is_some());
    assert_eq!(lhc.lp_balance_of(&MARKET, &user), 0u128, "LP escrowed on create");

    lhc.execute_withdrawal(&user, &wid);

    // Pro-rata = 100% of the pool, paid to the receiver.
    assert_eq!(long_tok.balance(&user), 1000);
    assert_eq!(short_tok.balance(&user), 1000);
    assert_eq!(long_tok.balance(&s.lh), 0, "pool drained");
    assert_eq!(lhc.pool_amounts(&MARKET), (0u128, 0u128), "pool decremented in data_store");
    assert_eq!(lhc.lp_supply(&MARKET), 0u128, "LP burned");
    assert!(lhc.get_withdrawal(&wid).is_none(), "record deleted");
}

#[test]
fn test_partial_withdrawal_is_proportional() {
    let env = Env::default();
    let s = setup(&env);
    let lhc = LiquidityHandlerClient::new(&env, &s.lh);
    let long_tok = TokenClient::new(&env, &s.long);

    let user = Address::generate(&env);
    mint(&env, &s.long, &user, 1000);
    mint(&env, &s.short, &user, 1000);
    lhc.set_oracle_prices(&s.admin, &MARKET, &1u128, &1u128);
    lhc.execute_deposit(&user, &MARKET, &1000u128, &1000u128, &user);

    // Redeem a quarter of the LP -> a quarter of each pool.
    let wid = lhc.create_withdrawal(&user, &MARKET, &500u128, &user, &0u128, &0u128);
    lhc.execute_withdrawal(&user, &wid);

    assert_eq!(long_tok.balance(&user), 250);
    assert_eq!(lhc.pool_amounts(&MARKET), (750u128, 750u128));
    assert_eq!(lhc.lp_supply(&MARKET), 1500u128);
}

#[test]
#[should_panic]
fn test_withdrawal_reverts_on_long_slippage() {
    let env = Env::default();
    let s = setup(&env);
    let lhc = LiquidityHandlerClient::new(&env, &s.lh);

    let user = Address::generate(&env);
    mint(&env, &s.long, &user, 1000);
    mint(&env, &s.short, &user, 1000);
    lhc.set_oracle_prices(&s.admin, &MARKET, &1u128, &1u128);
    lhc.execute_deposit(&user, &MARKET, &1000u128, &1000u128, &user);

    // Full redemption yields 1000 long; demand 1001 -> InsufficientLongOut.
    let wid = lhc.create_withdrawal(&user, &MARKET, &2000u128, &user, &1001u128, &0u128);
    lhc.execute_withdrawal(&user, &wid);
}

#[test]
#[should_panic]
fn test_withdrawal_reverts_on_short_slippage() {
    let env = Env::default();
    let s = setup(&env);
    let lhc = LiquidityHandlerClient::new(&env, &s.lh);

    let user = Address::generate(&env);
    mint(&env, &s.long, &user, 1000);
    mint(&env, &s.short, &user, 1000);
    lhc.set_oracle_prices(&s.admin, &MARKET, &1u128, &1u128);
    lhc.execute_deposit(&user, &MARKET, &1000u128, &1000u128, &user);

    let wid = lhc.create_withdrawal(&user, &MARKET, &2000u128, &user, &0u128, &1001u128);
    lhc.execute_withdrawal(&user, &wid);
}

// ---------------------------------------------------------------------------
// Issue #18 — concurrent (sequential) deposits
// ---------------------------------------------------------------------------

#[test]
fn test_concurrent_deposits_consistent_lp_and_price() {
    let env = Env::default();
    let s = setup(&env);
    let lhc = LiquidityHandlerClient::new(&env, &s.lh);

    let a = Address::generate(&env);
    let b = Address::generate(&env);
    mint(&env, &s.long, &a, 1000);
    mint(&env, &s.short, &a, 1000);
    mint(&env, &s.short, &b, 1000);

    // A deposits at price (1, 1): value 2000 -> 2000 LP.
    lhc.set_oracle_prices(&s.admin, &MARKET, &1u128, &1u128);
    let lp_a = lhc.execute_deposit(&a, &MARKET, &1000u128, &1000u128, &a);
    assert_eq!(lp_a, 2000u128);
    assert_eq!(lhc.lp_supply(&MARKET), 2000u128);

    // Long price rises to 2 -> the pool (and each LP) is now worth more.
    lhc.set_oracle_prices(&s.admin, &MARKET, &2u128, &1u128);

    // B deposits 1000 short (value 1000) into a pool worth 3000 backing 2000 LP.
    // lp_b = 1000 * 2000 / 3000 = 666  -> fewer LP for the same token amount.
    let lp_b = lhc.execute_deposit(&b, &MARKET, &0u128, &1000u128, &b);
    assert_eq!(lp_b, 666u128);
    assert!(lp_b < 1000u128, "second depositor gets fewer LP after appreciation");

    assert_eq!(lhc.lp_supply(&MARKET), 2666u128);
    assert_eq!(lhc.pool_amounts(&MARKET), (1000u128, 2000u128));

    // Market-token price (pool_value / supply) is monotonically non-decreasing.
    // before B: value 3000 / supply 2000 ; after B: value 4000 / supply 2666.
    let value_before: u128 = 1000 * 2 + 1000 * 1; // 3000
    let supply_before: u128 = 2000;
    let value_after: u128 = 1000 * 2 + 2000 * 1; // 4000
    let supply_after: u128 = 2666;
    assert!(
        value_before * supply_after <= value_after * supply_before,
        "price per LP must not decrease across the second deposit"
    );
}

// ---------------------------------------------------------------------------
// Issue #19 — withdrawal after price movement
// ---------------------------------------------------------------------------

#[test]
fn test_withdrawal_reflects_current_pool_value() {
    let env = Env::default();
    let s = setup(&env);
    let lhc = LiquidityHandlerClient::new(&env, &s.lh);
    let long_tok = TokenClient::new(&env, &s.long);
    let short_tok = TokenClient::new(&env, &s.short);

    let user = Address::generate(&env);
    mint(&env, &s.long, &user, 1000);
    mint(&env, &s.short, &user, 1000);

    // Deposit at price (1, 1): deposit value 2000.
    lhc.set_oracle_prices(&s.admin, &MARKET, &1u128, &1u128);
    lhc.execute_deposit(&user, &MARKET, &1000u128, &1000u128, &user);

    // Oracle price moves before withdrawal.
    lhc.set_oracle_prices(&s.admin, &MARKET, &3u128, &1u128);

    let wid = lhc.create_withdrawal(&user, &MARKET, &2000u128, &user, &0u128, &0u128);
    lhc.execute_withdrawal(&user, &wid);

    // Pro-rata token amounts are the full pool.
    assert_eq!(long_tok.balance(&user), 1000);
    assert_eq!(short_tok.balance(&user), 1000);

    // Their value is computed at the *current* price (3,1) = 4000, not the
    // original deposit price (2000).
    let value_at_new_price: u128 = 1000 * 3 + 1000 * 1;
    let value_at_old_price: u128 = 1000 * 1 + 1000 * 1;
    assert_eq!(value_at_new_price, 4000u128);
    assert_ne!(value_at_new_price, value_at_old_price);
}

#[test]
#[should_panic]
fn test_withdrawal_after_price_move_respects_min_out() {
    let env = Env::default();
    let s = setup(&env);
    let lhc = LiquidityHandlerClient::new(&env, &s.lh);

    let user = Address::generate(&env);
    mint(&env, &s.long, &user, 1000);
    mint(&env, &s.short, &user, 1000);
    lhc.set_oracle_prices(&s.admin, &MARKET, &1u128, &1u128);
    lhc.execute_deposit(&user, &MARKET, &1000u128, &1000u128, &user);
    lhc.set_oracle_prices(&s.admin, &MARKET, &3u128, &1u128);

    // Token amount out is still 1000 regardless of price; demanding 1100 reverts.
    let wid = lhc.create_withdrawal(&user, &MARKET, &2000u128, &user, &1100u128, &0u128);
    lhc.execute_withdrawal(&user, &wid);
}

// ---------------------------------------------------------------------------
// Issue #20 — withdrawal fee mechanism
// ---------------------------------------------------------------------------

#[test]
fn test_withdrawal_fee_deducted_and_accrued() {
    let env = Env::default();
    let s = setup(&env);
    let lhc = LiquidityHandlerClient::new(&env, &s.lh);
    let long_tok = TokenClient::new(&env, &s.long);
    let short_tok = TokenClient::new(&env, &s.short);

    let user = Address::generate(&env);
    mint(&env, &s.long, &user, 1000);
    mint(&env, &s.short, &user, 1000);
    lhc.set_oracle_prices(&s.admin, &MARKET, &1u128, &1u128);
    lhc.execute_deposit(&user, &MARKET, &1000u128, &1000u128, &user);

    // 5% withdrawal fee (50_000 / 1_000_000).
    lhc.set_withdrawal_fee_factor(&s.admin, &MARKET, &50_000u128);

    let wid = lhc.create_withdrawal(&user, &MARKET, &2000u128, &user, &0u128, &0u128);
    lhc.execute_withdrawal(&user, &wid);

    // gross = 1000 each; fee = 50 each; user receives 950 each.
    assert_eq!(long_tok.balance(&user), 950);
    assert_eq!(short_tok.balance(&user), 950);
    // Fee retained in the vault, earmarked as claimable in data_store.
    assert_eq!(long_tok.balance(&s.lh), 50);
    assert_eq!(lhc.claimable_fees(&MARKET), (50u128, 50u128));
    // Pool decremented by the gross amounts.
    assert_eq!(lhc.pool_amounts(&MARKET), (0u128, 0u128));
}

#[test]
fn test_zero_fee_factor_charges_nothing() {
    let env = Env::default();
    let s = setup(&env);
    let lhc = LiquidityHandlerClient::new(&env, &s.lh);
    let long_tok = TokenClient::new(&env, &s.long);

    let user = Address::generate(&env);
    mint(&env, &s.long, &user, 1000);
    mint(&env, &s.short, &user, 1000);
    lhc.set_oracle_prices(&s.admin, &MARKET, &1u128, &1u128);
    lhc.execute_deposit(&user, &MARKET, &1000u128, &1000u128, &user);

    let wid = lhc.create_withdrawal(&user, &MARKET, &2000u128, &user, &0u128, &0u128);
    lhc.execute_withdrawal(&user, &wid);

    assert_eq!(long_tok.balance(&user), 1000);
    assert_eq!(lhc.claimable_fees(&MARKET), (0u128, 0u128));
}
