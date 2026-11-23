pub const TESTNET_RPC_URL: &str = "https://soroban-testnet.stellar.org";
pub const FUTURENET_RPC_URL: &str = "https://rpc-futurenet.stellar.org";

pub const MAINNET_PASSPHRASE: &str = "Public Global Stellar Network ; November 2015";
pub const TESTNET_PASSPHRASE: &str = "Test SDF Network ; September 2015";
pub const FUTURENET_PASSPHRASE: &str = "Test SDF Futurenet ; October 2022";

#[cfg(test)]
mod tests {
    use super::*;

    // #1421 — TESTNET_RPC_URL once shipped as "https://stellar.org", which is
    // not an RPC endpoint. Pin scheme and exact host so a slip fails CI.
    fn assert_https_host(raw: &str, host: &str) {
        let url = reqwest::Url::parse(raw).expect("valid URL");
        assert_eq!(url.scheme(), "https", "{raw}");
        assert_eq!(url.host_str(), Some(host), "{raw}");
    }

    #[test]
    fn testnet_rpc_url_is_the_soroban_testnet_endpoint() {
        assert_https_host(TESTNET_RPC_URL, "soroban-testnet.stellar.org");
    }

    #[test]
    fn futurenet_rpc_url_is_the_futurenet_endpoint() {
        assert_https_host(FUTURENET_RPC_URL, "rpc-futurenet.stellar.org");
    }

    #[test]
    fn passphrases_are_the_published_network_strings() {
        assert_eq!(
            MAINNET_PASSPHRASE,
            "Public Global Stellar Network ; November 2015"
        );
        assert_eq!(TESTNET_PASSPHRASE, "Test SDF Network ; September 2015");
        assert_eq!(FUTURENET_PASSPHRASE, "Test SDF Futurenet ; October 2022");
    }
}
