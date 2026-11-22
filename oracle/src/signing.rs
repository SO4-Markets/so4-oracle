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

impl std::error::Error for SigningError {}

/// Layout: `network_passphrase || ledger_seq || token_strkey || min || max || timestamp`
///
/// Data types:
/// - `network_passphrase`: UTF-8 bytes
/// - `ledger_seq`: u32 Big-Endian
/// - `token_strkey`: UTF-8 bytes
/// - `min`: i128 Big-Endian
/// - `max`: i128 Big-Endian
/// - `timestamp`: u64 Big-Endian
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

/// Sign a price update message using the ed25519 keeper key.
///
/// Layout: `network_passphrase || ledger_seq || token_strkey || min || max || timestamp`
///
/// Data types:
/// - `network_passphrase`: UTF-8 bytes
/// - `ledger_seq`: u32 Big-Endian
/// - `token_strkey`: UTF-8 bytes
/// - `min`: i128 Big-Endian
/// - `max`: i128 Big-Endian
/// - `timestamp`: u64 Big-Endian
//Fix size implementation for i128 and u64 to ensure correct byte representation
pub fn sign_price(
    private_key_hex: &str,
    network_passphrase: &str,
    ledger_seq: u32,
    token_strkey: &str,
    min: i128,
    max: i128,
    timestamp: u64,
) -> Result<Signature, SigningError> {
    let signing_key = signing_key_from_hex(private_key_hex)?;
    Ok(sign_price_with_key(
        &signing_key,
        network_passphrase,
        ledger_seq,
        token_strkey,
        min,
        max,
        timestamp,
    ))
}

/// Decode a hex ed25519 seed into a `SigningKey` (#1226). Intermediate buffers
/// are zeroised, and `SigningKey` itself wipes its seed on drop, so decode
/// once at startup and reuse the key instead of re-decoding per signature.
pub fn signing_key_from_hex(private_key_hex: &str) -> Result<SigningKey, SigningError> {
    let key_bytes = zeroize::Zeroizing::new(
        hex::decode(private_key_hex).map_err(|_| SigningError::InvalidHexKey)?,
    );
    let key_array: zeroize::Zeroizing<[u8; 32]> = zeroize::Zeroizing::new(
        key_bytes
            .as_slice()
            .try_into()
            .map_err(|_| SigningError::InvalidKeyLength)?,
    );
    Ok(SigningKey::from_bytes(&key_array))
}

/// Sign a price with an already-decoded key (no per-call key decoding, #1226).
pub fn sign_price_with_key(
    signing_key: &SigningKey,
    network_passphrase: &str,
    ledger_seq: u32,
    token_strkey: &str,
    min: i128,
    max: i128,
    timestamp: u64,
) -> Signature {
    let payload = build_price_message(
        network_passphrase,
        ledger_seq,
        token_strkey,
        min,
        max,
        timestamp,
    );
    signing_key.sign(&payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Verifier;

    // #1226 — signing with a key decoded once must match per-call decoding.
    #[test]
    fn sign_price_with_key_matches_sign_price() {
        let hex_key = "1111111111111111111111111111111111111111111111111111111111111111";
        let key = signing_key_from_hex(hex_key).unwrap();
        let a = sign_price(hex_key, "net", 7, "tok", 10, 20, 100).unwrap();
        let b = sign_price_with_key(&key, "net", 7, "tok", 10, 20, 100);
        assert_eq!(a, b);
    }

    #[test]
    fn signing_key_from_hex_rejects_bad_input() {
        assert!(matches!(
            signing_key_from_hex("not hex"),
            Err(SigningError::InvalidHexKey)
        ));
        assert!(matches!(
            signing_key_from_hex("1111"),
            Err(SigningError::InvalidKeyLength)
        ));
    }

    #[test]
    fn test_sign_price_validates() {
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

        let expected_payload = build_price_message(
            network_passphrase,
            ledger_seq,
            token_strkey,
            min,
            max,
            timestamp,
        );

        assert!(
            public_key.verify(&expected_payload, &signature).is_ok(),
            "Signature must be valid"
        );
    }

    /// #387 — build_price_message regression vector: fixed input → known hex output.
    #[test]
    fn test_build_price_message_regression_vector() {
        let passphrase = "Test SDF Network ; September 2015";
        let ledger_seq: u32 = 123456;
        let token_strkey = "CBTCADDR";
        let min: i128 = 45000_0000000;
        let max: i128 = 46000_0000000;
        let timestamp: u64 = 1690000000;

        let msg = build_price_message(passphrase, ledger_seq, token_strkey, min, max, timestamp);

        let mut expected = Vec::new();
        expected.extend_from_slice(passphrase.as_bytes());
        expected.extend_from_slice(&ledger_seq.to_be_bytes());
        expected.extend_from_slice(token_strkey.as_bytes());
        expected.extend_from_slice(&min.to_be_bytes());
        expected.extend_from_slice(&max.to_be_bytes());
        expected.extend_from_slice(&timestamp.to_be_bytes());

        assert_eq!(hex::encode(&msg), hex::encode(&expected));
    }

    /// #388 — "not hex" input must return InvalidHexKey.
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

    /// #389 — "1111" (2 bytes) must return InvalidKeyLength.
    #[test]
    fn test_sign_price_invalid_length() {
        let err = sign_price("1111", "net", 1, "tok", 10, 20, 100).unwrap_err();
        assert_eq!(err, SigningError::InvalidKeyLength);
    }

    #[test]
    fn test_build_price_message_layout() {
        let payload = build_price_message(
            "Test SDF Network ; September 2015",
            123456,
            "CBTCADDR",
            45000_0000000,
            46000_0000000,
            1690000000,
        );

        let expected = b"Test SDF Network ; September 2015";
        assert_eq!(&payload[0..expected.len()], expected);

        let offset = expected.len();
        assert_eq!(&payload[offset..offset + 4], &123456u32.to_be_bytes());

        let offset = offset + 4;
        assert_eq!(&payload[offset..offset + 8], b"CBTCADDR");

        let offset = offset + 8;
        assert_eq!(
            &payload[offset..offset + 16],
            &45000_0000000i128.to_be_bytes()
        );

        let offset = offset + 16;
        assert_eq!(
            &payload[offset..offset + 16],
            &46000_0000000i128.to_be_bytes()
        );

        let offset = offset + 16;
        assert_eq!(&payload[offset..offset + 8], &1690000000u64.to_be_bytes());

        assert_eq!(payload.len(), expected.len() + 4 + 8 + 16 + 16 + 8);
    }
}
