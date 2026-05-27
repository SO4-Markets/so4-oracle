use soroban_sdk::{
    contract, contractimpl, contracttype, panic_with_error, Address, Env,
};

use crate::{
    data_store::DataStoreClient,
    keys::{
        claimable_fee_long_key, claimable_fee_short_key, pool_long_amount_key,
        pool_short_amount_key, withdrawal_fee_factor_key,
    },
    market_factory::market_keeper_role,
    market_token,
    role_store::{role_admin_id, RoleStoreClient},
    types::{LiquidityError, MarketTokens, OraclePrices, Withdrawal},
};

use crate::libs::math::checked_sub_u128;

/// Denominator for the withdrawal fee factor. A factor of `50_000` against this
/// denominator (`1_000_000`) therefore charges a 5% fee.
pub const FEE_FACTOR_DENOMINATOR: u128 = 1_000_000;

/// Liquidity handler: deposits long/short tokens into a market pool, mints LP
/// shares, and redeems LP back to pool tokens (pro-rata, with an optional
/// withdrawal fee).
///
/// Pool amounts and claimable fees are persisted to the shared `data_store`
/// (issues #17, #20). LP shares, oracle prices, pool-token registrations, and
/// pending withdrawals live in this contract's own storage.
#[contract]
pub struct LiquidityHandler;

#[contracttype]
enum LhKey {
    RoleStore,
    DataStore,
    /// Registered pool token pair for a market -> `MarketTokens`.
    Market(u32),
    /// Oracle prices for a market -> `OraclePrices`.
    Price(u32),
    /// Total LP shares outstanding for a market -> `u128`.
    LpSupply(u32),
    /// LP balance for `(market_id, account)` -> `u128`.
    Lp(u32, Address),
    /// Pending withdrawal by id -> `Withdrawal`.
    Withdrawal(u32),
    /// Monotonic withdrawal id counter -> `u32`.
    WithdrawalCount,
}

#[contractimpl]
impl LiquidityHandler {
    // -----------------------------------------------------------------------
    // Bootstrap & configuration
    // -----------------------------------------------------------------------

    /// Initialise with references to the deployed `role_store` and `data_store`.
    pub fn initialize(env: Env, role_store: Address, data_store: Address) {
        if env.storage().instance().has(&LhKey::RoleStore) {
            panic!("already initialised");
        }
        env.storage().instance().set(&LhKey::RoleStore, &role_store);
        env.storage().instance().set(&LhKey::DataStore, &data_store);
    }

    /// Register the long/short pool tokens for `market_id`.
    /// Caller must hold `ROLE_ADMIN` or `MARKET_KEEPER`.
    pub fn register_market(
        env: Env,
        caller: Address,
        market_id: u32,
        long_token: Address,
        short_token: Address,
    ) {
        caller.require_auth();
        Self::require_admin_or_keeper(&env, &caller);
        env.storage().persistent().set(
            &LhKey::Market(market_id),
            &MarketTokens { long_token, short_token },
        );
    }

    /// Set the oracle prices used to value deposits for `market_id`.
    /// Caller must hold `ROLE_ADMIN` or `MARKET_KEEPER`.
    pub fn set_oracle_prices(
        env: Env,
        caller: Address,
        market_id: u32,
        long_price: u128,
        short_price: u128,
    ) {
        caller.require_auth();
        Self::require_admin_or_keeper(&env, &caller);
        env.storage().persistent().set(
            &LhKey::Price(market_id),
            &OraclePrices { long_price, short_price },
        );
    }

    /// Set the optional withdrawal fee factor for `market_id` (stored in
    /// `data_store`). Pass `0` to disable. Caller must hold `ROLE_ADMIN` or
    /// `MARKET_KEEPER`.
    pub fn set_withdrawal_fee_factor(env: Env, caller: Address, market_id: u32, factor: u128) {
        caller.require_auth();
        Self::require_admin_or_keeper(&env, &caller);
        let ds = Self::data_store(&env);
        ds.set_u128(
            &env.current_contract_address(),
            &withdrawal_fee_factor_key(&env, market_id),
            &factor,
        );
    }

