use soroban_sdk::{contract, contractimpl, contracttype, panic_with_error, Address, BytesN, Env};

use crate::{
    data_store::DataStoreClient,
    types::{PositionError, PositionProps},
};

#[contract]
pub struct OrderHandler;

#[contracttype]
enum OhKey {
    DataStore,
}

#[contractimpl]
impl OrderHandler {
    /// Initialise with reference to the deployed `data_store`.
    pub fn initialize(env: Env, data_store: Address) {
        if env.storage().instance().has(&OhKey::DataStore) {
            panic!("already initialised");
        }
        env.storage().instance().set(&OhKey::DataStore, &data_store);
    }

    /// Open or increase a position.
    /// Writes props and adds the position key to the global list, account list, and open interest list.
    pub fn increase_position(
        env: Env,
        caller: Address,
        position_key: BytesN<32>,
        account: Address,
        market_id: u32,
        quantity: u128,
        collateral_amount: u128,
        average_price: u128,
        is_long: bool,
    ) {
        caller.require_auth();
        let ds = Self::data_store(&env);
        let contract_addr = env.current_contract_address();

        let props = PositionProps {
            position_key: position_key.clone(),
            account: account.clone(),
            market_id,
            quantity,
            collateral_amount,
            average_price,
            is_long,
            is_open: true,
        };

        // Write properties
        ds.set_position_props(&contract_addr, &position_key, &props);

        // Add to lists
        ds.add_position(&contract_addr, &position_key);
        ds.add_account_position(&contract_addr, &account, &position_key);
        ds.add_position_to_oi_list(&contract_addr, &market_id, &is_long, &position_key);

        env.events().publish(
            ("pos_increase",),
            (position_key, account, market_id, quantity),
        );
    }

    /// Helper to fully close a position from the given path name.
    /// Sets is_open = false and cleans up all data store list sets.
    fn fully_close_position(env: &Env, caller: &Address, position_key: &BytesN<32>, path: &'static str) {
        caller.require_auth();
        let ds = Self::data_store(env);
        let contract_addr = env.current_contract_address();

        let mut pos = match ds.get_position_props(position_key) {
            Some(p) => p,
            None => panic_with_error!(env, PositionError::PositionNotFound),
        };

        if !pos.is_open {
            return;
        }

        // Close position
        pos.is_open = false;
        ds.set_position_props(&contract_addr, position_key, &pos);

        // Cleanup lists
        ds.remove_position(&contract_addr, position_key);
        ds.remove_account_position(&contract_addr, &pos.account, position_key);
        ds.remove_position_from_oi_list(&contract_addr, &pos.market_id, &pos.is_long, position_key);

        env.events().publish(
            ("pos_close",),
            (position_key.clone(), pos.account.clone(), pos.market_id, path),
        );
    }

    /// Full-close path: Market Decrease
    pub fn execute_market_decrease(env: Env, caller: Address, position_key: BytesN<32>) {
        Self::fully_close_position(&env, &caller, &position_key, "market_decrease");
    }

    /// Full-close path: Stop Loss
    pub fn execute_stop_loss(env: Env, caller: Address, position_key: BytesN<32>) {
        Self::fully_close_position(&env, &caller, &position_key, "stop_loss");
    }

    /// Full-close path: Liquidation
    pub fn execute_liquidation(env: Env, caller: Address, position_key: BytesN<32>) {
        Self::fully_close_position(&env, &caller, &position_key, "liquidation");
    }

    /// Full-close path: ADL (Auto-Deleveraging)
    pub fn execute_adl(env: Env, caller: Address, position_key: BytesN<32>) {
        Self::fully_close_position(&env, &caller, &position_key, "adl");
    }

    // -----------------------------------------------------------------------
    // Internal helper
    // -----------------------------------------------------------------------

    fn data_store(env: &Env) -> DataStoreClient<'_> {
        let addr: Address = env
            .storage()
            .instance()
            .get(&OhKey::DataStore)
            .expect("not initialised");
        DataStoreClient::new(env, &addr)
    }
}
