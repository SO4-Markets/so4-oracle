use stellar_xdr::ScVal;

use crate::state::CachedPrice;

pub fn encode_signed_price(price: &CachedPrice) -> Result<ScVal, String> {
    let sig_bytes =
        hex::decode(&price.signature).map_err(|e| format!("invalid signature hex: {e}"))?;
    if sig_bytes.len() != 64 {
        return Err(format!(
            "signature must be 64 bytes, got {}",
            sig_bytes.len()
        ));
    }

    let contract_addr = strkey_to_sc_address(&price.token_address)?;

    let min_parts = i128_to_int128_parts(price.min);
    let max_parts = i128_to_int128_parts(price.max);

    let entries = vec![
        sc_map_entry("keeper_index", ScVal::U32(0)),
        sc_map_entry("ledger_seq", ScVal::U32(price.ledger_seq)),
        sc_map_entry("max_price", ScVal::I128(max_parts)),
        sc_map_entry("min_price", ScVal::I128(min_parts)),
        sc_map_entry(
            "signature",
            ScVal::Bytes(stellar_xdr::ScBytes(
                sig_bytes
                    .try_into()
                    .map_err(|_| "failed to convert sig bytes".to_string())?,
            )),
        ),
        sc_map_entry("timestamp", ScVal::U64(price.timestamp)),
        sc_map_entry("token", ScVal::Address(contract_addr)),
    ];

    let sc_map = stellar_xdr::ScMap(
        entries
            .try_into()
            .map_err(|e| format!("failed to build ScMap: {e}"))?,
    );
    Ok(ScVal::Map(Some(sc_map)))
}

pub fn encode_prices_vec(prices: &[&CachedPrice]) -> Result<ScVal, String> {
    let encoded: Result<Vec<ScVal>, String> =
        prices.iter().map(|p| encode_signed_price(p)).collect();
    let vals = encoded?;
    let sc_vec: stellar_xdr::ScVec = vals
        .try_into()
        .map_err(|e| format!("failed to build ScVec: {e}"))?;
    Ok(ScVal::Vec(Some(sc_vec)))
}

fn sc_map_entry(key: &str, val: ScVal) -> stellar_xdr::ScMapEntry {
    let sym: stellar_xdr::ScSymbol = key
        .to_string()
        .try_into()
        .expect("sc_map_entry key too long");
    stellar_xdr::ScMapEntry {
        key: ScVal::Symbol(sym),
        val,
    }
}

fn i128_to_int128_parts(value: i128) -> stellar_xdr::Int128Parts {
    let hi = (value >> 64) as i64;
    let lo = value as u64;
    stellar_xdr::Int128Parts { hi, lo }
}

pub fn strkey_to_sc_address(strkey: &str) -> Result<stellar_xdr::ScAddress, String> {
    let decoded = stellar_strkey::Strkey::from_string(strkey)
        .map_err(|e| format!("invalid strkey '{strkey}': {e}"))?;

    match decoded {
        stellar_strkey::Strkey::PublicKeyEd25519(pk) => {
            let mut bytes = [0u8; 32];
            bytes.copy_from_slice(pk.0.as_ref());
            Ok(stellar_xdr::ScAddress::Account(stellar_xdr::AccountId(
                stellar_xdr::PublicKey::PublicKeyTypeEd25519(stellar_xdr::Uint256(bytes)),
            )))
        }
        stellar_strkey::Strkey::Contract(c) => {
            let mut bytes = [0u8; 32];
            bytes.copy_from_slice(c.0.as_ref());
            Ok(stellar_xdr::ScAddress::Contract(stellar_xdr::ContractId(
                stellar_xdr::Hash(bytes),
            )))
        }
        other => Err(format!("unsupported strkey type: {other:?}")),
    }
}

