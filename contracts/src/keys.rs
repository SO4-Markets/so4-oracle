use soroban_sdk::{BytesN, Env};

// ---------------------------------------------------------------------------
// Market key generators
// ---------------------------------------------------------------------------

/// Returns the data-store key that holds the config for `market_id`.
pub fn market_props_key(env: &Env, market_id: u32) -> BytesN<32> {
    let mut buf = [0u8; 32];
    buf[..6].copy_from_slice(b"mprops");
    let id_bytes = market_id.to_be_bytes();
    buf[6..10].copy_from_slice(&id_bytes);
    BytesN::from_array(env, &buf)
}

/// Returns the data-store key for the long-token address of `market_id`.
pub fn market_long_token_key(env: &Env, market_id: u32) -> BytesN<32> {
    let mut buf = [0u8; 32];
    buf[..8].copy_from_slice(b"mlt_addr");
    let id_bytes = market_id.to_be_bytes();
    buf[8..12].copy_from_slice(&id_bytes);
    BytesN::from_array(env, &buf)
}

/// Returns the data-store key for the short-token address of `market_id`.
pub fn market_short_token_key(env: &Env, market_id: u32) -> BytesN<32> {
    let mut buf = [0u8; 32];
    buf[..8].copy_from_slice(b"mst_addr");
    let id_bytes = market_id.to_be_bytes();
    buf[8..12].copy_from_slice(&id_bytes);
    BytesN::from_array(env, &buf)
}

/// Returns the data-store key for the market-token address of `market_id`.
pub fn market_token_key(env: &Env, market_id: u32) -> BytesN<32> {
    let mut buf = [0u8; 32];
    buf[..7].copy_from_slice(b"mtkaddr");
    let id_bytes = market_id.to_be_bytes();
    buf[7..11].copy_from_slice(&id_bytes);
    BytesN::from_array(env, &buf)
}

/// Returns the data-store key that holds a `u128` flag (1 = paused) for
/// `market_id`.  Used by issue #11 pause/unpause logic.
pub fn market_paused_key(env: &Env, market_id: u32) -> BytesN<32> {
    let mut buf = [0u8; 32];
    buf[..8].copy_from_slice(b"mpaused_");
    let id_bytes = market_id.to_be_bytes();
    buf[8..12].copy_from_slice(&id_bytes);
    BytesN::from_array(env, &buf)
}

/// Returns the data-store key holding the total number of markets ever created
/// (monotonically increasing counter).
pub fn market_count_key(env: &Env) -> BytesN<32> {
    let mut buf = [0u8; 32];
    buf[..9].copy_from_slice(b"mkt_count");
    BytesN::from_array(env, &buf)
}

// ---------------------------------------------------------------------------
// Liquidity key generators (pools, fees) — issues #17, #20
// ---------------------------------------------------------------------------

/// Builds a 32-byte key from an 8-byte ASCII `prefix` and `market_id`.
fn market_scoped_key(env: &Env, prefix: &[u8; 8], market_id: u32) -> BytesN<32> {
    let mut buf = [0u8; 32];
    buf[..8].copy_from_slice(prefix);
    buf[8..12].copy_from_slice(&market_id.to_be_bytes());
    BytesN::from_array(env, &buf)
}

/// Returns the data-store key for the long-token pool amount of `market_id`.
pub fn pool_long_amount_key(env: &Env, market_id: u32) -> BytesN<32> {
    market_scoped_key(env, b"plong_am", market_id)
}

/// Returns the data-store key for the short-token pool amount of `market_id`.
pub fn pool_short_amount_key(env: &Env, market_id: u32) -> BytesN<32> {
    market_scoped_key(env, b"pshrt_am", market_id)
}

/// Returns the data-store key for the claimable long-token fee of `market_id`.
/// Fees accrue here and are claimable by the `fee_handler`.
pub fn claimable_fee_long_key(env: &Env, market_id: u32) -> BytesN<32> {
    market_scoped_key(env, b"clmfee_l", market_id)
}

/// Returns the data-store key for the claimable short-token fee of `market_id`.
pub fn claimable_fee_short_key(env: &Env, market_id: u32) -> BytesN<32> {
    market_scoped_key(env, b"clmfee_s", market_id)
}

/// Returns the data-store key holding the optional withdrawal fee factor for
/// `market_id`. When unset (or zero) no fee is charged. The factor is expressed
/// against [`crate::liquidity_handler::FEE_FACTOR_DENOMINATOR`].
pub fn withdrawal_fee_factor_key(env: &Env, market_id: u32) -> BytesN<32> {
    market_scoped_key(env, b"wfeefact", market_id)
}

/// Returns the data-store key holding the maintenance margin factor for
/// `market_id`.
pub fn market_maintenance_margin_factor_key(env: &Env, market_id: u32) -> BytesN<32> {
    market_scoped_key(env, b"mm_factr", market_id)
}

// ---------------------------------------------------------------------------
// Position / open-interest key generators (issues #43, #45, #46)
// ---------------------------------------------------------------------------

/// Returns the data-store key for the current long open interest of `market_id`.
pub fn open_interest_long_key(env: &Env, market_id: u32) -> BytesN<32> {
    market_scoped_key(env, b"oi_long_", market_id)
}

/// Returns the data-store key for the current short open interest of `market_id`.
pub fn open_interest_short_key(env: &Env, market_id: u32) -> BytesN<32> {
    market_scoped_key(env, b"oi_shrt_", market_id)
}

/// Returns the data-store key for the max open interest cap on the long side.
pub fn max_open_interest_long_key(env: &Env, market_id: u32) -> BytesN<32> {
    market_scoped_key(env, b"maxoi_lo", market_id)
}

/// Returns the data-store key for the max open interest cap on the short side.
pub fn max_open_interest_short_key(env: &Env, market_id: u32) -> BytesN<32> {
    market_scoped_key(env, b"maxoi_sh", market_id)
}

/// Returns the data-store key for the account balance used in PnL settlement.
pub fn account_balance_key(env: &Env, market_id: u32) -> BytesN<32> {
    market_scoped_key(env, b"acct_bal", market_id)
}
