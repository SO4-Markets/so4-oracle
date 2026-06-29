use sha2::{Digest, Sha256};
use stellar_xdr::{
    AccountId, DecoratedSignature, Hash as XdrHash, HostFunction, InvokeContractArgs, Memo,
    MuxedAccount, Operation, OperationBody, Preconditions, PublicKey, ReadXdr, ScAddress, ScSymbol,
    ScVal, SequenceNumber, Signature, Transaction, TransactionEnvelope, TransactionExt,
    TransactionV1Envelope, Uint256, WriteXdr,
};

use crate::scval::{encode_signed_prices_vec, ScValError, SignedPrice};
use crate::stellar_rpc::RpcError;

#[derive(Debug, PartialEq)]
pub enum TxBuilderError {
    RpcError(RpcError),
    ScValError(ScValError),
    SimulationError(String),
    XdrError(String),
    MissingSequence,
    MissingFootprint,
    InvalidKey(String),
}

impl std::fmt::Display for TxBuilderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TxBuilderError::RpcError(e) => write!(f, "RPC error: {e}"),
            TxBuilderError::ScValError(e) => write!(f, "ScVal error: {e}"),
            TxBuilderError::SimulationError(msg) => write!(f, "simulation error: {msg}"),
            TxBuilderError::XdrError(msg) => write!(f, "XDR error: {msg}"),
            TxBuilderError::MissingSequence => write!(f, "account sequence not found"),
            TxBuilderError::MissingFootprint => {
                write!(f, "simulation result missing footprint")
            }
            TxBuilderError::InvalidKey(msg) => write!(f, "invalid key: {msg}"),
        }
    }
}

impl std::error::Error for TxBuilderError {}

impl From<ScValError> for TxBuilderError {
    fn from(e: ScValError) -> Self {
        TxBuilderError::ScValError(e)
    }
}

#[derive(Debug, Clone)]
pub struct TransactionEnv {
    pub rpc_url: String,
    pub contract_id: String,
    pub passphrase: String,
    pub keeper_secret_key: String,
    pub keeper_account_id: String,
}

#[derive(Debug, Clone)]
pub struct SimulationResult {
    pub footprint_xdr: String,
    pub resource_fee: u32,
    pub auth: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct SignedTransaction {
    pub envelope_xdr: String,
    pub hash: String,
}

pub async fn get_account_sequence(rpc_url: &str, account_id: &str) -> Result<u64, TxBuilderError> {
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getAccount",
        "params": { "address": account_id }
    });

    let body = crate::stellar_rpc::rpc_post(rpc_url, payload.to_string())
        .await
        .map_err(TxBuilderError::RpcError)?;

    let resp: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| TxBuilderError::SimulationError(e.to_string()))?;

    if let Some(error) = resp.get("error") {
        return Err(TxBuilderError::RpcError(RpcError::RpcFault {
            code: error.get("code").and_then(|c| c.as_i64()).unwrap_or(-1),
            message: error
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown error")
                .to_string(),
        }));
    }

    let result = resp.get("result").ok_or(TxBuilderError::MissingSequence)?;

    result
        .get("sequence")
        .and_then(|s| s.as_str())
        .and_then(|s| s.parse::<u64>().ok())
        .ok_or(TxBuilderError::MissingSequence)
}

pub async fn simulate_transaction(
    rpc_url: &str,
    tx_xdr: &str,
) -> Result<SimulationResult, TxBuilderError> {
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "simulateTransaction",
        "params": { "transaction": tx_xdr }
    });

    let body = crate::stellar_rpc::rpc_post(rpc_url, payload.to_string())
        .await
        .map_err(TxBuilderError::RpcError)?;

    let resp: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| TxBuilderError::SimulationError(e.to_string()))?;

    if let Some(error) = resp.get("error") {
        return Err(TxBuilderError::SimulationError(error.to_string()));
    }

    let result = resp
        .get("result")
        .ok_or_else(|| TxBuilderError::SimulationError("missing result".to_string()))?;

    let footprint = result
        .get("transactionData")
        .and_then(|td| td.as_str())
        .or_else(|| result.get("footprint").and_then(|f| f.as_str()))
        .ok_or(TxBuilderError::MissingFootprint)?
        .to_string();

    let resource_fee = result
        .get("minResourceFee")
        .and_then(|f| f.as_str())
        .and_then(|f| f.parse::<u32>().ok())
        .unwrap_or(100_000);

    let auth = result
        .get("auth")
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    Ok(SimulationResult {
        footprint_xdr: footprint,
        resource_fee,
        auth,
    })
}

