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
/// `market_id`.
pub fn market_paused_key(env: &Env, market_id: u32) -> BytesN<32> {
    let mut buf = [0u8; 32];
    buf[..8].copy_from_slice(b"mpaused_");
    let id_bytes = market_id.to_be_bytes();
    buf[8..12].copy_from_slice(&id_bytes);
    BytesN::from_array(env, &buf)
}

/// Returns the data-store key holding the total number of markets ever created.
pub fn market_count_key(env: &Env) -> BytesN<32> {
    let mut buf = [0u8; 32];
    buf[..9].copy_from_slice(b"mkt_count");
    BytesN::from_array(env, &buf)
}

// ---------------------------------------------------------------------------
// Liquidity key generators
// ---------------------------------------------------------------------------

fn market_scoped_key(env: &Env, prefix: &[u8; 8], market_id: u32) -> BytesN<32> {
    let mut buf = [0u8; 32];
    buf[..8].copy_from_slice(prefix);
    buf[8..12].copy_from_slice(&market_id.to_be_bytes());
    BytesN::from_array(env, &buf)
}

pub fn pool_long_amount_key(env: &Env, market_id: u32) -> BytesN<32> {
    market_scoped_key(env, b"plong_am", market_id)
}

pub fn pool_short_amount_key(env: &Env, market_id: u32) -> BytesN<32> {
    market_scoped_key(env, b"pshrt_am", market_id)
}

pub fn claimable_fee_long_key(env: &Env, market_id: u32) -> BytesN<32> {
    market_scoped_key(env, b"clmfee_l", market_id)
}

pub fn claimable_fee_short_key(env: &Env, market_id: u32) -> BytesN<32> {
    market_scoped_key(env, b"clmfee_s", market_id)
}

pub fn withdrawal_fee_factor_key(env: &Env, market_id: u32) -> BytesN<32> {
    market_scoped_key(env, b"wfeefact", market_id)
}

pub fn market_maintenance_margin_factor_key(env: &Env, market_id: u32) -> BytesN<32> {
    market_scoped_key(env, b"mm_factr", market_id)
}

pub fn position_oi_list_key(env: &Env, market_id: u32, is_long: bool) -> BytesN<32> {
    let mut buf = [0u8; 32];
    buf[..7].copy_from_slice(b"poilist");
    let id_bytes = market_id.to_be_bytes();
    buf[7..11].copy_from_slice(&id_bytes);
    buf[11] = if is_long { 1 } else { 0 };
    BytesN::from_array(env, &buf)
}

pub fn position_list_key(env: &Env) -> BytesN<32> {
    let mut buf = [0u8; 32];
    buf[..8].copy_from_slice(b"pos_list");
    BytesN::from_array(env, &buf)
}

// ---------------------------------------------------------------------------
// Position / open-interest key generators
// ---------------------------------------------------------------------------

pub fn open_interest_long_key(env: &Env, market_id: u32) -> BytesN<32> {
    market_scoped_key(env, b"oi_long_", market_id)
}

pub fn open_interest_short_key(env: &Env, market_id: u32) -> BytesN<32> {
    market_scoped_key(env, b"oi_shrt_", market_id)
}

pub fn max_open_interest_long_key(env: &Env, market_id: u32) -> BytesN<32> {
    market_scoped_key(env, b"maxoi_lo", market_id)
}

pub fn max_open_interest_short_key(env: &Env, market_id: u32) -> BytesN<32> {
    market_scoped_key(env, b"maxoi_sh", market_id)
}

pub fn account_balance_key(env: &Env, market_id: u32) -> BytesN<32> {
    market_scoped_key(env, b"acct_bal", market_id)
}

/// Returns the data-store key for the maximum number of markets allowed in a
/// swap path.
pub fn max_swap_path_length_key(env: &Env) -> BytesN<32> {
    let mut buf = [0u8; 32];
    buf[..8].copy_from_slice(b"maxswpth");
    BytesN::from_array(env, &buf)
}

// ---------------------------------------------------------------------------
// Config handler key generators
// ---------------------------------------------------------------------------

pub fn max_pool_amount_key(env: &Env, market_id: u32) -> BytesN<32> {
    market_scoped_key(env, b"cfgmxpam", market_id)
}

pub fn max_open_interest_key(env: &Env, market_id: u32) -> BytesN<32> {
    market_scoped_key(env, b"cfgmxpoi", market_id)
}

pub fn position_fee_factor_key(env: &Env, market_id: u32) -> BytesN<32> {
    market_scoped_key(env, b"cfgpffee", market_id)
}

pub fn borrowing_factor_key(env: &Env, market_id: u32) -> BytesN<32> {
    market_scoped_key(env, b"cfgbrrwf", market_id)
}

pub fn funding_factor_key(env: &Env, market_id: u32) -> BytesN<32> {
    market_scoped_key(env, b"cfgfundf", market_id)
}

pub fn min_collateral_factor_key(env: &Env, market_id: u32) -> BytesN<32> {
    market_scoped_key(env, b"cfgmncol", market_id)
}

pub fn max_leverage_key(env: &Env, market_id: u32) -> BytesN<32> {
    market_scoped_key(env, b"cfgmxlev", market_id)
}

// ---------------------------------------------------------------------------
// ADL key generators
// ---------------------------------------------------------------------------

pub fn max_pnl_factor_key(env: &Env, market_id: u32) -> BytesN<32> {
    market_scoped_key(env, b"maxpnlfc", market_id)
}
