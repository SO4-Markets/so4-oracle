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
pub mod preflight;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn system_now_secs() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("test host clock is after the Unix epoch")
            .as_secs()
    }

    // #1420 — 11 call sites read prices' and cycles' ages from this value.
    #[test]
    fn current_timestamp_secs_tracks_the_system_clock() {
        let before = system_now_secs();
        let now = current_timestamp_secs();
        let after = system_now_secs();
        assert!(
            before <= now && now <= after,
            "{before} <= {now} <= {after}"
        );
    }

    // `unwrap_or(0)` only applies to a pre-epoch clock; on any sane host the
    // value must be a real, recent timestamp, never the 0 fallback.
    #[test]
    fn current_timestamp_secs_is_not_the_pre_epoch_fallback() {
        const JAN_1_2020: u64 = 1_577_836_800;
        assert!(current_timestamp_secs() > JAN_1_2020);
    }
}
