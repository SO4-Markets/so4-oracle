//! Shared `reqwest` HTTP client.
//!
//! A single connection-pooled client is reused across all outbound calls
//! (price sources, Stellar RPC, Horizon, Friendbot). Replaces the Cloudflare
//! Worker `worker::Fetch` API now that the oracle runs as a native service.

use std::sync::OnceLock;
use std::time::Duration;

/// Returns the process-wide shared HTTP client, initializing it on first use (resolves #355).
pub fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .user_agent("so4-oracle/0.1")
            .build()
            .expect("failed to build reqwest client")
    })
}
