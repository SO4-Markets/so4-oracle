use soroban_sdk::{contract, contractimpl, contracttype, panic_with_error, token, Address, BytesN, Env};

use crate::{
    data_store::DataStoreClient,
    decrease_position_utils::decrease_position,
    keys::{
        claimable_fee_amount_key, limit_order_expiry_ledgers_key, market_order_expiry_ledgers_key,
        open_interest_long_key, open_interest_short_key, pool_long_amount_key, pool_short_amount_key,
    },
    liquidity_handler::LiquidityHandlerClient,
    role_store::{role_admin_id, RoleStoreClient},
    types::{Order, OrderError, OrderType, Position, PositionError, PositionProps},
};

const FACTOR_DENOMINATOR: u128 = 1_000_000;

#[contract]
pub struct OrderHandler;

#[contracttype]
enum OrderHandlerKey {
    DataStore,
    RoleStore,
    LiquidityHandler,
    AdlHandler,
    OrderCount,
    Order(u32),
    Position(u32, Address, bool),
    SwapFeeFactor,
    PriceImpactFactor,
}

pub fn order_keeper_role(env: &Env) -> BytesN<32> {
    let mut buf = [0u8; 32];
    buf[..12].copy_from_slice(b"ORDER_KEEPER");
    BytesN::from_array(env, &buf)
}

#[contractimpl]
impl OrderHandler {
    pub fn initialize(env: Env, data_store: Address) {
        if env.storage().instance().has(&OrderHandlerKey::DataStore) {
            panic!("already initialised");
        }
        env.storage().instance().set(&OrderHandlerKey::DataStore, &data_store);
    }

    pub fn configure(env: Env, role_store: Address, liquidity_handler: Address) {
        env.storage().instance().set(&OrderHandlerKey::RoleStore, &role_store);
        env.storage()
            .instance()
            .set(&OrderHandlerKey::LiquidityHandler, &liquidity_handler);
    }

    pub fn set_adl_handler(env: Env, caller: Address, adl_handler: Address) {
        caller.require_auth();
        Self::require_admin(&env, &caller);
        env.storage().instance().set(&OrderHandlerKey::AdlHandler, &adl_handler);
    }

    pub fn set_order_expiry_ledgers(
        env: Env,
        caller: Address,
        market_order_ledgers: u32,
        limit_order_ledgers: u32,
    ) {
        caller.require_auth();
        Self::require_admin(&env, &caller);
        let ds = Self::data_store(&env);
        let writer = env.current_contract_address();
        ds.set_u128(&writer, &market_order_expiry_ledgers_key(&env), &(market_order_ledgers as u128));
        ds.set_u128(&writer, &limit_order_expiry_ledgers_key(&env), &(limit_order_ledgers as u128));
    }

    pub fn set_swap_fee_factor(env: Env, caller: Address, factor: u128) {
        caller.require_auth();
        Self::require_admin(&env, &caller);
        env.storage().instance().set(&OrderHandlerKey::SwapFeeFactor, &factor);
    }

    pub fn set_price_impact_factor(env: Env, caller: Address, factor: u128) {
        caller.require_auth();
        Self::require_admin(&env, &caller);
        env.storage().instance().set(&OrderHandlerKey::PriceImpactFactor, &factor);
    }

    pub fn create_order(
        env: Env,
        caller: Address,
        market_id: u32,
        order_type: OrderType,
        collateral_token: Address,
        is_long: bool,
        size_delta_usd: u128,
        collateral_delta: u128,
        trigger_price: u128,
        acceptable_price: u128,
        min_output_amount: u128,
    ) -> u32 {
        caller.require_auth();
        if collateral_delta > 0 {
            token::TokenClient::new(&env, &collateral_token).transfer(
                &caller,
                &env.current_contract_address(),
                &(collateral_delta as i128),
            );
        }

        let key = Self::next_order_id(&env);
        let order = Order {
            key,
            account: caller.clone(),
            market_id,
            order_type,
            is_long,
            size_delta_usd,
            collateral_delta,
            trigger_price,
            acceptable_price,
            min_output_amount,
            collateral_token,
            amount_in: 0,
            created_at: env.ledger().sequence(),
            is_frozen: false,
        };
        env.storage().persistent().set(&OrderHandlerKey::Order(key), &order);
        env.events().publish(("order_create",), (key, market_id, order.order_type));
        key
    }

