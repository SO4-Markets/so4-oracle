//! Shared token configuration for the so4-oracle workspace.
//!
//! The oracle Worker consumes `TokenConfig` through `PRICE_FEED_CONFIG`.
//! `config/tokens.json` remains as a checked-in example for local setup.

use serde::Deserialize;

// ── Unified token config ─────────────────────────────────────────────────────

/// A single token entry used by both the oracle cron pipeline and the API
/// server. Every field here is consumed somewhere: `symbol`,
/// `stellar_address` and `sources` drive the oracle feed, the `*_bps` and
/// `stale_after_seconds` fields are the risk/freshness thresholds, and
/// `display_symbol` is surfaced by the API.
///
/// There are deliberately no static price-bound fields. An earlier revision
/// carried `min`/`max` documented as "used by the API server for display", but
/// nothing ever read them — the API serves the runtime percentile bounds from
/// `CachedPrice`/`AggregatedPrice`, which are a different pair of values. They
/// were removed rather than wired up (the same treatment the already-removed
/// `sources_used` field got), so a config that still sets them now fails the
/// `deny_unknown_fields` check below instead of being silently ignored.
/// #504 — deny_unknown_fields ensures a typo'd key (e.g. "max_deviaton_bps") is
/// rejected at parse time instead of being silently ignored and falling back to
/// the Default value, which would let the oracle run with wrong risk thresholds.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct TokenConfig {
    /// On-chain token symbol, e.g. "TWBTC", "TETH". Used as the canonical key.
    pub symbol: String,
    /// External market symbol, e.g. "BTC", "ETH".
    pub display_symbol: Option<String>,
    /// Stellar contract address for the token.
    pub stellar_address: String,
    /// Price sources the oracle should query (e.g. `["binance", "coinbase"]`).
    pub sources: Vec<String>,
    /// Optional Binance-specific symbol override (e.g. "BTCUSDT").
    pub binance_symbol: Option<String>,
    /// Optional Coinbase-specific base currency override (e.g. "BTC").
    pub coinbase_symbol: Option<String>,
    /// Optional Pyth feed ID.
    pub pyth_feed_id: Option<String>,
    /// Fixed price in 1e30 precision, encoded as a decimal integer string.
    pub fixed_price: Option<String>,
    /// Minimum source count required after source fetches and outlier filtering.
    pub min_sources: usize,
    /// Maximum allowed source deviation from the median in basis points.
    /// Used for both outlier rejection during price aggregation and to
    /// widen the returned price band (min/max) around the percentile-based
    /// or median-based center.
    pub max_deviation_bps: u32,
    /// Source freshness limit.
    pub stale_after_seconds: u64,
    /// Minimum movement before on-chain submission, in basis points.
    pub submit_threshold_bps: u32,
    /// Maximum allowed Pyth confidence interval width in basis points.
    pub pyth_max_confidence_bps: u32,
}

impl Default for TokenConfig {
    fn default() -> Self {
        Self {
            symbol: String::new(),
            display_symbol: None,
            stellar_address: String::new(),
            sources: vec![],
            binance_symbol: None,
            coinbase_symbol: None,
            pyth_feed_id: None,
            fixed_price: None,
            min_sources: 2,
            max_deviation_bps: 100,
            stale_after_seconds: 60,
            submit_threshold_bps: 10,
            pyth_max_confidence_bps: 50,
        }
    }
}

impl TokenConfig {
    /// Canonical token address for lookups.  Returns `stellar_address` if set,
    /// otherwise falls back to the lowercased symbol.
    pub fn lookup_key(&self) -> String {
        if self.stellar_address.is_empty() {
            self.symbol.to_lowercase()
        } else {
            self.stellar_address.to_lowercase()
        }
    }

    /// Returns the configured `display_symbol`, or falls back to `symbol`
    /// if unset or empty.
    pub fn display_symbol(&self) -> &str {
        self.display_symbol
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.symbol)
    }
}

// ── Loading helpers ──────────────────────────────────────────────────────────

