//! The `scval` → `tx_builder` boundary as one contract (#1422).
//!
//! Each half tests itself, but the only thing the chain sees is the signed
//! envelope: a `set_prices` invocation built from real `CachedPrice` values,
//! signed, base64-encoded and decoded back must carry exactly the function
//! name, argument count and argument shapes the deployed contract expects.

use stellar_xdr::{
    HostFunction, Limits, OperationBody, ReadXdr, ScSymbol, ScVal, TransactionEnvelope,
};

use super::scval::{encode_prices_vec, strkey_to_sc_address};
use super::tx_builder::{
    build_invoke_tx, sign_transaction_with_key, signing_key_from_secret, TxParams,
    build_invoke_tx, sign_transaction_with_key, signing_key_from_secret, InvokeTxOptions,
};

use crate::network_config::TESTNET_PASSPHRASE;
use crate::state::CachedPrice;

const KEEPER_ACCOUNT: &str = "GDIEVMRSOQV3JKZ2CNUL2RQV4TTNAISKW4NAC25PQUQKGMWJO6DTOAE7";
const ORACLE_CONTRACT: &str = "CBEMTV23SIJJBIST3V5HTMWHR4MHYGHNBIG4M26U4LGUJTWZXTFSVQEY";
const TOKEN_CONTRACT: &str = "CBAN5YU3KRDKPTQ2H76D6S7HQFPRBGUD524F65BUM2RQCITPTRLKWKES";

/// Map keys of one signed price, in the order `encode_signed_price` emits
/// them (ScMap keys must be sorted).
const PRICE_FIELDS: [&str; 7] = [
    "keeper_index",
    "ledger_seq",
    "max_price",
    "min_price",
    "signature",
    "timestamp",
    "token",
];

fn cached_price(symbol: &str, min: i128, max: i128) -> CachedPrice {
    CachedPrice {
        token_address: TOKEN_CONTRACT.to_string(),
        symbol: symbol.to_string(),
        display_symbol: symbol.to_string(),
        keeper_index: 2,
        min,
        max,
        median: min / 2 + max / 2,
        timestamp: 1_700_000_000,
        ledger_seq: 123_456,
        sources_used: vec!["fixed".to_string()],
        signature: "ab".repeat(64),
    }
}

fn int128(value: &ScVal) -> i128 {
    let ScVal::I128(parts) = value else {
        panic!("expected I128, got {value:?}");
    };
    (i128::from(parts.hi) << 64) | i128::from(parts.lo)
}

#[test]
fn set_prices_survives_build_sign_encode_decode() {
    let btc = cached_price("BTC", 1, 2);
    let eth = cached_price("ETH", -5, i128::MAX);
    let prices = encode_prices_vec(&[&btc, &eth]).unwrap();

    let tx = build_invoke_tx(
        KEEPER_ACCOUNT,
        ORACLE_CONTRACT,
        "set_prices",
        vec![prices.clone()],
        TxParams::new(100, 7, 0),
        100,
        7,
        InvokeTxOptions {
            soroban_data: None,
            max_time: 0,
        },
    )
    .unwrap();
    let key = signing_key_from_secret(&"11".repeat(32)).unwrap();
    let signed = sign_transaction_with_key(&tx, &key, TESTNET_PASSPHRASE).unwrap();

    let envelope = TransactionEnvelope::from_xdr_base64(&signed, Limits::none());
    let Ok(TransactionEnvelope::Tx(v1)) = envelope else {
        panic!("expected a decodable v1 transaction envelope");
    };
    assert_eq!(v1.tx, tx, "envelope must carry the built tx");
    assert_eq!(v1.signatures.len(), 1);

    let operations = v1.tx.operations.to_vec();
    let [operation] = operations.as_slice() else {
        panic!("expected exactly one operation");
    };
    let OperationBody::InvokeHostFunction(invoke) = &operation.body else {
        panic!("expected InvokeHostFunction, got {:?}", operation.body);
    };
    let auth = invoke.auth.to_vec();
    assert!(auth.is_empty(), "set_prices carries no auth entries");
    let HostFunction::InvokeContract(call) = &invoke.host_function else {
        panic!("expected InvokeContract, got {:?}", invoke.host_function);
    };

    let set_prices: ScSymbol = "set_prices".to_string().try_into().unwrap();
    assert_eq!(call.function_name, set_prices);
    let oracle = strkey_to_sc_address(ORACLE_CONTRACT).unwrap();
    assert_eq!(call.contract_address, oracle);

    // One argument: the Vec of signed prices, byte-identical to what was encoded.
    let args = call.args.to_vec();
    let [arg] = args.as_slice() else {
        panic!("set_prices takes one argument, got {}", args.len());
    };
    assert_eq!(arg, &prices);

    let ScVal::Vec(Some(entries)) = arg else {
        panic!("expected Vec<SignedPrice>, got {arg:?}");
    };
    let entries = entries.0.to_vec();
    assert_eq!(entries.len(), 2);

    let keys: Vec<ScVal> = PRICE_FIELDS
        .iter()
        .map(|name| ScVal::Symbol(name.to_string().try_into().unwrap()))
        .collect();

    for (entry, expected) in entries.iter().zip([&btc, &eth]) {
        let ScVal::Map(Some(map)) = entry else {
            panic!("expected a SignedPrice map, got {entry:?}");
        };
        let fields = map.0.to_vec();
        let got_keys: Vec<ScVal> = fields.iter().map(|field| field.key.clone()).collect();
        assert_eq!(got_keys, keys);

        assert_eq!(fields[0].val, ScVal::U32(expected.keeper_index));
        assert_eq!(fields[1].val, ScVal::U32(expected.ledger_seq));
        assert_eq!(int128(&fields[2].val), expected.max);
        assert_eq!(int128(&fields[3].val), expected.min);
        let ScVal::Bytes(signature) = &fields[4].val else {
            panic!("expected signature Bytes, got {:?}", fields[4].val);
        };
        let expected_signature = hex::decode(&expected.signature).unwrap();
        assert_eq!(signature.0.to_vec(), expected_signature);
        assert_eq!(fields[5].val, ScVal::U64(expected.timestamp));
        let token = strkey_to_sc_address(&expected.token_address).unwrap();
        assert_eq!(fields[6].val, ScVal::Address(token));
    }
}
