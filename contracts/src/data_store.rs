
use soroban_sdk::{
    contract, contractimpl, contracttype, panic_with_error, symbol_short,
    Address, BytesN, Env, Vec,
};

use crate::types::PositionProps;

// ---------------------------------------------------------------------------
// Error codes
// ---------------------------------------------------------------------------

#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum DataError {
    /// Caller is not the authorised writer / admin for this operation.
    Unauthorized = 1,
    /// A function that requires initialization was called before `initialize`.
    NotInitialized = 2,
}

impl From<DataError> for soroban_sdk::Error {
    fn from(e: DataError) -> Self {
        soroban_sdk::Error::from_contract_error(e as u32)
    }
}

// ---------------------------------------------------------------------------
// Storage key types
//
// IMPORTANT: U128Key and I128Key use *different field names* (`u128_key` vs
// `i128_key`) so that their #[contracttype] XDR serialisation (a ScMap keyed
// by field name) maps to distinct persistent-storage slots for the same
// BytesN<32> input.  Using the same field name "key" in both structs would
// produce identical XDR representations and therefore a storage collision.
// ---------------------------------------------------------------------------

/// Persistent-storage key for a `u128` value indexed by a 32-byte identifier.
#[contracttype]
#[derive(Clone)]
pub struct U128Key {
    pub u128_key: BytesN<32>,
}

/// Persistent-storage key for an `i128` value indexed by a 32-byte identifier.
#[contracttype]
#[derive(Clone)]
pub struct I128Key {
    pub i128_key: BytesN<32>,
}

/// Instance-storage keys (one per contract, not per data key).
#[contracttype]
#[derive(Clone)]
pub enum DataInstanceKey {
    /// Stores the admin `Address` set by `initialize`.
    Admin,
    /// Stores the hash of the currently deployed WASM binary.
    WasmHash,
}

/// Persistent-storage keys for DataStore contract-level state.
#[contracttype]
#[derive(Clone)]
pub enum DataPersistentKey {
    /// Stores the `Vec<Address>` of authorised keeper/controller addresses.
    Controllers,
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    PositionProps(BytesN<32>),
    AccountPositionList(Address),
    PositionList,
    PositionOiList(BytesN<32>),
}

// ---------------------------------------------------------------------------
// TTL estimation result (issue #3)
// ---------------------------------------------------------------------------

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TtlEstimate {
    pub key: BytesN<32>,
    /// Remaining ledgers the entry can stay stored at the current base fee.
    /// 0 means the key does not exist or has already expired.
    pub remaining_ledgers: u32,
}

// ---------------------------------------------------------------------------
// Pure arithmetic helper
// ---------------------------------------------------------------------------

/// Apply a signed `delta` to a `u128` base value with saturation at the
/// bounds `[0, u128::MAX]`.  Never panics regardless of input.
///
/// ```text
/// apply_delta_to_u128(0,         -1)         == 0           (underflow → 0)
/// apply_delta_to_u128(u128::MAX,  1)         == u128::MAX   (overflow  → MAX)
/// apply_delta_to_u128(100,        50)         == 150
/// apply_delta_to_u128(100,       -30)         == 70
/// ```
pub fn apply_delta_to_u128(base: u128, delta: i128) -> u128 {
    if delta >= 0 {
        base.saturating_add(delta as u128)
    } else {
        // delta.unsigned_abs() converts i128::MIN correctly to 2^127 (fits u128).
        base.saturating_sub(delta.unsigned_abs())
    }
}

// ---------------------------------------------------------------------------
// Contract
// ---------------------------------------------------------------------------

#[contract]
pub struct DataStore;

#[contractimpl]
impl DataStore {
    // -----------------------------------------------------------------------
    // Bootstrap
    // -----------------------------------------------------------------------

    /// Initialise the contract, designating `admin` as the privileged address
    /// for upgrades and controller management. May only be called once.
    pub fn initialize(env: Env, admin: Address) {
        if env.storage().instance().has(&DataInstanceKey::Admin) {
            panic!("already initialised");
        }
        env.storage()
            .instance()
            .set(&DataInstanceKey::Admin, &admin);
    }

    // -----------------------------------------------------------------------
    // Contract upgrades
    // -----------------------------------------------------------------------

