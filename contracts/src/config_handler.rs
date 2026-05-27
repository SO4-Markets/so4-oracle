use soroban_sdk::{contract, contractimpl, contracttype, panic_with_error, Address, BytesN, Env};

use crate::{
    data_store::DataStoreClient,
    keys::{
        borrowing_factor_key, funding_factor_key, max_leverage_key, max_open_interest_key,
        max_pool_amount_key, min_collateral_factor_key, position_fee_factor_key,
    },
    market_factory::market_keeper_role,
    role_store::{role_admin_id, RoleStoreClient},
    types::ConfigError,
};

/// Instance-storage keys for dependency addresses.
#[contracttype]
enum InstanceKey {
    RoleStore,
    DataStore,
}

/// Per-market configuration parameters that can be updated at runtime by a
/// `MARKET_KEEPER` or `ROLE_ADMIN`.
#[contract]
pub struct ConfigHandler;

#[contractimpl]
impl ConfigHandler {
    // -----------------------------------------------------------------------
    // Bootstrap
    // -----------------------------------------------------------------------

    /// Initialise the config handler with references to the existing
    /// `role_store` and `data_store` contracts.
    pub fn initialize(env: Env, role_store: Address, data_store: Address) {
        if env.storage().instance().has(&InstanceKey::RoleStore) {
            panic!("already initialised");
        }
        env.storage()
            .instance()
            .set(&InstanceKey::RoleStore, &role_store);
        env.storage()
            .instance()
            .set(&InstanceKey::DataStore, &data_store);
    }

    // -----------------------------------------------------------------------
    // Setters (MARKET_KEEPER or ROLE_ADMIN gated)
    // -----------------------------------------------------------------------

    pub fn set_max_pool_amount(env: Env, caller: Address, market_id: u32, value: u128) {
        Self::set_config_value(&env, &caller, market_id, max_pool_amount_key, value, "max_pool_amount");
    }

    pub fn set_max_open_interest(env: Env, caller: Address, market_id: u32, value: u128) {
        Self::set_config_value(&env, &caller, market_id, max_open_interest_key, value, "max_open_interest");
    }

    pub fn set_position_fee_factor(env: Env, caller: Address, market_id: u32, value: u128) {
        Self::set_config_value(&env, &caller, market_id, position_fee_factor_key, value, "position_fee_factor");
    }

    pub fn set_borrowing_factor(env: Env, caller: Address, market_id: u32, value: u128) {
        Self::set_config_value(&env, &caller, market_id, borrowing_factor_key, value, "borrowing_factor");
    }

    pub fn set_funding_factor(env: Env, caller: Address, market_id: u32, value: u128) {
        Self::set_config_value(&env, &caller, market_id, funding_factor_key, value, "funding_factor");
    }

    pub fn set_min_collateral_factor(env: Env, caller: Address, market_id: u32, value: u128) {
        Self::set_config_value(&env, &caller, market_id, min_collateral_factor_key, value, "min_collateral_factor");
    }

    pub fn set_max_leverage(env: Env, caller: Address, market_id: u32, value: u128) {
        Self::set_config_value(&env, &caller, market_id, max_leverage_key, value, "max_leverage");
    }

    // -----------------------------------------------------------------------
    // Getters
    // -----------------------------------------------------------------------

    pub fn get_max_pool_amount(env: Env, market_id: u32) -> u128 {
        Self::get_config_value(&env, market_id, max_pool_amount_key)
    }

    pub fn get_max_open_interest(env: Env, market_id: u32) -> u128 {
        Self::get_config_value(&env, market_id, max_open_interest_key)
    }

    pub fn get_position_fee_factor(env: Env, market_id: u32) -> u128 {
        Self::get_config_value(&env, market_id, position_fee_factor_key)
    }

    pub fn get_borrowing_factor(env: Env, market_id: u32) -> u128 {
        Self::get_config_value(&env, market_id, borrowing_factor_key)
    }

    pub fn get_funding_factor(env: Env, market_id: u32) -> u128 {
        Self::get_config_value(&env, market_id, funding_factor_key)
    }

    pub fn get_min_collateral_factor(env: Env, market_id: u32) -> u128 {
        Self::get_config_value(&env, market_id, min_collateral_factor_key)
    }

    pub fn get_max_leverage(env: Env, market_id: u32) -> u128 {
        Self::get_config_value(&env, market_id, max_leverage_key)
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    fn deps(env: &Env) -> (Address, Address) {
        let rs: Address = env
            .storage()
            .instance()
            .get(&InstanceKey::RoleStore)
            .expect("not initialised");
        let ds: Address = env
            .storage()
            .instance()
            .get(&InstanceKey::DataStore)
            .expect("not initialised");
        (rs, ds)
    }

    fn require_admin_or_keeper(env: &Env, role_store_addr: &Address, caller: &Address) {
        let rs = RoleStoreClient::new(env, role_store_addr);
        let has_admin = rs.has_role(&role_admin_id(env), caller);
        let has_keeper = rs.has_role(&market_keeper_role(env), caller);
        if !has_admin && !has_keeper {
            panic_with_error!(env, ConfigError::Unauthorized);
        }
    }

    fn set_config_value(
        env: &Env,
        caller: &Address,
        market_id: u32,
        key_fn: fn(&Env, u32) -> BytesN<32>,
        new_value: u128,
        param_name: &str,
    ) {
        caller.require_auth();
        let (rs_addr, ds_addr) = Self::deps(env);
        Self::require_admin_or_keeper(env, &rs_addr, caller);

        let ds = DataStoreClient::new(env, &ds_addr);
        let key = key_fn(env, market_id);
        let old_value = ds.get_u128(&key).unwrap_or(0);

        ds.set_u128(caller, &key, &new_value);

        env.events().publish(
            ("config_updated",),
            (market_id, param_name, old_value, new_value),
        );
    }

    fn get_config_value(
        env: &Env,
        market_id: u32,
        key_fn: fn(&Env, u32) -> BytesN<32>,
    ) -> u128 {
        let (_, ds_addr) = Self::deps(env);
        let ds = DataStoreClient::new(env, &ds_addr);
        ds.get_u128(&key_fn(env, market_id)).unwrap_or(0)
    }
}