fn secret_key_to_ed25519(secret: &str) -> Result<ed25519_dalek::SigningKey, TxBuilderError> {
    if secret.len() == 64 {
        if let Ok(bytes) = hex::decode(secret) {
            if let Ok(arr) = <[u8; 32]>::try_from(bytes) {
                return Ok(ed25519_dalek::SigningKey::from_bytes(&arr));
            }
        }
    }
    let sk: stellar_strkey::ed25519::PrivateKey =
        stellar_strkey::ed25519::PrivateKey::from_string(secret)
            .map_err(|e| TxBuilderError::InvalidKey(e.to_string()))?;
    Ok(ed25519_dalek::SigningKey::from_bytes(&sk.0))
}

fn contract_str_to_sc_address(contract: &str) -> Result<ScAddress, TxBuilderError> {
    let c: stellar_strkey::Contract = contract
        .parse()
        .map_err(|e: stellar_strkey::DecodeError| TxBuilderError::InvalidKey(e.to_string()))?;
    Ok(ScAddress::Contract(stellar_xdr::ContractId(XdrHash(c.0))))
}

pub fn build_unsigned_tx_xdr(
    prices: &[SignedPrice],
    account_id: &str,
    sequence: u64,
    env: &TransactionEnv,
) -> Result<String, TxBuilderError> {
    let pk: stellar_strkey::ed25519::PublicKey =
        stellar_strkey::ed25519::PublicKey::from_string(account_id)
            .map_err(|e| TxBuilderError::InvalidKey(e.to_string()))?;

    let source_account = MuxedAccount::Ed25519(Uint256(pk.0));

    let contract_addr = contract_str_to_sc_address(&env.contract_id)?;

    let caller_addr = ScAddress::Account(AccountId(PublicKey::PublicKeyTypeEd25519(Uint256(pk.0))));

    let prices_vec = encode_signed_prices_vec(prices)?;

    let args = vec![ScVal::Address(caller_addr), prices_vec];

    let invoke_fn = HostFunction::InvokeContract(InvokeContractArgs {
        contract_address: contract_addr,
        function_name: ScSymbol(b"set_prices".to_vec().try_into().unwrap()),
        args: args.try_into().unwrap(),
    });

    let op = Operation {
        source_account: None,
        body: OperationBody::InvokeHostFunction(stellar_xdr::InvokeHostFunctionOp {
            host_function: invoke_fn,
            auth: vec![].try_into().unwrap(),
        }),
    };

    let tx = Transaction {
        source_account,
        fee: 1_000_000,
        seq_num: SequenceNumber::from(sequence as i64),
        cond: Preconditions::None,
        memo: Memo::None,
        operations: vec![op].try_into().unwrap(),
        ext: TransactionExt::V0,
    };

    tx.to_xdr_base64(stellar_xdr::Limits::none())
        .map_err(|e| TxBuilderError::XdrError(e.to_string()))
}

pub fn sign_transaction(
    tx_xdr_base64: &str,
    env: &TransactionEnv,
) -> Result<SignedTransaction, TxBuilderError> {
    use ed25519_dalek::Signer;

    let signing_key = secret_key_to_ed25519(&env.keeper_secret_key)?;

    let tx_hash = crate::scval::compute_transaction_hash(&env.passphrase, tx_xdr_base64)?;

    let sig = signing_key.sign(&tx_hash);

    let pk_bytes = signing_key.verifying_key().to_bytes();

    let hint = crate::scval::signature_hint(&env.passphrase, &pk_bytes);

    let decorated_sig = DecoratedSignature {
        hint: hint.into(),
        signature: Signature(sig.to_bytes().to_vec().try_into().unwrap()),
    };

    let tx = Transaction::from_xdr_base64(tx_xdr_base64, stellar_xdr::Limits::none())
        .map_err(|e| TxBuilderError::XdrError(e.to_string()))?;

    let envelope = TransactionEnvelope::Tx(TransactionV1Envelope {
        tx,
        signatures: vec![decorated_sig].try_into().unwrap(),
    });

    let envelope_xdr = envelope
        .to_xdr_base64(stellar_xdr::Limits::none())
        .map_err(|e| TxBuilderError::XdrError(e.to_string()))?;

    let mut hasher = Sha256::new();
    hasher.update(env.passphrase.as_bytes());
    hasher.update(
        &base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &envelope_xdr)
            .map_err(|e| TxBuilderError::XdrError(e.to_string()))?,
    );
    let hash = hex::encode(hasher.finalize());

    Ok(SignedTransaction { envelope_xdr, hash })
}

