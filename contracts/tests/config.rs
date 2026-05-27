//! Integration tests for issue #30 — config handler.

#![cfg(test)]

use contracts::{
    config_handler::{ConfigHandler, ConfigHandlerClient},
    data_store::DataStore,
    market_factory::{market_keeper_role, MarketFactory, MarketFactoryClient},
    role_store::{RoleStore, RoleStoreClient},
};
use soroban_sdk::{
    testutils::{Address as _, Events as _},
    Address, Env,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn admin_role(env: &Env) -> soroban_sdk::BytesN<32> {
    soroban_sdk::BytesN::from_array(env, &[0u8; 32])
}

fn setup(env: &Env) -> (ConfigHandlerClient<'_>, MarketFactoryClient<'_>, RoleStoreClient<'_>, Address) {
    let rs_id = env.register(RoleStore, ());
    let ds_id = env.register(DataStore, ());
    let mf_id = env.register(MarketFactory, ());
    let ch_id = env.register(ConfigHandler, ());

    let rs = RoleStoreClient::new(env, &rs_id);
    let mf = MarketFactoryClient::new(env, &mf_id);
    let ch = ConfigHandlerClient::new(env, &ch_id);

    let admin = Address::generate(env);
    rs.initialize(&admin);
    mf.initialize(&rs_id, &ds_id);
    ch.initialize(&rs_id, &ds_id);

    (ch, mf, rs, admin)
}

fn create_market(env: &Env, mf: &MarketFactoryClient, admin: &Address) -> u32 {
    mf.create_market(
        admin,
        &Address::generate(env),
        &Address::generate(env),
        &Address::generate(env),
        &None,
    )
}

// ---------------------------------------------------------------------------
// Issue #30 — set / get pairs
// ---------------------------------------------------------------------------

#[test]
fn test_set_and_get_max_pool_amount() {
    let env = Env::default();
    env.mock_all_auths();
    let (ch, mf, _rs, admin) = setup(&env);
    let market_id = create_market(&env, &mf, &admin);

    assert_eq!(ch.get_max_pool_amount(&market_id), 0);

    ch.set_max_pool_amount(&admin, &market_id, &1_000_000);
    assert_eq!(ch.get_max_pool_amount(&market_id), 1_000_000);

    ch.set_max_pool_amount(&admin, &market_id, &2_000_000);
    assert_eq!(ch.get_max_pool_amount(&market_id), 2_000_000);
}

#[test]
fn test_set_and_get_max_open_interest() {
    let env = Env::default();
    env.mock_all_auths();
    let (ch, mf, _rs, admin) = setup(&env);
    let market_id = create_market(&env, &mf, &admin);

    assert_eq!(ch.get_max_open_interest(&market_id), 0);

    ch.set_max_open_interest(&admin, &market_id, &500_000);
    assert_eq!(ch.get_max_open_interest(&market_id), 500_000);
}

#[test]
fn test_set_and_get_position_fee_factor() {
    let env = Env::default();
    env.mock_all_auths();
    let (ch, mf, _rs, admin) = setup(&env);
    let market_id = create_market(&env, &mf, &admin);

    assert_eq!(ch.get_position_fee_factor(&market_id), 0);

    ch.set_position_fee_factor(&admin, &market_id, &100);
    assert_eq!(ch.get_position_fee_factor(&market_id), 100);
}

#[test]
fn test_set_and_get_borrowing_factor() {
    let env = Env::default();
    env.mock_all_auths();
    let (ch, mf, _rs, admin) = setup(&env);
    let market_id = create_market(&env, &mf, &admin);

    assert_eq!(ch.get_borrowing_factor(&market_id), 0);

    ch.set_borrowing_factor(&admin, &market_id, &50);
    assert_eq!(ch.get_borrowing_factor(&market_id), 50);
}

#[test]
fn test_set_and_get_funding_factor() {
    let env = Env::default();
    env.mock_all_auths();
    let (ch, mf, _rs, admin) = setup(&env);
    let market_id = create_market(&env, &mf, &admin);

    assert_eq!(ch.get_funding_factor(&market_id), 0);

    ch.set_funding_factor(&admin, &market_id, &25);
    assert_eq!(ch.get_funding_factor(&market_id), 25);
}

#[test]
fn test_set_and_get_min_collateral_factor() {
    let env = Env::default();
    env.mock_all_auths();
    let (ch, mf, _rs, admin) = setup(&env);
    let market_id = create_market(&env, &mf, &admin);

    assert_eq!(ch.get_min_collateral_factor(&market_id), 0);

    ch.set_min_collateral_factor(&admin, &market_id, &10_000);
    assert_eq!(ch.get_min_collateral_factor(&market_id), 10_000);
}

#[test]
fn test_set_and_get_max_leverage() {
    let env = Env::default();
    env.mock_all_auths();
    let (ch, mf, _rs, admin) = setup(&env);
    let market_id = create_market(&env, &mf, &admin);

    assert_eq!(ch.get_max_leverage(&market_id), 0);

    ch.set_max_leverage(&admin, &market_id, &500_000);
    assert_eq!(ch.get_max_leverage(&market_id), 500_000);
}

// ---------------------------------------------------------------------------
// MARKET_KEEPER auth
// ---------------------------------------------------------------------------

#[test]
fn test_keeper_can_set_config() {
    let env = Env::default();
    env.mock_all_auths();
    let (ch, mf, rs, admin) = setup(&env);
    let market_id = create_market(&env, &mf, &admin);

    let keeper = Address::generate(&env);
    rs.grant_role(&admin, &market_keeper_role(&env), &keeper);

    ch.set_max_pool_amount(&keeper, &market_id, &42);
    assert_eq!(ch.get_max_pool_amount(&market_id), 42);
}

#[test]
#[should_panic]
fn test_unauthorized_caller_panics() {
    let env = Env::default();
    env.mock_all_auths();
    let (ch, mf, _rs, admin) = setup(&env);
    let market_id = create_market(&env, &mf, &admin);

    let nobody = Address::generate(&env);
    ch.set_max_pool_amount(&nobody, &market_id, &999);
}

// ---------------------------------------------------------------------------
// Event emission
// ---------------------------------------------------------------------------

#[test]
fn test_config_updated_event_emitted() {
    let env = Env::default();
    env.mock_all_auths();
    let (ch, mf, _rs, admin) = setup(&env);
    let market_id = create_market(&env, &mf, &admin);

    ch.set_max_pool_amount(&admin, &market_id, &100);

    let events = env.events().all();
    assert!(!events.is_empty(), "expected at least one event");
}

// ---------------------------------------------------------------------------
// Getter returns 0 for non-existent keys
// ---------------------------------------------------------------------------

#[test]
fn test_getters_return_zero_for_unset_values() {
    let env = Env::default();
    env.mock_all_auths();
    let (ch, mf, _rs, admin) = setup(&env);
    let market_id = create_market(&env, &mf, &admin);

    assert_eq!(ch.get_max_pool_amount(&market_id), 0);
    assert_eq!(ch.get_max_open_interest(&market_id), 0);
    assert_eq!(ch.get_position_fee_factor(&market_id), 0);
    assert_eq!(ch.get_borrowing_factor(&market_id), 0);
    assert_eq!(ch.get_funding_factor(&market_id), 0);
    assert_eq!(ch.get_min_collateral_factor(&market_id), 0);
    assert_eq!(ch.get_max_leverage(&market_id), 0);
}

// ---------------------------------------------------------------------------
// Independent markets
// ---------------------------------------------------------------------------

#[test]
fn test_config_independent_per_market() {
    let env = Env::default();
    env.mock_all_auths();
    let (ch, mf, _rs, admin) = setup(&env);

    let m0 = create_market(&env, &mf, &admin);
    let m1 = create_market(&env, &mf, &admin);

    ch.set_max_pool_amount(&admin, &m0, &100);
    ch.set_max_pool_amount(&admin, &m1, &200);

    assert_eq!(ch.get_max_pool_amount(&m0), 100);
    assert_eq!(ch.get_max_pool_amount(&m1), 200);
}
