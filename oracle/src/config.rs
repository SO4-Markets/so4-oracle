use std::fmt;
use std::net::SocketAddr;
use std::time::Duration;

use shared_config::{ConfigError, TokenConfig};

use crate::keeper::DEFAULT_MIN_KEEPER_BALANCE_XLM;
use crate::network_config::{MAINNET_PASSPHRASE, TESTNET_PASSPHRASE, TESTNET_RPC_URL};

pub const ENV_KEY: &str = "PRICE_FEED_CONFIG";
pub const DEFAULT_BIND_ADDR: &str = "0.0.0.0:8080";
pub const DEFAULT_TESTNET_HORIZON_URL: &str = "https://horizon-testnet.stellar.org";
pub const DEFAULT_MAINNET_HORIZON_URL: &str = "https://horizon.stellar.org";
pub const DEFAULT_PRICE_LOOP_MS: u64 = 1_000;
pub const DEFAULT_KEEPER_LOOP_MS: u64 = 1_500;

/// Oracle-specific view of a token feed config.
/// Re-exports fields from `TokenConfig` for backward compatibility with
/// the rest of the oracle crate.
pub type TokenFeedConfig = TokenConfig;

#[derive(Debug, Clone)]
pub struct PriceFeedConfig {
    pub tokens: Vec<TokenFeedConfig>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Network {
    Testnet,
    Mainnet,
}

impl Network {
    pub fn as_str(&self) -> &'static str {
        match self {
            Network::Testnet => "testnet",
            Network::Mainnet => "mainnet",
        }
    }
}

#[derive(Clone)]
pub struct SecretString(String);

