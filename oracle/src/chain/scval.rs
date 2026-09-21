// ... existing code ...

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::xdr::{ScAddress, Uint256};
    // The `stellar_strkey` crate provides helpers for the various Stellar
    // StrKey types.  We use the `contract` and `secret` modules to generate
    // deterministic test vectors for a contract address and an unsupported
    // address type respectively.
    use stellar_strkey::contract::ContractId;
    use stellar_strkey::secret::SecretSeed;

    /// Helper that creates a `ScAddress::Contract` from a raw 32‑byte array.
    fn contract_sc_address(bytes: [u8; 32]) -> ScAddress {
        ScAddress::Contract(Uint256(bytes))
    }

    #[test]
    fn test_strkey_to_sc_address_contract() {
        // A deterministic 32‑byte payload – the actual value is irrelevant,
        // it just needs to be a valid contract identifier.
        let raw_bytes = [0xABu8; 32];

        // Encode the raw bytes as a Stellar contract StrKey (C…).
        let contract_id = ContractId::from_bytes(&raw_bytes)
            .expect("failed to create ContractId from raw bytes");
        let contract_strkey = contract_id.to_string();

        // The function under test should successfully decode the C… strkey
        // into a `ScAddress::Contract` containing the original bytes.
        let decoded = strkey_to_sc_address(&contract_strkey)
            .expect("strkey_to_sc_address failed to decode a valid contract strkey");

        match decoded {
            ScAddress::Contract(contract) => {
                assert_eq!(contract.0, raw_bytes, "decoded contract bytes do not match original");
            }
            other => panic!("expected ScAddress::Contract, got {:?}", other),
        }
    }

    #[test]
    fn test_strkey_to_sc_address_unsupported_type() {
        // Generate a secret seed (S…) which is *not* a supported address type
        // for `strkey_to_sc_address`.  The function should return an error.
        let seed = SecretSeed::from_bytes(&[0u8; 32])
            .expect("failed to create SecretSeed from raw bytes");
        let seed_strkey = seed.to_string();

        let err = strkey_to_sc_address(&seed_strkey)
            .expect_err("expected an error when decoding an unsupported strkey type");

        // The exact error type is defined in `scval.rs`; we only assert that
        // an error was returned.  If the error type implements `Debug`,
        // printing it can aid future debugging.
        println!("Received expected error: {:?}", err);
    }

    // The original test for public‑key accounts is retained to ensure we do
    // not regress existing coverage.
    #[test]
    fn test_strkey_to_sc_address_account() {
        // Example public‑key ed25519 account (G…).
        let account_strkey = "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF";
        let decoded = strkey_to_sc_address(account_strkey)
            .expect("failed to decode a valid account strkey");

        match decoded {
            ScAddress::Account(pub_key) => {
                // The public key bytes are deterministic for the above
                // placeholder; we simply ensure the variant is correct.
                let _ = pub_key; // silence unused‑variable warning
            }
            other => panic!("expected ScAddress::Account, got {:?}", other),
        }
    }
}
