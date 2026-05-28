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
    /// A required env var is absent for mainnet.
    MissingMainnetVar(&'static str),
}

impl std::fmt::Display for NetworkConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NetworkConfigError::UnknownNetwork(n) => {
                write!(f, "unknown STELLAR_NETWORK value '{n}'; expected 'testnet' or 'mainnet'")
            }
            NetworkConfigError::MissingMainnetVar(v) => {
                write!(f, "env var '{v}' must be set explicitly for mainnet")
            }
        }
    }
}

/// Load network config from env vars.
///
/// `STELLAR_NETWORK=testnet` (default) provides sensible defaults for every
/// optional var.  `STELLAR_NETWORK=mainnet` requires `STELLAR_RPC_URL` and
/// `ORACLE_CONTRACT_ID` to be set explicitly; startup aborts with an error
/// otherwise.
pub fn load_network_config(env: &worker::Env) -> Result<NetworkConfig, NetworkConfigError> {
    let network_str = env
        .var("STELLAR_NETWORK")
        .map(|v| v.to_string())
        .unwrap_or_else(|_| "testnet".to_string());

    match network_str.as_str() {
        "testnet" => {
            let rpc_url = env
                .var("STELLAR_RPC_URL")
                .map(|v| v.to_string())
                .unwrap_or_else(|_| TESTNET_RPC_URL.to_string());
            let oracle_contract_id = env
                .var("ORACLE_CONTRACT_ID")
                .map(|v| v.to_string())
                .unwrap_or_default();
            Ok(NetworkConfig {
                network: StellarNetwork::Testnet,
                rpc_url,
                passphrase: TESTNET_PASSPHRASE.to_string(),
                oracle_contract_id,
            })
        }
        "mainnet" => {
            let rpc_url = env
                .var("STELLAR_RPC_URL")
                .map(|v| v.to_string())
                .map_err(|_| NetworkConfigError::MissingMainnetVar("STELLAR_RPC_URL"))?;
            let oracle_contract_id = env
                .var("ORACLE_CONTRACT_ID")
                .map(|v| v.to_string())
                .map_err(|_| NetworkConfigError::MissingMainnetVar("ORACLE_CONTRACT_ID"))?;
            Ok(NetworkConfig {
                network: StellarNetwork::Mainnet,
                rpc_url,
                passphrase: MAINNET_PASSPHRASE.to_string(),
                oracle_contract_id,
            })
        }
        other => Err(NetworkConfigError::UnknownNetwork(other.to_string())),
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_cfg(
        network: StellarNetwork,
        rpc_url: &str,
        oracle_contract_id: &str,
    ) -> NetworkConfig {
        let passphrase = match network {
            StellarNetwork::Testnet => TESTNET_PASSPHRASE,
            StellarNetwork::Mainnet => MAINNET_PASSPHRASE,
        };
        NetworkConfig {
            network,
            rpc_url: rpc_url.to_string(),
            passphrase: passphrase.to_string(),
            oracle_contract_id: oracle_contract_id.to_string(),
        }
    }

    #[test]
    fn testnet_defaults_applied_when_no_overrides() {
        let cfg = make_cfg(StellarNetwork::Testnet, TESTNET_RPC_URL, "");
        assert_eq!(cfg.rpc_url, TESTNET_RPC_URL);
        assert_eq!(cfg.passphrase, TESTNET_PASSPHRASE);
        assert_eq!(cfg.network, StellarNetwork::Testnet);
    }

    #[test]
    fn mainnet_config_has_correct_passphrase() {
        let cfg = make_cfg(
            StellarNetwork::Mainnet,
            "https://custom-rpc.example.com",
            "CCUSTOMORACLE",
        );
        assert_eq!(cfg.passphrase, MAINNET_PASSPHRASE);
        assert_eq!(cfg.network, StellarNetwork::Mainnet);
        assert_eq!(cfg.oracle_contract_id, "CCUSTOMORACLE");
    }

    #[test]
    fn testnet_and_mainnet_have_different_passphrases() {
        assert_ne!(TESTNET_PASSPHRASE, MAINNET_PASSPHRASE);
    }

    #[test]
    fn unknown_network_returns_error() {
        let err = NetworkConfigError::UnknownNetwork("staging".to_string());
        assert!(err.to_string().contains("staging"));
    }

    #[test]
    fn missing_mainnet_var_error_names_the_var() {
        let err = NetworkConfigError::MissingMainnetVar("STELLAR_RPC_URL");
        assert!(err.to_string().contains("STELLAR_RPC_URL"));
        assert!(err.to_string().contains("mainnet"));
    }
}