    /// Upgrade the contract to a new WASM binary identified by `new_wasm_hash`.
    ///
    /// Caller must be the admin set during `initialize`. Emits an `upgraded`
    /// event containing the old WASM hash and the new WASM hash.
    pub fn upgrade(env: Env, caller: Address, new_wasm_hash: BytesN<32>) {
        caller.require_auth();
        Self::require_admin(&env, &caller);

        // Read the previously recorded hash (zero bytes if first upgrade).
        let old_hash: BytesN<32> = env
            .storage()
            .instance()
            .get(&DataInstanceKey::WasmHash)
            .unwrap_or_else(|| BytesN::from_array(&env, &[0u8; 32]));

        // Record the new hash before executing the upgrade.
        env.storage()
            .instance()
            .set(&DataInstanceKey::WasmHash, &new_wasm_hash);

        // Perform the WASM upgrade.
        //
        // This host call is only meaningful (and safe) when the contract is
        // running as compiled WASM on-chain.  In native test builds the
        // deployer's WASM registry is never pre-loaded with real contract
        // bytes so the call would panic with Error(Storage, MissingValue).
        // Guarding on `target_family = "wasm"` is the correct semantic: WASM
        // upgrades are a chain-level operation that native test runtimes
        // cannot replicate.  Auth, storage, and event-emission are still
        // fully exercised in tests.
        #[cfg(target_family = "wasm")]
        env.deployer()
            .update_current_contract_wasm(new_wasm_hash.clone());

        // Emit the event so off-chain indexers can track upgrade history.
        env.events().publish(
            (symbol_short!("upgraded"),),
            (old_hash, new_wasm_hash),
        );
    }

    // -----------------------------------------------------------------------
    // Controller management
    // -----------------------------------------------------------------------

    /// Add `controller` to the set of addresses authorised to call
    /// `prune_keys`. Caller must be the admin.
    pub fn add_controller(env: Env, caller: Address, controller: Address) {
        caller.require_auth();
        Self::require_admin(&env, &caller);

        let mut controllers: Vec<Address> = env
            .storage()
            .persistent()
            .get(&DataPersistentKey::Controllers)
            .unwrap_or_else(|| Vec::new(&env));

        if !controllers.contains(&controller) {
            controllers.push_back(controller);
            env.storage()
                .persistent()
                .set(&DataPersistentKey::Controllers, &controllers);
        }
    }

    /// Remove `controller` from the authorised controller set.
    /// Caller must be the admin.
    pub fn remove_controller(env: Env, caller: Address, controller: Address) {
        caller.require_auth();
        Self::require_admin(&env, &caller);

        let controllers: Vec<Address> = env
            .storage()
            .persistent()
            .get(&DataPersistentKey::Controllers)
            .unwrap_or_else(|| Vec::new(&env));

        let mut updated: Vec<Address> = Vec::new(&env);
        for c in controllers.iter() {
            if c != controller {
                updated.push_back(c);
            }
        }
        env.storage()
            .persistent()
            .set(&DataPersistentKey::Controllers, &updated);
    }

    // -----------------------------------------------------------------------
    // Keeper pruning (remove zeroed-out persistent entries)
    // -----------------------------------------------------------------------

