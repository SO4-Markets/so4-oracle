use soroban_sdk::{contract, contractimpl, contracttype, panic_with_error, Address, BytesN, Env};

use crate::{
    data_store::DataStoreClient,
    keys::{
        max_pnl_factor_key, open_interest_long_key, open_interest_short_key,
        pool_long_amount_key, pool_short_amount_key,
    },
    libs::math::checked_sub_u128,
    liquidity_handler::LiquidityHandlerClient,
    position_utils,
    types::{AdlError, PositionProps},
};

/// Precision denominator for PnL factor scaling (matches PRECISION in position_utils).
pub const PRECISION: u128 = 1_000_000;

#[contract]
pub struct AdlHandler;

#[contracttype]
enum AdlKey {
    DataStore,
    LiquidityHandler,
}

#[contractimpl]
impl AdlHandler {
    /// Initialise with references to the deployed `data_store` and
    /// `liquidity_handler`.
    pub fn initialize(env: Env, data_store: Address, liquidity_handler: Address) {
        if env.storage().instance().has(&AdlKey::DataStore) {
            panic!("already initialised");
        }
        env.storage()
            .instance()
            .set(&AdlKey::DataStore, &data_store);
        env.storage()
            .instance()
            .set(&AdlKey::LiquidityHandler, &liquidity_handler);
    }

    /// Execute an auto-deleveraging (ADL) on the position at `position_key`.
    ///
    /// 1. Verifies the target position is profitable (`pnl > 0`).
    /// 2. Verifies ADL is required (`pnl_factor > max_pnl_factor`).
    /// 3. Closes (or decreases) the position by `size_delta_usd`.
    /// 4. Emits the `adl` event with PnL factor before and after.
    ///
    /// When `size_delta_usd >= position.quantity` the position is fully closed;
    /// otherwise the position is decreased proportionally.
    pub fn execute_adl(
        env: Env,
        caller: Address,
        position_key: BytesN<32>,
        size_delta_usd: u128,
    ) {
        caller.require_auth();

        let ds = Self::data_store(&env);
        let lh = Self::liquidity_handler(&env);
        let contract_addr = env.current_contract_address();

        let pos: PositionProps = match ds.get_position_props(&position_key) {
            Some(p) => p,
            None => panic_with_error!(env, AdlError::PositionNotFound),
        };

        if !pos.is_open {
            panic_with_error!(env, AdlError::PositionNotFound);
        }

        let prices = lh.oracle_prices(&pos.market_id);
        let price = if pos.is_long {
            prices.long_price
        } else {
            prices.short_price
        };

        // Verify the target position is profitable.
        let pos_pnl = position_utils::calculate_pnl(&pos, price);
        if pos_pnl <= 0 {
            panic_with_error!(env, AdlError::NotProfitable);
        }

        // Compute PnL factor *before* ADL.
        let pnl_factor_before =
            Self::compute_pnl_factor(&env, &ds, &lh, pos.market_id, pos.is_long);

        // Verify ADL is required.
        let max_factor = ds
            .get_u128(&max_pnl_factor_key(&env, pos.market_id))
            .unwrap_or(u128::MAX);

        if pnl_factor_before <= max_factor {
            panic_with_error!(env, AdlError::AdlNotRequired);
        }

        // Execute the close / decrease.
        let is_full_close = size_delta_usd >= pos.quantity;

        if is_full_close {
            Self::fully_close(&env, &ds, &contract_addr, &position_key, &pos);
        } else {
            Self::decrease_position(&env, &ds, &contract_addr, &position_key, &pos, size_delta_usd);
        }

        // Compute PnL factor *after* ADL.
        let pnl_factor_after =
            Self::compute_pnl_factor(&env, &ds, &lh, pos.market_id, pos.is_long);

        env.events().publish(
            ("adl",),
            (
                position_key,
                pos.account,
                pos.market_id,
                size_delta_usd,
                pnl_factor_before,
                pnl_factor_after,
            ),
        );
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    fn fully_close(
        _env: &Env,
        ds: &DataStoreClient,
        contract_addr: &Address,
        position_key: &BytesN<32>,
        pos: &PositionProps,
    ) {
        let mut updated = pos.clone();
        updated.is_open = false;
        ds.set_position_props(contract_addr, position_key, &updated);
        ds.remove_position(contract_addr, position_key);
        ds.remove_account_position(contract_addr, &pos.account, position_key);
        ds.remove_position_from_oi_list(
            contract_addr,
            &pos.market_id,
            &pos.is_long,
            position_key,
        );
    }

    fn decrease_position(
        env: &Env,
        ds: &DataStoreClient,
        contract_addr: &Address,
        position_key: &BytesN<32>,
        pos: &PositionProps,
        size_delta_usd: u128,
    ) {
        let released_collateral = pos.collateral_amount * size_delta_usd / pos.quantity;
        let remaining_quantity = checked_sub_u128(pos.quantity, size_delta_usd);
        let remaining_collateral = checked_sub_u128(pos.collateral_amount, released_collateral);

        let mut updated = pos.clone();
        updated.quantity = remaining_quantity;
        updated.collateral_amount = remaining_collateral;
        ds.set_position_props(contract_addr, position_key, &updated);

        // Update open interest.
        let oi_key = if pos.is_long {
            open_interest_long_key(env, pos.market_id)
        } else {
            open_interest_short_key(env, pos.market_id)
        };
        let current_oi = ds.get_u128(&oi_key).unwrap_or(0);
        ds.set_u128(
            contract_addr,
            &oi_key,
            &current_oi.saturating_sub(size_delta_usd),
        );
    }

    /// Compute the PnL factor for all positions on a given market side.
    ///
    /// `pnl_factor = total_pnl * PRECISION / pool_value`
    ///
    /// Returns `0` when total PnL is non-positive and `u128::MAX` when the
    /// pool value is zero (to signal an extreme imbalance).
    fn compute_pnl_factor(
        env: &Env,
        ds: &DataStoreClient,
        lh: &LiquidityHandlerClient,
        market_id: u32,
        is_long: bool,
    ) -> u128 {
        let prices = lh.oracle_prices(&market_id);
        let price = if is_long {
            prices.long_price
        } else {
            prices.short_price
        };

        let positions =
            ds.get_all_positions_for_market(&market_id, &is_long, &0u32, &u32::MAX);

        let mut total_pnl: i128 = 0;
        for p in positions.iter() {
            total_pnl += position_utils::calculate_pnl(&p, price);
        }

        if total_pnl <= 0 {
            return 0;
        }

        let pool_long = ds
            .get_u128(&pool_long_amount_key(env, market_id))
            .unwrap_or(0);
        let pool_short = ds
            .get_u128(&pool_short_amount_key(env, market_id))
            .unwrap_or(0);
        let pool_value =
            pool_long * prices.long_price + pool_short * prices.short_price;

        if pool_value == 0 {
            return u128::MAX;
        }

        (total_pnl as u128) * PRECISION / pool_value
    }

    fn data_store(env: &Env) -> DataStoreClient<'_> {
        let addr: Address = env
            .storage()
            .instance()
            .get(&AdlKey::DataStore)
            .expect("not initialised");
        DataStoreClient::new(env, &addr)
    }

    fn liquidity_handler(env: &Env) -> LiquidityHandlerClient<'_> {
        let addr: Address = env
            .storage()
            .instance()
            .get(&AdlKey::LiquidityHandler)
            .expect("not initialised");
        LiquidityHandlerClient::new(env, &addr)
    }
}
