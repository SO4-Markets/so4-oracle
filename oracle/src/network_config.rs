//! Network reference constants (RPC URLs and passphrases).
//!
//! Network *selection* and the resolved [`crate::config::Config`] live in
//! `config.rs`; this module only holds the per-network defaults it consumes.

pub const TESTNET_RPC_URL: &str = "https://soroban-testnet.stellar.org";
pub const TESTNET_PASSPHRASE: &str = "Test SDF Network ; September 2015";

pub const MAINNET_RPC_URL: &str = "https://soroban.stellar.org";
pub const MAINNET_PASSPHRASE: &str = "Public Global Stellar Network ; September 2015";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn testnet_and_mainnet_have_different_passphrases() {
        assert_ne!(TESTNET_PASSPHRASE, MAINNET_PASSPHRASE);
    }
}