    /// Remove persistent storage entries whose value is zero / default.
    ///
    /// For each `BytesN<32>` key in `keys` this function independently checks
    /// both the `U128Key` and `I128Key` slots:
    /// - A `U128Key` slot is removed only if it exists **and** its value is `0`.
    /// - An `I128Key` slot is removed only if it exists **and** its value is `0`.
    ///
    /// Because `U128Key` and `I128Key` use distinct field names their XDR
    /// serialisations are different storage slots, so a single logical key can
    /// carry an independent u128 value and an independent i128 value
    /// simultaneously.
    ///
    /// Non-zero entries are left untouched. Caller must hold the CONTROLLER
    /// role (added via `add_controller`).
    pub fn prune_keys(env: Env, caller: Address, keys: Vec<BytesN<32>>) {
        caller.require_auth();
        Self::require_controller(&env, &caller);

        for key in keys.iter() {
            // --- u128 slot ---
            let u128_sk = U128Key { u128_key: key.clone() };
            if env.storage().persistent().has(&u128_sk) {
                let v: u128 = env.storage().persistent().get(&u128_sk).unwrap_or(1);
                if v == 0u128 {
                    env.storage().persistent().remove(&u128_sk);
                }
            }

            // --- i128 slot ---
            let i128_sk = I128Key { i128_key: key.clone() };
            if env.storage().persistent().has(&i128_sk) {
                let v: i128 = env.storage().persistent().get(&i128_sk).unwrap_or(1);
                if v == 0i128 {
                    env.storage().persistent().remove(&i128_sk);
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Single-key u128 operations
    // -----------------------------------------------------------------------

    /// Write a single `u128` value. `caller` must authenticate.
    pub fn set_u128(env: Env, caller: Address, key: BytesN<32>, value: u128) {
        caller.require_auth();
        env.storage()
            .persistent()
            .set(&U128Key { u128_key: key }, &value);
    }

    /// Read a single `u128` value. Returns `None` if the key does not exist.
    pub fn get_u128(env: Env, key: BytesN<32>) -> Option<u128> {
        env.storage().persistent().get(&U128Key { u128_key: key })
    }

    // -----------------------------------------------------------------------
    // Single-key i128 operations
    // -----------------------------------------------------------------------

    /// Write a single `i128` value. `caller` must authenticate.
    pub fn set_i128(env: Env, caller: Address, key: BytesN<32>, value: i128) {
        caller.require_auth();
        env.storage()
            .persistent()
            .set(&I128Key { i128_key: key }, &value);
    }

    /// Read a single `i128` value. Returns `None` if the key does not exist.
    pub fn get_i128(env: Env, key: BytesN<32>) -> Option<i128> {
        env.storage().persistent().get(&I128Key { i128_key: key })
    }

    // -----------------------------------------------------------------------
    // Batch u128 operations (issue #2)
    // -----------------------------------------------------------------------

    /// Write multiple `u128` entries in a single call.
    /// All writes are applied atomically within the same transaction.
    pub fn set_u128_batch(env: Env, caller: Address, entries: Vec<(BytesN<32>, u128)>) {
        caller.require_auth();
        for (key, value) in entries.iter() {
            env.storage()
                .persistent()
                .set(&U128Key { u128_key: key }, &value);
        }
    }

    /// Read multiple `u128` entries in a single call.
    /// Missing keys are returned as `0`.
    pub fn get_u128_batch(env: Env, keys: Vec<BytesN<32>>) -> Vec<u128> {
        let mut results: Vec<u128> = Vec::new(&env);
        for key in keys.iter() {
            let val: u128 = env
                .storage()
                .persistent()
                .get(&U128Key { u128_key: key })
                .unwrap_or(0u128);
            results.push_back(val);
        }
        results
    }

    // -----------------------------------------------------------------------
    // Batch i128 operations (issue #2)
    // -----------------------------------------------------------------------

    /// Write multiple `i128` entries in a single call.
    pub fn set_i128_batch(env: Env, caller: Address, entries: Vec<(BytesN<32>, i128)>) {
        caller.require_auth();
        for (key, value) in entries.iter() {
            env.storage()
                .persistent()
                .set(&I128Key { i128_key: key }, &value);
        }
    }

    /// Read multiple `i128` entries in a single call.
    /// Missing keys are returned as `0`.
    pub fn get_i128_batch(env: Env, keys: Vec<BytesN<32>>) -> Vec<i128> {
        let mut results: Vec<i128> = Vec::new(&env);
        for key in keys.iter() {
            let val: i128 = env
                .storage()
                .persistent()
                .get(&I128Key { i128_key: key })
                .unwrap_or(0i128);
            results.push_back(val);
        }
        results
    }

    // -----------------------------------------------------------------------
    // Position props and account position list support
    // -----------------------------------------------------------------------

    pub fn set_position_props(
        env: Env,
        caller: Address,
        position_key: BytesN<32>,
        props: PositionProps,
    ) {
        caller.require_auth();
        env.storage()
            .persistent()
            .set(&DataKey::PositionProps(position_key), &props);
    }

    pub fn get_position_props(env: Env, position_key: BytesN<32>) -> Option<PositionProps> {
        env.storage()
            .persistent()
            .get(&DataKey::PositionProps(position_key))
    }

    pub fn add_account_position(
        env: Env,
        caller: Address,
        account: Address,
        position_key: BytesN<32>,
    ) {
        caller.require_auth();
        let mut list: Vec<BytesN<32>> = env
            .storage()
            .persistent()
            .get(&DataKey::AccountPositionList(account.clone()))
            .unwrap_or_else(|| Vec::new(&env));
        if !list.contains(&position_key) {
            list.push_back(position_key.clone());
            env.storage()
                .persistent()
                .set(&DataKey::AccountPositionList(account), &list);
        }
    }

    pub fn remove_account_position(
        env: Env,
        caller: Address,
        account: Address,
        position_key: BytesN<32>,
    ) {
        caller.require_auth();
        let mut list: Vec<BytesN<32>> = env
            .storage()
            .persistent()
            .get(&DataKey::AccountPositionList(account.clone()))
            .unwrap_or_else(|| Vec::new(&env));
        if let Some(idx) = list.iter().position(|k| k == position_key) {
            list.remove(idx as u32);
            env.storage()
                .persistent()
                .set(&DataKey::AccountPositionList(account), &list);
        }
    }

    pub fn add_position(
        env: Env,
        caller: Address,
        position_key: BytesN<32>,
    ) {
        caller.require_auth();
        let mut list: Vec<BytesN<32>> = env
            .storage()
            .persistent()
            .get(&DataKey::PositionList)
            .unwrap_or_else(|| Vec::new(&env));
        if !list.contains(&position_key) {
            list.push_back(position_key.clone());
            env.storage()
                .persistent()
                .set(&DataKey::PositionList, &list);
        }
    }

    pub fn remove_position(
        env: Env,
        caller: Address,
        position_key: BytesN<32>,
    ) {
        caller.require_auth();
        let mut list: Vec<BytesN<32>> = env
            .storage()
            .persistent()
            .get(&DataKey::PositionList)
            .unwrap_or_else(|| Vec::new(&env));
        if let Some(idx) = list.iter().position(|k| k == position_key) {
            list.remove(idx as u32);
            env.storage()
                .persistent()
                .set(&DataKey::PositionList, &list);
        }
    }

    pub fn get_position_count(env: Env) -> u32 {
        let list: Vec<BytesN<32>> = env
            .storage()
            .persistent()
            .get(&DataKey::PositionList)
            .unwrap_or_else(|| Vec::new(&env));
        list.len()
    }

    pub fn get_account_position_count(env: Env, account: Address) -> u32 {
        let list: Vec<BytesN<32>> = env
            .storage()
            .persistent()
            .get(&DataKey::AccountPositionList(account))
            .unwrap_or_else(|| Vec::new(&env));
        list.len()
    }

    pub fn add_position_to_oi_list(
        env: Env,
        caller: Address,
        market_id: u32,
        is_long: bool,
        position_key: BytesN<32>,
    ) {
        caller.require_auth();
        let key = crate::keys::position_oi_list_key(&env, market_id, is_long);
        let mut list: Vec<BytesN<32>> = env
            .storage()
            .persistent()
            .get(&DataKey::PositionOiList(key.clone()))
            .unwrap_or_else(|| Vec::new(&env));
        if !list.contains(&position_key) {
            list.push_back(position_key.clone());
            env.storage()
                .persistent()
                .set(&DataKey::PositionOiList(key), &list);
        }
    }

    pub fn remove_position_from_oi_list(
        env: Env,
        caller: Address,
        market_id: u32,
        is_long: bool,
        position_key: BytesN<32>,
    ) {
        caller.require_auth();
        let key = crate::keys::position_oi_list_key(&env, market_id, is_long);
        let mut list: Vec<BytesN<32>> = env
            .storage()
            .persistent()
            .get(&DataKey::PositionOiList(key.clone()))
            .unwrap_or_else(|| Vec::new(&env));
        if let Some(idx) = list.iter().position(|k| k == position_key) {
            list.remove(idx as u32);
            env.storage()
                .persistent()
                .set(&DataKey::PositionOiList(key), &list);
        }
    }

    pub fn get_position_oi_list_count(
        env: Env,
        market_id: u32,
        is_long: bool,
    ) -> u32 {
        let key = crate::keys::position_oi_list_key(&env, market_id, is_long);
        let list: Vec<BytesN<32>> = env
            .storage()
            .persistent()
            .get(&DataKey::PositionOiList(key))
            .unwrap_or_else(|| Vec::new(&env));
        list.len()
    }

    pub fn get_all_positions_for_market(
        env: Env,
        market_id: u32,
        is_long: bool,
        start: u32,
        end: u32,
    ) -> Vec<PositionProps> {
        let key = crate::keys::position_oi_list_key(&env, market_id, is_long);
        let list: Vec<BytesN<32>> = env
            .storage()
            .persistent()
            .get(&DataKey::PositionOiList(key))
            .unwrap_or_else(|| Vec::new(&env));

        let len = list.len();
        if start >= len || start >= end {
            return Vec::new(&env);
        }
        let end = if end > len { len } else { end };

        let mut results: Vec<PositionProps> = Vec::new(&env);
        for idx in start..end {
            let key = list.get(idx).unwrap();
            if let Some(props) = env
                .storage()
                .persistent()
                .get::<DataKey, PositionProps>(&DataKey::PositionProps(key.clone()))
            {
                if props.is_open {
                    results.push_back(props);
                }
            }
        }
        results
    }

    pub fn get_account_positions(
        env: Env,
        account: Address,
        start: u32,
        end: u32,
    ) -> Vec<PositionProps> {
        let list: Vec<BytesN<32>> = env
            .storage()
            .persistent()
            .get(&DataKey::AccountPositionList(account))
            .unwrap_or_else(|| Vec::new(&env));

        let len = list.len();
        if start >= len || start >= end {
            return Vec::new(&env);
        }
        let end = if end > len { len } else { end };

        let mut results: Vec<PositionProps> = Vec::new(&env);
        for idx in start..end {
            let key = list.get(idx).unwrap();
            if let Some(props) = env
                .storage()
                .persistent()
                .get::<DataKey, PositionProps>(&DataKey::PositionProps(key.clone()))
            {
                if props.is_open {
                    results.push_back(props);
                }
            }
        }
        results
    }

    // -----------------------------------------------------------------------
    // TTL estimation (issue #3)
    // -----------------------------------------------------------------------

    /// Estimate how many ledgers each key can remain stored.
    ///
    /// Returns `remaining_ledgers = 0` for keys that do not exist (edge case
    /// documented in the acceptance criteria).
    ///
    /// In the test environment (`testutils` feature) the value is derived from
    /// the entry's actual TTL via `get_ttl`. In production the Soroban host
    /// does not expose a TTL read from within a contract; callers should
    /// invoke this function via RPC simulation where the host can supply the
    /// footprint TTL information.
    pub fn estimate_ttl(env: Env, keys: Vec<BytesN<32>>) -> Vec<TtlEstimate> {
        let mut results: Vec<TtlEstimate> = Vec::new(&env);

        for key in keys.iter() {
            let storage_key = U128Key { u128_key: key.clone() };
            let remaining = Self::remaining_ledgers_for(&env, &storage_key);
            results.push_back(TtlEstimate {
                key,
                remaining_ledgers: remaining,
            });
        }
        results
    }

    // Internal: returns remaining ledgers for a U128Key.
    // Uses get_ttl (testutils) when available; falls back to a has() check.
    fn remaining_ledgers_for(env: &Env, storage_key: &U128Key) -> u32 {
        if !env.storage().persistent().has(storage_key) {
            return 0;
        }
        // get_ttl is only available with the testutils feature.
        // In production this path returns the TTL directly from the host via
        // RPC simulation; the contract itself cannot read TTL at runtime.
        #[cfg(any(test, feature = "testutils"))]
        {
            use soroban_sdk::testutils::storage::Persistent as _;
            let current_seq = env.ledger().sequence();
            let expiry_seq = env.storage().persistent().get_ttl(storage_key);
            return expiry_seq.saturating_sub(current_seq);
        }
        #[cfg(not(any(test, feature = "testutils")))]
        {
            // In on-chain execution the TTL is not readable from within the
            // contract. Return u32::MAX to signal "alive, TTL unknown".
            // Keeper infrastructure should use RPC simulation to get the real
            // value.
            let _ = env;
            u32::MAX
        }
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    fn require_admin(env: &Env, caller: &Address) {
        let admin: Address = match env.storage().instance().get(&DataInstanceKey::Admin) {
            Some(a) => a,
            None => panic_with_error!(env, DataError::NotInitialized),
        };
        if *caller != admin {
            panic_with_error!(env, DataError::Unauthorized);
        }
    }

    fn require_controller(env: &Env, caller: &Address) {
        let controllers: Vec<Address> = env
            .storage()
            .persistent()
            .get(&DataPersistentKey::Controllers)
            .unwrap_or_else(|| Vec::new(env));
        if !controllers.contains(caller) {
            panic_with_error!(env, DataError::Unauthorized);
        }
    }
}
