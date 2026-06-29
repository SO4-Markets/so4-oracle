use sha2::{Digest, Sha256};
use stellar_strkey::ed25519::PublicKey as StrkeyPublicKey;
use stellar_strkey::Contract;
use stellar_xdr::{
    Int128Parts, ScAddress, ScBytes, ScMap, ScMapEntry, ScSymbol, ScVal, ScVec, WriteXdr,
};

#[derive(Debug, Clone)]
pub struct SignedPrice {
    pub keeper_index: u32,
    pub ledger_seq: u32,
    pub max_price: i128,
    pub min_price: i128,
    pub signature: Vec<u8>,
    pub timestamp: u64,
    pub token: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ScValError {
    InvalidStrkey(String),
    XdrEncoding(String),
    SignatureLength(usize),
}

impl std::fmt::Display for ScValError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScValError::InvalidStrkey(msg) => write!(f, "invalid strkey: {msg}"),
            ScValError::XdrEncoding(msg) => write!(f, "XDR encoding error: {msg}"),
            ScValError::SignatureLength(len) => {
                write!(f, "signature must be 64 bytes, got {len}")
            }
        }
    }
}

impl std::error::Error for ScValError {}

fn contract_to_sc_address(contract_str: &str) -> Result<ScAddress, ScValError> {
    let contract: Contract = contract_str
        .parse()
        .map_err(|e: stellar_strkey::DecodeError| ScValError::InvalidStrkey(e.to_string()))?;
    Ok(ScAddress::Contract(stellar_xdr::ContractId(
        stellar_xdr::Hash(contract.0),
    )))
}

pub fn pubkey_to_sc_address(pubkey_str: &str) -> Result<ScAddress, ScValError> {
    let pk: StrkeyPublicKey = pubkey_str
        .parse()
        .map_err(|e: stellar_strkey::DecodeError| ScValError::InvalidStrkey(e.to_string()))?;
    Ok(ScAddress::Account(stellar_xdr::AccountId(
        stellar_xdr::PublicKey::PublicKeyTypeEd25519(stellar_xdr::Uint256(pk.0)),
    )))
}

fn i128_to_parts(value: i128) -> Int128Parts {
    let bits = value as u128;
    Int128Parts {
        hi: (bits >> 64) as i64,
        lo: (bits & 0xFFFF_FFFF_FFFF_FFFF) as u64,
    }
}

fn make_sc_symbol(s: &str) -> ScSymbol {
    ScSymbol(s.as_bytes().to_vec().try_into().unwrap())
}

pub fn encode_signed_price(price: &SignedPrice) -> Result<ScVal, ScValError> {
    let sig_bytes = if price.signature.len() == 64 {
        price.signature.clone()
    } else {
        return Err(ScValError::SignatureLength(price.signature.len()));
    };

    let entries = vec![
        ScMapEntry {
            key: ScVal::Symbol(make_sc_symbol("keeper_index")),
            val: ScVal::U32(price.keeper_index),
        },
        ScMapEntry {
            key: ScVal::Symbol(make_sc_symbol("ledger_seq")),
            val: ScVal::U32(price.ledger_seq),
        },
        ScMapEntry {
            key: ScVal::Symbol(make_sc_symbol("max_price")),
            val: ScVal::I128(i128_to_parts(price.max_price)),
        },
        ScMapEntry {
            key: ScVal::Symbol(make_sc_symbol("min_price")),
            val: ScVal::I128(i128_to_parts(price.min_price)),
        },
        ScMapEntry {
            key: ScVal::Symbol(make_sc_symbol("signature")),
            val: ScVal::Bytes(ScBytes(sig_bytes.try_into().unwrap())),
        },
        ScMapEntry {
            key: ScVal::Symbol(make_sc_symbol("timestamp")),
            val: ScVal::U64(price.timestamp),
        },
        ScMapEntry {
            key: ScVal::Symbol(make_sc_symbol("token")),
            val: ScVal::Address(contract_to_sc_address(&price.token)?),
        },
    ];

    Ok(ScVal::Map(Some(ScMap(entries.try_into().unwrap()))))
}

pub fn encode_signed_price_base64(price: &SignedPrice) -> Result<String, ScValError> {
    let sc_val = encode_signed_price(price)?;
    sc_val
        .to_xdr_base64(stellar_xdr::Limits::none())
        .map_err(|e| ScValError::XdrEncoding(e.to_string()))
}

pub fn encode_signed_prices_vec(prices: &[SignedPrice]) -> Result<ScVal, ScValError> {
    let mut sc_vals = Vec::with_capacity(prices.len());
    for price in prices {
        sc_vals.push(encode_signed_price(price)?);
    }
    Ok(ScVal::Vec(Some(ScVec(sc_vals.try_into().unwrap()))))
}

