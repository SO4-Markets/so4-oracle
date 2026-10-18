pub mod auth;
pub mod binance;
pub mod chain;
pub mod coinbase;
pub mod config;
pub mod fixed;
pub mod http;
pub mod keeper;
pub mod keeper_loop;
pub mod metrics;
pub mod network_config;
pub mod price_loop;
pub mod prices;
pub mod pyth;
pub mod retry;
pub mod signing;
pub mod state;
pub mod stellar_rpc;
pub mod submit;

pub mod api;

pub use config::{Config, EnvErrors};
pub use state::AppState;

use std::time::{SystemTime, UNIX_EPOCH};

/// Number of decimal places every on-chain price is scaled to.
///
/// The single source of truth for the precision invariant (#709):
/// [`FLOAT_PRECISION`] and every per-provider exponent bound in `binance.rs`
/// and `pyth.rs` are derived from this, so the scale can't drift between
/// files.
pub const SCALE_DIGITS: u32 = 30;

/// `10^SCALE_DIGITS` — the fixed-point scaling factor applied to prices before
/// they go on-chain.
pub const FLOAT_PRECISION: i128 = 10i128.pow(SCALE_DIGITS);

/// Highest price a single source may report, in [`FLOAT_PRECISION`]-scaled
/// units.
///
/// This is `10_000_000` whole units ($10,000,000 at [`SCALE_DIGITS`]
/// decimals) — two orders of magnitude above any plausible listed asset —
/// while staying far enough below `i128::MAX` that aggregation arithmetic on
/// accepted prices cannot overflow. A source value above this bound is
/// rejected where the raw string is parsed, before it can reach the
/// median/mean math, where near-`i128::MAX` inputs used to overflow the
/// unchecked additions; with `overflow-checks` enabled in the release profile
/// that panic kills the price loop and main.rs shuts the whole process down,
/// instead of the quote being rejected as an outlier (#1290).
pub const MAX_SANE_PRICE: i128 = FLOAT_PRECISION * 10_000_000;

/// Returns the current Unix timestamp in seconds.
///
/// This is a convenience wrapper around `SystemTime::now() - UNIX_EPOCH`
/// that returns the duration as seconds (`u64`). Closes #399.
pub fn current_timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}