impl SecretString {
    pub fn new(value: String) -> Self {
        Self(value)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub bind_addr: SocketAddr,
    pub network: Network,
    pub network_passphrase: String,
    pub stellar_rpc_url: String,
    pub horizon_url: String,
    pub oracle_contract_id: String,
    pub role_store_contract_id: String,
    pub data_store_contract_id: String,
    pub order_handler_contract_id: String,
    pub deposit_handler_contract_id: String,
    pub withdrawal_handler_contract_id: String,
    pub reader_contract_id: String,
    pub keeper_private_key: SecretString,
    pub keeper_secret_key: SecretString,
    pub keeper_account_id: String,
    pub keeper_index: u32,
    pub admin_api_token: Option<SecretString>,
    pub min_keeper_balance_xlm: f64,
    pub price_loop_interval: Duration,
    pub keeper_loop_interval: Duration,
    pub price_feed: PriceFeedConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvError {
    MissingVar(&'static str),
    InvalidVar { var: &'static str, reason: String },
    TokenConfig(String),
}

impl fmt::Display for EnvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EnvError::MissingVar(var) => write!(f, "required env var '{var}' is not set"),
            EnvError::InvalidVar { var, reason } => write!(f, "invalid env var '{var}': {reason}"),
            EnvError::TokenConfig(reason) => write!(f, "invalid PRICE_FEED_CONFIG: {reason}"),
        }
    }
}

impl std::error::Error for EnvError {}

impl From<ConfigError> for EnvError {
    fn from(value: ConfigError) -> Self {
        EnvError::TokenConfig(value.to_string())
    }
}

impl Config {
    pub fn from_env() -> Result<Self, EnvError> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    fn from_lookup(mut lookup: impl FnMut(&str) -> Option<String>) -> Result<Self, EnvError> {
        let network_raw = lookup("STELLAR_NETWORK").unwrap_or_else(|| "testnet".to_string());
        let network = match network_raw.as_str() {
            "testnet" => Network::Testnet,
            "mainnet" => Network::Mainnet,
            other => {
                return Err(EnvError::InvalidVar {
                    var: "STELLAR_NETWORK",
                    reason: format!("unknown network '{other}'; expected 'testnet' or 'mainnet'"),
                })
            }
        };

        let bind_addr = parse_or_default(&mut lookup, "BIND_ADDR", DEFAULT_BIND_ADDR)?;
        let (network_passphrase, stellar_rpc_url, horizon_url) = match network {
            Network::Testnet => (
                TESTNET_PASSPHRASE.to_string(),
                lookup("STELLAR_RPC_URL").unwrap_or_else(|| TESTNET_RPC_URL.to_string()),
                lookup("HORIZON_URL").unwrap_or_else(|| DEFAULT_TESTNET_HORIZON_URL.to_string()),
            ),
            Network::Mainnet => (
                MAINNET_PASSPHRASE.to_string(),
                required(&mut lookup, "STELLAR_RPC_URL")?,
                lookup("HORIZON_URL").unwrap_or_else(|| DEFAULT_MAINNET_HORIZON_URL.to_string()),
            ),
        };

        let oracle_contract_id = match network {
            Network::Mainnet => required(&mut lookup, "ORACLE_CONTRACT_ID")?,
            Network::Testnet => required_any(&mut lookup, "ORACLE_CONTRACT_ID", "ORACLE")?,
        };

        let price_feed = load_price_feed_config(lookup("PRICE_FEED_CONFIG").as_deref())?;

        Ok(Self {
            bind_addr,
            network,
            network_passphrase,
            stellar_rpc_url,
            horizon_url,
            oracle_contract_id,
            role_store_contract_id: required(&mut lookup, "ROLE_STORE")?,
            data_store_contract_id: required(&mut lookup, "DATA_STORE")?,
            order_handler_contract_id: required(&mut lookup, "ORDER_HANDLER")?,
            deposit_handler_contract_id: required(&mut lookup, "DEPOSIT_HANDLER")?,
            withdrawal_handler_contract_id: required(&mut lookup, "WITHDRAWAL_HANDLER")?,
            reader_contract_id: required(&mut lookup, "READER")?,
            keeper_private_key: SecretString::new(validate_hex_key(
                "KEEPER_PRIVATE_KEY",
                required(&mut lookup, "KEEPER_PRIVATE_KEY")?,
                32,
            )?),
            keeper_secret_key: SecretString::new(validate_strkey(
                "KEEPER_SECRET_KEY",
                required(&mut lookup, "KEEPER_SECRET_KEY")?,
                'S',
            )?),
            keeper_account_id: validate_strkey(
                "KEEPER_ACCOUNT_ID",
                required(&mut lookup, "KEEPER_ACCOUNT_ID")?,
                'G',
            )?,
            keeper_index: parse_or_default(&mut lookup, "KEEPER_INDEX", "0")?,
            // Optional: when unset, admin-only endpoints reject with 503 rather
            // than refusing to boot. Keeps the foundation runnable without secrets.
            admin_api_token: lookup("ADMIN_API_TOKEN")
                .filter(|value| !value.trim().is_empty())
                .map(SecretString::new),
            min_keeper_balance_xlm: parse_or_default(
                &mut lookup,
                "MIN_KEEPER_BALANCE_XLM",
                &DEFAULT_MIN_KEEPER_BALANCE_XLM.to_string(),
            )?,
            price_loop_interval: Duration::from_millis(parse_or_default(
                &mut lookup,
                "PRICE_LOOP_MS",
                &DEFAULT_PRICE_LOOP_MS.to_string(),
            )?),
            keeper_loop_interval: Duration::from_millis(parse_or_default(
                &mut lookup,
                "KEEPER_LOOP_MS",
                &DEFAULT_KEEPER_LOOP_MS.to_string(),
            )?),
            price_feed,
        })
    }
}

fn required(
    lookup: &mut impl FnMut(&str) -> Option<String>,
    var: &'static str,
) -> Result<String, EnvError> {
    lookup(var)
        .filter(|value| !value.trim().is_empty())
        .ok_or(EnvError::MissingVar(var))
}

fn required_any(
    lookup: &mut impl FnMut(&str) -> Option<String>,
    primary: &'static str,
    fallback: &'static str,
) -> Result<String, EnvError> {
    lookup(primary)
        .filter(|value| !value.trim().is_empty())
        .or_else(|| lookup(fallback).filter(|value| !value.trim().is_empty()))
        .ok_or(EnvError::MissingVar(primary))
}

fn parse_or_default<T>(
    lookup: &mut impl FnMut(&str) -> Option<String>,
    var: &'static str,
    default: &str,
) -> Result<T, EnvError>
where
    T: std::str::FromStr,
    T::Err: fmt::Display,
{
    let raw = lookup(var).unwrap_or_else(|| default.to_string());
    raw.parse::<T>().map_err(|err| EnvError::InvalidVar {
        var,
        reason: err.to_string(),
    })
}

fn validate_hex_key(
    var: &'static str,
    value: String,
    expected_len: usize,
) -> Result<String, EnvError> {
    let bytes = hex::decode(&value).map_err(|err| EnvError::InvalidVar {
        var,
        reason: err.to_string(),
    })?;
    if bytes.len() != expected_len {
        return Err(EnvError::InvalidVar {
            var,
            reason: format!("expected {expected_len} bytes, got {}", bytes.len()),
        });
    }
    Ok(value)
}

/// Validate a Stellar strkey (account `G…` / secret seed `S…`) for shape only:
/// 56-char base32 with the expected version prefix. This catches typos and
/// swapped vars at boot; it does not verify the CRC16 or that a secret derives
/// the configured account (those are wired with the keeper in #3).
fn validate_strkey(var: &'static str, value: String, prefix: char) -> Result<String, EnvError> {
    let invalid = |reason: String| EnvError::InvalidVar { var, reason };
    if value.len() != 56 {
        return Err(invalid(format!(
            "expected 56 characters, got {}",
            value.len()
        )));
    }
    if !value.starts_with(prefix) {
        return Err(invalid(format!("must start with '{prefix}'")));
    }
    if let Some(bad) = value.chars().find(|c| !matches!(c, 'A'..='Z' | '2'..='7')) {
        return Err(invalid(format!(
            "invalid base32 character '{bad}' (expected A-Z, 2-7)"
        )));
    }
    Ok(value)
}

fn load_price_feed_config(raw: Option<&str>) -> Result<PriceFeedConfig, ConfigError> {
    let raw = raw.unwrap_or(include_str!("../../config/tokens.json"));
    parse_price_feed_config(raw)
}

/// Parse and validate the `PRICE_FEED_CONFIG` JSON string.
///
/// Expected format:
/// ```json
/// [{"symbol":"BTC","stellar_address":"C...","sources":["binance","coinbase"]}]
/// ```
pub fn parse_price_feed_config(raw: &str) -> Result<PriceFeedConfig, ConfigError> {
    let tokens = shared_config::parse_token_configs(raw)?;

    // Oracle-specific validation: stellar_address and sources are required.
    for token in &tokens {
        if token.stellar_address.is_empty() {
            return Err(ConfigError::InvalidToken {
                symbol: token.symbol.clone(),
                reason: "stellar_address must not be empty".to_string(),
            });
        }
        if token.sources.is_empty() {
            return Err(ConfigError::InvalidToken {
                symbol: token.symbol.clone(),
                reason: "sources list must not be empty".to_string(),
            });
        }
        for source in &token.sources {
            if source.is_empty() {
                return Err(ConfigError::InvalidToken {
                    symbol: token.symbol.clone(),
                    reason: "source names must not be empty strings".to_string(),
                });
            }
            match source.as_str() {
                "binance" if token.binance_symbol.is_none() => {
                    return Err(ConfigError::InvalidToken {
                        symbol: token.symbol.clone(),
                        reason: "binance_symbol is required for binance source".to_string(),
                    });
                }
                "coinbase" if token.coinbase_symbol.is_none() => {
                    return Err(ConfigError::InvalidToken {
                        symbol: token.symbol.clone(),
                        reason: "coinbase_symbol is required for coinbase source".to_string(),
                    });
                }
                "pyth" if token.pyth_feed_id.is_none() => {
                    return Err(ConfigError::InvalidToken {
                        symbol: token.symbol.clone(),
                        reason: "pyth_feed_id is required for pyth source".to_string(),
                    });
                }
                "fixed" if token.fixed_price.is_none() => {
                    return Err(ConfigError::InvalidToken {
                        symbol: token.symbol.clone(),
                        reason: "fixed_price is required for fixed source".to_string(),
                    });
                }
                "binance" | "coinbase" | "pyth" | "fixed" => {}
                other => {
                    return Err(ConfigError::InvalidToken {
                        symbol: token.symbol.clone(),
                        reason: format!("unsupported source '{other}'"),
                    });
                }
            }
        }
        if token.min_sources == 0 {
            return Err(ConfigError::InvalidToken {
                symbol: token.symbol.clone(),
                reason: "min_sources must be greater than zero".to_string(),
            });
        }
    }

    Ok(PriceFeedConfig { tokens })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    const VALID_JSON: &str = r#"[
        {"symbol":"BTC","stellar_address":"CBTCADDR","sources":["binance","coinbase"],"binance_symbol":"BTCUSDT","coinbase_symbol":"BTC"},
        {"symbol":"ETH","stellar_address":"CETHADDR","sources":["binance"],"binance_symbol":"ETHUSDT"}
    ]"#;

