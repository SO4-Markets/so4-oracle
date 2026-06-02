//! Tests for market-token allowance TTL expiry.
//!
//! #13 test(market_token): allowance TTL expiry test
//!
//! Soroban's SEP-41 `approve()` stores allowances in temporary storage with
//! an `expiration_ledger`. These tests verify that:
//!   - `allowance()` returns 0 after the ledger advances past `expiration_ledger`
//!   - `transfer_from` panics with InsufficientAllowance post-expiry

#![cfg(test)]

use soroban_sdk::{
    testutils::{Address as _, Ledger as _},
    token::{StellarAssetClient, TokenClient},
    Address, Env,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Deploy a native/Stellar asset token contract and return both the admin
/// client (for minting / approving as issuer) and the standard token client.
fn setup_token(env: &Env) -> (Address, StellarAssetClient, TokenClient) {
    let admin = Address::generate(env);
    let contract_id = env.register_stellar_asset_contract_v2(admin.clone());
    let sac = StellarAssetClient::new(env, &contract_id.address());
    let token = TokenClient::new(env, &contract_id.address());
    (admin, sac, token)
}

// ---------------------------------------------------------------------------
// Issue #13 — allowance TTL expiry
// ---------------------------------------------------------------------------

/// An allowance approved with `expiration_ledger = N` must return 0 once the
/// current ledger sequence advances past N.
#[test]
fn test_allowance_returns_zero_after_expiry() {
    let env = Env::default();
    env.mock_all_auths();

    let (admin, sac, token) = setup_token(&env);

    let owner = Address::generate(&env);
    let spender = Address::generate(&env);

    // Fund the owner so the approve is meaningful.
    sac.mint(&owner, &1_000i128);

    // Current ledger sequence is 0; set expiry one ledger ahead.
    let current_seq = env.ledger().sequence();
    let expiration_ledger = current_seq + 1;

    // Approve 500 tokens with a TTL that expires at ledger `expiration_ledger`.
    token.approve(&owner, &spender, &500i128, &expiration_ledger);

    // Confirm the allowance is visible before expiry.
    assert_eq!(
        token.allowance(&owner, &spender),
        500i128,
        "allowance should be 500 before expiry"
    );

    // Advance the ledger past the expiration point.
    env.ledger().with_mut(|li| {
        li.sequence_number = expiration_ledger + 1;
    });

    // Post-expiry: allowance must return 0.
    assert_eq!(
        token.allowance(&owner, &spender),
        0i128,
        "allowance should be 0 after expiration_ledger has passed"
    );
}

/// `transfer_from` must panic with InsufficientAllowance once the allowance
/// TTL has expired, even if the approved amount was non-zero.
#[test]
#[should_panic]
fn test_transfer_from_panics_after_allowance_expiry() {
    let env = Env::default();
    env.mock_all_auths();

    let (admin, sac, token) = setup_token(&env);

    let owner = Address::generate(&env);
    let spender = Address::generate(&env);
    let recipient = Address::generate(&env);

    // Fund the owner.
    sac.mint(&owner, &1_000i128);

    // Approve with an expiry of ledger 5.
    let expiration_ledger: u32 = 5;
    token.approve(&owner, &spender, &500i128, &expiration_ledger);

    // Advance ledger past expiry.
    env.ledger().with_mut(|li| {
        li.sequence_number = expiration_ledger + 1;
    });

    // This must panic: the allowance has expired → InsufficientAllowance.
    token.transfer_from(&spender, &owner, &recipient, &100i128);
}

/// Sanity check: `transfer_from` succeeds when called *before* the TTL
/// expires, confirming that the expiry logic does not affect valid spends.
#[test]
fn test_transfer_from_succeeds_before_expiry() {
    let env = Env::default();
    env.mock_all_auths();

    let (admin, sac, token) = setup_token(&env);

    let owner = Address::generate(&env);
    let spender = Address::generate(&env);
    let recipient = Address::generate(&env);

    sac.mint(&owner, &1_000i128);

    // Approve with a generous future expiry.
    let expiration_ledger: u32 = env.ledger().sequence() + 100;
    token.approve(&owner, &spender, &500i128, &expiration_ledger);

    // Transfer 200 within the valid window.
    token.transfer_from(&spender, &owner, &recipient, &200i128);

    // Remaining allowance: 500 - 200 = 300.
    assert_eq!(
        token.allowance(&owner, &spender),
        300i128,
        "allowance should be reduced by the transfer amount"
    );

    // Recipient balance updated correctly.
    assert_eq!(
        token.balance(&recipient),
        200i128,
        "recipient should have received 200 tokens"
    );
}

/// Boundary: approving at exactly the current ledger sequence means the
/// allowance is already expired on the next ledger.
#[test]
fn test_allowance_expired_at_boundary_ledger() {
    let env = Env::default();
    env.mock_all_auths();

    let (admin, sac, token) = setup_token(&env);

    let owner = Address::generate(&env);
    let spender = Address::generate(&env);

    sac.mint(&owner, &1_000i128);

    // Set expiration_ledger = current sequence (expires immediately on advance).
    let expiration_ledger = env.ledger().sequence();
    token.approve(&owner, &spender, &500i128, &expiration_ledger);

    // Advance by exactly 1 ledger.
    env.ledger().with_mut(|li| {
        li.sequence_number = expiration_ledger + 1;
    });

    // Must be expired.
    assert_eq!(
        token.allowance(&owner, &spender),
        0i128,
        "allowance approved at boundary ledger should be 0 after any advance"
    );
}