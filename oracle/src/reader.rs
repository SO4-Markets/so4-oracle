use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderCount {
    pub count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderKey {
    pub key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderKeys {
    pub keys: Vec<OrderKey>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WithdrawalCount {
    pub count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WithdrawalKey {
    pub key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WithdrawalKeys {
    pub keys: Vec<WithdrawalKey>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ReaderError {
    SimulationError(String),
    RpcError(String),
    DecodeError(String),
}

impl std::fmt::Display for ReaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReaderError::SimulationError(msg) => write!(f, "simulation error: {msg}"),
            ReaderError::RpcError(msg) => write!(f, "RPC error: {msg}"),
            ReaderError::DecodeError(msg) => write!(f, "decode error: {msg}"),
        }
    }
}

pub async fn get_order_count(
    rpc_url: &str,
    reader_contract_id: &str,
    data_store_id: &str,
) -> Result<u32, ReaderError> {
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "simulateTransaction",
        "params": {
            "transaction": {
                "source_account": data_store_id,
                "fee": "100",
                "operations": [{
                    "type": "invoke",
                    "contract": reader_contract_id,
                    "function": "get_order_count",
                    "args": [data_store_id]
                }]
            }
        }
    });

    let body = crate::stellar_rpc::rpc_post(rpc_url, payload.to_string())
        .await
        .map_err(|e| ReaderError::RpcError(e.to_string()))?;

    let resp: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| ReaderError::SimulationError(e.to_string()))?;

    if let Some(error) = resp.get("error") {
        return Err(ReaderError::SimulationError(error.to_string()));
    }

    let result = resp
        .get("result")
        .ok_or_else(|| ReaderError::SimulationError("missing result".to_string()))?;

    let count = result
        .get("transactionResult")
        .and_then(|r| r.as_str())
        .and_then(|r| r.parse::<u32>().ok())
        .unwrap_or(0);

    Ok(count)
}

pub async fn get_order_keys(
    rpc_url: &str,
    reader_contract_id: &str,
    data_store_id: &str,
    start: u32,
    count: u32,
) -> Result<Vec<String>, ReaderError> {
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "simulateTransaction",
        "params": {
            "transaction": {
                "source_account": data_store_id,
                "fee": "100",
                "operations": [{
                    "type": "invoke",
                    "contract": reader_contract_id,
                    "function": "get_order_keys",
                    "args": [data_store_id, start, count]
                }]
            }
        }
    });

    let body = crate::stellar_rpc::rpc_post(rpc_url, payload.to_string())
        .await
        .map_err(|e| ReaderError::RpcError(e.to_string()))?;

    let resp: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| ReaderError::SimulationError(e.to_string()))?;

    if let Some(error) = resp.get("error") {
        return Err(ReaderError::SimulationError(error.to_string()));
    }

    let result = resp
        .get("result")
        .ok_or_else(|| ReaderError::SimulationError("missing result".to_string()))?;

    let keys = result
        .get("transactionResult")
        .and_then(|r| r.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    Ok(keys)
}

pub async fn get_withdrawal_count(
    rpc_url: &str,
    reader_contract_id: &str,
    data_store_id: &str,
) -> Result<u32, ReaderError> {
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "simulateTransaction",
        "params": {
            "transaction": {
                "source_account": data_store_id,
                "fee": "100",
                "operations": [{
                    "type": "invoke",
                    "contract": reader_contract_id,
                    "function": "get_withdrawal_count",
                    "args": [data_store_id]
                }]
            }
        }
    });

    let body = crate::stellar_rpc::rpc_post(rpc_url, payload.to_string())
        .await
        .map_err(|e| ReaderError::RpcError(e.to_string()))?;

    let resp: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| ReaderError::SimulationError(e.to_string()))?;

    if let Some(error) = resp.get("error") {
        return Err(ReaderError::SimulationError(error.to_string()));
    }

    let result = resp
        .get("result")
        .ok_or_else(|| ReaderError::SimulationError("missing result".to_string()))?;

    let count = result
        .get("transactionResult")
        .and_then(|r| r.as_str())
        .and_then(|r| r.parse::<u32>().ok())
        .unwrap_or(0);

    Ok(count)
}

pub async fn get_withdrawal_keys(
    rpc_url: &str,
    reader_contract_id: &str,
    data_store_id: &str,
    start: u32,
    count: u32,
) -> Result<Vec<String>, ReaderError> {
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "simulateTransaction",
        "params": {
            "transaction": {
                "source_account": data_store_id,
                "fee": "100",
                "operations": [{
                    "type": "invoke",
                    "contract": reader_contract_id,
                    "function": "get_withdrawal_keys",
                    "args": [data_store_id, start, count]
                }]
            }
        }
    });

    let body = crate::stellar_rpc::rpc_post(rpc_url, payload.to_string())
        .await
        .map_err(|e| ReaderError::RpcError(e.to_string()))?;

    let resp: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| ReaderError::SimulationError(e.to_string()))?;

    if let Some(error) = resp.get("error") {
        return Err(ReaderError::SimulationError(error.to_string()));
    }

    let result = resp
        .get("result")
        .ok_or_else(|| ReaderError::SimulationError("missing result".to_string()))?;

    let keys = result
        .get("transactionResult")
        .and_then(|r| r.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    Ok(keys)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_order_count_deserialize() {
        let json = r#"{"count": 5}"#;
        let count: OrderCount = serde_json::from_str(json).unwrap();
        assert_eq!(count.count, 5);
    }

    #[test]
    fn test_order_keys_deserialize() {
        let json = r#"{"keys": [{"key": "key1"}, {"key": "key2"}]}"#;
        let keys: OrderKeys = serde_json::from_str(json).unwrap();
        assert_eq!(keys.keys.len(), 2);
    }

    #[test]
    fn test_withdrawal_count_deserialize() {
        let json = r#"{"count": 3}"#;
        let count: WithdrawalCount = serde_json::from_str(json).unwrap();
        assert_eq!(count.count, 3);
    }
}