    #[test]
    fn parse_or_default_uses_value_when_set() {
        let mut env = HashMap::new();
        env.insert("TEST_VAR".to_string(), "42".to_string());
        let result =
            parse_or_default::<u64>(&mut |key| env.get(key).cloned(), "TEST_VAR", "10").unwrap();
        assert_eq!(result, 42);
    }

    #[test]
    fn parse_or_default_uses_default_when_absent() {
        let env: HashMap<String, String> = HashMap::new();
        let result =
            parse_or_default::<u64>(&mut |key| env.get(key).cloned(), "TEST_VAR", "10").unwrap();
        assert_eq!(result, 10);
    }

    #[test]
    fn parse_or_default_rejects_invalid_value() {
        let mut env = HashMap::new();
        env.insert("TEST_VAR".to_string(), "not_a_number".to_string());
        let err = parse_or_default::<u64>(&mut |key| env.get(key).cloned(), "TEST_VAR", "10")
            .unwrap_err();
        assert!(matches!(
            err,
            EnvError::InvalidVar {
                var: "TEST_VAR",
                ..
            }
        ));
    }

    #[test]
    fn parse_or_default_parses_socket_addr() {
        let mut env = HashMap::new();
        env.insert("BIND_ADDR".to_string(), "127.0.0.1:3000".to_string());
        let result = parse_or_default::<std::net::SocketAddr>(
            &mut |key| env.get(key).cloned(),
            "BIND_ADDR",
            "0.0.0.0:8080",
        )
        .unwrap();
        assert_eq!(result.port(), 3000);
    }

