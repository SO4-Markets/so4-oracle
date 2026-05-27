use soroban_sdk::{contract, contractimpl, contracttype, Address, BytesN, Env, Vec};

use crate::{
    data_store::DataStoreClient,
    liquidity_handler::LiquidityHandlerClient,
    position_utils::calculate_pnl,
    types::PositionProps,
};

#[contract]
pub struct Reader;

#[contracttype]
enum ReaderKey {
    DataStore,
    LiquidityHandler,
}

/// ADL target entry: (account, position_key, unrealised_pnl_usd).
pub type AdlTarget = (Address, BytesN<32>, i128);

#[contractimpl]
impl Reader {
    pub fn initialize(env: Env, data_store: Address, liquidity_handler: Address) {
        if env.storage().instance().has(&ReaderKey::DataStore) {
            panic!("already initialised");
        }
        env.storage().instance().set(&ReaderKey::DataStore, &data_store);
        env.storage().instance().set(&ReaderKey::LiquidityHandler, &liquidity_handler);
    }

    /// Returns the top-`count` most profitable open positions for `market_id`
    /// on the given side (`is_long`), sorted by `unrealised_pnl_usd` descending.
    ///
    /// Each entry is `(account, position_key, unrealised_pnl_usd)`.
    pub fn get_adl_targets(
        env: Env,
        market_id: u32,
        is_long: bool,
        count: u32,
    ) -> Vec<AdlTarget> {
        let ds = Self::data_store(&env);
        let lh = Self::liquidity_handler(&env);

        let prices = lh.oracle_prices(&market_id);
        let current_price = if is_long {
            prices.long_price
        } else {
            prices.short_price
        };

        let positions: Vec<PositionProps> =
            ds.get_all_positions_for_market(&market_id, &is_long, &0, &u32::MAX);

        let mut entries: Vec<AdlTarget> = Vec::new(&env);
        for pos in positions.iter() {
            let pnl = calculate_pnl(&pos, current_price);
            entries.push_back((pos.account.clone(), pos.position_key.clone(), pnl));
        }

        // Sort by PnL descending using insertion sort (no_std compatible).
        let len = entries.len();
        for i in 1..len {
            let current = entries.get(i).unwrap();
            let mut j = i;
            while j > 0 {
                let prev = entries.get(j - 1).unwrap();
                if prev.2 >= current.2 {
                    break;
                }
                entries.set(j, prev);
                j -= 1;
            }
            entries.set(j, current);
        }

        // Truncate to `count` entries.
        let limit = if count < entries.len() { count } else { entries.len() };
        let mut result: Vec<AdlTarget> = Vec::new(&env);
        for i in 0..limit {
            result.push_back(entries.get(i).unwrap());
        }
        result
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    fn data_store(env: &Env) -> DataStoreClient<'_> {
        let addr: Address = env
            .storage()
            .instance()
            .get(&ReaderKey::DataStore)
            .expect("not initialised");
        DataStoreClient::new(env, &addr)
    }

    fn liquidity_handler(env: &Env) -> LiquidityHandlerClient<'_> {
        let addr: Address = env
            .storage()
            .instance()
            .get(&ReaderKey::LiquidityHandler)
            .expect("not initialised");
        LiquidityHandlerClient::new(env, &addr)
    }
}
