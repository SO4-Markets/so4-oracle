pub mod price;

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
pub mod reader;
pub mod retry;
pub mod scval;
pub mod signing;
pub mod state;
pub mod stellar_rpc;
pub mod submit;
pub mod tx_builder;

pub mod api;

pub use config::Config;
pub use state::AppState;

use std::time::{SystemTime, UNIX_EPOCH};

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