    // -----------------------------------------------------------------------
    // Deposits
    // -----------------------------------------------------------------------

    /// Deposit long/short tokens into `market_id`'s pool and mint LP shares to
    /// `receiver`. LP minted is proportional to the deposit's oracle value
    /// relative to the existing pool value, so a depositor receives fewer
    /// shares once the pool has appreciated. Returns the LP amount minted.
    pub fn execute_deposit(
        env: Env,
        caller: Address,
        market_id: u32,
        long_amount: u128,
        short_amount: u128,
        receiver: Address,
    ) -> u128 {
        caller.require_auth();
        if long_amount == 0 && short_amount == 0 {
            panic_with_error!(&env, LiquidityError::ZeroAmount);
        }

        let tokens = Self::market_tokens(&env, market_id);
        let prices = Self::oracle_prices_internal(&env, market_id);
        let ds = Self::data_store(&env);
        let writer = env.current_contract_address();

        // Pull the deposited tokens into the pool (vault).
        market_token::deposit_to_pool(&env, &tokens.long_token, &caller, long_amount);
        market_token::deposit_to_pool(&env, &tokens.short_token, &caller, short_amount);

        let pool_long = ds.get_u128(&pool_long_amount_key(&env, market_id)).unwrap_or(0);
        let pool_short = ds.get_u128(&pool_short_amount_key(&env, market_id)).unwrap_or(0);
        let supply = Self::lp_supply(env.clone(), market_id);

        let deposit_value = long_amount * prices.long_price + short_amount * prices.short_price;
        let lp_minted = if supply == 0 {
            // First deposit: LP supply is seeded with the deposit value.
            deposit_value
        } else {
            let pool_value = pool_long * prices.long_price + pool_short * prices.short_price;
            if pool_value == 0 {
                deposit_value
            } else {
                deposit_value * supply / pool_value
            }
        };

        // Update pool amounts in the shared data_store.
        ds.set_u128(&writer, &pool_long_amount_key(&env, market_id), &(pool_long + long_amount));
        ds.set_u128(&writer, &pool_short_amount_key(&env, market_id), &(pool_short + short_amount));

        // Mint LP shares to the receiver.
        Self::credit_lp(&env, market_id, &receiver, lp_minted);
        Self::set_lp_supply(&env, market_id, supply + lp_minted);

        env.events()
            .publish(("dep_exec",), (market_id, receiver, lp_minted));
        lp_minted
    }

    // -----------------------------------------------------------------------
    // Withdrawals (issue #17)
    // -----------------------------------------------------------------------

    /// Create a pending withdrawal, escrowing `lp_amount` of the caller's LP.
    /// Returns the withdrawal id consumed by `execute_withdrawal`.
    pub fn create_withdrawal(
        env: Env,
        caller: Address,
        market_id: u32,
        lp_amount: u128,
        receiver: Address,
        min_long_out: u128,
        min_short_out: u128,
    ) -> u32 {
        caller.require_auth();
        // Ensure the market is registered.
        let _ = Self::market_tokens(&env, market_id);
        if lp_amount == 0 {
            panic_with_error!(&env, LiquidityError::ZeroAmount);
        }

        let balance = Self::lp_balance_of(env.clone(), market_id, caller.clone());
        if balance < lp_amount {
            panic_with_error!(&env, LiquidityError::InsufficientLp);
        }
        // Escrow: debit the caller's LP now; supply is reduced when burned at
        // execution time.
        Self::set_lp_balance(&env, market_id, &caller, checked_sub_u128(balance, lp_amount));

        let id = Self::next_withdrawal_id(&env);
        let record = Withdrawal {
            account: caller,
            market_id,
            lp_amount,
            receiver,
            min_long_out,
            min_short_out,
        };
        env.storage().persistent().set(&LhKey::Withdrawal(id), &record);

        env.events()
            .publish(("with_create",), (id, market_id, lp_amount));
        id
    }