    pub fn create_market_swap(
        env: Env,
        caller: Address,
        market_id: u32,
        collateral_token: Address,
        amount_in: u128,
        min_output_amount: u128,
    ) -> u32 {
        caller.require_auth();
        let tokens = Self::liquidity_handler(&env).market_tokens(&market_id);
        let is_long = if collateral_token == tokens.long_token {
            true
        } else if collateral_token == tokens.short_token {
            false
        } else {
            panic_with_error!(&env, OrderError::InvalidOrderType);
        };

        token::TokenClient::new(&env, &collateral_token).transfer(
            &caller,
            &env.current_contract_address(),
            &(amount_in as i128),
        );

        let key = Self::next_order_id(&env);
        let order = Order {
            key,
            account: caller.clone(),
            market_id,
            order_type: OrderType::MarketSwap,
            is_long,
            size_delta_usd: 0,
            collateral_delta: 0,
            trigger_price: 0,
            acceptable_price: 0,
            min_output_amount,
            collateral_token,
            amount_in,
            created_at: env.ledger().sequence(),
            is_frozen: false,
        };
        env.storage().persistent().set(&OrderHandlerKey::Order(key), &order);
        env.events().publish(("order_create",), (key, market_id, order.order_type));
        key
    }

    pub fn update_order(
        env: Env,
        caller: Address,
        key: u32,
        trigger_price: u128,
        acceptable_price: u128,
        size_delta_usd: u128,
        min_output_amount: u128,
    ) {
        caller.require_auth();
        let mut order = Self::get_order_or_panic(&env, key);
        if caller != order.account {
            panic_with_error!(&env, OrderError::Unauthorized);
        }

        if matches!(
            order.order_type,
            OrderType::MarketSwap | OrderType::MarketIncrease | OrderType::MarketDecrease
        ) {
            panic_with_error!(&env, OrderError::CannotUpdateMarketOrder);
        }

        order.trigger_price = trigger_price;
        order.acceptable_price = acceptable_price;
        order.size_delta_usd = size_delta_usd;
        order.min_output_amount = min_output_amount;
        order.is_frozen = false;
        env.storage().persistent().set(&OrderHandlerKey::Order(key), &order);

        env.events().publish(
            ("order_update",),
            (key, trigger_price, acceptable_price, size_delta_usd, min_output_amount),
        );
    }

    pub fn set_order_frozen(env: Env, caller: Address, key: u32, is_frozen: bool) {
        caller.require_auth();
        Self::require_admin(&env, &caller);
        let mut order = Self::get_order_or_panic(&env, key);
        order.is_frozen = is_frozen;
        env.storage().persistent().set(&OrderHandlerKey::Order(key), &order);
    }

    pub fn cancel_expired_order(env: Env, caller: Address, key: u32) {
        caller.require_auth();
        Self::require_order_keeper(&env, &caller);

        let order = Self::get_order_or_panic(&env, key);
        if !Self::is_expired(&env, &order) {
            panic_with_error!(&env, OrderError::OrderNotExpired);
        }

        let refund_amount = if order.amount_in > 0 { order.amount_in } else { order.collateral_delta };
        if refund_amount > 0 {
            token::TokenClient::new(&env, &order.collateral_token).transfer(
                &env.current_contract_address(),
                &order.account,
                &(refund_amount as i128),
            );
        }

        env.storage().persistent().remove(&OrderHandlerKey::Order(key));
        env.events().publish(("order_expired",), (key, order.account, refund_amount));
    }

    pub fn execute_order(env: Env, caller: Address, key: u32) {
        caller.require_auth();
        Self::require_order_keeper(&env, &caller);
        let order = Self::get_order_or_panic(&env, key);

        match order.order_type {
            OrderType::MarketSwap => Self::execute_market_swap(&env, &order),
            _ => panic_with_error!(&env, OrderError::InvalidOrderType),
        }
    }

    pub fn set_position(env: Env, caller: Address, position: Position) {
        caller.require_auth();
        Self::require_admin(&env, &caller);
        env.storage().persistent().set(
            &OrderHandlerKey::Position(position.market_id, position.account.clone(), position.is_long),
            &position,
        );
    }

    pub fn get_position(env: Env, account: Address, market_id: u32, is_long: bool) -> Option<Position> {
        env.storage()
            .persistent()
            .get(&OrderHandlerKey::Position(market_id, account, is_long))
    }

