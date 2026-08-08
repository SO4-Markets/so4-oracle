#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use shared_config::TokenConfig;

use oracle::config::{Config, Network, PriceFeedConfig, SecretString};
use oracle::state::AppState;

pub fn test_config(rpc_url: &str, horizon_url: &str) -> Arc<Config> {
    test_config_with_tokens(rpc_url, horizon_url, vec![])
}

pub fn test_config_with_tokens(
    rpc_url: &str,
    horizon_url: &str,
    tokens: Vec<TokenConfig>,
) -> Arc<Config> {
    Arc::new(Config {
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        network: Network::Testnet,
        network_passphrase: "Test SDF Network ; September 2015".to_string(),
        stellar_rpc_url: rpc_url.to_string(),
        horizon_url: horizon_url.to_string(),
        oracle_contract_id: "CBEMTV23SIJJBIST3V5HTMWHR4MHYGHNBIG4M26U4LGUJTWZXTFSVQEY".to_string(),
        role_store_contract_id: "CBSUAIAMIFFS4AXQYZ7KR7FNO7IMKAPS5WF4DXANVXDTPKH2F7YUIN6Q"
            .to_string(),
        data_store_contract_id: "CCZ3VKBEDLNBO2JM3EXL3SNBDJOV5BTN52FVQPER7F6D5GCE53PITQ3J"
            .to_string(),
        order_handler_contract_id: "CC35OFZVWUTAZPV3B6UKSDVAVORZEWUUMOMTHO33H4YR4C5FKPEFODKY"
            .to_string(),
        deposit_handler_contract_id: "CDWOFIP4YQJGMCYAOWLSRBAWN2OTJUG2I5WOFC32O2TX2SRU56RWBE5C"
            .to_string(),
        withdrawal_handler_contract_id: "CCA5HRHMG6E6BVYRICSLZ5CK5KNPAAKXQ7XWDM34WWVGNHWHA26GRVVE"
            .to_string(),
        reader_contract_id: "CC6OZUHF3LVO6PNP3V2EB36ORB3YSVYSH3LWD3RFLO4NUO3BYCXSWSYC".to_string(),
        keeper_private_key: SecretString::new(
            "1111111111111111111111111111111111111111111111111111111111111111".to_string(),
        ),
        keeper_secret_key: SecretString::new(
            "1111111111111111111111111111111111111111111111111111111111111111".to_string(),
        ),
        keeper_account_id: "GAUHMCMUP5FZO5675W3ISZ6E6CNYJGXBUW5WANE2JR4TGAARYCTSCBKI".to_string(),
        keeper_index: 0,
        admin_api_token: Some(SecretString::new("test-admin-token".to_string())),
        min_keeper_balance_xlm: 10.0,
        set_prices_tx_fee: oracle::config::DEFAULT_SET_PRICES_TX_FEE,
        keeper_tx_fee: oracle::config::DEFAULT_KEEPER_TX_FEE,
        price_loop_interval: Duration::from_millis(1000),
        keeper_loop_interval: Duration::from_millis(1500),
        price_feed: PriceFeedConfig { tokens },
        pyth_api_key: None,
    })
}

pub fn fixed_token(symbol: &str, address: &str) -> TokenConfig {
    fixed_token_with_price(symbol, address, "1000000000000000000000000000000")
}

pub fn fixed_token_with_price(symbol: &str, address: &str, price: &str) -> TokenConfig {
    TokenConfig {
        symbol: symbol.to_string(),
        display_symbol: Some(symbol.to_string()),
        stellar_address: address.to_string(),
        sources: vec!["fixed".to_string()],
        fixed_price: Some(price.to_string()),
        binance_symbol: None,
        coinbase_symbol: None,
        pyth_feed_id: None,
        min_sources: 1,
        max_deviation_bps: 100,
        stale_after_seconds: 60,
        submit_threshold_bps: 10,
        min: 0.0,
        max: 0.0,
        sources_used: vec![],
    }
}

pub fn bad_token(symbol: &str, address: &str) -> TokenConfig {
    TokenConfig {
        symbol: symbol.to_string(),
        display_symbol: Some(symbol.to_string()),
        stellar_address: address.to_string(),
        sources: vec!["unsupported_source".to_string()],
        fixed_price: None,
        binance_symbol: None,
        coinbase_symbol: None,
        pyth_feed_id: None,
        min_sources: 1,
        max_deviation_bps: 100,
        stale_after_seconds: 60,
        submit_threshold_bps: 10,
        min: 0.0,
        max: 0.0,
        sources_used: vec![],
    }
}

pub fn test_state(rpc_url: &str, tokens: Vec<TokenConfig>) -> Arc<AppState> {
    let config = Arc::new(Config {
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        network: Network::Testnet,
        network_passphrase: "Test SDF Network ; September 2015".to_string(),
        stellar_rpc_url: rpc_url.to_string(),
        horizon_url: "http://localhost:0".to_string(),
        oracle_contract_id: "CORACLE".to_string(),
        role_store_contract_id: "CROLE".to_string(),
        data_store_contract_id: "CDATA".to_string(),
        order_handler_contract_id: "CORDER".to_string(),
        deposit_handler_contract_id: "CDEPOSIT".to_string(),
        withdrawal_handler_contract_id: "CWITHDRAW".to_string(),
        reader_contract_id: "CREADER".to_string(),
        keeper_private_key: SecretString::new(
            "1111111111111111111111111111111111111111111111111111111111111111".to_string(),
        ),
        keeper_secret_key: SecretString::new("SSECRET".to_string()),
        keeper_account_id: "GACCOUNT".to_string(),
        keeper_index: 0,
        admin_api_token: None,
        min_keeper_balance_xlm: 0.0,
        set_prices_tx_fee: oracle::config::DEFAULT_SET_PRICES_TX_FEE,
        keeper_tx_fee: oracle::config::DEFAULT_KEEPER_TX_FEE,
        price_loop_interval: Duration::from_millis(1000),
        keeper_loop_interval: Duration::from_millis(1000),
        price_feed: PriceFeedConfig { tokens },
        pyth_api_key: None,
    });
    Arc::new(AppState::new(config))
}