    /// Execute a pending withdrawal: compute pro-rata long/short amounts from
    /// the LP share, apply the optional fee, enforce slippage, burn the LP,
    /// pay the receiver, decrement pool amounts, and delete the record.
    pub fn execute_withdrawal(env: Env, caller: Address, withdrawal_id: u32) {
        caller.require_auth();

        let w: Withdrawal = match env.storage().persistent().get(&LhKey::Withdrawal(withdrawal_id)) {
            Some(w) => w,
            None => panic_with_error!(&env, LiquidityError::WithdrawalNotFound),
        };

        let tokens = Self::market_tokens(&env, w.market_id);
        // Oracle prices must be set for the market (they may have moved since
        // the deposit — withdrawal uses current pool value).
        let _ = Self::oracle_prices_internal(&env, w.market_id);

        let ds = Self::data_store(&env);
        let writer = env.current_contract_address();

        let pool_long = ds.get_u128(&pool_long_amount_key(&env, w.market_id)).unwrap_or(0);
        let pool_short = ds.get_u128(&pool_short_amount_key(&env, w.market_id)).unwrap_or(0);
        let supply = Self::lp_supply(env.clone(), w.market_id);
        if supply == 0 {
            panic_with_error!(&env, LiquidityError::InsufficientLp);
        }

        // Pro-rata: amount = lp_amount × pool_amount / lp_supply.
        let long_gross = w.lp_amount * pool_long / supply;
        let short_gross = w.lp_amount * pool_short / supply;

        // Optional withdrawal fee.
        let fee_factor = ds
            .get_u128(&withdrawal_fee_factor_key(&env, w.market_id))
            .unwrap_or(0);
        let long_fee = long_gross * fee_factor / FEE_FACTOR_DENOMINATOR;
        let short_fee = short_gross * fee_factor / FEE_FACTOR_DENOMINATOR;
        let long_out = checked_sub_u128(long_gross, long_fee);
        let short_out = checked_sub_u128(short_gross, short_fee);

        // Slippage guards.
        if long_out < w.min_long_out {
            panic_with_error!(&env, LiquidityError::InsufficientLongOut);
        }
        if short_out < w.min_short_out {
            panic_with_error!(&env, LiquidityError::InsufficientShortOut);
        }

        // Burn the escrowed LP from the vault (supply was not reduced at create).
        Self::set_lp_supply(&env, w.market_id, checked_sub_u128(supply, w.lp_amount));

        // Decrement pool amounts by the gross (the fee portion stays in the
        // vault, earmarked as claimable).
        ds.set_u128(&writer, &pool_long_amount_key(&env, w.market_id), &checked_sub_u128(pool_long, long_gross));
        ds.set_u128(&writer, &pool_short_amount_key(&env, w.market_id), &checked_sub_u128(pool_short, short_gross));

        // Accrue claimable fees to data_store for the fee_handler.
        if long_fee > 0 {
            let key = claimable_fee_long_key(&env, w.market_id);
            let accrued = ds.get_u128(&key).unwrap_or(0);
            ds.set_u128(&writer, &key, &(accrued + long_fee));
        }
        if short_fee > 0 {
            let key = claimable_fee_short_key(&env, w.market_id);
            let accrued = ds.get_u128(&key).unwrap_or(0);
            ds.set_u128(&writer, &key, &(accrued + short_fee));
        }

        // Pay the receiver the net amounts.
        market_token::withdraw_from_pool(&env, &tokens.long_token, &w.receiver, long_out);
        market_token::withdraw_from_pool(&env, &tokens.short_token, &w.receiver, short_out);

        // Delete the withdrawal record.
        env.storage().persistent().remove(&LhKey::Withdrawal(withdrawal_id));

        env.events().publish(
            ("with_exec",),
            (withdrawal_id, w.market_id, long_out, short_out),
        );
    }

    // -----------------------------------------------------------------------
    // Views
    // -----------------------------------------------------------------------

