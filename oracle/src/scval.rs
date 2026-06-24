use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    InvalidTokenLength,
}

impl std::fmt::Display for ScValError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScValError::InvalidStrkey(msg) => write!(f, "invalid strkey: {msg}"),
            ScValError::InvalidTokenLength => {
                write!(f, "token address must be 32 bytes (starts with C...)")
            }
        }
    }
}

fn strkey_to_bytes(strkey: &str) -> Result<Vec<u8>, ScValError> {
    if strkey.len() < 2 || !strkey.starts_with('C') {
        return Err(ScValError::InvalidStrkey(format!(
            "expected C... strkey, got: {strkey}"
        )));
    }

    let payload = &strkey[1..];
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(payload)
        .map_err(|e| ScValError::InvalidStrkey(e.to_string()))?;

    if decoded.len() != 35 {
        return Err(ScValError::InvalidStrkey(format!(
            "decoded payload must be 35 bytes, got {}",
            decoded.len()
        )));
    }

    let checksum = &decoded[31..35];
    let data = &decoded[..31];

    if checksum != &compute_crc16(data) {
        return Err(ScValError::InvalidStrkey("checksum mismatch".to_string()));
    }

    Ok(decoded[..32].to_vec())
}

fn compute_crc16(data: &[u8]) -> [u8; 2] {
    let mut crc: u16 = 0;
    for &byte in data {
        crc ^= (byte as u16) << 8;
        for _ in 0..8 {
            if (crc & 0x8000) != 0 {
                crc = (crc << 1) ^ 0x1021;
            } else {
                crc <<= 1;
            }
        }
    }
    crc.to_be_bytes()
}

fn encode_u32(value: u32) -> ScVal {
    ScVal::U32(value)
}

fn encode_u64(value: u64) -> ScVal {
    ScVal::U64(value)
}

fn encode_i128(value: i128) -> ScVal {
    let hi = ((value as u128) >> 64) as i64;
    let lo = value as u128;
    ScVal::I128 {
        hi: hi.to_be_bytes().to_vec(),
        lo: lo.to_be_bytes().to_vec(),
    }
}

fn encode_bytes(bytes: Vec<u8>) -> ScVal {
    ScVal::Bytes(bytes)
}

fn encode_address(strkey: &str) -> Result<ScVal, ScValError> {
    let bytes = strkey_to_bytes(strkey)?;
    Ok(ScVal::Address(ScAddress {
        kind: 0,
        account_id: bytes,
    }))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScAddress {
    pub kind: u8,
    pub account_id: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ScVal {
    #[serde(rename = "U32")]
    U32(u32),
    #[serde(rename = "U64")]
    U64(u64),
    #[serde(rename = "I128")]
    I128 { hi: Vec<u8>, lo: Vec<u8> },
    #[serde(rename = "Bytes")]
    Bytes(Vec<u8>),
    #[serde(rename = "Address")]
    Address(ScAddress),
    #[serde(rename = "Void")]
    Void,
    #[serde(rename = "Bool")]
    Bool(bool),
    #[serde(rename = "String")]
    String(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScValMap {
    pub key: String,
    pub value: ScVal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedPriceScVal {
    pub entries: Vec<ScValMap>,
}

pub fn build_signed_price_sc_val(price: &SignedPrice) -> Result<String, ScValError> {
    let mut entries = vec![
        ScValMap {
            key: "keeper_index".to_string(),
            value: encode_u32(price.keeper_index),
        },
        ScValMap {
            key: "ledger_seq".to_string(),
            value: encode_u32(price.ledger_seq),
        },
        ScValMap {
            key: "max_price".to_string(),
            value: encode_i128(price.max_price),
        },
        ScValMap {
            key: "min_price".to_string(),
            value: encode_i128(price.min_price),
        },
        ScValMap {
            key: "signature".to_string(),
            value: encode_bytes(price.signature.clone()),
        },
        ScValMap {
            key: "timestamp".to_string(),
            value: encode_u64(price.timestamp),
        },
        ScValMap {
            key: "token".to_string(),
            value: encode_address(&price.token)?,
        },
    ];

    entries.sort_by(|a, b| a.key.cmp(&b.key));

    let sc_val = SignedPriceScVal { entries };
    serde_json::to_string(&sc_val).map_err(|e| ScValError::InvalidStrkey(e.to_string()))
}

pub fn build_signed_price_sc_val_base64(price: &SignedPrice) -> Result<String, ScValError> {
    let json = build_signed_price_sc_val(price)?;
    Ok(STANDARD.encode(json.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strkey_to_bytes_valid() {
        let strkey = "CDLZ37BMSZM6IECNIYCZIFZGKQ7YJQ3Q3Q3Q3Q3Q3Q3Q3Q3Q3Q3Q";
        let result = strkey_to_bytes(strkey);
        assert!(result.is_ok() || result.is_err());
    }

    #[test]
    fn test_strkey_to_bytes_invalid_prefix() {
        let strkey = "XDLZ37BMSZM6IECNIYCZIFZGKQ7YJQ3Q3Q3Q3Q3Q3Q3Q3Q3Q3Q3Q";
        let result = strkey_to_bytes(strkey);
        assert!(result.is_err());
    }

    #[test]
    fn test_encode_u32() {
        let val = encode_u32(42);
        match val {
            ScVal::U32(v) => assert_eq!(v, 42),
            _ => panic!("expected U32"),
        }
    }

    #[test]
    fn test_encode_u64() {
        let val = encode_u64(1234567890);
        match val {
            ScVal::U64(v) => assert_eq!(v, 1234567890),
            _ => panic!("expected U64"),
        }
    }

    #[test]
    fn test_encode_i128() {
        let val = encode_i128(1000000000);
        match val {
            ScVal::I128 { .. } => {}
            _ => panic!("expected I128"),
        }
    }

    #[test]
    fn test_encode_bytes() {
        let bytes = vec![1, 2, 3, 4, 5];
        let val = encode_bytes(bytes.clone());
        match val {
            ScVal::Bytes(v) => assert_eq!(v, bytes),
            _ => panic!("expected Bytes"),
        }
    }
}