    #[test]
    fn parse_valid_config() {
        let cfg = parse_price_feed_config(VALID_JSON).unwrap();
        assert_eq!(cfg.tokens.len(), 2);
        assert_eq!(cfg.tokens[0].symbol, "BTC");
        assert_eq!(cfg.tokens[0].sources, vec!["binance", "coinbase"]);
        assert_eq!(cfg.tokens[1].symbol, "ETH");
        assert_eq!(cfg.tokens[1].sources, vec!["binance"]);
    }

    #[test]
    fn reject_malformed_json() {
        let err = parse_price_feed_config("{not json}").unwrap_err();
        assert!(matches!(err, ConfigError::MalformedJson(_)));
    }

    #[test]
    fn reject_empty_token_list() {
        let err = parse_price_feed_config("[]").unwrap_err();
        assert!(matches!(err, ConfigError::EmptyTokenList));
    }

    #[test]
    fn reject_token_with_empty_symbol() {
        let json = r#"[{"symbol":"","stellar_address":"CADDR","sources":["binance"],"binance_symbol":"BTCUSDT"}]"#;
        let err = parse_price_feed_config(json).unwrap_err();
        assert!(matches!(err, ConfigError::InvalidToken { .. }));
    }

    #[test]
    fn reject_token_with_empty_stellar_address() {
        let json = r#"[{"symbol":"BTC","stellar_address":"","sources":["binance"],"binance_symbol":"BTCUSDT"}]"#;
        let err = parse_price_feed_config(json).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::InvalidToken { ref symbol, .. } if symbol == "BTC"
        ));
    }

    #[test]
    fn reject_token_with_empty_sources() {
        let json = r#"[{"symbol":"BTC","stellar_address":"CADDR","sources":[]}]"#;
        let err = parse_price_feed_config(json).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::InvalidToken { ref symbol, .. } if symbol == "BTC"
        ));
    }

    #[test]
    fn per_token_source_list_preserved() {
        let json = r#"[
            {"symbol":"BTC","stellar_address":"CBADDR","sources":["binance"],"binance_symbol":"BTCUSDT"},
            {"symbol":"ETH","stellar_address":"CEADDR","sources":["coinbase"],"coinbase_symbol":"ETH"}
        ]"#;
        let cfg = parse_price_feed_config(json).unwrap();
        assert_eq!(cfg.tokens[0].sources, vec!["binance"]);
        assert_eq!(cfg.tokens[1].sources, vec!["coinbase"]);
    }

    #[test]
    fn reject_missing_coinbase_symbol() {
        let json = r#"[{"symbol":"TWBTC","stellar_address":"CADDR","sources":["coinbase"]}]"#;
        let err = parse_price_feed_config(json).unwrap_err();
        assert!(matches!(err, ConfigError::InvalidToken { .. }));
    }

    #[test]
    fn reject_missing_binance_symbol() {
        let json = r#"[{"symbol":"TWBTC","stellar_address":"CADDR","sources":["binance"]}]"#;
        let err = parse_price_feed_config(json).unwrap_err();
        assert!(matches!(err, ConfigError::InvalidToken { .. }));
    }

    #[test]
    fn reject_missing_pyth_feed_id() {
        let json = r#"[{"symbol":"TWBTC","stellar_address":"CADDR","sources":["pyth"]}]"#;
        let err = parse_price_feed_config(json).unwrap_err();
        assert!(matches!(err, ConfigError::InvalidToken { .. }));
    }

    #[test]
    fn reject_missing_fixed_price() {
        let json = r#"[{"symbol":"TUSDC","stellar_address":"CADDR","sources":["fixed"]}]"#;
        let err = parse_price_feed_config(json).unwrap_err();
        assert!(matches!(err, ConfigError::InvalidToken { .. }));
    }

    #[test]
    fn parse_current_testnet_shape() {
        let json = r#"[
            {"symbol":"TUSDC","display_symbol":"USDC","stellar_address":"CBAN5YU3KRDKPTQ2H76D6S7HQFPRBGUD524F65BUM2RQCITPTRLKWKES","sources":["fixed"],"fixed_price":"1000000000000000000000000000000","min_sources":1},
            {"symbol":"TWBTC","display_symbol":"BTC","stellar_address":"CCFTOPHUPSUDO2MB4X5D3XYJ2HRJ7NJPAW4UVPAVN7ZLE63EZLSMXDUO","sources":["binance","coinbase","pyth"],"binance_symbol":"BTCUSDT","coinbase_symbol":"BTC","pyth_feed_id":"e62df6c8b4a85fe1a67db44dc12de5db330f7ac66b72dc658afedf0f4a415b43","min_sources":2}
        ]"#;
        let cfg = parse_price_feed_config(json).unwrap();
        assert_eq!(cfg.tokens[0].display_symbol(), "USDC");
        assert_eq!(cfg.tokens[1].coinbase_symbol.as_deref(), Some("BTC"));
    }

    #[test]
    fn load_price_feed_config_uses_env_when_set() {
        let json = r#"[{"symbol":"BTC","stellar_address":"CADDR","sources":["binance"],"binance_symbol":"BTCUSDT"}]"#;
        let cfg = load_price_feed_config(Some(json)).unwrap();
        assert_eq!(cfg.tokens.len(), 1);
        assert_eq!(cfg.tokens[0].symbol, "BTC");
    }

    #[test]
    fn load_price_feed_config_falls_back_to_file() {
        let cfg = load_price_feed_config(None).unwrap();
        assert!(!cfg.tokens.is_empty());
    }

    fn valid_env() -> HashMap<&'static str, String> {
        HashMap::from([
            ("STELLAR_NETWORK", "testnet".to_string()),
            (
                "ORACLE_CONTRACT_ID",
                "CBEMTV23SIJJBIST3V5HTMWHR4MHYGHNBIG4M26U4LGUJTWZXTFSVQEY".to_string(),
            ),
            (
                "ROLE_STORE",
                "CBSUAIAMIFFS4AXQYZ7KR7FNO7IMKAPS5WF4DXANVXDTPKH2F7YUIN6Q".to_string(),
            ),
            (
                "DATA_STORE",
                "CCZ3VKBEDLNBO2JM3EXL3SNBDJOV5BTN52FVQPER7F6D5GCE53PITQ3J".to_string(),
            ),
            (
                "ORDER_HANDLER",
                "CC35OFZVWUTAZPV3B6UKSDVAVORZEWUUMOMTHO33H4YR4C5FKPEFODKY".to_string(),
            ),
            (
                "DEPOSIT_HANDLER",
                "CDWOFIP4YQJGMCYAOWLSRBAWN2OTJUG2I5WOFC32O2TX2SRU56RWBE5C".to_string(),
            ),
            (
                "WITHDRAWAL_HANDLER",
                "CCA5HRHMG6E6BVYRICSLZ5CK5KNPAAKXQ7XWDM34WWVGNHWHA26GRVVE".to_string(),
            ),
            (
                "READER",
                "CC6OZUHF3LVO6PNP3V2EB36ORB3YSVYSH3LWD3RFLO4NUO3BYCXSWSYC".to_string(),
            ),
            (
                "KEEPER_PRIVATE_KEY",
                "1111111111111111111111111111111111111111111111111111111111111111".to_string(),
            ),
            (
                "KEEPER_SECRET_KEY",
                "SAUHMCMUP5FZO5675W3ISZ6E6CNYJGXBUW5WANE2JR4TGAARYCTSCBKI".to_string(),
            ),
            (
                "KEEPER_ACCOUNT_ID",
                "GAUHMCMUP5FZO5675W3ISZ6E6CNYJGXBUW5WANE2JR4TGAARYCTSCBKI".to_string(),
            ),
            ("ADMIN_API_TOKEN", "test-admin-token".to_string()),
        ])
    }

    #[test]
    fn config_from_lookup_uses_testnet_defaults_and_token_file_fallback() {
        let env = valid_env();
        let cfg = Config::from_lookup(|key| env.get(key).cloned()).unwrap();

        assert_eq!(cfg.network, Network::Testnet);
        assert_eq!(cfg.stellar_rpc_url, TESTNET_RPC_URL);
        assert_eq!(cfg.network_passphrase, TESTNET_PASSPHRASE);
        assert_eq!(
            cfg.price_loop_interval,
            Duration::from_millis(DEFAULT_PRICE_LOOP_MS)
        );
        assert!(!cfg.price_feed.tokens.is_empty());
    }

    #[test]
    fn config_from_lookup_names_missing_required_var() {
        let mut env = valid_env();
        env.remove("KEEPER_PRIVATE_KEY");

        let err = Config::from_lookup(|key| env.get(key).cloned()).unwrap_err();
        assert_eq!(err, EnvError::MissingVar("KEEPER_PRIVATE_KEY"));
        assert!(err.to_string().contains("KEEPER_PRIVATE_KEY"));
    }

    #[test]
    fn validate_strkey_accepts_s_prefixed() {
        let value = "SAUHMCMUP5FZO5675W3ISZ6E6CNYJGXBUW5WANE2JR4TGAARYCTSCBKI";
        let result = validate_strkey("KEEPER_SECRET_KEY", value.to_string(), 'S');
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), value);
    }

    #[test]
    fn validate_strkey_rejects_g_prefixed_for_secret() {
        let value = "GAUHMCMUP5FZO5675W3ISZ6E6CNYJGXBUW5WANE2JR4TGAARYCTSCBKI";
        let result = validate_strkey("KEEPER_SECRET_KEY", value.to_string(), 'S');
        assert!(result.is_err());
    }

    #[test]
    fn validate_strkey_rejects_wrong_length() {
        let value = "SAUHMC";
        let result = validate_strkey("KEEPER_SECRET_KEY", value.to_string(), 'S');
        assert!(result.is_err());
    }

    #[test]
    fn validate_strkey_rejects_invalid_base32_chars() {
        let value = "0AUHMCMUP5FZO5675W3ISZ6E6CNYJGXBUW5WANE2JR4TGAARYCTSCB";
        let result = validate_strkey("KEEPER_SECRET_KEY", value.to_string(), 'S');
        assert!(result.is_err());
    }

    #[test]
    fn config_from_lookup_rejects_malformed_keeper_account() {
        let mut env = valid_env();
        env.insert("KEEPER_ACCOUNT_ID", "not-a-strkey".to_string());

        let err = Config::from_lookup(|key| env.get(key).cloned()).unwrap_err();

        assert!(matches!(
            err,
            EnvError::InvalidVar {
                var: "KEEPER_ACCOUNT_ID",
                ..
            }
        ));
    }

    #[test]
    fn config_from_lookup_rejects_account_with_secret_prefix() {
        let mut env = valid_env();
        env.insert(
            "KEEPER_ACCOUNT_ID",
            "SAUHMCMUP5FZO5675W3ISZ6E6CNYJGXBUW5WANE2JR4TGAARYCTSCBKI".to_string(),
        );

        let err = Config::from_lookup(|key| env.get(key).cloned()).unwrap_err();

        assert!(matches!(
            err,
            EnvError::InvalidVar { var: "KEEPER_ACCOUNT_ID", .. }
        ));
    }

    #[test]
    fn config_from_lookup_rejects_secret_key_with_account_prefix() {
        let mut env = valid_env();
        // A G-prefixed value in the secret slot is a classic swapped-var typo.
        env.insert(
            "KEEPER_SECRET_KEY",
            "GAUHMCMUP5FZO5675W3ISZ6E6CNYJGXBUW5WANE2JR4TGAARYCTSCBKI".to_string(),
        );

        let err = Config::from_lookup(|key| env.get(key).cloned()).unwrap_err();

        assert!(matches!(
            err,
            EnvError::InvalidVar {
                var: "KEEPER_SECRET_KEY",
                ..
            }
        ));
    }

    #[test]
    fn config_from_lookup_admin_token_is_optional() {
        let mut env = valid_env();
        env.remove("ADMIN_API_TOKEN");

        let cfg = Config::from_lookup(|key| env.get(key).cloned()).unwrap();

        assert!(cfg.admin_api_token.is_none());
    }

    #[test]
    fn config_from_lookup_accepts_deployed_oracle_alias_on_testnet() {
        let mut env = valid_env();
        let oracle = env.remove("ORACLE_CONTRACT_ID").unwrap();
        env.insert("ORACLE", oracle.clone());

        let cfg = Config::from_lookup(|key| env.get(key).cloned()).unwrap();

        assert_eq!(cfg.oracle_contract_id, oracle);
    }

    #[test]
    fn config_from_lookup_requires_explicit_mainnet_rpc() {
        let mut env = valid_env();
        env.insert("STELLAR_NETWORK", "mainnet".to_string());
        env.remove("STELLAR_RPC_URL");

        let err = Config::from_lookup(|key| env.get(key).cloned()).unwrap_err();

        assert_eq!(err, EnvError::MissingVar("STELLAR_RPC_URL"));
    }

    #[test]
    fn config_from_env_names_missing_required_var() {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let _guard = LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
        let keys = [
            "STELLAR_NETWORK",
            "ORACLE_CONTRACT_ID",
            "ROLE_STORE",
            "DATA_STORE",
            "ORDER_HANDLER",
            "DEPOSIT_HANDLER",
            "WITHDRAWAL_HANDLER",
            "READER",
            "KEEPER_PRIVATE_KEY",
            "KEEPER_SECRET_KEY",
            "KEEPER_ACCOUNT_ID",
            "ADMIN_API_TOKEN",
        ];
        let original: Vec<_> = keys
            .iter()
            .map(|key| (*key, std::env::var(key).ok()))
            .collect();

        for key in keys {
            std::env::remove_var(key);
        }
        for (key, value) in valid_env() {
            std::env::set_var(key, value);
        }
        std::env::remove_var("KEEPER_PRIVATE_KEY");

        let err = Config::from_env().unwrap_err();

        for (key, value) in original {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }

        assert_eq!(err, EnvError::MissingVar("KEEPER_PRIVATE_KEY"));
    }
}