    pub fn lp_balance_of(env: Env, market_id: u32, account: Address) -> u128 {
        env.storage()
            .persistent()
            .get(&LhKey::Lp(market_id, account))
            .unwrap_or(0)
    }

    pub fn lp_supply(env: Env, market_id: u32) -> u128 {
        env.storage()
            .persistent()
            .get(&LhKey::LpSupply(market_id))
            .unwrap_or(0)
    }

    /// Returns `(pool_long, pool_short)` from the data_store.
    pub fn pool_amounts(env: Env, market_id: u32) -> (u128, u128) {
        let ds = Self::data_store(&env);
        (
            ds.get_u128(&pool_long_amount_key(&env, market_id)).unwrap_or(0),
            ds.get_u128(&pool_short_amount_key(&env, market_id)).unwrap_or(0),
        )
    }

    /// Returns `(claimable_long, claimable_short)` fees from the data_store.
    pub fn claimable_fees(env: Env, market_id: u32) -> (u128, u128) {
        let ds = Self::data_store(&env);
        (
            ds.get_u128(&claimable_fee_long_key(&env, market_id)).unwrap_or(0),
            ds.get_u128(&claimable_fee_short_key(&env, market_id)).unwrap_or(0),
        )
    }

    pub fn get_withdrawal(env: Env, withdrawal_id: u32) -> Option<Withdrawal> {
        env.storage().persistent().get(&LhKey::Withdrawal(withdrawal_id))
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    fn data_store(env: &Env) -> DataStoreClient<'_> {
        let addr: Address = env
            .storage()
            .instance()
            .get(&LhKey::DataStore)
            .expect("not initialised");
        DataStoreClient::new(env, &addr)
    }

    fn role_store_addr(env: &Env) -> Address {
        env.storage()
            .instance()
            .get(&LhKey::RoleStore)
            .expect("not initialised")
    }

    fn require_admin_or_keeper(env: &Env, caller: &Address) {
        let rs = RoleStoreClient::new(env, &Self::role_store_addr(env));
        let has_admin = rs.has_role(&role_admin_id(env), caller);
        let has_keeper = rs.has_role(&market_keeper_role(env), caller);
        if !has_admin && !has_keeper {
            panic_with_error!(env, LiquidityError::Unauthorized);
        }
    }

    fn market_tokens(env: &Env, market_id: u32) -> MarketTokens {
        match env.storage().persistent().get(&LhKey::Market(market_id)) {
            Some(t) => t,
            None => panic_with_error!(env, LiquidityError::MarketNotRegistered),
        }
    }

    pub fn oracle_prices(env: Env, market_id: u32) -> OraclePrices {
        Self::oracle_prices_internal(&env, market_id)
    }

    fn oracle_prices_internal(env: &Env, market_id: u32) -> OraclePrices {
        match env.storage().persistent().get(&LhKey::Price(market_id)) {
            Some(p) => p,
            None => panic_with_error!(env, LiquidityError::PricesNotSet),
        }
    }

    fn credit_lp(env: &Env, market_id: u32, account: &Address, amount: u128) {
        let current = Self::lp_balance_of(env.clone(), market_id, account.clone());
        Self::set_lp_balance(env, market_id, account, current + amount);
    }

    fn set_lp_balance(env: &Env, market_id: u32, account: &Address, amount: u128) {
        let key = LhKey::Lp(market_id, account.clone());
        if amount == 0 {
            env.storage().persistent().remove(&key);
        } else {
            env.storage().persistent().set(&key, &amount);
        }
    }

    fn set_lp_supply(env: &Env, market_id: u32, amount: u128) {
        env.storage()
            .persistent()
            .set(&LhKey::LpSupply(market_id), &amount);
    }

    fn next_withdrawal_id(env: &Env) -> u32 {
        let id: u32 = env
            .storage()
            .instance()
            .get(&LhKey::WithdrawalCount)
            .unwrap_or(0);
        env.storage().instance().set(&LhKey::WithdrawalCount, &(id + 1));
        id
    }
}
