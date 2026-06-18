//! Application configuration — loaded from environment variables at boot.

use std::fmt;
use crate::StellarNetwork;

/// Top-level configuration for the so4-oracle service.
///
/// All values are loaded from environment variables and validated at boot.
/// See `§7` of `plan.md` for the full variable reference.
#[derive(Debug, Clone)]
pub struct Config {
    // ── Network ──────────────────────────────────────────────────────────────
    pub network: StellarNetwork,
    pub rpc_url: String,
    pub horizon_url: String,
    pub passphrase: String,

    // ── Contract IDs ─────────────────────────────────────────────────────────
    pub oracle_contract_id: String,
    pub order_handler: String,
    pub deposit_handler: String,
    pub withdrawal_handler: String,
    pub reader: String,
    pub data_store: String,
    pub role_store: String,

    // ── Loops & thresholds ───────────────────────────────────────────────────
    pub price_loop_ms: u64,
    pub keeper_loop_ms: u64,
    pub min_keeper_balance_xlm: f64,
    pub keeper_index: u32,
    pub bind_addr: String,

    // ── Secrets ──────────────────────────────────────────────────────────────
    /// ed25519 hex key for signing prices.
    pub keeper_private_key: String,
    /// S... seed for transaction signing (distinct from price key per GMX #6).
    pub keeper_secret_key: String,
    /// G... public account ID of the keeper.
    pub keeper_account_id: String,
    /// Bearer token for admin API endpoints.
    pub admin_api_token: String,

    // ── Price feed ───────────────────────────────────────────────────────────
    /// JSON string of token configs (optional; falls back to `config/tokens.json`).
    pub price_feed_config: Option<String>,
}

// ── Default network constants ────────────────────────────────────────────────

impl Default for Config {
    fn default() -> Self {
        Self {
            network: StellarNetwork::Testnet,
            rpc_url: "https://soroban-testnet.stellar.org".into(),
            horizon_url: "https://horizon-testnet.stellar.org".into(),
            passphrase: "Test SDF Network ; September 2015".into(),
            oracle_contract_id: Default::default(),
            order_handler: Default::default(),
            deposit_handler: Default::default(),
            withdrawal_handler: Default::default(),
            reader: Default::default(),
            data_store: Default::default(),
            role_store: Default::default(),
            price_loop_ms: 1000,
            keeper_loop_ms: 1000,
            min_keeper_balance_xlm: 5.0,
            keeper_index: 0,
            bind_addr: "0.0.0.0:3000".into(),
            keeper_private_key: Default::default(),
            keeper_secret_key: Default::default(),
            keeper_account_id: Default::default(),
            admin_api_token: Default::default(),
            price_feed_config: None,
        }
    }
}

