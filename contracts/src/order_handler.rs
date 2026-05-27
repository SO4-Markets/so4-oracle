use soroban_sdk::{contract, contractimpl, contracttype, panic_with_error, Address, BytesN, Env};

use crate::{
    data_store::DataStoreClient,
    increase_position_utils::increase_position as apply_increase_position,
    types::{OrderError, OrderType, Position, PositionError, PositionProps},
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

    /// Execute a stored order.
    pub fn execute_order(
        env: Env,
        keeper: Address,
        order_key: BytesN<32>,
        position_key: BytesN<32>,
        index_price: u128,
        acceptable_price: u128,
        execution_fee: u128,
    ) -> PositionProps {
        let order_type = {
            let ds = Self::data_store(&env);
            match ds.get_order(&order_key) {
                Some(order) => order.order_type,
                None => panic_with_error!(env, OrderError::OrderNotFound),
            }
        };

        match order_type {
            OrderType::MarketIncrease => Self::execute_market_increase_order(
                env,
                keeper,
                order_key,
                position_key,
                index_price,
                acceptable_price,
                execution_fee,
            ),
            _ => panic_with_error!(env, OrderError::OrderNotFound),
        }
    }

    /// Execute a stored MarketIncrease order.
    pub fn execute_market_increase_order(
        env: Env,
        keeper: Address,
        order_key: BytesN<32>,
        position_key: BytesN<32>,
        index_price: u128,
        acceptable_price: u128,
        execution_fee: u128,
    ) -> PositionProps {
        keeper.require_auth();
        let ds = Self::data_store(&env);
        let contract_addr = env.current_contract_address();

        let order = match ds.get_order(&order_key) {
            Some(order) => order,
            None => panic_with_error!(env, OrderError::OrderNotFound),
        };

        if order.order_type != OrderType::MarketIncrease {
            panic_with_error!(env, OrderError::OrderNotFound);
        }

        if (order.is_long && index_price > acceptable_price)
            || (!order.is_long && index_price < acceptable_price)
        {
            panic_with_error!(env, OrderError::UnacceptablePrice);
        }

        let existing = ds.get_position_props(&position_key);
        let previous_quantity = existing.as_ref().map(|pos| pos.quantity).unwrap_or(0);
        let previous_average_price = existing
            .as_ref()
            .map(|pos| pos.average_price)
            .unwrap_or(index_price);

        let mut position = match existing {
            Some(pos) => Position {
                account: pos.account,
                market_id: pos.market_id,
                is_long: pos.is_long,
                size_in_usd: pos.quantity,
                size_in_tokens: if pos.average_price > 0 {
                    pos.quantity / pos.average_price
                } else {
                    0
                },
                collateral_amount: pos.collateral_amount,
            },
            None => Position {
                account: order.account.clone(),
                market_id: order.market_id,
                is_long: order.is_long,
                size_in_usd: 0,
                size_in_tokens: 0,
                collateral_amount: 0,
            },
        };

        apply_increase_position(
            &env,
            &ds,
            &contract_addr,
            &mut position,
            order.size_delta_usd,
            order.collateral_delta,
            index_price,
        );

        let average_price = if previous_quantity == 0 {
            index_price
        } else {
            let weighted_previous = previous_average_price.saturating_mul(previous_quantity);
            let weighted_delta = index_price.saturating_mul(order.size_delta_usd);
            weighted_previous
                .saturating_add(weighted_delta)
                / position.size_in_usd
        };

        let props = PositionProps {
            position_key: position_key.clone(),
            account: position.account.clone(),
            market_id: position.market_id,
            quantity: position.size_in_usd,
            collateral_amount: position.collateral_amount,
            average_price,
            is_long: position.is_long,
            is_open: true,
        };

        ds.set_position_props(&contract_addr, &position_key, &props);
        ds.add_position(&contract_addr, &position_key);
        ds.add_account_position(&contract_addr, &props.account, &position_key);
        ds.add_position_to_oi_list(
            &contract_addr,
            &props.market_id,
            &props.is_long,
            &position_key,
        );
        ds.credit_execution_fee(&contract_addr, &keeper, &execution_fee);
        ds.remove_order(&contract_addr, &order_key);

        env.events().publish(
            ("order_exec",),
            (
                order_key,
                position_key,
                props.account.clone(),
                props.market_id,
                props.quantity,
                props.collateral_amount,
                props.average_price,
                execution_fee,
            ),
        );

        props
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