pub fn compute_transaction_hash(
    network_passphrase: &str,
    tx_xdr_base64: &str,
) -> Result<Vec<u8>, ScValError> {
    let tx_bytes =
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, tx_xdr_base64)
            .map_err(|e| ScValError::XdrEncoding(e.to_string()))?;

    let mut hasher = Sha256::new();
    hasher.update(network_passphrase.as_bytes());
    hasher.update(&tx_bytes);
    Ok(hasher.finalize().to_vec())
}

pub fn signature_hint(network_passphrase: &str, public_key: &[u8; 32]) -> [u8; 4] {
    let pk_hash = Sha256::digest(public_key);
    let pp_hash = Sha256::digest(network_passphrase.as_bytes());
    let mut hint = [0u8; 4];
    for i in 0..4 {
        hint[i] = pk_hash[28 + i] ^ pp_hash[28 + i];
    }
    hint
}

#[cfg(test)]
mod tests {
    use super::*;
    use stellar_xdr::ReadXdr;

    #[test]
    fn test_encode_u32() {
        let price = SignedPrice {
            keeper_index: 0,
            ledger_seq: 100,
            max_price: 45000_0000000,
            min_price: 44000_0000000,
            signature: vec![0u8; 64],
            timestamp: 1690000000,
            token: "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4".to_string(),
        };
        let sc_val = encode_signed_price(&price).unwrap();
        let xdr_b64 = sc_val.to_xdr_base64(stellar_xdr::Limits::none()).unwrap();
        assert!(!xdr_b64.is_empty());

        let decoded = ScVal::from_xdr_base64(&xdr_b64, stellar_xdr::Limits::none()).unwrap();
        assert_eq!(sc_val, decoded);
    }

    #[test]
    fn test_i128_parts_positive() {
        let parts = i128_to_parts(1_000_000_000);
        assert_eq!(parts.hi, 0i64);
        assert_eq!(parts.lo, 1_000_000_000u64);
    }

    #[test]
    fn test_i128_parts_large() {
        let val: i128 = (1i128 << 80) + 42;
        let parts = i128_to_parts(val);
        assert_eq!(parts.hi, 1i64 << 16);
        assert_eq!(parts.lo, 42u64);
    }

    #[test]
    fn test_contract_address_encoding() {
        let addr =
            contract_to_sc_address("CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4")
                .unwrap();
        match addr {
            ScAddress::Contract(_) => {}
            _ => panic!("expected Contract variant"),
        }
    }

    #[test]
    fn test_pubkey_address_encoding() {
        let addr = pubkey_to_sc_address("GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF")
            .unwrap();
        match addr {
            ScAddress::Account(_) => {}
            _ => panic!("expected Account variant"),
        }
    }

    #[test]
    fn test_signature_length_validation() {
        let price = SignedPrice {
            keeper_index: 0,
            ledger_seq: 1,
            max_price: 100,
            min_price: 90,
            signature: vec![0u8; 32],
            timestamp: 1000,
            token: "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4".to_string(),
        };
        let err = encode_signed_price(&price).unwrap_err();
        assert!(matches!(err, ScValError::SignatureLength(32)));
    }

    #[test]
    fn test_signed_prices_vec_encoding() {
        let price = SignedPrice {
            keeper_index: 0,
            ledger_seq: 100,
            max_price: 45000_0000000,
            min_price: 44000_0000000,
            signature: vec![0u8; 64],
            timestamp: 1690000000,
            token: "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4".to_string(),
        };
        let vec_val = encode_signed_prices_vec(&[price]).unwrap();
        match vec_val {
            ScVal::Vec(_) => {}
            _ => panic!("expected ScVec"),
        }
        let xdr_b64 = vec_val.to_xdr_base64(stellar_xdr::Limits::none()).unwrap();
        assert!(!xdr_b64.is_empty());
    }

    #[test]
    fn test_sc_val_roundtrip() {
        let price = SignedPrice {
            keeper_index: 42,
            ledger_seq: 999,
            max_price: 123456789,
            min_price: -100,
            signature: vec![0xAA; 64],
            timestamp: 1700000000,
            token: "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4".to_string(),
        };
        let sc_val = encode_signed_price(&price).unwrap();
        let xdr_b64 = sc_val.to_xdr_base64(stellar_xdr::Limits::none()).unwrap();
        let decoded = ScVal::from_xdr_base64(&xdr_b64, stellar_xdr::Limits::none()).unwrap();
        assert_eq!(sc_val, decoded);
    }
}