impl Config {
    /// Load and validate `Config` from environment variables.
    ///
    /// Required vars:
    /// - `KEEPER_PRIVATE_KEY`, `KEEPER_SECRET_KEY`, `KEEPER_ACCOUNT_ID`, `ADMIN_API_TOKEN`
    /// - `ORACLE_CONTRACT_ID`, `ORDER_HANDLER`, `DEPOSIT_HANDLER`, `WITHDRAWAL_HANDLER`
    /// - `READER`, `DATA_STORE`
    ///
    /// Optional with defaults:
    /// - `STELLAR_NETWORK` — `"testnet"` or `"mainnet"` (default: `"testnet"`)
    /// - `STELLAR_RPC_URL`, `HORIZON_URL`, `NETWORK_PASSPHRASE` (defaults based on network)
    /// - `PRICE_LOOP_MS` (default: `1000`), `KEEPER_LOOP_MS` (default: `1000`), `MIN_KEEPER_BALANCE_XLM` (default: `5.0`)
    /// - `KEEPER_INDEX` (default: `0`), `BIND_ADDR` (default: `0.0.0.0:3000`)
    /// - `PRICE_FEED_CONFIG` (optional JSON string)
    pub fn from_env() -> Result<Self, ConfigError> {
        let mut cfg = Config::default();

        // ── Network selection ────────────────────────────────────────────────
        let network_str = std::env::var("STELLAR_NETWORK")
            .unwrap_or_else(|_| "testnet".to_string())
            .to_lowercase();

        cfg.network = match network_str.as_str() {
            "testnet" => StellarNetwork::Testnet,
            "mainnet" => StellarNetwork::Mainnet,
            other => return Err(ConfigError::InvalidValue {
                var: "STELLAR_NETWORK".into(),
                msg: format!("expected 'testnet' or 'mainnet', got '{other}'"),
            }),
        };

        // ── RPC / Horizon / passphrase (defaults per network) ────────────────
        if let Ok(v) = std::env::var("STELLAR_RPC_URL") { cfg.rpc_url = v; }
        if let Ok(v) = std::env::var("HORIZON_URL") { cfg.horizon_url = v; }
        if let Ok(v) = std::env::var("NETWORK_PASSPHRASE") { cfg.passphrase = v; }

        // ── Contract IDs ─────────────────────────────────────────────────────
        cfg.oracle_contract_id = env_required("ORACLE_CONTRACT_ID")?;
        cfg.order_handler = env_required("ORDER_HANDLER")?;
        cfg.deposit_handler = env_required("DEPOSIT_HANDLER")?;
        cfg.withdrawal_handler = env_required("WITHDRAWAL_HANDLER")?;
        cfg.reader = env_required("READER")?;
        cfg.data_store = env_required("DATA_STORE")?;

        // role_store is optional (but warn if missing)
        if let Ok(v) = std::env::var("ROLE_STORE") {
            cfg.role_store = v;
        }

        // ── Loop intervals & thresholds ──────────────────────────────────────
        if let Ok(v) = std::env::var("PRICE_LOOP_MS") {
            cfg.price_loop_ms = v.parse().map_err(|_| ConfigError::InvalidValue {
                var: "PRICE_LOOP_MS".into(),
                msg: format!("expected u64, got '{v}'"),
            })?;
        }
        if let Ok(v) = std::env::var("KEEPER_LOOP_MS") {
            cfg.keeper_loop_ms = v.parse().map_err(|_| ConfigError::InvalidValue {
                var: "KEEPER_LOOP_MS".into(),
                msg: format!("expected u64, got '{v}'"),
            })?;
        }
        if let Ok(v) = std::env::var("MIN_KEEPER_BALANCE_XLM") {
            cfg.min_keeper_balance_xlm = v.parse().map_err(|_| ConfigError::InvalidValue {
                var: "MIN_KEEPER_BALANCE_XLM".into(),
                msg: format!("expected f64, got '{v}'"),
            })?;
        }
        if let Ok(v) = std::env::var("KEEPER_INDEX") {
            cfg.keeper_index = v.parse().map_err(|_| ConfigError::InvalidValue {
                var: "KEEPER_INDEX".into(),
                msg: format!("expected u32, got '{v}'"),
            })?;
        }
        if let Ok(v) = std::env::var("BIND_ADDR") {
            cfg.bind_addr = v;
        }

        // ── Secrets ──────────────────────────────────────────────────────────
        cfg.keeper_private_key = env_required("KEEPER_PRIVATE_KEY")?;
        cfg.keeper_secret_key = env_required("KEEPER_SECRET_KEY")?;
        cfg.keeper_account_id = env_required("KEEPER_ACCOUNT_ID")?;
        cfg.admin_api_token = env_required("ADMIN_API_TOKEN")?;

        // ── Price feed config (optional) ─────────────────────────────────────
        cfg.price_feed_config = std::env::var("PRICE_FEED_CONFIG").ok();
        if cfg.price_feed_config.as_deref() == Some("") {
            cfg.price_feed_config = None;
        }

        Ok(cfg)
    }
}

// ── Error type ───────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum ConfigError {
    MissingVar { var: String },
    InvalidValue { var: String, msg: String },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::MissingVar { var } => {
                write!(f, "required env var '{var}' is not set")
            }
            ConfigError::InvalidValue { var, msg } => {
                write!(f, "env var '{var}' is invalid: {msg}")
            }
        }
    }
}

impl std::error::Error for ConfigError {}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn env_required(var: &str) -> Result<String, ConfigError> {
    std::env::var(var).map_err(|_| ConfigError::MissingVar { var: var.into() })
}