pub fn account_strkey_to_muxed(strkey: &str) -> Result<stellar_xdr::MuxedAccount, String> {
    let decoded = stellar_strkey::Strkey::from_string(strkey)
        .map_err(|e| format!("invalid strkey '{strkey}': {e}"))?;

    match decoded {
        stellar_strkey::Strkey::PublicKeyEd25519(pk) => {
            let mut bytes = [0u8; 32];
            bytes.copy_from_slice(pk.0.as_ref());
            Ok(stellar_xdr::MuxedAccount::Ed25519(stellar_xdr::Uint256(
                bytes,
            )))
        }
        other => Err(format!("expected G... account strkey, got: {other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_i128_to_int128_parts_roundtrip() {
        let val: i128 = 1_000_000_000_000_000_000;
        let parts = i128_to_int128_parts(val);
        let reconstructed = ((parts.hi as i128) << 64) | (parts.lo as i128);
        assert_eq!(val, reconstructed);
    }

    #[test]
    fn test_i128_to_int128_parts_zero() {
        let parts = i128_to_int128_parts(0);
        assert_eq!(parts.hi, 0);
        assert_eq!(parts.lo, 0);
    }

    #[test]
    fn test_i128_to_int128_parts_negative() {
        let val: i128 = -1;
        let parts = i128_to_int128_parts(val);
        let reconstructed = ((parts.hi as i128) << 64) | (parts.lo as i128);
        assert_eq!(val, reconstructed);
    }

    #[test]
    fn test_strkey_to_sc_address_account() {
        let addr = "GAUHMCMUP5FZO5675W3ISZ6E6CNYJGXBUW5WANE2JR4TGAARYCTSCBKI";
        let sc_addr = strkey_to_sc_address(addr).unwrap();
        assert!(matches!(sc_addr, stellar_xdr::ScAddress::Account(_)));
    }

    #[test]
    fn test_encode_signed_price_produces_sorted_map() {
        let price = CachedPrice {
            token_address: "GAUHMCMUP5FZO5675W3ISZ6E6CNYJGXBUW5WANE2JR4TGAARYCTSCBKI".to_string(),
            symbol: "TUSDC".to_string(),
            display_symbol: "USDC".to_string(),
            min: 1_000_000_000_000_000_000_000_000_000_000,
            max: 1_000_000_000_000_000_000_000_000_000_000,
            median: 1_000_000_000_000_000_000_000_000_000_000,
            timestamp: 1718400000,
            ledger_seq: 12345,
            sources_used: vec!["fixed".to_string()],
            signature: "00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000".to_string(),
        };

        let scval = encode_signed_price(&price).unwrap();
        match scval {
            ScVal::Map(Some(map)) => {
                let entries: Vec<_> = map.0.iter().collect();
                assert_eq!(entries.len(), 7);
                let keys: Vec<String> = entries
                    .iter()
                    .map(|e| match &e.key {
                        ScVal::Symbol(s) => String::from_utf8_lossy(s.as_ref()).to_string(),
                        _ => panic!("expected symbol key"),
                    })
                    .collect();
                let mut sorted_clone = keys.clone();
                sorted_clone.sort();
                assert_eq!(keys, sorted_clone, "map keys must be alphabetically sorted");
            }
            _ => panic!("expected ScVal::Map"),
        }
    }

    #[test]
    fn test_encode_prices_vec() {
        let price = CachedPrice {
            token_address: "GAUHMCMUP5FZO5675W3ISZ6E6CNYJGXBUW5WANE2JR4TGAARYCTSCBKI".to_string(),
            symbol: "TUSDC".to_string(),
            display_symbol: "USDC".to_string(),
            min: 1_000_000_000_000_000_000_000_000_000_000,
            max: 1_000_000_000_000_000_000_000_000_000_000,
            median: 1_000_000_000_000_000_000_000_000_000_000,
            timestamp: 1718400000,
            ledger_seq: 12345,
            sources_used: vec!["fixed".to_string()],
            signature: "00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000".to_string(),
        };

        let scval = encode_prices_vec(&[&price]).unwrap();
        match scval {
            ScVal::Vec(Some(vec)) => {
                assert_eq!(vec.0.len(), 1);
            }
            _ => panic!("expected ScVal::Vec"),
        }
    }
}
