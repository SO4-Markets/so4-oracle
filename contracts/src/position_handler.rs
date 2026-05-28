use soroban_sdk::{contract, contractimpl, contracttype, panic_with_error, Address, BytesN, Env};

use crate::{
    data_store::DataStoreClient,
    liquidity_handler::LiquidityHandlerClient,
    keys::market_maintenance_margin_factor_key,
    types::{PositionError, PositionProps},
    position_utils,
};

#[contract]
pub struct PositionHandler;

#[contracttype]
enum PositionHandlerKey {
    DataStore,
    LiquidityHandler,
}

#[contractimpl]
impl PositionHandler {
    /// Initialise with references to the deployed `data_store` and `liquidity_handler`.
    pub fn initialize(env: Env, data_store: Address, liquidity_handler: Address) {
        if env.storage().instance().has(&PositionHandlerKey::DataStore) {
            panic!("already initialised");
        }
        env.storage().instance().set(&PositionHandlerKey::DataStore, &data_store);
        env.storage().instance().set(&PositionHandlerKey::LiquidityHandler, &liquidity_handler);
    }

    /// Returns whether the position at `position_key` is liquidatable.
    ///
    /// **Pricing — `maximize = true` (worst-case for the position):**
    ///
    /// To be conservative, this function always selects the price that
    /// *maximises* the position's unrealised loss:
    ///
    /// | direction | price used       | why                                      |
    /// |-----------|------------------|------------------------------------------|
    /// | long      | `long_price`     | lower price → larger unrealised loss     |
    /// | short     | `short_price`    | higher price → larger unrealised loss    |
    ///
    /// Using `maximize = false` (the opposite choice — `short_price` for longs,
    /// `long_price` for shorts) would understate the risk and could allow a
    /// genuinely under-collateralised position to pass the check.
    pub fn is_liquidatable(env: Env, position_key: BytesN<32>) -> bool {
        let ds = Self::data_store(&env);

        let pos: PositionProps = match ds.get_position_props(&position_key) {
            Some(p) => p,
            None => panic_with_error!(&env, PositionError::PositionNotFound),
        };

        if !pos.is_open {
            return false;
        }

        let lh = Self::liquidity_handler(&env);
        let prices = lh.oracle_prices(&pos.market_id);

        let margin_factor = ds
            .get_u128(&market_maintenance_margin_factor_key(&env, pos.market_id))
            .unwrap_or(0);

        // maximize = true: use the worst-case price for this position's direction.
        let price = if pos.is_long {
            prices.long_price
        } else {
            prices.short_price
        };

        position_utils::is_liquidatable(&pos, price, margin_factor)
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    fn data_store(env: &Env) -> DataStoreClient {
        let addr: Address = env
            .storage()
            .instance()
            .get(&PositionHandlerKey::DataStore)
            .expect("not initialised");
        DataStoreClient::new(env, &addr)
    }

    fn liquidity_handler(env: &Env) -> LiquidityHandlerClient {
        let addr: Address = env
            .storage()
            .instance()
            .get(&PositionHandlerKey::LiquidityHandler)
            .expect("not initialised");
        LiquidityHandlerClient::new(env, &addr)
    }
}