    pub fn get_order(env: Env, key: u32) -> Option<Order> {
        env.storage().persistent().get(&OrderHandlerKey::Order(key))
    }

    pub fn execute_adl(
        env: Env,
        caller: Address,
        account: Address,
        market_id: u32,
        collateral_token: Address,
        is_long: bool,
        size_delta_usd: u128,
    ) {
        caller.require_auth();
        let adl_handler: Address = env
            .storage()
            .instance()
            .get(&OrderHandlerKey::AdlHandler)
            .expect("adl handler not configured");
        if caller != adl_handler {
            panic_with_error!(&env, OrderError::Unauthorized);
        }

        let mut position = Self::get_position_internal(&env, &account, market_id, is_long);
        let prices = Self::liquidity_handler(&env).oracle_prices(&market_id);
        let index_price = if is_long { prices.long_price } else { prices.short_price };

        let ds = Self::data_store(&env);
        let writer = env.current_contract_address();
        let released_collateral = decrease_position(
            &env,
            &ds,
            &writer,
            &mut position,
            size_delta_usd,
            index_price,
        );

        if released_collateral > 0 {
            token::TokenClient::new(&env, &collateral_token).transfer(
                &env.current_contract_address(),
                &account,
                &(released_collateral as i128),
            );
        }

        let position_storage_key = OrderHandlerKey::Position(market_id, account.clone(), is_long);
        if position.size_in_usd == 0 {
            env.storage().persistent().remove(&position_storage_key);
        } else {
            env.storage().persistent().set(&position_storage_key, &position);
        }

        env.events().publish(
            ("adl_exec",),
            (account, market_id, is_long, size_delta_usd, released_collateral),
        );
    }

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

        ds.set_position_props(&contract_addr, &position_key, &props);
        ds.add_position(&contract_addr, &position_key);
        ds.add_account_position(&contract_addr, &account, &position_key);
        ds.add_position_to_oi_list(&contract_addr, &market_id, &is_long, &position_key);

