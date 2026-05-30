//! Shared token configuration for the so4-oracle workspace (Issue #165).
//!
//! Both the `oracle` (Cloudflare Worker) and `apis` (Axum server) crates
//! consume the same `TokenConfig` definition, eliminating the duplication
//! that previously existed between `oracle::config` and `apis::config`.
//!
//! Loading precedence:
//!   - **oracle (worker):** env var `PRICE_FEED_CONFIG` (JSON string) takes
//!     priority; falls back to the bundled `tokens.json` file.
//!   - **apis (server):** reads `config/tokens.json` from the filesystem.

use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

// ── Unified token config ─────────────────────────────────────────────────────

/// A single token entry used by both the oracle cron pipeline and the API
/// server.  Fields cover both use-cases:
///   - `symbol`, `stellar_address`, `sources` — oracle feed config
///   - `min`, `max`, `sources_used` — API price-lookup metadata
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct TokenConfig {
    /// Token symbol, e.g. "BTC", "ETH".  Used as the canonical key.
    pub symbol: String,
    /// Stellar contract address for the token.
    #[serde(default)]
    pub stellar_address: String,
    /// Price sources the oracle should query (e.g. `["binance", "coinbase"]`).
    #[serde(default)]
    pub sources: Vec<String>,
    /// Optional Binance-specific symbol override (e.g. "BTCUSDT").
    #[serde(default)]
    pub binance_symbol: Option<String>,
    /// Optional Pyth feed ID.
    #[serde(default)]
    pub pyth_feed_id: Option<String>,
    /// Minimum price bound (used by the API server for display).
    #[serde(default)]
    pub min: f64,
    /// Maximum price bound (used by the API server for display).
    #[serde(default)]
    pub max: f64,
    /// Sources that contributed to the latest price (populated at runtime).
    #[serde(default)]
    pub sources_used: Vec<String>,
}

/// Canonical token address for lookups.  Returns `stellar_address` if set,
/// otherwise falls back to the lowercased symbol.
impl TokenConfig {
    pub fn lookup_key(&self) -> String {
        if self.stellar_address.is_empty() {
            self.symbol.to_lowercase()
        } else {
            self.stellar_address.to_lowercase()
        }
    }
}

// ── Loading helpers ──────────────────────────────────────────────────────────

/// Error type for configuration loading.
#[derive(Debug)]
pub enum ConfigError {
    /// The `PRICE_FEED_CONFIG` env var is missing.
    MissingEnvVar,
    /// JSON parsing failed.
    MalformedJson(String),
    /// The token list is empty.
    EmptyTokenList,
    /// A token entry is invalid.
    InvalidToken { symbol: String, reason: String },
    /// File I/O error.
    IoError(String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::MissingEnvVar => {
                write!(f, "required env var 'PRICE_FEED_CONFIG' is not set")
            }
            ConfigError::MalformedJson(msg) => {
                write!(f, "PRICE_FEED_CONFIG is not valid JSON: {msg}")
            }
            ConfigError::EmptyTokenList => {
                write!(f, "PRICE_FEED_CONFIG must contain at least one token")
            }
            ConfigError::InvalidToken { symbol, reason } => {
                write!(f, "invalid token config for '{symbol}': {reason}")
            }
            ConfigError::IoError(msg) => {
                write!(f, "failed to read token config file: {msg}")
            }
        }
    }
}

impl std::error::Error for ConfigError {}

/// Parse a JSON array of `TokenConfig` entries and validate required fields.
pub fn parse_token_configs(raw: &str) -> Result<Vec<TokenConfig>, ConfigError> {
    let tokens: Vec<TokenConfig> =
        serde_json::from_str(raw).map_err(|e| ConfigError::MalformedJson(e.to_string()))?;

    if tokens.is_empty() {
        return Err(ConfigError::EmptyTokenList);
    }

    for token in &tokens {
        if token.symbol.is_empty() {
            return Err(ConfigError::InvalidToken {
                symbol: "(empty)".to_string(),
                reason: "symbol must not be empty".to_string(),
            });
        }
        // stellar_address and sources are optional for the API server path,
        // but required for the oracle path — the oracle validates separately.
    }

    Ok(tokens)
}

/// Load tokens from the `PRICE_FEED_CONFIG` env var (JSON string).
/// Returns `None` if the var is not set (caller can fall back to file).
pub fn load_from_env_var(env_value: Option<&str>) -> Result<Option<Vec<TokenConfig>>, ConfigError> {
    match env_value {
        Some(raw) => parse_token_configs(raw).map(Some),
        None => Ok(None),
    }
}

/// Load tokens from a JSON file on disk.
pub fn load_from_file(path: &Path) -> Result<Vec<TokenConfig>, ConfigError> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| ConfigError::IoError(e.to_string()))?;
    parse_token_configs(&raw)
}

/// Build a lookup map keyed by lowercased symbol.
pub fn build_lookup(tokens: &[TokenConfig]) -> HashMap<String, &TokenConfig> {
    let mut map = HashMap::new();
    for token in tokens {
        map.insert(token.symbol.to_lowercase(), token);
    }
    map
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_JSON: &str = r#"[
        {"symbol":"BTC","stellar_address":"CBTCADDR","sources":["binance","coinbase"],"min":44000.0,"max":46000.0},
        {"symbol":"ETH","stellar_address":"CETHADDR","sources":["binance"],"min":2400.0,"max":2600.0}
    ]"#;

    #[test]
    fn parse_valid_config() {
        let tokens = parse_token_configs(VALID_JSON).unwrap();
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[0].symbol, "BTC");
        assert_eq!(tokens[0].sources, vec!["binance", "coinbase"]);
        assert_eq!(tokens[0].min, 44000.0);
    }

    #[test]
    fn reject_malformed_json() {
        let err = parse_token_configs("{not json}").unwrap_err();
        assert!(matches!(err, ConfigError::MalformedJson(_)));
    }

    #[test]
    fn reject_empty_list() {
        let err = parse_token_configs("[]").unwrap_err();
        assert!(matches!(err, ConfigError::EmptyTokenList));
    }

    #[test]
    fn reject_empty_symbol() {
        let json = r#"[{"symbol":"","stellar_address":"CADDR","sources":["binance"]}]"#;
        let err = parse_token_configs(json).unwrap_err();
        assert!(matches!(err, ConfigError::InvalidToken { .. }));
    }

    #[test]
    fn load_from_env_var_returns_none_when_unset() {
        let result = load_from_env_var(None).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn load_from_env_var_parses_json() {
        let result = load_from_env_var(Some(VALID_JSON)).unwrap().unwrap();
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn lookup_key_uses_stellar_address() {
        let tokens = parse_token_configs(VALID_JSON).unwrap();
        assert_eq!(tokens[0].lookup_key(), "cbtcaddr");
    }

    #[test]
    fn lookup_key_falls_back_to_symbol() {
        let json = r#"[{"symbol":"BTC","sources":["binance"]}]"#;
        let tokens = parse_token_configs(json).unwrap();
        assert_eq!(tokens[0].lookup_key(), "btc");
    }

    #[test]
    fn build_lookup_creates_lowercase_map() {
        let tokens = parse_token_configs(VALID_JSON).unwrap();
        let map = build_lookup(&tokens);
        assert!(map.contains_key("btc"));
        assert!(map.contains_key("eth"));
        assert!(!map.contains_key("BTC"));
    }
}
