use once_cell::sync::Lazy;
use shared_config::TokenConfig;
use std::collections::HashMap;

/// Path to the workspace-root token config file (Issue #165).
const TOKENS_FILE: &str = "../config/tokens.json";
/// Fallback path for backward compatibility.
const TOKENS_FILE_FALLBACK: &str = "config/tokens.json";

static TOKENS: Lazy<HashMap<String, TokenConfig>> = Lazy::new(|| {
    // Try workspace root first, then the local config/ dir.
    let raw = std::fs::read_to_string(TOKENS_FILE)
        .or_else(|_| std::fs::read_to_string(TOKENS_FILE_FALLBACK))
        .unwrap_or_else(|_| "[]".to_string());
    let v: Vec<TokenConfig> = serde_json::from_str(&raw).unwrap_or_default();
    let mut m = HashMap::new();
    for e in v {
        // Key by lookup_key() (stellar_address if set, otherwise symbol) and
        // also by symbol so both address-based and symbol-based lookups work.
        let addr_key = e.lookup_key();
        let sym_key = e.symbol.to_lowercase();
        m.insert(addr_key, e.clone());
        if sym_key != e.lookup_key() {
            m.insert(sym_key, e);
        }
    }
    m
});

pub fn lookup_token(addr: &str) -> Option<TokenConfig> {
    TOKENS.get(&addr.to_lowercase()).cloned()
}

/// Return all configured token entries (used by the history background task).
pub fn all_tokens() -> Option<Vec<TokenConfig>> {
    let tokens: Vec<TokenConfig> = TOKENS.values().cloned().collect();
    if tokens.is_empty() {
        None
    } else {
        Some(tokens)
    }
}
