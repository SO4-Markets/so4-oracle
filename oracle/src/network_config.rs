pub const TESTNET_RPC_URL: &str = "https://soroban-testnet.stellar.org";

pub const MAINNET_PASSPHRASE: &str = "Public Global Stellar Network ; September 2015";
pub const TESTNET_PASSPHRASE: &str = "Test SDF Network ; September 2015";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn testnet_and_mainnet_constants_are_exact() {
        assert_eq!(TESTNET_RPC_URL, "https://soroban-testnet.stellar.org");
        assert_eq!(TESTNET_PASSPHRASE, "Test SDF Network ; September 2015");
        assert_eq!(
            MAINNET_PASSPHRASE,
            "Public Global Stellar Network ; September 2015"
        );
        assert_ne!(TESTNET_PASSPHRASE, MAINNET_PASSPHRASE);
    }
}
