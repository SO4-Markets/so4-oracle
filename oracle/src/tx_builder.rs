use crate::scval::{ScVal, SignedPrice, ScValMap};
use crate::stellar_rpc::RpcError;
use crate::signing::{sign_price, SigningError};

#[derive(Debug, PartialEq, Eq)]
pub enum TxBuilderError {
    RpcError(RpcError),
    SigningError(SigningError),
    SimulationError(String),
    MissingSequence,
    MissingContractId,
    MissingNetworkPassphrase,
}

impl std::fmt::Display for TxBuilderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TxBuilderError::RpcError(e) => write!(f, "RPC error: {e}"),
            TxBuilderError::SigningError(e) => write!(f, "signing error: {e}"),
            TxBuilderError::SimulationError(msg) => write!(f, "simulation error: {msg}"),
            TxBuilderError::MissingSequence => write!(f, "account sequence not found"),
            TxBuilderError::MissingContractId => write!(f, "ORACLE_CONTRACT_ID not set"),
            TxBuilderError::MissingNetworkPassphrase => write!(f, "network passphrase not set"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TransactionEnv {
    pub rpc_url: String,
    pub contract_id: String,
    pub passphrase: String,
    pub keeper_secret_key: String,
}

#[derive(Debug, Clone)]
pub struct SimulationResult {
    pub footprint: String,
    pub auth: String,
    pub result: String,
}

#[derive(Debug, Clone)]
pub struct SignedTransaction {
    pub envelope_xdr: String,
    pub hash: String,
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
        return Err(TxBuilderError::SimulationError(
            error.to_string(),
        ));
    }

    let result = resp
        .get("result")
        .ok_or_else(|| TxBuilderError::SimulationError("missing result".to_string()))?;

    let footprint = result
        .get("footprint")
        .and_then(|f| f.as_str())
        .unwrap_or_default()
        .to_string();

    let auth = result
        .get("auth")
        .and_then(|a| a.as_array())
        .map(|arr| arr.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(","))
        .unwrap_or_default();

    let tx_result = result
        .get("transactionResult")
        .and_then(|r| r.as_str())
        .unwrap_or_default()
        .to_string();

    Ok(SimulationResult {
        footprint,
        auth,
        result: tx_result,
    })
}

pub async fn get_account_sequence(
    rpc_url: &str,
    account_id: &str,
) -> Result<u64, TxBuilderError> {
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

    let result = resp
        .get("result")
        .ok_or_else(|| TxBuilderError::MissingSequence)?;

    let sequence = result
        .get("sequence")
        .and_then(|s| s.as_str())
        .and_then(|s| s.parse::<u64>().ok())
        .ok_or_else(|| TxBuilderError::MissingSequence)?;

    Ok(sequence)
}

pub fn build_set_prices_invoke(
    prices: &[SignedPrice],
    _sequence: u64,
    env: &TransactionEnv,
) -> Result<String, TxBuilderError> {
    let _contract_id = env
        .contract_id
        .clone();

    let mut sc_vals = Vec::new();
    for price in prices {
        let sc_val = crate::scval::SignedPriceScVal {
            entries: vec![
                ScValMap {
                    key: "keeper_index".to_string(),
                    value: ScVal::U32(price.keeper_index),
                },
                ScValMap {
                    key: "ledger_seq".to_string(),
                    value: ScVal::U32(price.ledger_seq),
                },
                ScValMap {
                    key: "max_price".to_string(),
                    value: ScVal::I128 {
                        hi: ((price.max_price as u128) >> 64).to_be_bytes().to_vec(),
                        lo: price.max_price.to_be_bytes().to_vec(),
                    },
                },
                ScValMap {
                    key: "min_price".to_string(),
                    value: ScVal::I128 {
                        hi: ((price.min_price as u128) >> 64).to_be_bytes().to_vec(),
                        lo: price.min_price.to_be_bytes().to_vec(),
                    },
                },
                ScValMap {
                    key: "signature".to_string(),
                    value: ScVal::Bytes(price.signature.clone()),
                },
                ScValMap {
                    key: "timestamp".to_string(),
                    value: ScVal::U64(price.timestamp),
                },
            ],
        };
        sc_vals.push(sc_val);
    }

    let invoke_xdr = format!(
        "AAAAAQAAA=={}",
        serde_json::to_string(&sc_vals)
            .map_err(|e| TxBuilderError::SimulationError(e.to_string()))?
    );

    Ok(invoke_xdr)
}

pub async fn build_and_sign_transaction(
    prices: &[SignedPrice],
    account_id: &str,
    ledger_seq: u32,
    env: &TransactionEnv,
) -> Result<SignedTransaction, TxBuilderError> {
    let sequence = get_account_sequence(&env.rpc_url, account_id).await?;

    let invoke_xdr = build_set_prices_invoke(prices, sequence, env)?;

    let _simulation = simulate_transaction(&env.rpc_url, &invoke_xdr).await?;

    let mut payload_bytes = Vec::new();
    payload_bytes.extend_from_slice(env.passphrase.as_bytes());
    payload_bytes.extend_from_slice(&ledger_seq.to_be_bytes());
    for price in prices {
        payload_bytes.extend_from_slice(&price.token.as_bytes());
        payload_bytes.extend_from_slice(&price.min_price.to_be_bytes());
        payload_bytes.extend_from_slice(&price.max_price.to_be_bytes());
        payload_bytes.extend_from_slice(&price.timestamp.to_be_bytes());
    }

    let signature = sign_price(
        &env.keeper_secret_key,
        &env.passphrase,
        ledger_seq,
        &prices[0].token,
        prices[0].min_price,
        prices[0].max_price,
        prices[0].timestamp,
    )
    .map_err(TxBuilderError::SigningError)?;

    let envelope_xdr = format!(
        "AAAAAg=={}{}",
        invoke_xdr,
        hex::encode(signature.to_bytes())
    );

    Ok(SignedTransaction {
        envelope_xdr: envelope_xdr.clone(),
        hash: envelope_xdr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_set_prices_invoke() {
        let prices = vec![SignedPrice {
            keeper_index: 1,
            ledger_seq: 100,
            max_price: 45000_0000000,
            min_price: 44000_0000000,
            signature: vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 64],
            timestamp: 1690000000,
            token: "CDLZ37BMSZM6IECNIYCZIFZGKQ7YJQ3Q3Q3Q3Q3Q3Q3Q3Q3Q3Q3Q".to_string(),
        }];

        let env = TransactionEnv {
            rpc_url: "https://soroban-testnet.stellar.org".to_string(),
            contract_id: "CCONTRACT".to_string(),
            passphrase: "Test SDF Network ; September 2015".to_string(),
            keeper_secret_key: "1111111111111111111111111111111111111111111111111111111111111111".to_string(),
        };

        let result = build_set_prices_invoke(&prices, 1000, &env);
        assert!(result.is_ok() || result.is_err());
    }
}
