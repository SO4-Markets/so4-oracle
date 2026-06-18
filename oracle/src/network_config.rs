pub const TESTNET_RPC_URL: &str = "https://soroban-testnet.stellar.org";
pub const TESTNET_PASSPHRASE: &str = "Test SDF Network ; September 2015";

pub const MAINNET_RPC_URL: &str = "https://soroban.stellar.org";
pub const MAINNET_PASSPHRASE: &str = "Public Global Stellar Network ; September 2015";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StellarNetwork {
    Testnet,
    Mainnet,
}

#[derive(Debug, Clone)]
pub struct NetworkConfig {
    pub network: StellarNetwork,
    pub rpc_url: String,
    pub passphrase: String,
    pub oracle_contract_id: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum NetworkConfigError {
    UnknownNetwork(String),
    MissingMainnetVar(&'static str),
}

impl std::fmt::Display for NetworkConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NetworkConfigError::UnknownNetwork(n) => {
                write!(
                    f,
                    "unknown STELLAR_NETWORK value '{n}'; expected 'testnet' or 'mainnet'"
                )
            }
            NetworkConfigError::MissingMainnetVar(v) => {
                write!(f, "env var '{v}' must be set explicitly for mainnet")
            }
        }
    }
}

impl std::error::Error for NetworkConfigError {}

/// Load network config from environment variables.
///
/// `STELLAR_NETWORK=testnet` (default) provides sensible defaults.
/// `STELLAR_NETWORK=mainnet` requires explicit `STELLAR_RPC_URL` and `ORACLE_CONTRACT_ID`.
pub fn load_network_config() -> Result<NetworkConfig, NetworkConfigError> {
    let network_str = std::env::var("STELLAR_NETWORK")
        .unwrap_or_else(|_| "testnet".to_string());

    match network_str.as_str() {
        "testnet" => {
            let rpc_url = std::env::var("STELLAR_RPC_URL")
                .unwrap_or_else(|_| TESTNET_RPC_URL.to_string());
            let passphrase = std::env::var("NETWORK_PASSPHRASE")
                .unwrap_or_else(|_| TESTNET_PASSPHRASE.to_string());
            let oracle_contract_id = std::env::var("ORACLE_CONTRACT_ID")
                .map_err(|_| NetworkConfigError::MissingMainnetVar("ORACLE_CONTRACT_ID"))?;
            Ok(NetworkConfig {
                network: StellarNetwork::Testnet,
                rpc_url,
                passphrase,
                oracle_contract_id,
            })
        }
        "mainnet" => {
            let rpc_url = std::env::var("STELLAR_RPC_URL")
                .map_err(|_| NetworkConfigError::MissingMainnetVar("STELLAR_RPC_URL"))?;
            let passphrase = std::env::var("NETWORK_PASSPHRASE")
                .unwrap_or_else(|_| MAINNET_PASSPHRASE.to_string());
            let oracle_contract_id = std::env::var("ORACLE_CONTRACT_ID")
                .map_err(|_| NetworkConfigError::MissingMainnetVar("ORACLE_CONTRACT_ID"))?;
            Ok(NetworkConfig {
                network: StellarNetwork::Mainnet,
                rpc_url,
                passphrase,
                oracle_contract_id,
            })
        }
        _ => Err(NetworkConfigError::UnknownNetwork(network_str)),
    }
}
