//! Pool token movement helpers for the liquidity handler.
//!
//! A market's "pool" is simply the balance of the long/short Soroban tokens
//! held by the liquidity-handler contract (the vault). These helpers wrap the
//! SEP-41 token client so the handler can move pool tokens in and out without
//! repeating the `current_contract_address` / `i128` plumbing at every call
//! site.

use soroban_sdk::{token, Address, Env};

/// Transfer `amount` of `token_addr` from `payer` into the pool (the
/// liquidity-handler contract). Used when funding a deposit.
pub fn deposit_to_pool(env: &Env, token_addr: &Address, payer: &Address, amount: u128) {
    if amount == 0 {
        return;
    }
    token::TokenClient::new(env, token_addr).transfer(
        payer,
        &env.current_contract_address(),
        &(amount as i128),
    );
}

/// Transfer `amount` of `token_addr` out of the pool (the liquidity-handler
/// contract) to `receiver`. Used when paying out a withdrawal.
pub fn withdraw_from_pool(env: &Env, token_addr: &Address, receiver: &Address, amount: u128) {
    if amount == 0 {
        return;
    }
    token::TokenClient::new(env, token_addr).transfer(
        &env.current_contract_address(),
        receiver,
        &(amount as i128),
    );
}

/// Returns the pool balance of `token_addr` currently held by the handler.
pub fn pool_balance(env: &Env, token_addr: &Address) -> i128 {
    token::TokenClient::new(env, token_addr).balance(&env.current_contract_address())
}