        env.events().publish(("pos_increase",), (position_key, account, market_id, quantity));
    }

    pub fn execute_market_decrease(env: Env, caller: Address, position_key: BytesN<32>) {
        Self::fully_close_position(&env, &caller, &position_key, "market_decrease");
    }

    pub fn execute_stop_loss(env: Env, caller: Address, position_key: BytesN<32>) {
        Self::fully_close_position(&env, &caller, &position_key, "stop_loss");
    }

    pub fn execute_liquidation(env: Env, caller: Address, position_key: BytesN<32>) {
        Self::fully_close_position(&env, &caller, &position_key, "liquidation");
    }

    fn execute_market_swap(env: &Env, order: &Order) {
        let lh = Self::liquidity_handler(env);
        let ds = Self::data_store(env);
        let writer = env.current_contract_address();
        let tokens = lh.market_tokens(&order.market_id);
        let prices = lh.oracle_prices(&order.market_id);

        let (input_price, output_price, output_token, input_pool_key, output_pool_key) = if order.is_long {
            (
                prices.long_price,
                prices.short_price,
                tokens.short_token,
                pool_long_amount_key(env, order.market_id),
                pool_short_amount_key(env, order.market_id),
            )
        } else {
            (
                prices.short_price,
                prices.long_price,
                tokens.long_token,
                pool_short_amount_key(env, order.market_id),
                pool_long_amount_key(env, order.market_id),
            )
        };

        let gross_output = order.amount_in.saturating_mul(input_price) / output_price.max(1);
        let price_impact_factor: u128 = env.storage().instance().get(&OrderHandlerKey::PriceImpactFactor).unwrap_or(0);
        let swap_fee_factor: u128 = env.storage().instance().get(&OrderHandlerKey::SwapFeeFactor).unwrap_or(0);

        let price_impact = gross_output.saturating_mul(price_impact_factor) / FACTOR_DENOMINATOR;
        let fee = gross_output.saturating_mul(swap_fee_factor) / FACTOR_DENOMINATOR;
        let output_amount = gross_output.saturating_sub(price_impact).saturating_sub(fee);
        if output_amount < order.min_output_amount {
            panic_with_error!(env, OrderError::InsufficientOutput);
        }

        token::TokenClient::new(env, &output_token).transfer(
            &env.current_contract_address(),
            &order.account,
            &(output_amount as i128),
        );

        let input_pool = ds.get_u128(&input_pool_key).unwrap_or(0);
        let output_pool = ds.get_u128(&output_pool_key).unwrap_or(0);
        ds.set_u128(&writer, &input_pool_key, &(input_pool + order.amount_in));
        ds.set_u128(&writer, &output_pool_key, &output_pool.saturating_sub(output_amount));

        let fee_key = claimable_fee_amount_key(env, order.market_id);
        let accrued_fee = ds.get_u128(&fee_key).unwrap_or(0);
        ds.set_u128(&writer, &fee_key, &(accrued_fee + fee));

        env.storage().persistent().remove(&OrderHandlerKey::Order(order.key));
        env.events().publish(
            ("swap_exec",),
            (order.key, order.market_id, order.amount_in, output_amount, fee, price_impact),
        );
    }

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

        pos.is_open = false;
        ds.set_position_props(&contract_addr, position_key, &pos);
        ds.remove_position(&contract_addr, position_key);
        ds.remove_account_position(&contract_addr, &pos.account, position_key);
        ds.remove_position_from_oi_list(&contract_addr, &pos.market_id, &pos.is_long, position_key);

        let oi_key = if pos.is_long {
            open_interest_long_key(env, pos.market_id)
        } else {
            open_interest_short_key(env, pos.market_id)
        };
        let current_oi = ds.get_u128(&oi_key).unwrap_or(0);
        ds.set_u128(&contract_addr, &oi_key, &current_oi.saturating_sub(pos.quantity));

        env.events().publish(("pos_close",), (position_key.clone(), pos.account.clone(), pos.market_id, path));
    }

    fn get_order_or_panic(env: &Env, key: u32) -> Order {
        match env.storage().persistent().get(&OrderHandlerKey::Order(key)) {
            Some(order) => order,
            None => panic_with_error!(env, OrderError::OrderNotFound),
        }
    }

    fn get_position_internal(env: &Env, account: &Address, market_id: u32, is_long: bool) -> Position {
        match env
            .storage()
            .persistent()
            .get(&OrderHandlerKey::Position(market_id, account.clone(), is_long))
        {
            Some(position) => position,
            None => panic!("position not found"),
        }
    }

    fn is_expired(env: &Env, order: &Order) -> bool {
        let ds = Self::data_store(env);
        let expiry_key = if matches!(
            order.order_type,
            OrderType::MarketSwap | OrderType::MarketIncrease | OrderType::MarketDecrease
        ) {
            market_order_expiry_ledgers_key(env)
        } else {
            limit_order_expiry_ledgers_key(env)
        };
        let expiry_ledgers = ds.get_u128(&expiry_key).unwrap_or(0) as u32;
        env.ledger().sequence() > order.created_at.saturating_add(expiry_ledgers)
    }

    fn next_order_id(env: &Env) -> u32 {
        let id: u32 = env.storage().instance().get(&OrderHandlerKey::OrderCount).unwrap_or(0);
        env.storage().instance().set(&OrderHandlerKey::OrderCount, &(id + 1));
        id
    }

    fn require_admin(env: &Env, caller: &Address) {
        let rs = RoleStoreClient::new(env, &Self::role_store_addr(env));
        if !rs.has_role(&role_admin_id(env), caller) {
            panic_with_error!(env, OrderError::Unauthorized);
        }
    }

    fn require_order_keeper(env: &Env, caller: &Address) {
        let rs = RoleStoreClient::new(env, &Self::role_store_addr(env));
        if !rs.has_role(&order_keeper_role(env), caller) {
            panic_with_error!(env, OrderError::Unauthorized);
        }
    }

    fn role_store_addr(env: &Env) -> Address {
        env.storage().instance().get(&OrderHandlerKey::RoleStore).expect("not configured")
    }

    fn data_store(env: &Env) -> DataStoreClient<'_> {
        let addr: Address = env.storage().instance().get(&OrderHandlerKey::DataStore).expect("not initialised");
        DataStoreClient::new(env, &addr)
    }

    fn liquidity_handler(env: &Env) -> LiquidityHandlerClient<'_> {
        let addr: Address = env
            .storage()
            .instance()
            .get(&OrderHandlerKey::LiquidityHandler)
            .expect("not configured");
        LiquidityHandlerClient::new(env, &addr)
    }
}