/// Error type for configuration loading.
///
/// Derives `Clone`/`Eq` so it can be embedded in `oracle::config::EnvError`,
/// which is itself `Clone + PartialEq + Eq`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
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

    let mut symbols_seen = std::collections::HashSet::new();
    let mut stellar_addresses_seen = std::collections::HashSet::new();
    let mut pyth_feed_ids_seen = std::collections::HashSet::new();
    let mut binance_symbols_seen = std::collections::HashSet::new();
    let mut coinbase_symbols_seen = std::collections::HashSet::new();
    for token in &tokens {
        if token.symbol.is_empty() {
            return Err(ConfigError::InvalidToken {
                symbol: "(empty)".to_string(),
                reason: "symbol must not be empty".to_string(),
            });
        }
        let lower_symbol = token.symbol.to_lowercase();
        if !symbols_seen.insert(lower_symbol) {
            return Err(ConfigError::InvalidToken {
                symbol: token.symbol.clone(),
                reason: "duplicate symbol (case-insensitive)".to_string(),
            });
        }
        // Validate stellar_address uniqueness (#872).
        if !token.stellar_address.is_empty() {
            let lower_address = token.stellar_address.to_lowercase();
            if !stellar_addresses_seen.insert(lower_address) {
                return Err(ConfigError::InvalidToken {
                    symbol: token.symbol.clone(),
                    reason: "duplicate stellar_address (case-insensitive)".to_string(),
                });
            }
        }
        // stellar_address and sources are optional for the API server path,
        // but required for the oracle path — the oracle validates separately.

        // Reject duplicate cross-token source identifiers to catch copy-paste
        // errors where two tokens silently read the same price feed (#995).
        if let Some(ref feed_id) = token.pyth_feed_id {
            if !feed_id.is_empty() && !pyth_feed_ids_seen.insert(feed_id.clone()) {
                return Err(ConfigError::InvalidToken {
                    symbol: token.symbol.clone(),
                    reason: format!("duplicate pyth_feed_id '{feed_id}' across tokens"),
                });
            }
        }
        if let Some(ref sym) = token.binance_symbol {
            if !sym.is_empty() && !binance_symbols_seen.insert(sym.clone()) {
                return Err(ConfigError::InvalidToken {
                    symbol: token.symbol.clone(),
                    reason: format!("duplicate binance_symbol '{sym}' across tokens"),
                });
            }
        }
        if let Some(ref sym) = token.coinbase_symbol {
            if !sym.is_empty() && !coinbase_symbols_seen.insert(sym.clone()) {
                return Err(ConfigError::InvalidToken {
                    symbol: token.symbol.clone(),
                    reason: format!("duplicate coinbase_symbol '{sym}' across tokens"),
                });
            }
        }

        // Reject duplicate source entries so one config source cannot be
        // double-counted in price aggregation (#755).
        {
            let unique_sources: std::collections::HashSet<_> = token.sources.iter().collect();
            if unique_sources.len() != token.sources.len() {
                return Err(ConfigError::InvalidToken {
                    symbol: token.symbol.clone(),
                    reason: "sources list contains duplicate entries".to_string(),
                });
            }
        }

        for source in &token.sources {
            match source.as_str() {
                "binance" => {
                    if let Some(ref sym) = token.binance_symbol {
                        if sym.is_empty()
                            || !sym
                                .chars()
                                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                        {
                            return Err(ConfigError::InvalidToken {
                                symbol: token.symbol.clone(),
                                reason: format!("invalid binance_symbol '{sym}': must contain only alphanumeric characters, dashes, or underscores"),
                            });
                        }
                    }
                }
                "coinbase" => {
                    if let Some(ref sym) = token.coinbase_symbol {
                        if sym.is_empty()
                            || !sym
                                .chars()
                                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                        {
                            return Err(ConfigError::InvalidToken {
                                symbol: token.symbol.clone(),
                                reason: format!("invalid coinbase_symbol '{sym}': must contain only alphanumeric characters, dashes, or underscores"),
                            });
                        }
                    }
                }
                "pyth" | "fixed" => {}
                other => {
                    return Err(ConfigError::InvalidToken {
                        symbol: token.symbol.clone(),
                        reason: format!("unsupported source '{other}'"),
                    });
                }
            }
        }

        // #504 — range validation so misconfigured tuning fields fail loudly at
        // startup rather than silently running with wrong risk thresholds.
        if token.max_deviation_bps == 0 || token.max_deviation_bps > 10_000 {
            return Err(ConfigError::InvalidToken {
                symbol: token.symbol.clone(),
                reason: format!(
                    "max_deviation_bps ({}) must be between 1 and 10000",
                    token.max_deviation_bps
                ),
            });
        }
        if token.stale_after_seconds == 0 {
            return Err(ConfigError::InvalidToken {
                symbol: token.symbol.clone(),
                reason: "stale_after_seconds must be greater than 0".to_string(),
            });
        }
        if token.submit_threshold_bps > 10_000 {
            return Err(ConfigError::InvalidToken {
                symbol: token.symbol.clone(),
                reason: format!(
                    "submit_threshold_bps ({}) must be between 0 and 10000",
                    token.submit_threshold_bps
                ),
            });
        }
        if token.min_sources == 0 {
            return Err(ConfigError::InvalidToken {
                symbol: token.symbol.clone(),
                reason: "min_sources must be at least 1".to_string(),
            });
        }
        if token.pyth_max_confidence_bps > 10_000 {
            return Err(ConfigError::InvalidToken {
                symbol: token.symbol.clone(),
                reason: format!(
                    "pyth_max_confidence_bps ({}) must be between 0 and 10000",
                    token.pyth_max_confidence_bps
                ),
            });
        }
    }

    Ok(tokens)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_JSON: &str = r#"[
        {"symbol":"BTC","stellar_address":"CBTCADDR","sources":["binance","coinbase"],"min_sources":1,"max_deviation_bps":100},
        {"symbol":"ETH","stellar_address":"CETHADDR","sources":["binance"],"min_sources":1,"max_deviation_bps":100}
    ]"#;

    #[test]
    fn parse_valid_config() {
        let tokens = parse_token_configs(VALID_JSON).unwrap();
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[0].symbol, "BTC");
        assert_eq!(tokens[0].sources, vec!["binance", "coinbase"]);
        assert_eq!(tokens[0].min_sources, 1);
        assert_eq!(tokens[0].max_deviation_bps, 100);
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
    fn reject_case_colliding_symbols() {
        let json = r#"[
            {"symbol":"BTC","stellar_address":"CBTCADDR","sources":["binance"]},
            {"symbol":"btc","stellar_address":"CETHADDR","sources":["binance"]}
        ]"#;
        let err = parse_token_configs(json).unwrap_err();
        match err {
            ConfigError::InvalidToken { symbol, reason } => {
                assert_eq!(symbol, "btc");
                assert_eq!(reason, "duplicate symbol (case-insensitive)");
            }
            _ => panic!("expected ConfigError::InvalidToken"),
        }
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
    fn display_symbol_falls_back_to_symbol_when_empty() {
        let json = r#"[{"symbol":"BTC","display_symbol":"","sources":["binance"]}]"#;
        let tokens = parse_token_configs(json).unwrap();
        assert_eq!(tokens[0].display_symbol(), "BTC");
    }

    #[test]
    fn display_symbol_returns_configured_value() {
        let json = r#"[{"symbol":"BTC","display_symbol":"XBTC","sources":["binance"]}]"#;
        let tokens = parse_token_configs(json).unwrap();
        assert_eq!(tokens[0].display_symbol(), "XBTC");
    }

    // #504 — deny_unknown_fields: typo'd keys must be rejected, not silently ignored.
    #[test]
    fn reject_unknown_field_typo() {
        let json = r#"[{"symbol":"BTC","sources":["binance"],"max_deviaton_bps":50}]"#;
        let err = parse_token_configs(json).unwrap_err();
        assert!(
            matches!(err, ConfigError::MalformedJson(_)),
            "expected MalformedJson for unknown field, got: {err:?}"
        );
    }

    // #504 — range validation: zero max_deviation_bps must fail.
    #[test]
    fn reject_zero_max_deviation_bps() {
        let json = r#"[{"symbol":"BTC","sources":["binance"],"max_deviation_bps":0}]"#;
        let err = parse_token_configs(json).unwrap_err();
        assert!(matches!(err, ConfigError::InvalidToken { .. }), "{err:?}");
    }

    /// The removed `min`/`max` price bounds must now be rejected rather than
    /// silently ignored, so a config still carrying them fails loudly at parse
    /// time instead of appearing to take effect.
    #[test]
    fn reject_removed_min_max_price_bounds() {
        for field in ["min", "max"] {
            let json = format!(r#"[{{"symbol":"BTC","sources":["binance"],"{field}":44000.0}}]"#);
            let err = parse_token_configs(&json)
                .expect_err(&format!("removed field '{field}' must be rejected"));
            assert!(
                matches!(err, ConfigError::MalformedJson(_)),
                "expected MalformedJson for removed field '{field}', got: {err:?}"
            );
            assert!(
                err.to_string().contains(field),
                "error should name the offending field '{field}': {err}"
            );
        }
    }

    // #504 — range validation: max_deviation_bps above 10000 must fail.
    #[test]
    fn reject_max_deviation_bps_above_10000() {
        let json = r#"[{"symbol":"BTC","sources":["binance"],"max_deviation_bps":10001}]"#;
        let err = parse_token_configs(json).unwrap_err();
        assert!(matches!(err, ConfigError::InvalidToken { .. }), "{err:?}");
    }

    // #504 — range validation: stale_after_seconds = 0 must fail.
    #[test]
    fn reject_zero_stale_after_seconds() {
        let json = r#"[{"symbol":"BTC","sources":["binance"],"stale_after_seconds":0}]"#;
        let err = parse_token_configs(json).unwrap_err();
        assert!(matches!(err, ConfigError::InvalidToken { .. }), "{err:?}");
    }

    // #504 — range validation: submit_threshold_bps above 10000 must fail.
    #[test]
    fn reject_submit_threshold_bps_above_10000() {
        let json = r#"[{"symbol":"BTC","sources":["binance"],"submit_threshold_bps":10001}]"#;
        let err = parse_token_configs(json).unwrap_err();
        assert!(matches!(err, ConfigError::InvalidToken { .. }), "{err:?}");
    }

    // #504 — range validation: min_sources = 0 must fail.
    #[test]
    fn reject_zero_min_sources() {
        let json = r#"[{"symbol":"BTC","sources":["binance"],"min_sources":0}]"#;
        let err = parse_token_configs(json).unwrap_err();
        assert!(matches!(err, ConfigError::InvalidToken { .. }), "{err:?}");
    }

    // #995 — cross-token duplicate pyth_feed_id must be rejected.
    #[test]
    fn reject_duplicate_pyth_feed_id() {
        let json = r#"[
            {"symbol":"BTC","stellar_address":"CBTC","sources":["pyth"],"pyth_feed_id":"abc123"},
            {"symbol":"ETH","stellar_address":"CETH","sources":["pyth"],"pyth_feed_id":"abc123"}
        ]"#;
        let err = parse_token_configs(json).unwrap_err();
        match err {
            ConfigError::InvalidToken { symbol, reason } => {
                assert_eq!(symbol, "ETH");
                assert!(reason.contains("duplicate pyth_feed_id"));
            }
            _ => panic!("expected ConfigError::InvalidToken for duplicate pyth_feed_id"),
        }
    }

    // #995 — cross-token duplicate binance_symbol must be rejected.
    #[test]
    fn reject_duplicate_binance_symbol() {
        let json = r#"[
            {"symbol":"BTC","stellar_address":"CBTC","sources":["binance"],"binance_symbol":"BTCUSDT"},
            {"symbol":"WBTC","stellar_address":"CWBTC","sources":["binance"],"binance_symbol":"BTCUSDT"}
        ]"#;
        let err = parse_token_configs(json).unwrap_err();
        match err {
            ConfigError::InvalidToken { symbol, reason } => {
                assert_eq!(symbol, "WBTC");
                assert!(reason.contains("duplicate binance_symbol"));
            }
            _ => panic!("expected ConfigError::InvalidToken for duplicate binance_symbol"),
        }
    }

    // #995 — cross-token duplicate coinbase_symbol must be rejected.
    #[test]
    fn reject_duplicate_coinbase_symbol() {
        let json = r#"[
            {"symbol":"BTC","stellar_address":"CBTC","sources":["coinbase"],"coinbase_symbol":"BTC"},
            {"symbol":"TBTC","stellar_address":"CTBTC","sources":["coinbase"],"coinbase_symbol":"BTC"}
        ]"#;
        let err = parse_token_configs(json).unwrap_err();
        match err {
            ConfigError::InvalidToken { symbol, reason } => {
                assert_eq!(symbol, "TBTC");
                assert!(reason.contains("duplicate coinbase_symbol"));
            }
            _ => panic!("expected ConfigError::InvalidToken for duplicate coinbase_symbol"),
        }
    }

    // #995 — different source identifiers across different source types are OK.
    #[test]
    fn allow_different_source_identifiers() {
        let json = r#"[
            {"symbol":"BTC","stellar_address":"CBTC","sources":["binance","pyth"],"binance_symbol":"BTCUSDT","pyth_feed_id":"feed1"},
            {"symbol":"ETH","stellar_address":"CETH","sources":["coinbase"],"coinbase_symbol":"ETH"}
        ]"#;
        assert!(parse_token_configs(json).is_ok());
    }
}

// #872 — duplicate stellar_address (case-insensitive) must be rejected.
#[test]
fn reject_duplicate_stellar_address() {
    let json = r#"[
            {"symbol":"TWBTC","stellar_address":"CBTCADDR","sources":["binance"]},
            {"symbol":"TWETH","stellar_address":"cbtcaddr","sources":["coinbase"]}
        ]"#;
    let err = parse_token_configs(json).unwrap_err();
    match err {
        ConfigError::InvalidToken { symbol, reason } => {
            assert_eq!(symbol, "TWETH");
            assert_eq!(reason, "duplicate stellar_address (case-insensitive)");
        }
        _ => panic!("expected ConfigError::InvalidToken for duplicate stellar_address"),
    }
}

// #872 — empty stellar_address values should not trigger duplicate check.
#[test]
fn allow_multiple_empty_stellar_addresses() {
    let json = r#"[
            {"symbol":"BTC","stellar_address":"","sources":["binance"]},
            {"symbol":"ETH","stellar_address":"","sources":["coinbase"]}
        ]"#;
    assert!(parse_token_configs(json).is_ok());
}
