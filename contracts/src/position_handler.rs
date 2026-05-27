use soroban_sdk::{contract, contractimpl, contracttype, panic_with_error, Address, BytesN, Env};

use crate::{
    data_store::DataStoreClient,
    keys::{
        market_maintenance_margin_factor_key, max_pnl_factor_for_adl_key, pool_long_amount_key,
        pool_short_amount_key, should_enable_adl_key,
    },
    liquidity_handler::LiquidityHandlerClient,
    position_utils,
    types::{PositionError, PositionProps},
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
    /// Loads the position from `data_store`, fetches oracle prices from
    /// `liquidity_handler`, and uses `position_utils::is_liquidatable`.
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

        // Fetch maintenance margin factor from data_store.
        let margin_factor = ds.get_u128(&market_maintenance_margin_factor_key(&env, pos.market_id))
            .unwrap_or(0);

        // Use maximize = true pricing: choose the worst-case price for this position.
        // For long positions, the worst price is the long token price.
        // For short positions, the worst price is the short token price.
        let price = if pos.is_long {
            prices.long_price
        } else {
            prices.short_price
        };

        position_utils::is_liquidatable(&pos, price, margin_factor)
    }

    /// Computes the current PnL factor for a market side and stores the ADL
    /// keeper signal in data_store.
    pub fn update_adl_state(env: Env, market_id: u32, is_long: bool) -> bool {
        let ds = Self::data_store(&env);
        let lh = Self::liquidity_handler(&env);
        let prices = lh.oracle_prices(&market_id);
        let price = if is_long {
            prices.long_price
        } else {
            prices.short_price
        };

        let positions = ds.get_all_positions_for_market(&market_id, &is_long, &0u32, &u32::MAX);
        let mut total_pnl: u128 = 0;
        for pos in positions.iter() {
            let pnl = position_utils::calculate_pnl(&pos, price);
            if pnl > 0 {
                total_pnl = total_pnl.saturating_add(pnl as u128);
            }
        }

        let pool_long = ds
            .get_u128(&pool_long_amount_key(&env, market_id))
            .unwrap_or(0);
        let pool_short = ds
            .get_u128(&pool_short_amount_key(&env, market_id))
            .unwrap_or(0);
        let pool_value = pool_long
            .saturating_mul(prices.long_price)
            .saturating_add(pool_short.saturating_mul(prices.short_price));

        let pnl_factor = if pool_value == 0 {
            0
        } else {
            total_pnl.saturating_mul(position_utils::PRECISION) / pool_value
        };
        let max_pnl_factor = ds
            .get_u128(&max_pnl_factor_for_adl_key(&env, market_id, is_long))
            .unwrap_or(u128::MAX);
        let should_enable_adl = pnl_factor > max_pnl_factor;
        let flag = if should_enable_adl { 1u128 } else { 0u128 };

        ds.set_u128(
            &env.current_contract_address(),
            &should_enable_adl_key(&env, market_id, is_long),
            &flag,
        );

        should_enable_adl
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
