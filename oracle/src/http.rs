//! Shared `reqwest` HTTP client.
//!
//! A single connection-pooled client is reused across all outbound calls
//! (price sources, Stellar RPC, Horizon, Friendbot). Replaces the Cloudflare
//! Worker `worker::Fetch` API now that the oracle runs as a native service.

use std::sync::OnceLock;
use std::time::Duration;

/// Max chars of an HTTP error body retained in error variants / logs.
pub const HTTP_ERROR_BODY_MAX_CHARS: usize = 512;

/// Truncate a response body for inclusion in error types.
pub fn truncate_error_body(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.chars().count() <= HTTP_ERROR_BODY_MAX_CHARS {
        return trimmed.to_string();
    }
    trimmed.chars().take(HTTP_ERROR_BODY_MAX_CHARS).collect()
}

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

/// Header carrying the correlation id on every outbound request (#1455).
pub const REQUEST_ID_HEADER: &str = "x-request-id";

tokio::task_local! {
    /// Correlation id for the work running in the current task: the inbound
    /// request's `x-request-id` for API handlers, or a generated cycle id for
    /// the price and keeper loops.
    static CORRELATION_ID: String;
}

/// Runs `fut` with `id` as the correlation id that [`with_correlation`]
/// attaches to outbound requests made inside it.
pub async fn with_correlation_id<F: std::future::Future>(id: String, fut: F) -> F::Output {
    CORRELATION_ID.scope(id, fut).await
}

/// The correlation id of the current task, if one was set.
pub fn current_correlation_id() -> Option<String> {
    CORRELATION_ID.try_with(|id| id.clone()).ok()
}

/// A fresh random id for a background cycle.
pub fn new_cycle_id() -> String {
    format!("{:016x}", rand::random::<u64>())
}

/// Adds the current correlation id as an `x-request-id` header, so an
/// upstream failure can be matched to the inbound request or loop cycle that
/// caused it, and quoted to the provider.
pub fn with_correlation(request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    match current_correlation_id() {
        Some(id) => request.header(REQUEST_ID_HEADER, id),
        None => request,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn seen_id() -> Option<String> {
        current_correlation_id()
    }

    fn build_get() -> reqwest::Request {
        with_correlation(client().get("http://127.0.0.1:9/"))
            .build()
            .unwrap()
    }

    async fn build_get_async() -> reqwest::Request {
        build_get()
    }

    #[tokio::test]
    async fn correlation_id_is_scoped_to_the_future() {
        assert_eq!(seen_id().await, None);
        let seen = with_correlation_id("a1".into(), seen_id()).await;
        assert_eq!(seen.as_deref(), Some("a1"));
        assert_eq!(seen_id().await, None);
    }

    #[tokio::test]
    async fn outbound_requests_carry_the_correlation_id() {
        let built = with_correlation_id("r1".into(), build_get_async()).await;
        assert_eq!(built.headers().get(REQUEST_ID_HEADER).unwrap(), "r1");
        assert!(build_get().headers().get(REQUEST_ID_HEADER).is_none());
    }

    #[test]
    fn cycle_ids_are_16_hex_chars() {
        let id = new_cycle_id();
        assert_eq!(id.len(), 16);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
