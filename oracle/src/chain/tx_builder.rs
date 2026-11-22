use ed25519_dalek::Signer;
use sha2::{Digest, Sha256};
use stellar_xdr::{
    DecoratedSignature, InvokeContractArgs, InvokeHostFunctionOp, Limits, Memo, Operation,
    OperationBody, Preconditions, ScSymbol, ScVal, SequenceNumber, Signature, SignatureHint,
    SorobanTransactionData, TimeBounds, TimePoint, Transaction, TransactionEnvelope,
    TransactionExt, TransactionSignaturePayload, TransactionSignaturePayloadTaggedTransaction,
    TransactionV1Envelope, VecM, WriteXdr,
};

use crate::chain::scval::{account_strkey_to_muxed, strkey_to_sc_address};

/// Seconds added to the current Unix timestamp when the caller wants a hard
/// expiry on keeper transactions (#1236). A timed-out transaction is
/// guaranteed `tx_too_late` before the key is retried.
pub const TX_MAX_TIME_SECS: u64 = 60;

/// `max_time`: Unix timestamp of the transaction's hard deadline.
/// Pass `0` to omit time bounds (e.g. for simulation calls where sequence is 0).
pub fn build_invoke_tx(
    source_account: &str,
    contract_id: &str,
    method: &str,
    args: Vec<ScVal>,
    fee: u32,
    sequence: u64,
    soroban_data: Option<SorobanTransactionData>,
    max_time: u64,
) -> Result<Transaction, String> {
    let source_muxed = account_strkey_to_muxed(source_account)?;
    let contract_addr = strkey_to_sc_address(contract_id)?;

    let method_sym: ScSymbol = method
        .to_string()
        .try_into()
        .map_err(|_| "method name too long for ScSymbol".to_string())?;

    let invoke_args = InvokeContractArgs {
        contract_address: contract_addr,
        function_name: method_sym,
        args: args
            .try_into()
            .map_err(|e| format!("failed to convert args to VecM: {e}"))?,
    };

    let host_function = stellar_xdr::HostFunction::InvokeContract(invoke_args);
    let op = Operation {
        source_account: None,
        body: OperationBody::InvokeHostFunction(InvokeHostFunctionOp {
            host_function,
            auth: VecM::default(),
        }),
    };

    let operations: VecM<Operation, 100> = vec![op]
        .try_into()
        .map_err(|e| format!("failed to build operations VecM: {e}"))?;

    let ext = match soroban_data {
        Some(data) => TransactionExt::V1(data),
        None => TransactionExt::V0,
    };

    let cond = if max_time > 0 {
        Preconditions::Time(TimeBounds {
            min_time: TimePoint(0),
            max_time: TimePoint(max_time),
        })
    } else {
        Preconditions::None
    };

    Ok(Transaction {
        source_account: source_muxed,
        fee,
        seq_num: SequenceNumber(sequence as i64),
        cond,
        memo: Memo::None,
        operations,
        ext,
    })
}

pub fn sign_transaction(
    tx: &Transaction,
    secret_key: &str,
    network_passphrase: &str,
) -> Result<String, String> {
    let signing_key = signing_key_from_secret(secret_key)?;
    sign_transaction_with_key(tx, &signing_key, network_passphrase)
}

/// Decode an S-strkey or 64-char hex secret into a `SigningKey` (#1226).
/// Intermediate buffers are zeroised and `SigningKey` wipes itself on drop, so
/// decode once at startup rather than per transaction.
pub fn signing_key_from_secret(secret_key: &str) -> Result<ed25519_dalek::SigningKey, String> {
    let key_bytes_raw = zeroize::Zeroizing::new(if secret_key.starts_with('S') {
        stellar_strkey::ed25519::PrivateKey::from_string(secret_key)
            .map_err(|e| format!("invalid secret strkey: {e}"))?
            .0
            .to_vec()
    } else if secret_key.len() == 64 && secret_key.chars().all(|c| c.is_ascii_hexdigit()) {
        hex::decode(secret_key).map_err(|e| format!("invalid secret key hex: {e}"))?
    } else {
        // #735 — never echo any part of the secret key into an error message.
        // Report only its length and format, which is enough to diagnose a
        // misconfigured env var without leaking key material into logs.
        return Err(format!(
            "secret key must be an S-prefixed strkey or 64-char hex (got {} chars, non-hex or wrong prefix)",
            secret_key.len()
        ));
    });

    if key_bytes_raw.len() != 32 {
        return Err(format!(
            "secret key must be 32 bytes, got {}",
            key_bytes_raw.len()
        ));
    }

    let mut key_array = zeroize::Zeroizing::new([0u8; 32]);
    key_array.copy_from_slice(&key_bytes_raw);
    Ok(ed25519_dalek::SigningKey::from_bytes(&key_array))
}