pub async fn build_and_sign_transaction(
    prices: &[SignedPrice],
    account_id: &str,
    _ledger_seq: u32,
    env: &TransactionEnv,
) -> Result<SignedTransaction, TxBuilderError> {
    let sequence = get_account_sequence(&env.rpc_url, account_id).await?;

    let tx_xdr = build_unsigned_tx_xdr(prices, account_id, sequence, env)?;

    let sim_result = simulate_transaction(&env.rpc_url, &tx_xdr).await?;

    let tx = Transaction::from_xdr_base64(&tx_xdr, stellar_xdr::Limits::none())
        .map_err(|e| TxBuilderError::XdrError(e.to_string()))?;

    let footprint = stellar_xdr::LedgerFootprint::from_xdr_base64(
        &sim_result.footprint_xdr,
        stellar_xdr::Limits::none(),
    )
    .map_err(|e| TxBuilderError::XdrError(format!("invalid footprint XDR: {e}")))?;

    let soroban_data = stellar_xdr::SorobanTransactionData {
        ext: stellar_xdr::SorobanTransactionDataExt::V0,
        resources: stellar_xdr::SorobanResources {
            footprint,
            instructions: 0,
            disk_read_bytes: 0,
            write_bytes: 0,
        },
        resource_fee: sim_result.resource_fee as i64,
    };

    let assembled_tx = Transaction {
        source_account: tx.source_account,
        fee: tx.fee + sim_result.resource_fee,
        seq_num: tx.seq_num,
        cond: tx.cond,
        memo: tx.memo,
        operations: tx.operations,
        ext: TransactionExt::V1(soroban_data),
    };

    let assembled_xdr = assembled_tx
        .to_xdr_base64(stellar_xdr::Limits::none())
        .map_err(|e| TxBuilderError::XdrError(e.to_string()))?;

    sign_transaction(&assembled_xdr, env)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_env() -> TransactionEnv {
        TransactionEnv {
            rpc_url: "https://soroban-testnet.stellar.org".to_string(),
            contract_id: "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4".to_string(),
            passphrase: "Test SDF Network ; September 2015".to_string(),
            keeper_secret_key: "d13eec59465e73ed718ca7c93b3474f5052561168fcbfca5b2dc7ae53ba78876"
                .to_string(),
            keeper_account_id: "GBOWKUHRR4KEUKLY2NM5G54TCHTCAQIVNRKS22FPH72KLKPWZMOLUDVY"
                .to_string(),
        }
    }

    fn test_prices() -> Vec<SignedPrice> {
        vec![SignedPrice {
            keeper_index: 0,
            ledger_seq: 100,
            max_price: 45000_0000000,
            min_price: 44000_0000000,
            signature: vec![0u8; 64],
            timestamp: 1690000000,
            token: "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4".to_string(),
        }]
    }

    #[test]
    fn test_build_unsigned_tx_xdr() {
        let env = test_env();
        let prices = test_prices();

        let result = build_unsigned_tx_xdr(&prices, &env.keeper_account_id, 1000, &env);
        assert!(result.is_ok(), "build_unsigned_tx_xdr failed: {result:?}");

        let tx_xdr = result.unwrap();
        assert!(!tx_xdr.is_empty());

        let tx = Transaction::from_xdr_base64(&tx_xdr, stellar_xdr::Limits::none());
        assert!(tx.is_ok(), "failed to parse built tx XDR: {tx:?}");
    }

    #[test]
    fn test_sign_transaction() {
        let env = test_env();
        let prices = test_prices();

        let tx_xdr = build_unsigned_tx_xdr(&prices, &env.keeper_account_id, 1000, &env).unwrap();
        let result = sign_transaction(&tx_xdr, &env);
        assert!(result.is_ok(), "sign_transaction failed: {result:?}");

        let signed = result.unwrap();
        assert!(!signed.envelope_xdr.is_empty());
        assert!(!signed.hash.is_empty());
        assert_eq!(signed.hash.len(), 64);
    }

    #[test]
    fn test_signed_envelope_is_valid_xdr() {
        let env = test_env();
        let prices = test_prices();

        let tx_xdr = build_unsigned_tx_xdr(&prices, &env.keeper_account_id, 1000, &env).unwrap();
        let signed = sign_transaction(&tx_xdr, &env).unwrap();

        let envelope =
            TransactionEnvelope::from_xdr_base64(&signed.envelope_xdr, stellar_xdr::Limits::none());
        assert!(
            envelope.is_ok(),
            "signed envelope is not valid XDR: {envelope:?}"
        );
    }
}
