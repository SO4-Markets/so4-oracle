use ed25519_dalek::{Signature, Signer, SigningKey};

/// Error that can occur during signing
#[derive(Debug, PartialEq, Eq)]
pub enum SigningError {
    MissingPrivateKey,
    InvalidHexKey,
    InvalidKeyLength,
}

impl std::fmt::Display for SigningError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SigningError::MissingPrivateKey => write!(f, "KEEPER_PRIVATE_KEY is not set"),
            SigningError::InvalidHexKey => write!(f, "KEEPER_PRIVATE_KEY is not valid hex"),
            SigningError::InvalidKeyLength => write!(
                f,
                "KEEPER_PRIVATE_KEY must be exactly 32 bytes (64 hex chars)"
            ),
        }
    }
}

/// Sign a price update message using the ed25519 keeper key.
///
/// The message layout is:
/// `network_passphrase ‖ ledger_seq ‖ token_strkey ‖ min ‖ max ‖ timestamp`
///
/// Data types:
/// - `network_passphrase`: UTF-8 bytes
/// - `ledger_seq`: u32 Big-Endian
/// - `token_strkey`: UTF-8 bytes
/// - `min`: i128 Big-Endian
/// - `max`: i128 Big-Endian
/// - `timestamp`: u64 Big-Endian
pub fn sign_price(
    private_key_hex: &str,
    network_passphrase: &str,
    ledger_seq: u32,
    token_strkey: &str,
    min: i128,
    max: i128,
    timestamp: u64,
) -> Result<Signature, SigningError> {
    // 1. Parse the private key securely without logging it
    let key_bytes = hex::decode(private_key_hex).map_err(|_| SigningError::InvalidHexKey)?;
    if key_bytes.len() != 32 {
        return Err(SigningError::InvalidKeyLength);
    }

    let key_array: [u8; 32] = key_bytes.try_into().unwrap();
    let signing_key = SigningKey::from_bytes(&key_array);

    // 2. Construct the byte layout
    let payload = build_price_message(
        network_passphrase,
        ledger_seq,
        token_strkey,
        min,
        max,
        timestamp,
    );

    // 3. Sign the payload
    let signature = signing_key.sign(&payload);

    Ok(signature)
}

pub fn build_price_message(
    network_passphrase: &str,
    ledger_seq: u32,
    token_strkey: &str,
    min: i128,
    max: i128,
    timestamp: u64,
) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(network_passphrase.as_bytes());
    payload.extend_from_slice(&ledger_seq.to_be_bytes());
    payload.extend_from_slice(token_strkey.as_bytes());
    payload.extend_from_slice(&min.to_be_bytes());
    payload.extend_from_slice(&max.to_be_bytes());
    payload.extend_from_slice(&timestamp.to_be_bytes());
    payload
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Verifier;

    #[test]
    fn test_sign_price_validates() {
        // A known dummy 32-byte private key in hex
        let private_key_hex = "1111111111111111111111111111111111111111111111111111111111111111";
        let signing_key =
            SigningKey::from_bytes(&hex::decode(private_key_hex).unwrap().try_into().unwrap());
        let public_key = signing_key.verifying_key();

        let network_passphrase = "Test SDF Network ; September 2015";
        let ledger_seq: u32 = 123456;
        let token_strkey = "CBTCADDR";
        let min: i128 = 45000_0000000;
        let max: i128 = 46000_0000000;
        let timestamp: u64 = 1690000000;

        // Sign the payload
        let signature = sign_price(
            private_key_hex,
            network_passphrase,
            ledger_seq,
            token_strkey,
            min,
            max,
            timestamp,
        )
        .expect("signing failed");

        // Construct expected payload
        let mut expected_payload = Vec::new();
        expected_payload.extend_from_slice(network_passphrase.as_bytes());
        expected_payload.extend_from_slice(&ledger_seq.to_be_bytes());
        expected_payload.extend_from_slice(token_strkey.as_bytes());
        expected_payload.extend_from_slice(&min.to_be_bytes());
        expected_payload.extend_from_slice(&max.to_be_bytes());
        expected_payload.extend_from_slice(&timestamp.to_be_bytes());

        // Verify the signature against the public key
        assert!(
            public_key.verify(&expected_payload, &signature).is_ok(),
            "Signature must be valid"
        );
    }

    #[test]
    fn price_message_layout_regression_vector() {
        let bytes = build_price_message(
            "Test SDF Network ; September 2015",
            123456,
            "CBAN5YU3KRDKPTQ2H76D6S7HQFPRBGUD524F65BUM2RQCITPTRLKWKES",
            1_234_567_890_000_000_000_000_000_000_000i128,
            1_234_667_890_000_000_000_000_000_000_000i128,
            1_690_000_000,
        );

        assert_eq!(
            hex::encode(bytes),
            "5465737420534446204e6574776f726b203b2053657074656d62657220323031350001e2404342414e355955334b52444b505451324837364436533748514650524247554435323446363542554d3252514349545054524c4b574b45530000000f951a9f9cf13829cddf4000000000000f956d576fce0036a0c34000000000000064bb5a80"
        );
    }

    #[test]
    fn test_sign_price_invalid_hex() {
        let err = sign_price("not hex", "net", 1, "tok", 10, 20, 100).unwrap_err();
        assert_eq!(err, SigningError::InvalidHexKey);
    }

    #[test]
    fn test_sign_price_invalid_length() {
        let err = sign_price("1111", "net", 1, "tok", 10, 20, 100).unwrap_err();
        assert_eq!(err, SigningError::InvalidKeyLength);
    }
}