/// Sign `tx` with an already-decoded key (no per-call key decoding, #1226).
pub fn sign_transaction_with_key(
    tx: &Transaction,
    signing_key: &ed25519_dalek::SigningKey,
    network_passphrase: &str,
) -> Result<String, String> {
    let network_id_hash = compute_network_id(network_passphrase);

    let payload = TransactionSignaturePayload {
        network_id: stellar_xdr::Hash(network_id_hash),
        tagged_transaction: TransactionSignaturePayloadTaggedTransaction::Tx(tx.clone()),
    };

    let payload_bytes = payload
        .to_xdr(Limits::none())
        .map_err(|e| format!("failed to serialize payload: {e}"))?;
    let payload_hash = sha256_hash(&payload_bytes);

    let signature = signing_key.sign(&payload_hash);

    let verifying_key: ed25519_dalek::VerifyingKey = signing_key.verifying_key();
    let pubkey_bytes = verifying_key.to_bytes();

    let hint = SignatureHint([
        pubkey_bytes[28],
        pubkey_bytes[29],
        pubkey_bytes[30],
        pubkey_bytes[31],
    ]);

    let sig_bytes: [u8; 64] = signature.to_bytes();
    let sig_bytesm: stellar_xdr::BytesM<64> = sig_bytes
        .to_vec()
        .try_into()
        .map_err(|_| "signature too long for BytesM<64>".to_string())?;

    let decorated_sig = DecoratedSignature {
        hint,
        signature: Signature(sig_bytesm),
    };

    let sigs: VecM<DecoratedSignature, 20> = vec![decorated_sig]
        .try_into()
        .map_err(|e| format!("failed to build signatures VecM: {e}"))?;

    let envelope = TransactionEnvelope::Tx(TransactionV1Envelope {
        tx: tx.clone(),
        signatures: sigs,
    });

    let envelope_bytes = envelope
        .to_xdr(Limits::none())
        .map_err(|e| format!("failed to serialize envelope: {e}"))?;
    Ok(base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        &envelope_bytes,
    ))
}

fn compute_network_id(passphrase: &str) -> [u8; 32] {
    sha256_hash(passphrase.as_bytes())
}

