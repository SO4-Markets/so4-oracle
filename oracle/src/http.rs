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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_body_is_trimmed_and_otherwise_unchanged() {
        assert_eq!(truncate_error_body("  rate limited \n"), "rate limited");
    }

    #[test]
    fn body_at_exactly_the_cap_is_not_truncated() {
        let body = "a".repeat(HTTP_ERROR_BODY_MAX_CHARS);
        assert_eq!(truncate_error_body(&body), body);
    }

    #[test]
    fn longer_body_is_cut_to_exactly_the_cap() {
        let body = "a".repeat(HTTP_ERROR_BODY_MAX_CHARS + 100);
        let truncated = truncate_error_body(&body);
        assert_eq!(truncated.chars().count(), HTTP_ERROR_BODY_MAX_CHARS);
    }

    // The cap counts chars, not bytes: multi-byte text is never split
    // mid-character, and the result may exceed the cap in bytes.
    #[test]
    fn multi_byte_body_is_cut_on_char_boundaries() {
        let body = "€".repeat(HTTP_ERROR_BODY_MAX_CHARS + 1);
        let truncated = truncate_error_body(&body);
        assert_eq!(truncated.chars().count(), HTTP_ERROR_BODY_MAX_CHARS);
        assert_eq!(truncated.len(), HTTP_ERROR_BODY_MAX_CHARS * '€'.len_utf8());
        assert!(truncated.chars().all(|c| c == '€'));
    }

    #[test]
    fn whitespace_only_body_becomes_empty() {
        assert_eq!(truncate_error_body(" \t\r\n "), "");
    }

    // #355 — the OnceLock must hand out one shared, connection-pooled client.
    #[test]
    fn client_is_a_process_wide_singleton() {
        assert!(std::ptr::eq(client(), client()));
    }
}