fn sha256_hash(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    use ed25519_dalek::{SigningKey, Verifier, VerifyingKey};
    use stellar_xdr::ReadXdr;

    fn decode_envelope(xdr: &str) -> TransactionV1Envelope {
        let bytes = STANDARD.decode(xdr).expect("envelope must be valid base64");
        match TransactionEnvelope::from_xdr(bytes, Limits::none())
            .expect("envelope must be valid XDR")
        {
            TransactionEnvelope::Tx(v1) => v1,
            other => panic!("expected a v1 transaction envelope, got {other:?}"),
        }
    }

    /// Rebuild the signing payload exactly as the network verifier does and
    /// check the decorated signature against `verifying_key`.
    fn signature_verifies_under(
        envelope: &TransactionV1Envelope,
        tx: &Transaction,
        verifying_key: &VerifyingKey,
        passphrase: &str,
    ) -> bool {
        let payload = TransactionSignaturePayload {
            network_id: stellar_xdr::Hash(compute_network_id(passphrase)),
            tagged_transaction: TransactionSignaturePayloadTaggedTransaction::Tx(tx.clone()),
        };
        let payload_bytes = payload.to_xdr(Limits::none()).unwrap();
        let payload_hash = sha256_hash(&payload_bytes);

        let sig_bytes: [u8; 64] = envelope.signatures[0]
            .signature
            .0
            .as_slice()
            .try_into()
            .expect("decorated signature must be 64 bytes");
        verifying_key
            .verify(
                &payload_hash,
                &ed25519_dalek::Signature::from_bytes(&sig_bytes),
            )
            .is_ok()
    }

    #[test]
    fn test_build_invoke_tx_produces_valid_transaction() {
        let tx = build_invoke_tx(
            "GAUHMCMUP5FZO5675W3ISZ6E6CNYJGXBUW5WANE2JR4TGAARYCTSCBKI",
            "CBEMTV23SIJJBIST3V5HTMWHR4MHYGHNBIG4M26U4LGUJTWZXTFSVQEY",
            "set_prices",
            vec![ScVal::Void],
            100,
            1,
            None,
            0,
        )
        .unwrap();

        assert_eq!(tx.fee, 100);
        assert_eq!(tx.seq_num.0, 1);
        assert_eq!(tx.operations.len(), 1);
        assert_eq!(tx.cond, Preconditions::None);
    }

    #[test]
    fn test_build_invoke_tx_sets_time_bounds_when_max_time_nonzero() {
        let max_time = 9_999_999_999u64;
        let tx = build_invoke_tx(
            "GAUHMCMUP5FZO5675W3ISZ6E6CNYJGXBUW5WANE2JR4TGAARYCTSCBKI",
            "CBEMTV23SIJJBIST3V5HTMWHR4MHYGHNBIG4M26U4LGUJTWZXTFSVQEY",
            "set_prices",
            vec![ScVal::Void],
            100,
            1,
            None,
            max_time,
        )
        .unwrap();

        assert_eq!(
            tx.cond,
            Preconditions::Time(TimeBounds {
                min_time: TimePoint(0),
                max_time: TimePoint(max_time),
            })
        );
    }

    /// `sign_transaction` produces the envelope for every transaction this
    /// service submits. Assert the whole contract: exact transaction bytes,
    /// signature hint, signature over the network-id-derived payload, and
    /// rejection under a different passphrase (the MAINNET_PASSPHRASE bug
    /// class this repo has already shipped).
    #[test]
    fn test_sign_transaction_signs_the_given_tx_for_the_given_network() {
        let secret_hex = "1111111111111111111111111111111111111111111111111111111111111111";
        let passphrase = "Test SDF Network ; September 2015";
        let tx = build_invoke_tx(
            "GAUHMCMUP5FZO5675W3ISZ6E6CNYJGXBUW5WANE2JR4TGAARYCTSCBKI",
            "CBEMTV23SIJJBIST3V5HTMWHR4MHYGHNBIG4M26U4LGUJTWZXTFSVQEY",
            "set_prices",
            vec![ScVal::Void],
            100,
            1,
            None,
            0,
        )
        .unwrap();

        let xdr = sign_transaction(&tx, secret_hex, passphrase).unwrap();
        let envelope = decode_envelope(&xdr);

        assert_eq!(
            envelope.tx, tx,
            "envelope must wrap exactly the transaction that was passed in"
        );
        assert_eq!(envelope.signatures.len(), 1);

        let key_bytes: [u8; 32] = hex::decode(secret_hex).unwrap().try_into().unwrap();
        let verifying_key = SigningKey::from_bytes(&key_bytes).verifying_key();
        let pubkey = verifying_key.to_bytes();
        assert_eq!(
            envelope.signatures[0].hint,
            SignatureHint([pubkey[28], pubkey[29], pubkey[30], pubkey[31]]),
            "signature hint must be the last four bytes of the signer's public key"
        );

        assert!(
            signature_verifies_under(&envelope, &tx, &verifying_key, passphrase),
            "signature must verify over the submitted tx with the expected network id"
        );

        // A signature over a different payload must not verify, even with the
        // correct key — catches signing the wrong tx or the wrong network.
        let different_tx = build_invoke_tx(
            "GAUHMCMUP5FZO5675W3ISZ6E6CNYJGXBUW5WANE2JR4TGAARYCTSCBKI",
            "CBEMTV23SIJJBIST3V5HTMWHR4MHYGHNBIG4M26U4LGUJTWZXTFSVQEY",
            "set_prices",
            vec![ScVal::Void],
            100,
            2,
            None,
            0,
        )
        .unwrap();
        assert!(
            !signature_verifies_under(&envelope, &different_tx, &verifying_key, passphrase),
            "signature must not verify against a different transaction"
        );
        assert!(
            !signature_verifies_under(
                &envelope,
                &tx,
                &verifying_key,
                "Public Global Stellar Network ; September 2015",
            ),
            "signature must not verify under a different network passphrase"
        );
    }

    #[test]
    fn test_build_invoke_tx_wrong_account_length() {
        let err = build_invoke_tx(
            "GSHORT",
            "CBEMTV23SIJJBIST3V5HTMWHR4MHYGHNBIG4M26U4LGUJTWZXTFSVQEY",
            "set_prices",
            vec![ScVal::Void],
            100,
            1,
            None,
            0,
        );
        assert!(err.is_err());
    }

    /// Regression test for #1025: assert that `compute_network_id` produces the
    /// well-known network ID hash for the real Stellar testnet passphrase. This
    /// catches silent corruption of `TESTNET_PASSPHRASE` in `network_config.rs`.
    #[test]
    fn compute_network_id_matches_known_testnet_hash() {
        let expected = sha256_hash(b"Test SDF Network ; September 2015");
        let actual = compute_network_id(crate::network_config::TESTNET_PASSPHRASE);
        assert_eq!(
            actual, expected,
            "TESTNET_PASSPHRASE in network_config.rs does not match the well-known testnet passphrase"
        );
    }

    /// Regression test for #1025: same check for mainnet.
    #[test]
    fn compute_network_id_matches_known_mainnet_hash() {
        let expected = sha256_hash(b"Public Global Stellar Network ; November 2015");
        let actual = compute_network_id(crate::network_config::MAINNET_PASSPHRASE);
        assert_eq!(
            actual, expected,
            "MAINNET_PASSPHRASE in network_config.rs does not match the well-known mainnet passphrase"
        );
    }

    /// `FUTURENET_PASSPHRASE` is exported even though `Config` currently only
    /// resolves testnet/mainnet, which makes it an easy constant to edit by
    /// hand. Pin its network-id hash so a silent change is caught here too.
    #[test]
    fn compute_network_id_matches_known_futurenet_hash() {
        let expected = sha256_hash(b"Test SDF Futurenet ; October 2022");
        let actual = compute_network_id(crate::network_config::FUTURENET_PASSPHRASE);
        assert_eq!(
            actual, expected,
            "FUTURENET_PASSPHRASE in network_config.rs does not match the well-known futurenet passphrase"
        );
    }
}
